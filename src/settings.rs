//! Persistent key-value settings.
//!
//! Settings live under `~/.pwrde/`. The default path is
//! `~/.pwrde/settings.json`. When the process is launched from a linked git
//! worktree (see [`crate::git::worktree_scope`]), the path becomes
//! `~/.pwrde/worktrees/<slug>/settings.json` so each worktree keeps its own
//! settings. On first launch in a worktree the scoped file is missing, so
//! `init` forks from the base `~/.pwrde/settings.json`; subsequent `set`
//! calls write only the scoped file.
//!
//! The file is a flat JSON object (key → value). It is read once at startup
//! into a global in-memory store; `set` updates the store and rewrites the
//! file, so the file is always the durable truth and the store is always
//! current. A missing or corrupt file yields an empty store — settings must
//! never prevent the app from launching.
//!
//! Load/save are pure functions over an explicit path so tests can run
//! against a temp directory without touching the real config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::Value;

/// BTreeMap so the serialized file has a stable key order (diff-friendly).
type Map = BTreeMap<String, Value>;

static STORE: OnceLock<RwLock<Map>> = OnceLock::new();

/// `~/.pwrde` — the config directory for all pwrde user files.
pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".pwrde")
}

/// Settings path under `base` for an optional worktree scope.
///
/// - `None` → `<base>/settings.json`
/// - `Some(slug)` → `<base>/worktrees/<slug>/settings.json`
pub fn path_in(base: &Path, scope: Option<&str>) -> PathBuf {
    match scope {
        Some(slug) => base.join("worktrees").join(slug).join("settings.json"),
        None => base.join("settings.json"),
    }
}

pub fn path() -> PathBuf {
    path_in(&config_dir(), crate::git::worktree_scope().as_deref())
}

/// Choose which settings file to load for init (fork-from-base).
///
/// Prefer `scoped` when it exists on disk; otherwise fall back to `base`
/// (a new worktree starts as a copy of the primary settings). If neither
/// path yields content, the empty map is returned.
fn load_for_init(scoped: &Path, base: &Path) -> Map {
    if scoped.exists() {
        load(scoped)
    } else {
        load(base)
    }
}

/// Load `settings.json` into the global store. Call once at startup, before
/// anything reads a setting.
///
/// When running in a linked worktree, the scoped file is preferred if present;
/// otherwise the base `~/.pwrde/settings.json` is loaded (fork-from-base).
/// Subsequent `set` calls always write the scoped path when scoped.
pub fn init() {
    let base_path = path_in(&config_dir(), None);
    let scoped_path = path();
    let map = if scoped_path == base_path {
        load(&base_path)
    } else {
        load_for_init(&scoped_path, &base_path)
    };
    let _ = STORE.set(RwLock::new(map));
}

fn store() -> &'static RwLock<Map> {
    // Tolerate a missing init() (tests, future call sites): an empty store
    // behaves like a fresh install.
    STORE.get_or_init(|| RwLock::new(Map::new()))
}

pub fn get_str(key: &str) -> Option<String> {
    store().read().ok()?.get(key)?.as_str().map(str::to_owned)
}

/// The command auto-run in a group's primary pane when the group is created.
/// Defaults to `claude`; an explicitly empty value disables the auto-run.
pub fn primary_command() -> String {
    get_str("session.primary_command").unwrap_or_else(|| "claude".into())
}

/// Whether terminal sessions persist across app restarts (`terminal.persist`):
/// shells run inside shpool and the group layout is snapshotted to SQLite.
/// Defaults to **on** — losing every session on a restart is far worse than
/// an unexpected shpool dependency, which [`crate::term::Session::new`]
/// degrades gracefully around when the binary is missing.
pub fn persist_sessions() -> bool {
    get_bool("terminal.persist", true)
}

pub fn get_bool(key: &str, default: bool) -> bool {
    store()
        .read()
        .ok()
        .and_then(|map| map.get(key)?.as_bool())
        .unwrap_or(default)
}

/// A numeric setting as `f32`, or `default` when unset/non-numeric.
pub fn get_f32(key: &str, default: f32) -> f32 {
    store()
        .read()
        .ok()
        .and_then(|map| map.get(key)?.as_f64())
        .map(|v| v as f32)
        .unwrap_or(default)
}

