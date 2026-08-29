//! Workspace profiles: saved group layouts stored in `.pwrspace.json` files.
//!
//! A profile captures a split tree with ratios, per-tile tab strips, and
//! per-tab commands. Profiles are offered when creating a new group so the
//! user can restore a familiar layout instantly.
//!
//! The file format mirrors `persist::LayoutNode`'s serde-untagged convention:
//! leaves and splits are disambiguated by the presence of `"split"`.
//!
//! All IO is pure over explicit `&Path` arguments (like `settings.rs`) so
//! tests can run against temp directories without touching real config.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Top-level wrapper matching the JSON `{ "profiles": [...] }` envelope.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ProfileFile {
    #[serde(default)]
    profiles: Vec<WorkspaceProfile>,
}

/// A named, saved group layout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceProfile {
    /// Short human-readable identifier; used as the upsert key.
    pub name: String,
    /// Optional prose description shown in the picker.
    #[serde(default)]
    pub description: String,
    /// Root node of the saved split/leaf tree.
    pub layout: ProfileNode,
}

/// One tab inside a leaf tile.
///
/// An empty JSON object `{}` maps to `ProfileTab { command: None }` (bare shell).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProfileTab {
    /// Command to run in the tab, if any. `None` means an interactive shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// A node in the profile layout tree — either a leaf tile or a binary split.
///
/// Serde `untagged` so the JSON matches the exact format described in the
/// module-level doc comment (leaves identified by `"tabs"`, splits by `"split"`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProfileNode {
    /// A split pane with two children and a ratio.
    Split(ProfileSplit),
    /// A leaf tile containing one or more tabs.
    Leaf(ProfileLeaf),
}

/// Axis of a binary split.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDir {
    Row,
    Column,
}

/// A binary split node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSplit {
    pub split: SplitDir,
    /// Fraction of the space given to child `a` (0.0–1.0).
    pub ratio: f32,
    pub a: Box<ProfileNode>,
    pub b: Box<ProfileNode>,
}

/// A leaf tile node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileLeaf {
    pub tabs: Vec<ProfileTab>,
    /// Index of the active tab; defaults to 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub active: usize,
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

// ---------------------------------------------------------------------------
// IO helpers
// ---------------------------------------------------------------------------

/// Load all profiles from `path`.
///
/// A missing or corrupt file returns an empty `Vec` — never panics or returns
/// an error that would prevent group creation.
pub fn load_profiles(path: &Path) -> Vec<WorkspaceProfile> {
    let Some(text) = crate::storage::read_text(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<ProfileFile>(&text) {
        Ok(pf) => pf.profiles,
        Err(e) => {
            eprintln!("pwrspace: could not parse {:?}: {e}", path);
            Vec::new()
        }
    }
}

/// Upsert `profile` into the file at `path`.
///
/// Creates the file (and parent directories) if absent. An existing profile
/// with the same name is replaced in place; a new name is appended. The file
/// is written as pretty-printed JSON. Returns the IO error so the caller can
/// surface it in the UI; failure never panics.
pub fn save_profile(path: &Path, profile: &WorkspaceProfile) -> std::io::Result<()> {
    let mut profiles = load_profiles(path);
    if let Some(existing) = profiles.iter_mut().find(|p| p.name == profile.name) {
        *existing = profile.clone();
    } else {
        profiles.push(profile.clone());
    }
    write_profile_file(path, &ProfileFile { profiles })
}

/// Write `ProfileFile` to `path` as pretty-printed JSON, creating parents.
fn write_profile_file(path: &Path, pf: &ProfileFile) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(pf)?;
    crate::storage::write_text(path, &text)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// One discovered profile with a short source label.
pub type LabeledProfile = (WorkspaceProfile, String);

/// Load profiles from each `(path, label)` pair in order, deduping by name
/// with earliest-path-wins.
///
/// `label` is a short string like `"dir"`, `"repo"`, or `"user"` that the UI
/// uses to annotate each profile row.
pub fn discover(candidate_paths: &[(PathBuf, &str)]) -> Vec<LabeledProfile> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<LabeledProfile> = Vec::new();

    for (path, label) in candidate_paths {
        if !path.exists() {
            continue;
        }
        for profile in load_profiles(path) {
            if seen.insert(profile.name.clone()) {
                out.push((profile, label.to_string()));
            }
        }
    }
    out
}

