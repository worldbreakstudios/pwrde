//! Notes vaults: registered markdown directories + doc scanning. gpui-free.
//!
//! A *vault* is just a directory on disk holding markdown files (the
//! Obsidian convention).  The registered vault list is persisted in settings
//! under `notes.vaults` as a JSON array of path strings; scanning a vault is
//! a pure filesystem walk so it can be unit-tested against a temp dir.
//! Nothing here touches gpui — the element tree lives in `notes_ui.rs`,
//! side-effects in `main.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::settings;

/// Settings key holding the JSON array of vault directories.
const VAULTS_KEY: &str = "notes.vaults";

// ---------------------------------------------------------------------------
// Registered vaults
// ---------------------------------------------------------------------------

/// The registered vault directories. Unset or malformed settings yield an
/// empty list (graceful degradation — the UI then shows its empty state).
pub fn vaults() -> Vec<PathBuf> {
    let Some(raw) = settings::get_str(VAULTS_KEY) else { return Vec::new() };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else { return Vec::new() };
    items.iter().filter_map(|v| v.as_str()).map(PathBuf::from).collect()
}

/// Persist `v` as the registered vault list.
pub fn set_vaults(v: &[PathBuf]) {
    let items: Vec<Value> =
        v.iter().map(|p| Value::String(p.to_string_lossy().into_owned())).collect();
    let json = Value::Array(items).to_string();
    settings::set(VAULTS_KEY, Value::String(json));
}

/// Register `p`, ignoring duplicates, and return the path actually stored.
///
/// The path is canonicalized first (falling back to the raw path when that
/// fails, e.g. a broken symlink) so the same directory reached via a relative
/// path, a trailing `.`, or a symlink alias isn't registered twice — matching
/// the convention in [`crate::pwrspace`]. Returns the stored path so callers
/// can select the freshly-added vault.
pub fn add_vault(p: PathBuf) -> PathBuf {
    let p = p.canonicalize().unwrap_or(p);
    let mut v = vaults();
    if !v.iter().any(|existing| existing == &p) {
        v.push(p.clone());
        set_vaults(&v);
    }
    p
}

/// Unregister `p` (no-op when it is not registered).
#[allow(dead_code)]
pub fn remove_vault(p: &Path) {
    let mut v = vaults();
    let before = v.len();
    v.retain(|existing| existing.as_path() != p);
    if v.len() != before {
        set_vaults(&v);
    }
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// One markdown document inside a vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDoc {
    pub path: PathBuf,
    /// File stem, shown as the document title.
    pub title: String,
    /// Path relative to the vault root, `/`-joined — the list-row label.
    pub rel: String,
}

/// Every markdown file under `dir`, recursively, sorted by `rel`.
///
/// Hidden entries (any component starting with `.`, e.g. `.git`,
/// `.obsidian`) are skipped.  Unreadable directories are simply omitted.
pub fn scan_vault(dir: &Path) -> Vec<NoteDoc> {
    let mut out = Vec::new();
    // Canonical paths of directories already entered — the cycle guard that
    // lets us follow symlinks (so shared/linked-in notes still show up)
    // without a self-referential link recursing until the stack overflows.
    let mut visited = HashSet::new();
    walk(dir, dir, &mut out, &mut visited);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<NoteDoc>, visited: &mut HashSet<PathBuf>) {
    // Skip a directory we've already walked (compared by canonical path), so a
    // symlink cycle terminates instead of recursing forever.
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canon) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Resolve the target kind with `metadata` (which DOES follow symlinks)
        // so legitimately symlinked notes/dirs are included; the `visited` set
        // above is what keeps symlinked-directory cycles bounded. A dangling or
        // unreadable link just resolves to Err and is skipped.
        let is_dir = match std::fs::metadata(&path) {
            Ok(md) => md.is_dir(),
            Err(_) => continue,
        };
        if is_dir {
            walk(root, &path, out, visited);
        } else if is_markdown(&path) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            out.push(NoteDoc { path, title, rel });
        }
    }
}

fn is_markdown(p: &Path) -> bool {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| e == "md" || e == "markdown")
}

// ---------------------------------------------------------------------------
// Document IO
// ---------------------------------------------------------------------------

pub fn read_doc(p: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(p)
}

pub fn write_doc(p: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, content)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty scratch directory under the system temp dir.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pwrde-notes-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn scan_finds_markdown_sorted_and_skips_noise() {
        let dir = scratch("scan");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
        std::fs::write(dir.join("a/b.md"), "# b").unwrap();
        std::fs::write(dir.join("zeta.markdown"), "# z").unwrap();
        std::fs::write(dir.join("notes.txt"), "plain").unwrap();
        std::fs::write(dir.join(".obsidian/x.md"), "# hidden").unwrap();

        let docs = scan_vault(&dir);
        let rels: Vec<&str> = docs.iter().map(|d| d.rel.as_str()).collect();
        assert_eq!(rels, vec!["a/b.md", "zeta.markdown"]);
        assert_eq!(docs[0].title, "b");
        assert_eq!(docs[0].path, dir.join("a/b.md"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_does_not_follow_symlink_cycles() {
        // A self-referential symlink must not send the recursive walk into an
        // unbounded loop (which would overflow the stack and crash the app).
        let dir = scratch("symlink-cycle");
        std::fs::write(dir.join("real.md"), "# real").unwrap();
        // `loop` -> the vault root itself: following it would recurse forever.
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();

        let docs = scan_vault(&dir);
        let rels: Vec<&str> = docs.iter().map(|d| d.rel.as_str()).collect();
        assert_eq!(rels, vec!["real.md"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_includes_symlinked_markdown() {
        // A note symlinked in from elsewhere (a common Obsidian pattern) must
        // still appear — the cycle guard only bounds recursion, it doesn't
        // hide linked files.
        let dir = scratch("symlink-file");
        let ext = scratch("symlink-file-ext");
        std::fs::write(ext.join("shared.md"), "# shared").unwrap();
        std::fs::write(dir.join("local.md"), "# local").unwrap();
        std::os::unix::fs::symlink(ext.join("shared.md"), dir.join("shared.md")).unwrap();

        let rels: Vec<String> = scan_vault(&dir).into_iter().map(|d| d.rel).collect();
        assert!(rels.contains(&"local.md".to_string()));
        assert!(rels.contains(&"shared.md".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&ext);
    }

    #[test]
    fn scan_of_missing_dir_is_empty() {
        let dir = std::env::temp_dir().join("pwrde-notes-test-does-not-exist");
        assert!(scan_vault(&dir).is_empty());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = scratch("io");
        let file = dir.join("nested/note.md");
        write_doc(&file, "# hello\n\nbody").unwrap();
        assert_eq!(read_doc(&file).unwrap(), "# hello\n\nbody");
        assert_eq!(scan_vault(&dir).len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
