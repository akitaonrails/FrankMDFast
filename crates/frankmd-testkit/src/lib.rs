//! Test-only helpers shared by every crate's conformance tests.
//!
//! Golden fixtures live in `tests/fixtures/golden/*.json` and were produced by
//! running the original FrankMD code (see `tools/golden/README.md`). Each file
//! has the shape `{source, frankmd_sha, generator, notes, cases: [{name, fn?, input, expected}]}`.
//!
//! This crate is a dev-dependency only; it may touch the filesystem freely.
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::missing_panics_doc
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

pub use serde_json;

/// One recorded case: an input fed to the original code and the output it produced.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    /// Human-readable case name (unique within a fixture file).
    pub name: String,
    /// Function under test, when a fixture covers several.
    #[serde(default, rename = "fn")]
    pub func: Option<String>,
    /// Input arguments as recorded.
    #[serde(default)]
    pub input: Value,
    /// Expected output as recorded. Errors look like `{"error": "Class", "message": "..."}`
    /// (Ruby) or `{"$error": {"name", "message"}}` (JS).
    #[serde(default)]
    pub expected: Value,
    /// Any other per-case fields (e.g. `utf16` length info).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Case {
    /// True when the original code raised/threw for this input.
    pub fn expects_error(&self) -> bool {
        self.expected.get("error").is_some() || self.expected.get("$error").is_some()
    }
}

/// A whole fixture file.
#[derive(Debug, Clone, Deserialize)]
pub struct Golden {
    /// Original source the cases were captured from.
    pub source: String,
    /// FrankMD commit the cases were captured at.
    #[serde(default)]
    pub frankmd_sha: Option<String>,
    /// Free-text caveats from the generator.
    #[serde(default)]
    pub notes: Option<String>,
    /// The recorded cases.
    pub cases: Vec<Case>,
}

impl Golden {
    /// Cases recorded for one function (`fn` field).
    pub fn cases_for<'a>(&'a self, func: &'a str) -> impl Iterator<Item = &'a Case> + 'a {
        self.cases
            .iter()
            .filter(move |c| c.func.as_deref() == Some(func))
    }
}

/// Repository root (two levels above this crate).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Absolute path of a file under `tests/fixtures/golden/`.
pub fn golden_path(rel: &str) -> PathBuf {
    repo_root().join("tests/fixtures/golden").join(rel)
}

/// Load `tests/fixtures/golden/<name>.json`.
pub fn golden(name: &str) -> Golden {
    let path = golden_path(&format!("{name}.json"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Raw bytes of a non-JSON golden file (e.g. `fed_template.fed`).
pub fn golden_bytes(rel: &str) -> Vec<u8> {
    std::fs::read(golden_path(rel)).unwrap()
}

/// Absolute path of a file under `assets/`.
pub fn asset_path(rel: &str) -> PathBuf {
    repo_root().join("assets").join(rel)
}

/// Golden cases (by name, unique across the corpus) whose recorded result needs a
/// case-sensitive filesystem: the generator ran on Linux, so on a case-insensitive
/// one (macOS APFS, Windows NTFS) `Note.md` and `note.md` are the same file.
const CASE_SENSITIVE_FS_ONLY: &[&str] = &[
    "template_source uppercase .HUGO_TEMPLATE.md ignored",
    "empty root images/ and videos/ hidden; Images/ (case) kept",
    "folder names equal after downcase (tie)",
    "create case-variant of existing (case-sensitive fs)",
    "case-only rename of file (case-sensitive fs)",
    "case-only rename of folder",
    "case-only rename when both exist",
    "rejects traversal, hidden, outside symlinks, non-markdown and malformed paths",
    "NotesService#exists? path=\"OK.md\"",
    "NotesService#exists? path=\"SUB/n.md\"",
    "NotesService#exists? path=\"Sub2/n.md\"",
    "NotesService#exists? path=\"sub2/N.md\"",
    "NotesService#file? path=\"OK.md\"",
    "NotesService#file? path=\"SUB/n.md\"",
    "NotesService#file? path=\"Sub2/n.md\"",
    "NotesService#file? path=\"sub2/N.md\"",
    "NotesService#safe_path must_exist=true path=\"OK.md\"",
    "NotesService#safe_path must_exist=true path=\"SUB/n.md\"",
    "NotesService#safe_path must_exist=true path=\"Sub2/n.md\"",
    "NotesService#safe_path must_exist=true path=\"sub2/N.md\"",
];

/// Whether the temp dir (where [`TempTree`]s live) tells `a` and `A` apart.
pub fn fs_is_case_sensitive() -> bool {
    static SENSITIVE: OnceLock<bool> = OnceLock::new();
    *SENSITIVE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "").unwrap();
        !dir.path().join("A").exists()
    })
}