/// Build the ordered candidate path list for `dir`.
///
/// Order:
/// 1. `<dir>/.pwrspace.json` — labelled `"dir"`
/// 2. `<repo_root>/.pwrspace.json` — labelled `"repo"` — only when `dir` is
///    inside a git repo whose **common** checkout root differs from `dir`.
/// 3. `~/.pwrde/pwrspace.json` — labelled `"user"`
///
/// The caller can feed the result directly to [`discover`].
pub fn candidate_paths(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();

    // 1. dir-local
    out.push((dir.join(".pwrspace.json"), "dir"));

    // 2. repo root — only when it differs from dir (handles linked worktrees).
    // Compare canonicalized paths: `repo_root` canonicalizes, and `dir` may
    // arrive through a symlink (e.g. /tmp on macOS).
    if let Some(root) = crate::git::repo_root(dir) {
        let dir_canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if root != dir_canon {
            out.push((root.join(".pwrspace.json"), "repo"));
        }
    }

    // 3. user-global
    out.push((crate::settings::config_dir().join("pwrspace.json"), "user"));

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("pwrde-pwrspace-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    // -----------------------------------------------------------------------
    // JSON round-trip matching the exact spec format
    // -----------------------------------------------------------------------

    #[test]
    fn json_roundtrip_spec_format() {
        // The exact JSON from the spec, including bare-shell tab `{}`
        let json = r#"{
  "profiles": [
    {
      "name": "agent-dev",
      "description": "claude + sub0 + lciw",
      "layout": {
        "split": "row", "ratio": 0.5,
        "a": { "tabs": [{ "command": "claude" }] },
        "b": {
          "split": "column", "ratio": 0.3,
          "a": { "tabs": [{ "command": "sub0" }] },
          "b": { "tabs": [{ "command": "lciw" }, {}], "active": 0 }
        }
      }
    }
  ]
}"#;
        let pf: ProfileFile = serde_json::from_str(json).expect("parse spec JSON");
        assert_eq!(pf.profiles.len(), 1);
        let p = &pf.profiles[0];
        assert_eq!(p.name, "agent-dev");
        assert_eq!(p.description, "claude + sub0 + lciw");

        // Verify root is a split
        let ProfileNode::Split(root) = &p.layout else {
            panic!("expected split at root");
        };
        assert!(matches!(root.split, SplitDir::Row));
        assert!((root.ratio - 0.5).abs() < 1e-6);

        // Left child: leaf with one tab
        let ProfileNode::Leaf(left) = root.a.as_ref() else {
            panic!("expected leaf for a");
        };
        assert_eq!(left.tabs.len(), 1);
        assert_eq!(left.tabs[0].command.as_deref(), Some("claude"));

        // Right child: column split
        let ProfileNode::Split(right) = root.b.as_ref() else {
            panic!("expected split for b");
        };
        assert!((right.ratio - 0.3).abs() < 1e-6);

        // Right.b: two tabs, second is bare shell
        let ProfileNode::Leaf(rb) = right.b.as_ref() else {
            panic!("expected leaf for b.b");
        };
        assert_eq!(rb.tabs.len(), 2);
        assert_eq!(rb.tabs[0].command.as_deref(), Some("lciw"));
        assert!(rb.tabs[1].command.is_none(), "bare shell tab should have no command");

        // Re-serialize and re-parse to confirm round-trip stability
        let text = serde_json::to_string_pretty(&pf).unwrap();
        let pf2: ProfileFile = serde_json::from_str(&text).unwrap();
        assert_eq!(pf2.profiles.len(), 1);
        assert_eq!(pf2.profiles[0].name, "agent-dev");
    }

    // -----------------------------------------------------------------------
    // load_profiles: missing / corrupt file
    // -----------------------------------------------------------------------

    #[test]
    fn load_profiles_missing_file_returns_empty() {
        let dir = temp_dir("missing");
        let path = dir.join(".pwrspace.json");
        assert!(load_profiles(&path).is_empty());
    }

    #[test]
    fn load_profiles_corrupt_file_returns_empty() {
        let dir = temp_dir("corrupt");
        let path = dir.join(".pwrspace.json");
        fs::write(&path, b"this is not json at all!!!").unwrap();
        assert!(load_profiles(&path).is_empty());
    }

    // -----------------------------------------------------------------------
    // save_profile: upsert by name
    // -----------------------------------------------------------------------

    fn simple_profile(name: &str, cmd: &str) -> WorkspaceProfile {
        WorkspaceProfile {
            name: name.to_string(),
            description: String::new(),
            layout: ProfileNode::Leaf(ProfileLeaf {
                tabs: vec![ProfileTab { command: Some(cmd.to_string()) }],
                active: 0,
            }),
        }
    }

    #[test]
    fn save_profile_creates_file() {
        let dir = temp_dir("create");
        let path = dir.join(".pwrspace.json");
        let p = simple_profile("dev", "nvim");
        save_profile(&path, &p).unwrap();
        let loaded = load_profiles(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "dev");
    }

    #[test]
    fn save_profile_upsert_replaces_same_name() {
        let dir = temp_dir("upsert");
        let path = dir.join(".pwrspace.json");

        save_profile(&path, &simple_profile("dev", "nvim")).unwrap();
        save_profile(&path, &simple_profile("other", "htop")).unwrap();
        // Replace "dev" with an updated command
        save_profile(&path, &simple_profile("dev", "vim")).unwrap();

        let loaded = load_profiles(&path);
        assert_eq!(loaded.len(), 2, "upsert must not duplicate");

        let dev = loaded.iter().find(|p| p.name == "dev").unwrap();
        let ProfileNode::Leaf(leaf) = &dev.layout else { panic!() };
        assert_eq!(leaf.tabs[0].command.as_deref(), Some("vim"), "updated command");
    }

    #[test]
    fn save_profile_creates_parent_dirs() {
        let dir = temp_dir("parents");
        let path = dir.join("nested").join("deep").join(".pwrspace.json");
        save_profile(&path, &simple_profile("x", "bash")).unwrap();
        assert!(path.exists());
    }

    // -----------------------------------------------------------------------
    // discover: dedupes by name, earliest-path-wins, carries source labels
    // -----------------------------------------------------------------------

    #[test]
    fn discover_dedupes_earliest_wins() {
        let d1 = temp_dir("disc1");
        let d2 = temp_dir("disc2");
        let p1 = d1.join(".pwrspace.json");
        let p2 = d2.join(".pwrspace.json");

        save_profile(&p1, &simple_profile("shared", "cmd-from-d1")).unwrap();
        save_profile(&p2, &simple_profile("shared", "cmd-from-d2")).unwrap();
        save_profile(&p2, &simple_profile("unique", "htop")).unwrap();

        let candidates = vec![(p1, "dir"), (p2, "repo")];
        let results = discover(&candidates);

        assert_eq!(results.len(), 2);
        // "shared" from d1 wins
        let (shared, label) = results.iter().find(|(p, _)| p.name == "shared").unwrap();
        assert_eq!(label, "dir");
        let ProfileNode::Leaf(leaf) = &shared.layout else { panic!() };
        assert_eq!(leaf.tabs[0].command.as_deref(), Some("cmd-from-d1"));
        // "unique" from d2
        let (_, ulabel) = results.iter().find(|(p, _)| p.name == "unique").unwrap();
        assert_eq!(ulabel, "repo");
    }

    #[test]
    fn discover_skips_nonexistent_paths() {
        let d = temp_dir("discskip");
        let real = d.join(".pwrspace.json");
        save_profile(&real, &simple_profile("only", "bash")).unwrap();

        let ghost = d.join("ghost.json");
        let candidates = vec![(ghost, "dir"), (real, "user")];
        let results = discover(&candidates);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "user");
    }
}
