//! Level-b tests for the file dialogs (ui-spec §5.13, docs/inventory/frontend.md
//! §F.7–F.11): New note (empty + Hugo, creation destination), New folder,
//! Move to…, Delete to the Trash, and inline rename (explorer row + title).
#![allow(missing_docs, clippy::unwrap_used)]

extern crate gpui_kit as gpui;

mod support;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Modifiers, TestAppContext};
use support::harness::{self, WIDE};
use support::shell::{Shell, VaultFixture, open_shell};
use support::vm::{FocusRegion, OverlayKind};

fn click(cx: &mut TestAppContext, shell: &Shell, id: &'static str) {
    harness::in_window(cx, shell.window, |window, cx| window.click(id, cx));
}

fn right_click(cx: &mut TestAppContext, shell: &Shell, id: &'static str) {
    harness::in_window(cx, shell.window, |window, cx| window.right_click(id, cx));
}

fn run(cx: &mut TestAppContext, shell: &Shell, id: &'static str) {
    let view = shell.view.clone();
    harness::in_window(cx, shell.window, move |window, cx| {
        view.update(cx, |s, cx| s.run_command(id, window, cx))
    });
}

fn exists(shell: &Shell, path: &str) -> bool {
    shell.root.join(path).exists()
}

fn dialog(cx: &mut TestAppContext, shell: &Shell) -> Option<&'static str> {
    match shell.overlay(cx) {
        Some(OverlayKind::Dialog(kind)) => Some(kind),
        _ => None,
    }
}

// ---- New note ---------------------------------------------------------------------------

