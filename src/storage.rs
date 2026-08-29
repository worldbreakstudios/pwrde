//! The files pwrde keeps for the user — settings, the picker's pins/recents,
//! `.pwrspace.json` profiles, notes — behind one seam. Native reads and writes
//! the filesystem; wasm32 has no home directory or files, so the browser's
//! `localStorage` holds the same text under the path the file would have had.
//! Paths stay the vocabulary on both sides, so worktree scoping, fork-from-
//! base, and per-directory profiles work unchanged, and a reload keeps them.

use std::path::Path;
#[cfg(target_family = "wasm")]
use std::path::PathBuf;

/// The file's contents, or `None` when it cannot be read.
#[cfg(not(target_family = "wasm"))]
pub fn read_text(path: &Path) -> Option<String> {
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// Write `text` to `path`, creating parent directories.
#[cfg(not(target_family = "wasm"))]
pub fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

#[cfg(target_family = "wasm")]
const PREFIX: &str = "pwrde:";

#[cfg(target_family = "wasm")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

#[cfg(target_family = "wasm")]
fn key(path: &Path) -> String {
    format!("{PREFIX}{}", path.display())
}

/// See the module doc: `localStorage` under the path's key.
#[cfg(target_family = "wasm")]
pub fn read_text(path: &Path) -> Option<String> {
    storage()?.get_item(&key(path)).ok()?
}

/// See the module doc: `localStorage` under the path's key.
#[cfg(target_family = "wasm")]
pub fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    let unsupported = || std::io::Error::new(std::io::ErrorKind::Unsupported, "no localStorage");
    let storage = storage().ok_or_else(unsupported)?;
    storage.set_item(&key(path), text).map_err(|_| unsupported())
}

/// Every stored file under `dir` (any depth). Directories are only key
/// prefixes here, which is exactly what a vault scan needs.
#[cfg(target_family = "wasm")]
pub fn list_files(dir: &Path) -> Vec<PathBuf> {
    let Some(storage) = storage() else { return Vec::new() };
    let prefix = format!("{}/", key(dir).trim_end_matches('/'));
    let n = storage.length().unwrap_or(0);
    (0..n)
        .filter_map(|i| storage.key(i).ok().flatten())
        .filter(|k| k.starts_with(&prefix))
        .map(|k| PathBuf::from(&k[PREFIX.len()..]))
        .collect()
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_filesystem() {
        let dir = std::env::temp_dir().join(format!("pwrde-storage-{}", std::process::id()));
        let path = dir.join("nested").join("file.txt");
        write_text(&path, "hello").unwrap();
        assert_eq!(read_text(&path).as_deref(), Some("hello"));
        assert_eq!(read_text(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