/// Update one key in memory and persist the whole store to disk. A write
/// failure keeps the in-memory value (the session still works; only
/// persistence is lost).
pub fn set(key: &str, value: Value) {
    if let Ok(mut map) = store().write() {
        map.insert(key.to_owned(), value);
        let _ = save(&path(), &map);
    }
}

/// Read a settings map from `path`. Missing file, unreadable file, invalid
/// JSON, or a non-object root all yield an empty map.
fn load(path: &Path) -> Map {
    let Ok(bytes) = std::fs::read(path) else { return Map::new() };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(obj)) => obj.into_iter().collect(),
        _ => Map::new(),
    }
}

/// Write `map` to `path` as pretty-printed JSON, creating parent directories
/// (the `~/.pwrde` dir on first save).
fn save(path: &Path, map: &Map) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let obj: serde_json::Map<String, Value> =
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let text = serde_json::to_string_pretty(&Value::Object(obj))?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("pwrde-settings-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("settings.json")
    }

    #[test]
    fn save_load_roundtrip() {
        let path = temp_file("roundtrip");
        let mut map = Map::new();
        map.insert("theme".into(), Value::String("midnight".into()));
        map.insert("debug.overlay".into(), Value::Bool(true));
        save(&path, &map).unwrap();
        assert_eq!(load(&path), map);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn primary_command_defaults_to_claude() {
        // The global store is empty in tests (no init/set), so the unset key
        // must fall back to the default.
        assert_eq!(primary_command(), "claude");
    }

    #[test]
    fn persist_sessions_defaults_to_on() {
        // Losing every session on restart was the failure mode of an off
        // default; an unset key must read as persisted.
        assert!(persist_sessions());
    }

    #[test]
    fn get_f32_returns_default_for_unset_key() {
        // An unset key falls back to the default (the store has no such key).
        assert_eq!(get_f32("accessibility.__unset_test_key__", 15.0), 15.0);
    }

    #[test]
    fn missing_file_loads_empty() {
        assert!(load(Path::new("/nonexistent/pwrde/settings.json")).is_empty());
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let path = temp_file("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load(&path).is_empty());
        // A JSON root that isn't an object is also "corrupt" for our schema.
        std::fs::write(&path, b"[1,2,3]").unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn path_in_unscoped() {
        let base = Path::new("/tmp/fake-pwrde");
        assert_eq!(path_in(base, None), PathBuf::from("/tmp/fake-pwrde/settings.json"));
    }

    #[test]
    fn path_in_scoped() {
        let base = Path::new("/tmp/fake-pwrde");
        assert_eq!(
            path_in(base, Some("my-feature")),
            PathBuf::from("/tmp/fake-pwrde/worktrees/my-feature/settings.json")
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("pwrde-settings-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_for_init_prefers_scoped_when_present() {
        let dir = temp_dir("fork-scoped");
        let base = dir.join("settings.json");
        let scoped = dir.join("worktrees").join("wt").join("settings.json");
        std::fs::create_dir_all(scoped.parent().unwrap()).unwrap();

        let mut base_map = Map::new();
        base_map.insert("source".into(), Value::String("base".into()));
        save(&base, &base_map).unwrap();

        let mut scoped_map = Map::new();
        scoped_map.insert("source".into(), Value::String("scoped".into()));
        save(&scoped, &scoped_map).unwrap();

        assert_eq!(load_for_init(&scoped, &base), scoped_map);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_for_init_falls_back_to_base_when_scoped_missing() {
        let dir = temp_dir("fork-base");
        let base = dir.join("settings.json");
        let scoped = dir.join("worktrees").join("wt").join("settings.json");

        let mut base_map = Map::new();
        base_map.insert("source".into(), Value::String("base".into()));
        save(&base, &base_map).unwrap();

        assert_eq!(load_for_init(&scoped, &base), base_map);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_for_init_empty_when_neither_exists() {
        let dir = temp_dir("fork-neither");
        let base = dir.join("settings.json");
        let scoped = dir.join("worktrees").join("wt").join("settings.json");
        assert!(load_for_init(&scoped, &base).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
