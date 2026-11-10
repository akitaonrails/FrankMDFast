//! Port of `test/services/notes_service_test.rb` (FrankMD @ d47a89e).
//!
//! The `search_content` group (14 tests, lines 571-692) belongs to
//! `frankmd-search` and is ported there. mocha stubs become
//! `frankmd_fs::fault` injections; Ruby threads + queues become std threads +
//! a `Barrier`.
#![allow(clippy::disallowed_methods, clippy::unwrap_used)]

#[allow(missing_docs)]
mod common;

use common::{Sandbox, sha256_hex};
use frankmd_core::tree::{FileType, TreeNode};
use frankmd_fs::errno::{ENOENT, ENOSPC, EOPNOTSUPP, EROFS, EXDEV};
use frankmd_fs::fault::{self, FaultPoint};
use frankmd_fs::{FsError, NotesService, Vault};
use std::io::ErrorKind;
use std::sync::{Arc, Barrier};

/// `create_test_note(path, content = "# Test\n\nContent")`.
fn note(sb: &Sandbox, rel: &str) {
    sb.write(rel, "# Test\n\nContent");
}

fn names(nodes: &[TreeNode]) -> Vec<String> {
    nodes.iter().map(|n| n.name().to_owned()).collect()
}

fn children(node: &TreeNode) -> &[TreeNode] {
    match node {
        TreeNode::Folder { children, .. } => children,
        TreeNode::File { .. } => &[],
    }
}

fn is_folder(n: &TreeNode) -> bool {
    matches!(n, TreeNode::Folder { .. })
}

fn find<'a>(nodes: &'a [TreeNode], name: &str) -> Option<&'a TreeNode> {
    nodes.iter().find(|n| n.name() == name)
}

fn root_children(sb: &Sandbox) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(&sb.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn no_tmp_files(sb: &Sandbox) -> bool {
    !root_children(sb).iter().any(|n| n.ends_with(".tmp"))
}

// === list_tree ===

// port: test/services/notes_service_test.rb:17 "list_tree returns empty array for empty directory"
#[test]
fn list_tree_returns_empty_array_for_empty_directory() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    assert!(NotesService::new(&vault).list_tree().unwrap().is_empty());
}