/// Run `check` on every case and report all failures at once instead of stopping at the first.
/// Cases in [`CASE_SENSITIVE_FS_ONLY`] are skipped on a case-insensitive filesystem.
///
/// `check` returns `Err(description)` on mismatch.
pub fn assert_cases<'a>(
    fixture: &str,
    cases: impl IntoIterator<Item = &'a Case>,
    mut check: impl FnMut(&Case) -> Result<(), String>,
) {
    let mut failures = Vec::new();
    let mut total = 0usize;
    for case in cases {
        if CASE_SENSITIVE_FS_ONLY.contains(&case.name.as_str()) && !fs_is_case_sensitive() {
            eprintln!(
                "{fixture}: skipped [{}] (case-insensitive filesystem)",
                case.name
            );
            continue;
        }
        total += 1;
        if let Err(msg) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(case)))
            .unwrap_or_else(|p| Err(panic_message(&p)))
        {
            failures.push(format!("  [{}] {msg}", case.name));
        }
    }
    assert!(total > 0, "{fixture}: no cases selected");
    assert!(
        failures.is_empty(),
        "{fixture}: {}/{total} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .map_or_else(|| "panicked".to_owned(), |s| format!("panicked: {s}"))
}

/// A temporary notes directory built from a fixture tree spec.
///
/// Spec: `{"a/b.md": "content", "empty/": null, "link": {"symlink": "target"}}`.
pub struct TempTree {
    dir: tempfile::TempDir,
}

impl TempTree {
    /// Empty temp dir.
    pub fn empty() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Materialize a tree spec (a JSON object, or `null` for empty).
    pub fn from_spec(spec: &Value) -> Self {
        let t = Self::empty();
        if let Some(map) = spec.as_object() {
            for (rel, val) in map {
                t.put(rel, val);
            }
        }
        t
    }

    fn put(&self, rel: &str, val: &Value) {
        let path = self.dir.path().join(rel.trim_end_matches('/'));
        if rel.ends_with('/') {
            std::fs::create_dir_all(&path).unwrap();
            return;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        match val {
            Value::String(s) => std::fs::write(&path, s).unwrap(),
            Value::Object(o) if o.contains_key("symlink") => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(o["symlink"].as_str().unwrap(), &path).unwrap();
            }
            Value::Null => std::fs::write(&path, "").unwrap(),
            other => panic!("unsupported tree spec value for {rel}: {other}"),
        }
    }

    /// Root path.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Snapshot as `{relative path -> content}`, dirs ending in `/` mapped to `null`,
    /// matching the fixtures' `tree_after` encoding. Non-UTF-8 files are lossily decoded.
    pub fn snapshot(&self) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        walk(self.dir.path(), self.dir.path(), &mut out);
        out
    }

    /// Replace the temp root in a string with the fixtures' `<ROOT>` placeholder.
    pub fn normalize(&self, s: &str) -> String {
        s.replace(&self.dir.path().display().to_string(), "<ROOT>")
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Value>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        let ft = e.file_type().unwrap();
        if ft.is_symlink() {
            let target = std::fs::read_link(&p).unwrap();
            out.insert(
                rel,
                serde_json::json!({"symlink": target.to_string_lossy()}),
            );
        } else if ft.is_dir() {
            out.insert(format!("{rel}/"), Value::Null);
            walk(root, &p, out);
        } else {
            let bytes = std::fs::read(&p).unwrap();
            out.insert(
                rel,
                Value::String(String::from_utf8_lossy(&bytes).into_owned()),
            );
        }
    }
}

/// Decode the JS-fixture value encoding (`$undefined`, `$number`, `$utf16`, ...) into a
/// plain value. `$undefined` becomes `null`; special numbers become strings ("NaN", "-0").
pub fn decode_js(v: &Value) -> Value {
    match v {
        Value::Object(o) if o.len() == 1 => {
            let (k, inner) = o.iter().next().unwrap();
            match k.as_str() {
                "$undefined" => Value::Null,
                "$number" | "$date" | "$regexp" => inner.clone(),
                "$utf16" => {
                    let units: Vec<u16> = inner
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|u| u.as_u64().unwrap() as u16)
                        .collect();
                    Value::String(String::from_utf16_lossy(&units))
                }
                _ => Value::Object(o.iter().map(|(k, v)| (k.clone(), decode_js(v))).collect()),
            }
        }
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), decode_js(v))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(decode_js).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_golden_fixture_parses() {
        let dir = repo_root().join("tests/fixtures/golden");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "json") {
                let g = golden(p.file_stem().unwrap().to_str().unwrap());
                assert!(!g.cases.is_empty(), "{} has no cases", p.display());
                n += g.cases.len();
            }
        }
        assert!(n > 7000, "expected the full golden corpus, got {n} cases");
    }

    #[test]
    fn temp_tree_round_trips() {
        let spec = serde_json::json!({"a/b.md": "hi", "empty/": null});
        let t = TempTree::from_spec(&spec);
        let snap = t.snapshot();
        assert_eq!(snap.get("a/b.md"), Some(&Value::String("hi".into())));
        assert!(snap.contains_key("empty/"));
        assert!(snap.contains_key("a/"));
    }
}