// adapts: test/system/notes_test.rb:61 "creating a new note" (F.8: Enter submits,
// the new empty note opens with the editor focused)
#[gpui_kit::test]
fn ctrl_n_creates_a_note_at_the_root_and_opens_it(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 ctrl-n");
    assert_eq!(dialog(cx, &shell), Some("new-note"));
    assert!(harness::is_shown(cx, w, "dialog-new-note"));
    harness::type_text(cx, w, "Meeting notes");
    harness::press(cx, w, "enter");
    assert!(exists(&shell, "Meeting notes.md"));
    assert_eq!(shell.active_note(cx).as_deref(), Some("Meeting notes.md"));
    assert_eq!(shell.overlay(cx), None);
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// adapts: test/system/creation_destination_test.rb:9 "new note goes into the selected folder"
#[gpui_kit::test]
fn new_note_goes_into_the_selected_folder(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    click(cx, &shell, "explorer-row-projects");
    harness::press(cx, w, "ctrl-n");
    harness::type_text(cx, w, "Sample");
    harness::press(cx, w, "enter");
    assert!(exists(&shell, "projects/Sample.md"));
}

// adapts: creation_destination_test.rb:31 "a selected file creates at the root"
#[gpui_kit::test]
fn a_selected_note_creates_at_the_root(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    click(cx, &shell, "explorer-row-Inbox.md");
    harness::press(cx, w, "ctrl-n");
    harness::type_text(cx, w, "Rooted");
    harness::press(cx, w, "enter");
    assert!(exists(&shell, "Rooted.md"));
}

// adapts: test/system/notes_test.rb:76 "creating a Hugo blog post" (toolbar → blog/<date>/<slug>/index.md)
#[gpui_kit::test]
fn hugo_post_from_the_toolbar_goes_to_blog_with_front_matter(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    click(cx, &shell, "sidebar-new-note");
    click(cx, &shell, "dialog-new-note-hugo");
    harness::type_text(cx, w, "Hello World");
    assert!(harness::is_shown(cx, w, "dialog-new-note-slug"));
    harness::press(cx, w, "enter");
    let path = shell.active_note(cx).unwrap();
    assert!(
        path.starts_with("blog/") && path.ends_with("/hello-world/index.md"),
        "{path}"
    );
    let text = shell.read_note(&path).unwrap();
    assert!(
        text.starts_with("---\ntitle: \"Hello World\"\nslug: \"hello-world\"\n"),
        "{text}"
    );
}

// adapts: creation_destination_test.rb:52 "context menu Hugo post goes to that folder"
#[gpui_kit::test]
fn folder_context_menu_new_note_creates_inside_it(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    right_click(cx, &shell, "explorer-row-reference");
    assert!(harness::is_shown(cx, w, "explorer-context-menu"));
    click(cx, &shell, "explorer-menu-new-note");
    assert_eq!(dialog(cx, &shell), Some("new-note"));
    harness::type_text(cx, w, "Terms");
    harness::press(cx, w, "enter");
    assert!(exists(&shell, "reference/Terms.md"));
}

// adapts: test/system/notes_test.rb:93 "new note name cannot contain slashes" (Create gated)
#[gpui_kit::test]
fn invalid_note_name_keeps_the_dialog_open(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 ctrl-n");
    harness::type_text(cx, w, "a/b");
    harness::press(cx, w, "enter");
    assert_eq!(dialog(cx, &shell), Some("new-note"));
    assert!(!exists(&shell, "a/b.md"));
}

// F.8: "Note already exists" is shown inline and the dialog stays open.
#[gpui_kit::test]
fn duplicate_note_name_shows_an_inline_error(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 ctrl-n");
    harness::type_text(cx, w, "Inbox");
    harness::press(cx, w, "enter");
    assert_eq!(dialog(cx, &shell), Some("new-note"));
    assert!(harness::is_shown(cx, w, "dialog-error"));
}

// ---- New folder ---------------------------------------------------------------------

// adapts: test/system/notes_test.rb:127 "creating a folder" (F.9: nested paths allowed)
#[gpui_kit::test]
fn ctrl_shift_n_creates_nested_folders(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 ctrl-shift-n");
    assert_eq!(dialog(cx, &shell), Some("new-folder"));
    harness::type_text(cx, w, "archive/2026");
    harness::press(cx, w, "enter");
    assert!(shell.root.join("archive/2026").is_dir());
    assert_eq!(shell.overlay(cx), None);
}

// ---- Delete --------------------------------------------------------------------------

// adapts: test/system/notes_test.rb:409 "deleting a note" (changed §5.13: to the Trash)
#[gpui_kit::test]
fn context_menu_delete_confirms_then_moves_to_the_trash(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    right_click(cx, &shell, "explorer-row-Inbox.md");
    click(cx, &shell, "explorer-menu-delete");
    assert_eq!(dialog(cx, &shell), Some("delete"));
    assert!(harness::has_focus(cx, w, "dialog-delete"));
    // Focus starts on Cancel (FE-373): Enter cancels.
    harness::press(cx, w, "enter");
    assert_eq!(shell.overlay(cx), None);
    assert!(exists(&shell, "Inbox.md"));

    right_click(cx, &shell, "explorer-row-Inbox.md");
    click(cx, &shell, "explorer-menu-delete");
    click(cx, &shell, "dialog-delete-confirm");
    assert!(!exists(&shell, "Inbox.md"));
    assert_eq!(shell.overlay(cx), None);
}

// adapts: test/system/notes_test.rb:422 "deleting a non-empty folder" (changed §5.13 /
// FE-267: the Trash lets it succeed instead of "Folder not empty")
#[gpui_kit::test]
fn deleting_a_non_empty_folder_works(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    right_click(cx, &shell, "explorer-row-projects");
    click(cx, &shell, "explorer-menu-delete");
    click(cx, &shell, "dialog-delete-confirm");
    assert!(!exists(&shell, "projects"));
}

// adapts: test/system/notes_test.rb:441 "delete key in explorer deletes selected" and F.11 multi
#[gpui_kit::test]
fn delete_key_deletes_the_multi_selection_after_confirming(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    click(cx, &shell, "explorer-row-Inbox.md");
    harness::in_window(cx, w, |window, cx| {
        window.click_with_modifiers("explorer-row-reference", Modifiers::control(), cx)
    });
    harness::press(cx, w, "delete");
    assert_eq!(dialog(cx, &shell), Some("delete"));
    harness::press(cx, w, "ctrl-enter");
    assert!(!exists(&shell, "Inbox.md"));
    assert!(!exists(&shell, "reference"));
    assert!(exists(&shell, "projects/Launch list.md"));
}

// adapts: notes_test.rb:451 "Delete in the editor never deletes notes"
#[gpui_kit::test]
fn delete_in_the_editor_does_not_delete_the_note(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-2 delete");
    assert_eq!(shell.overlay(cx), None);
    assert!(exists(&shell, "blog/2026/Project brief.md"));
}

// ---- Rename / Move ---------------------------------------------------------------------

// ui-spec §5.13: F2 renames inline in the explorer row (new; VS Code-like).
#[gpui_kit::test]
fn explorer_f2_renames_inline(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    click(cx, &shell, "explorer-row-Inbox.md");
    harness::press(cx, w, "f2");
    assert!(harness::is_shown(cx, w, "explorer-rename-input"));
    // The stem is selected: typing replaces it.
    harness::type_text(cx, w, "Outbox");
    harness::press(cx, w, "enter");
    assert!(exists(&shell, "Outbox.md"));
    assert!(!exists(&shell, "Inbox.md"));
    assert!(!harness::is_shown(cx, w, "explorer-rename-input"));
}

// adapts: test/system/workspace_test.rb:264 "renames the current note from the header title"
#[gpui_kit::test]
fn clicking_the_pane_title_renames_the_open_note(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    click(cx, &shell, "pane-title");
    assert!(harness::is_shown(cx, w, "pane-title-input"));
    harness::press(cx, w, "secondary-a");
    harness::type_text(cx, w, "Brief v2");
    harness::press(cx, w, "enter");
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("blog/2026/Brief v2.md")
    );
    assert!(exists(&shell, "blog/2026/Brief v2.md"));
}