// port: test/services/notes_service_test.rb:21 "list_tree returns files with correct structure"
#[test]
fn list_tree_returns_files_with_correct_structure() {
    let sb = Sandbox::empty();
    note(&sb, "note1.md");
    note(&sb, "note2.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert_eq!(tree.len(), 2);
    assert!(tree.iter().all(|n| !is_folder(n)));
    let mut n = names(&tree);
    n.sort();
    assert_eq!(n, ["note1", "note2"]);
}

// port: test/services/notes_service_test.rb:31 "list_tree returns nested folders with children"
#[test]
fn list_tree_returns_nested_folders_with_children() {
    let sb = Sandbox::empty();
    sb.mkdir("folder1");
    note(&sb, "folder1/nested.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert_eq!(tree.len(), 1);
    let folder = &tree[0];
    assert!(is_folder(folder));
    assert_eq!(folder.name(), "folder1");
    assert_eq!(children(folder).len(), 1);
    assert_eq!(children(folder)[0].name(), "nested");
}

// port: test/services/notes_service_test.rb:45 "list_tree keeps root images/videos visible when they contain markdown"
#[test]
fn list_tree_keeps_root_media_folders_with_markdown() {
    let sb = Sandbox::empty();
    sb.write("images/photo.png", "media");
    sb.mkdir("images/notes");
    note(&sb, "images/notes/guide.md");
    sb.write("videos/clip.mp4", "media");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    let images =
        find(&tree, "images").expect("root images/ containing markdown must stay in the tree");
    let guide = children(images)
        .iter()
        .flat_map(|c| children(c).iter())
        .find(|n| n.name() == "guide");
    assert!(
        guide.is_some(),
        "nested markdown must be reachable through the tree"
    );
    assert!(find(&tree, "videos").is_none());
}

// port: test/services/notes_service_test.rb:63 "list_tree hides only root managed media folders"
#[test]
fn list_tree_hides_only_root_managed_media_folders() {
    let sb = Sandbox::empty();
    sb.write("images/photo.png", "media");
    sb.write("videos/clip.mp4", "media");
    note(&sb, "images-old/note.md");
    note(&sb, "archive/images/note.md");
    note(&sb, "archive/videos/note.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert!(
        !tree
            .iter()
            .any(|n| n.name() == "images" || n.name() == "videos")
    );
    let images_old = find(&tree, "images-old").unwrap();
    assert_eq!(children(images_old)[0].name(), "note");
    let archive = find(&tree, "archive").unwrap();
    let mut n = names(children(archive));
    n.sort();
    assert_eq!(n, ["images", "videos"]);
}

// port: test/services/notes_service_test.rb:84 "list_tree sorts folders before files"
#[test]
fn list_tree_sorts_folders_before_files() {
    let sb = Sandbox::empty();
    note(&sb, "zebra.md");
    sb.mkdir("alpha");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert!(is_folder(tree.first().unwrap()));
    assert!(!is_folder(tree.last().unwrap()));
}

// port: test/services/notes_service_test.rb:93 "list_tree keeps folder order stable regardless of mtime updates"
#[test]
fn list_tree_keeps_folder_order_stable_regardless_of_mtime() {
    let sb = Sandbox::empty();
    sb.mkdir("alpha");
    sb.mkdir("beta");
    note(&sb, "note.md");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let beta = std::fs::File::open(sb.abs("beta")).unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs((now + 60) as u64);
    beta.set_modified(t).unwrap();
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    let folders: Vec<_> = tree
        .iter()
        .filter(|n| is_folder(n))
        .map(|n| n.name().to_owned())
        .collect();
    assert_eq!(folders, ["alpha", "beta"]);
}

// port: test/services/notes_service_test.rb:109 "list_tree skips broken symlinks instead of raising"
#[test]
fn list_tree_skips_broken_symlinks() {
    let sb = Sandbox::empty();
    note(&sb, "real.md");
    std::os::unix::fs::symlink("/nonexistent/target.md", sb.abs("dangling.md")).unwrap();
    let vault = sb.vault();
    assert_eq!(
        names(&NotesService::new(&vault).list_tree().unwrap()),
        ["real"]
    );
}

// port: test/services/notes_service_test.rb:118 "list_tree and search skip symlinked directories"
// (search half: frankmd-search)
#[test]
fn list_tree_skips_symlinked_directories() {
    let sb = Sandbox::empty();
    std::fs::write(sb.outside.join("external.md"), "findme").unwrap();
    std::os::unix::fs::symlink(&sb.outside, sb.abs("external")).unwrap();
    let vault = sb.vault();
    assert!(find(&NotesService::new(&vault).list_tree().unwrap(), "external").is_none());
}

// port: test/services/notes_service_test.rb:128 "tree tolerates deletion during metadata enumeration"
#[test]
fn tree_tolerates_deletion_during_metadata_enumeration() {
    let sb = Sandbox::empty();
    note(&sb, "vanished.md");
    let _g = fault::inject(
        FaultPoint::Lstat("vanished.md".into()),
        ErrorKind::NotFound,
        Some(ENOENT),
        false,
    );
    let vault = sb.vault();
    assert!(NotesService::new(&vault).list_tree().unwrap().is_empty());
}

// port: test/services/notes_service_test.rb:135 "tree does not stat hidden configuration temporary files"
#[test]
fn tree_does_not_stat_hidden_configuration_temporary_files() {
    let sb = Sandbox::empty();
    sb.write(".fed.transient.tmp", "x");
    let point = FaultPoint::Lstat(".fed.transient.tmp".into());
    let _g = fault::inject(point.clone(), ErrorKind::Other, None, false);
    let vault = sb.vault();
    assert!(NotesService::new(&vault).list_tree().unwrap().is_empty());
    assert_eq!(
        fault::hits(&point),
        0,
        "hidden entries must be skipped before lstat"
    );
}

// port: test/services/notes_service_test.rb:142 "list_tree ignores hidden files"
#[test]
fn list_tree_ignores_hidden_files() {
    let sb = Sandbox::empty();
    note(&sb, ".hidden.md");
    note(&sb, "visible.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert_eq!(names(&tree), ["visible"]);
}

// port: test/services/notes_service_test.rb:151 "list_tree omits .fed config file"
#[test]
fn list_tree_omits_fed_config_file() {
    let sb = Sandbox::empty();
    sb.write(".fed", "theme = dark");
    note(&sb, "note.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert_eq!(tree.len(), 1);
    assert!(find(&tree, ".fed").is_none());
}

// port: test/services/notes_service_test.rb:160 "list_tree omits .fed in subfolders"
#[test]
fn list_tree_omits_fed_in_subfolders() {
    let sb = Sandbox::empty();
    sb.write("subfolder/.fed", "theme = dark");
    note(&sb, "subfolder/note.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    let folder = tree.iter().find(|n| is_folder(n)).unwrap();
    assert_eq!(names(children(folder)), ["note"]);
}

// port: test/services/notes_service_test.rb:174 "list_tree marks markdown files with file_type"
#[test]
fn list_tree_marks_markdown_files_with_file_type() {
    let sb = Sandbox::empty();
    note(&sb, "note.md");
    let vault = sb.vault();
    let tree = NotesService::new(&vault).list_tree().unwrap();
    assert!(matches!(
        tree[0],
        TreeNode::File {
            file_type: FileType::Markdown,
            ..
        }
    ));
    assert_eq!(common::tree_json(&tree)[0]["file_type"], "markdown");
}

// === read ===

// port: test/services/notes_service_test.rb:183 "read returns file content"
#[test]
fn read_returns_file_content() {
    let sb = Sandbox::empty();
    sb.write("test.md", "Hello World");
    let vault = sb.vault();
    assert_eq!(
        NotesService::new(&vault).read("test.md").unwrap().bytes,
        b"Hello World"
    );
}

// port: test/services/notes_service_test.rb:190 "read raises NotFoundError for missing file"
#[test]
fn read_missing_is_not_found() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    assert!(matches!(
        NotesService::new(&vault).read("nonexistent.md"),
        Err(FsError::NotFound(_))
    ));
}

// === write ===

// port: test/services/notes_service_test.rb:198 "write creates new file"
#[test]
fn write_creates_new_file() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    NotesService::new(&vault)
        .write("new.md", b"New content")
        .unwrap();
    assert_eq!(sb.read("new.md"), "New content");
}

// port: test/services/notes_service_test.rb:205 "create refuses to replace an existing note"
#[test]
fn create_refuses_to_replace_existing_note() {
    let sb = Sandbox::empty();
    sb.write("existing.md", "Original content");
    let vault = sb.vault();
    let r = NotesService::new(&vault).create("existing.md", b"replacement");
    assert!(matches!(r, Err(FsError::AlreadyExists(_))), "{r:?}");
    assert_eq!(sb.read("existing.md"), "Original content");
}

// port: test/services/notes_service_test.rb:215 "create falls back to an atomic rename when hard links are unsupported"
#[test]
fn create_falls_back_to_rename_without_hard_links() {
    let sb = Sandbox::empty();
    let _g = fault::inject(
        FaultPoint::HardLink,
        ErrorKind::Unsupported,
        Some(EOPNOTSUPP),
        false,
    );
    let vault = sb.vault();
    NotesService::new(&vault)
        .create("fallback.md", b"Created without hard links")
        .unwrap();
    assert_eq!(sb.read("fallback.md"), "Created without hard links");
    assert!(no_tmp_files(&sb));
}

// port: test/services/notes_service_test.rb:224 "concurrent creates for the same destination preserve one winner"
#[test]
fn concurrent_creates_preserve_one_winner() {
    let sb = Sandbox::empty();
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<&str> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let barrier = barrier.clone();
                let (root, state) = (sb.root.clone(), sb.state.clone());
                s.spawn(move || {
                    let vault = Vault::open(root, state).unwrap();
                    barrier.wait();
                    match NotesService::new(&vault)
                        .create("shared.md", format!("writer {i}").as_bytes())
                    {
                        Ok(_) => "created",
                        Err(FsError::AlreadyExists(_)) => "conflict",
                        Err(e) => panic!("unexpected {e:?}"),
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|r| **r == "created").count(), 1);
    assert_eq!(results.iter().filter(|r| **r == "conflict").count(), 1);
    assert!(["writer 0", "writer 1"].contains(&sb.read("shared.md").as_str()));
    assert_eq!(
        root_children(&sb),
        [".frankmd-filesystem.lock", "shared.md"]
    );
}

// port: test/services/notes_service_test.rb:249 "service instances share a NOTES_PATH lock despite independent app tmp directories"
#[test]
fn instances_share_notes_path_lock_despite_independent_state_dirs() {
    use rustix::fs::{FlockOperation, flock};
    let sb = Sandbox::empty();
    let first = Vault::open(&sb.root, sb.tmp_path.join("app-one/tmp")).unwrap();
    let second = Vault::open(&sb.root, sb.tmp_path.join("app-two/tmp")).unwrap();
    let lock_path = first.filesystem_lock_path();
    assert_eq!(lock_path, second.filesystem_lock_path());
    assert_eq!(lock_path, sb.root.join(".frankmd-filesystem.lock"));
    let open = |p: &std::path::Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(p)
            .unwrap()
    };
    let a = open(&lock_path);
    let b = open(&second.filesystem_lock_path());
    flock(&a, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert!(
        flock(&b, FlockOperation::NonBlockingLockExclusive).is_err(),
        "instances sharing NOTES_PATH must contend on the same lock file"
    );
    flock(&a, FlockOperation::Unlock).unwrap();
}

// port: test/services/notes_service_test.rb:270 "list_tree falls back to the app tmp lock when NOTES_PATH is read-only"
#[test]
fn list_tree_falls_back_to_state_lock_when_root_read_only() {
    let sb = Sandbox::empty();
    sb.write("visible.md", "Content");
    let _g = fault::inject(
        FaultPoint::LockOpen,
        ErrorKind::ReadOnlyFilesystem,
        Some(EROFS),
        false,
    );
    let vault = sb.vault();
    assert_eq!(
        names(&NotesService::new(&vault).list_tree().unwrap()),
        ["visible"]
    );
}

// port: test/services/notes_service_test.rb:283 "write overwrites existing file"
#[test]
fn write_overwrites_existing_file() {
    let sb = Sandbox::empty();
    sb.write("existing.md", "Old content");
    let vault = sb.vault();
    NotesService::new(&vault)
        .write("existing.md", b"New content")
        .unwrap();
    assert_eq!(sb.read("existing.md"), "New content");
}

// port: test/services/notes_service_test.rb:290 "update overwrites an existing file"
#[test]
fn update_overwrites_existing_file() {
    let sb = Sandbox::empty();
    sb.write("existing.md", "Old content");
    let vault = sb.vault();
    NotesService::new(&vault)
        .update("existing.md", b"New content")
        .unwrap();
    assert_eq!(sb.read("existing.md"), "New content");
}

// port: test/services/notes_service_test.rb:297 "update raises NotFoundError without creating a missing file or folder"
#[test]
fn update_missing_never_creates() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    let r = NotesService::new(&vault).update("missing/folder/note.md", b"stale content");
    assert!(matches!(r, Err(FsError::NotFound(_))), "{r:?}");
    assert!(!sb.exists("missing"));
}

// port: test/services/notes_service_test.rb:305 "write creates parent directories"
#[test]
fn write_creates_parent_directories() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    NotesService::new(&vault)
        .write("deep/nested/note.md", b"Content")
        .unwrap();
    assert!(sb.exists("deep/nested/note.md"));
}

// port: test/services/notes_service_test.rb:311 "write does not leave temp files behind"
#[test]
fn write_leaves_no_temp_files() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    NotesService::new(&vault)
        .write("note.md", b"content")
        .unwrap();
    assert!(
        no_tmp_files(&sb),
        "atomic write should clean up its temp file"
    );
}

// port: test/services/notes_service_test.rb:318 "write preserves the existing note's file mode across the atomic swap"
#[test]
fn write_preserves_existing_mode() {
    let sb = Sandbox::empty();
    sb.write("note.md", "original");
    sb.chmod("note.md", 0o600);
    let vault = sb.vault();
    NotesService::new(&vault)
        .write("note.md", b"updated")
        .unwrap();
    assert_eq!(
        sb.stat_mode("note.md"),
        "600",
        "atomic write must not reset the note's permission bits"
    );
    assert_eq!(sb.read("note.md"), "updated");
}

// port: test/services/notes_service_test.rb:329 "write lands its temp file in the target's own directory (same-filesystem rename)"
// Partial port: the Rails test captured `File.rename`'s source with a stub.
// `fault` has no observer for the rename source path, so we fail the final
// rename and check the temp never appeared outside the target dir and was
// cleaned from it (frankmd_fs::atomic::temp_name gives its [COMPAT] name).
#[test]
fn write_temp_file_lives_in_target_directory() {
    let sb = Sandbox::empty();
    sb.mkdir("sub");
    let _g = fault::inject(
        FaultPoint::AtomicRename,
        ErrorKind::StorageFull,
        Some(ENOSPC),
        false,
    );
    let vault = sb.vault();
    assert!(
        NotesService::new(&vault)
            .write("sub/note.md", b"content")
            .is_err()
    );
    assert_eq!(fault::hits(&FaultPoint::AtomicRename), 1);
    assert!(
        no_tmp_files(&sb),
        "temp must not be created in the vault root"
    );
    let tmp = frankmd_fs::atomic::temp_name("note.md");
    assert!(
        tmp.starts_with(".note.md.") && tmp.ends_with(".tmp"),
        "{tmp}"
    );
}

// port: test/services/notes_service_test.rb:346 "write preserves the original note when the atomic rename fails"
#[test]
fn write_preserves_original_when_rename_fails() {
    let sb = Sandbox::empty();
    sb.write("note.md", "Original content");
    let _g = fault::inject(
        FaultPoint::AtomicRename,
        ErrorKind::StorageFull,
        Some(ENOSPC),
        false,
    );
    let vault = sb.vault();
    match NotesService::new(&vault).write("note.md", b"New content that fails") {
        Err(FsError::Io(e)) => assert_eq!(e.raw_os_error(), Some(ENOSPC)),
        other => panic!("expected ENOSPC, got {other:?}"),
    }
    assert_eq!(sb.read("note.md"), "Original content");
    assert!(
        no_tmp_files(&sb),
        "failed atomic write should not leave a temp file behind"
    );
}

// port: test/services/notes_service_test.rb:359 "write creates Hugo blog post directory structure"
#[test]
fn write_creates_hugo_directory_structure() {
    let sb = Sandbox::empty();
    let hugo_path = "2026/01/30/my-first-post/index.md";
    let content = "---\ntitle: \"My First Post\"\nslug: \"my-first-post\"\ndate: 2026-01-30T14:30:00-0300\ndraft: true\ntags:\n-\n---\n\nPost content goes here.\n";
    let vault = sb.vault();
    NotesService::new(&vault)
        .write(hugo_path, content.as_bytes())
        .unwrap();
    for d in ["2026", "2026/01", "2026/01/30", "2026/01/30/my-first-post"] {
        assert!(sb.abs(d).is_dir(), "{d}");
    }
    assert!(sb.abs(hugo_path).is_file());
    assert_eq!(sb.read(hugo_path), content);
}

// === delete ===

// port: test/services/notes_service_test.rb:387 "delete removes file"
#[test]
fn delete_removes_file() {
    let sb = Sandbox::empty();
    note(&sb, "to_delete.md");
    let vault = sb.vault();
    NotesService::new(&vault)
        .delete("to_delete.md", None)
        .unwrap();
    assert!(!sb.exists("to_delete.md"));
}

// port: test/services/notes_service_test.rb:394 "delete removes file when expected revision matches"
#[test]
fn delete_with_matching_revision() {
    let sb = Sandbox::empty();
    sb.write("to_delete.md", "current content");
    let vault = sb.vault();
    let rev = sha256_hex(b"current content");
    NotesService::new(&vault)
        .delete("to_delete.md", Some(&rev))
        .unwrap();
    assert!(!sb.exists("to_delete.md"));
}

// port: test/services/notes_service_test.rb:406 "delete preserves file when expected revision does not match"
#[test]
fn delete_with_stale_revision_conflicts() {
    let sb = Sandbox::empty();
    sb.write("to_delete.md", "updated content");
    let vault = sb.vault();
    let rev = sha256_hex(b"old content");
    let r = NotesService::new(&vault).delete("to_delete.md", Some(&rev));
    assert!(matches!(r, Err(FsError::RevisionConflict(_))), "{r:?}");
    assert_eq!(sb.read("to_delete.md"), "updated content");
}

// port: test/services/notes_service_test.rb:416 "delete raises NotFoundError for missing file"
#[test]
fn delete_missing_is_not_found() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    assert!(matches!(
        NotesService::new(&vault).delete("nonexistent.md", None),
        Err(FsError::NotFound(_))
    ));
}

// === rename ===

// port: test/services/notes_service_test.rb:424 "rename moves file to new location"
#[test]
fn rename_moves_file() {
    let sb = Sandbox::empty();
    sb.write("old.md", "Content");
    let vault = sb.vault();
    NotesService::new(&vault)
        .rename("old.md", "new.md")
        .unwrap();
    assert!(!sb.exists("old.md"));
    assert_eq!(sb.read("new.md"), "Content");
}

// port: test/services/notes_service_test.rb:434 "rename moves file to different folder"
#[test]
fn rename_moves_file_to_other_folder() {
    let sb = Sandbox::empty();
    sb.write("root.md", "Content");
    sb.mkdir("subfolder");
    let vault = sb.vault();
    NotesService::new(&vault)
        .rename("root.md", "subfolder/moved.md")
        .unwrap();
    assert!(!sb.exists("root.md"));
    assert!(sb.exists("subfolder/moved.md"));
}

// port: test/services/notes_service_test.rb:444 "rename uses FileUtils cross-device fallback when rename raises EXDEV"
#[test]
fn rename_cross_device_fallback() {
    let sb = Sandbox::empty();
    sb.write("source.md", "Cross-device content");
    let _g = fault::inject(
        FaultPoint::Rename,
        ErrorKind::CrossesDevices,
        Some(EXDEV),
        false,
    );
    let vault = sb.vault();
    NotesService::new(&vault)
        .rename("source.md", "nested/destination.md")
        .unwrap();
    assert!(!sb.exists("source.md"));
    assert_eq!(sb.read("nested/destination.md"), "Cross-device content");
}