// adapts: workspace_test.rb:318 "shows an error when the new title already exists"
#[gpui_kit::test]
fn title_rename_collision_shows_an_inline_error(cx: &mut TestAppContext) {
    let shell = open_shell(
        cx,
        WIDE,
        VaultFixture::sample_with(&[("blog/2026/Other.md", "x")]),
    );
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 f2");
    harness::press(cx, w, "secondary-a");
    harness::type_text(cx, w, "Other");
    harness::press(cx, w, "enter");
    assert!(harness::is_shown(cx, w, "rename-error"));
    assert!(
        harness::is_shown(cx, w, "pane-title-input"),
        "still editing"
    );
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("blog/2026/Project brief.md")
    );
    harness::press(cx, w, "escape");
    assert!(!harness::is_shown(cx, w, "pane-title-input"));
}

// adapts: workspace_test.rb:300 "blank title is rejected"
#[gpui_kit::test]
fn blank_title_is_rejected(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 f2 secondary-a backspace enter");
    assert!(harness::is_shown(cx, w, "rename-error"));
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("blog/2026/Project brief.md")
    );
}

// ui-spec §5.13: "Move to…" shows a folder picker plus the name field.
#[gpui_kit::test]
fn move_to_moves_the_open_note_into_another_folder(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    run(cx, &shell, "file.move");
    assert_eq!(dialog(cx, &shell), Some("move"));
    click(cx, &shell, "dialog-move-folder-reference");
    click(cx, &shell, "dialog-move-confirm");
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("reference/Project brief.md")
    );
    assert!(exists(&shell, "reference/Project brief.md"));
    assert_eq!(shell.overlay(cx), None);
    let _ = w;
}

// adapts: test/system/notes_test.rb:338 "context menu appears on right-click" (F.5 items)
#[gpui_kit::test]
fn context_menus_list_the_items_of_their_target(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    right_click(cx, &shell, "explorer-row-Inbox.md");
    for id in [
        "explorer-menu-rename",
        "explorer-menu-move",
        "explorer-menu-delete",
    ] {
        assert!(harness::is_shown(cx, w, id), "{id}");
    }
    assert!(
        !harness::is_shown(cx, w, "explorer-menu-new-note"),
        "notes have no New Note"
    );
    harness::press(cx, w, "escape");
    assert!(!harness::is_shown(cx, w, "explorer-context-menu"));

    right_click(cx, &shell, "explorer-row-projects");
    for id in [
        "explorer-menu-new-note",
        "explorer-menu-new-folder",
        "explorer-menu-rename",
        "explorer-menu-delete",
    ] {
        assert!(harness::is_shown(cx, w, id), "{id}");
    }
    harness::press(cx, w, "escape");

    // Background (F.6 / §5.14): New note, New folder, Select all, Find file, Collapse all.
    right_click(cx, &shell, "explorer-tree");
    for id in [
        "explorer-menu-new-note",
        "explorer-menu-new-folder",
        "explorer-menu-select-all",
        "explorer-menu-find-file",
        "explorer-menu-collapse-all",
    ] {
        assert!(harness::is_shown(cx, w, id), "{id}");
    }
    click(cx, &shell, "explorer-menu-find-file");
    assert_eq!(shell.overlay(cx), Some(OverlayKind::QuickOpen));
}

// adapts: test/system/explorer_background_test.rb:21 "select all selects every visible row"
#[gpui_kit::test]
fn background_select_all_selects_every_visible_row(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample_no_note());
    right_click(cx, &shell, "explorer-tree");
    click(cx, &shell, "explorer-menu-select-all");
    let selected = shell
        .view
        .read_with(cx, |s, _| s.explorer().selection().len());
    assert_eq!(selected, 4, "blog, projects, reference, Inbox.md");
}