// port: test/services/notes_service_test.rb:454 "concurrent renames to the same destination preserve the losing source"
#[test]
fn concurrent_renames_preserve_losing_source() {
    let sb = Sandbox::empty();
    sb.write("first.md", "first content");
    sb.write("second.md", "second content");
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<(bool, &str)> = std::thread::scope(|s| {
        let handles: Vec<_> = ["first.md", "second.md"]
            .into_iter()
            .map(|src| {
                let barrier = barrier.clone();
                let (root, state) = (sb.root.clone(), sb.state.clone());
                s.spawn(move || {
                    let vault = Vault::open(root, state).unwrap();
                    barrier.wait();
                    match NotesService::new(&vault).rename(src, "shared.md") {
                        Ok(()) => (true, src),
                        Err(FsError::AlreadyExists(_)) => (false, src),
                        Err(e) => panic!("unexpected {e:?}"),
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|r| r.0).count(), 1);
    assert_eq!(results.iter().filter(|r| !r.0).count(), 1);
    let winner = results.iter().find(|r| r.0).unwrap().1;
    let loser = results.iter().find(|r| !r.0).unwrap().1;
    let content = |p: &str| format!("{} content", p.trim_end_matches(".md"));
    assert_eq!(sb.read("shared.md"), content(winner));
    assert_eq!(sb.read(loser), content(loser));
    assert!(!sb.exists(winner));
}

// port: test/services/notes_service_test.rb:487 "rename moves folder with contents"
#[test]
fn rename_moves_folder_with_contents() {
    let sb = Sandbox::empty();
    sb.write("old_folder/note.md", "Content");
    let vault = sb.vault();
    NotesService::new(&vault)
        .rename("old_folder", "new_folder")
        .unwrap();
    assert!(!sb.exists("old_folder"));
    assert!(sb.exists("new_folder"));
    assert!(sb.exists("new_folder/note.md"));
}

// port: test/services/notes_service_test.rb:498 "rename raises NotFoundError for missing source"
#[test]
fn rename_missing_source_is_not_found() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    assert!(matches!(
        NotesService::new(&vault).rename("nonexistent.md", "new.md"),
        Err(FsError::NotFound(_))
    ));
}

// port: test/services/notes_service_test.rb:504 "rename rejects an existing destination without modifying either file"
#[test]
fn rename_rejects_existing_destination() {
    let sb = Sandbox::empty();
    sb.write("old.md", "original");
    sb.write("new.md", "destination");
    let vault = sb.vault();
    let err = NotesService::new(&vault)
        .rename("old.md", "new.md")
        .unwrap_err();
    assert!(err.is_invalid_path(), "{err:?}");
    assert!(matches!(err, FsError::AlreadyExists(_)));
    assert_eq!(sb.read("old.md"), "original");
    assert_eq!(sb.read("new.md"), "destination");
}

// === create_folder ===

// port: test/services/notes_service_test.rb:516 "create_folder creates directory"
#[test]
fn create_folder_creates_directory() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    NotesService::new(&vault)
        .create_folder("new_folder")
        .unwrap();
    assert!(sb.abs("new_folder").is_dir());
}

// port: test/services/notes_service_test.rb:522 "create_folder creates nested directories"
#[test]
fn create_folder_creates_nested_directories() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    NotesService::new(&vault)
        .create_folder("deep/nested/folder")
        .unwrap();
    assert!(sb.abs("deep/nested/folder").is_dir());
}

// === delete_folder ===

// port: test/services/notes_service_test.rb:530 "delete_folder removes empty directory"
#[test]
fn delete_folder_removes_empty_directory() {
    let sb = Sandbox::empty();
    sb.mkdir("empty_folder");
    let vault = sb.vault();
    NotesService::new(&vault)
        .delete_folder("empty_folder")
        .unwrap();
    assert!(!sb.exists("empty_folder"));
}

// port: test/services/notes_service_test.rb:537 "delete_folder raises InvalidPathError for non-empty directory"
#[test]
fn delete_folder_non_empty_is_invalid_path() {
    let sb = Sandbox::empty();
    note(&sb, "folder/note.md");
    let vault = sb.vault();
    let err = NotesService::new(&vault)
        .delete_folder("folder")
        .unwrap_err();
    assert!(err.is_invalid_path(), "{err:?}");
    assert_eq!(err.compat_class(), "NotesService::InvalidPathError");
    assert!(sb.exists("folder/note.md"));
}

// === security ===

// port: test/services/notes_service_test.rb:548 "prevents path traversal attacks"
#[test]
fn prevents_path_traversal() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    assert!(matches!(
        NotesService::new(&vault).read("../../../etc/passwd"),
        Err(FsError::InvalidPath(_))
    ));
}

// port: test/services/notes_service_test.rb:554 "rejects paths that traverse out of the base directory"
#[test]
fn rejects_paths_traversing_out_of_base() {
    let sb = Sandbox::empty();
    sb.write("safe.md", "Safe content");
    let vault = sb.vault();
    assert!(matches!(
        NotesService::new(&vault).read("folder/../../../etc/passwd"),
        Err(FsError::InvalidPath(_))
    ));
}

// port: test/services/notes_service_test.rb:564 "allows a filename that legitimately contains double dots"
#[test]
fn allows_double_dots_in_filename() {
    let sb = Sandbox::empty();
    let vault = sb.vault();
    let svc = NotesService::new(&vault);
    svc.write("notes..v2.md", b"kept intact").unwrap();
    assert_eq!(svc.read("notes..v2.md").unwrap().bytes, b"kept intact");
    assert!(sb.exists("notes..v2.md"));
}
