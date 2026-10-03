//! User CLI tools: a settings-backed registry of named commands. gpui-free.
//!
//! A *CLI tool* is a named shell command the user can launch in a terminal
//! session (name, command string, working directory, icon glyph). The list is
//! persisted in settings under `tools.cli` as a JSON array of [`CliTool`]
//! objects serialized to a STRING value. Nothing here touches gpui.
//!
//! Semantics of the settings key:
//! - **absent** → [`tools`] returns [`default_tools`] (the built-in Cleanup entry).
//! - **present and valid** → the parsed list is returned as-is. An explicit
//!   empty array `[]` means the user removed the default on purpose.
//! - **present but malformed** → empty list (graceful degradation).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings;

/// Settings key holding the JSON array of CLI tools (as a string value).
pub const TOOLS_KEY: &str = "tools.cli";

/// A user-registered CLI tool: name, command, cwd, and icon glyph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliTool {
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub icon: String,
}

// ---------------------------------------------------------------------------
// Defaults + CRUD
// ---------------------------------------------------------------------------

/// Built-in tools returned when the settings key is absent.
pub fn default_tools() -> Vec<CliTool> {
    vec![CliTool {
        name: "Cleanup".into(),
        command: "drop -d".into(),
        cwd: "~/src".into(),
        icon: "\u{f00d4}".into(), // nf-md-broom
    }]
}

/// The registered CLI tools.
///
/// - Key **absent** → [`default_tools`].
/// - Key **present and valid** → the parsed list (an explicit `[]` means the
///   user cleared the default).
/// - Key **present but malformed** → empty list.
pub fn tools() -> Vec<CliTool> {
    let Some(raw) = settings::get_str(TOOLS_KEY) else {
        return default_tools();
    };
    parse_tools(&raw).unwrap_or_default()
}

/// Persist `t` as the registered tool list.
pub fn set_tools(t: &[CliTool]) {
    let json = serde_json::to_string(t).unwrap_or_else(|_| "[]".into());
    settings::set(TOOLS_KEY, Value::String(json));
}

/// Append `tool` to the registered list (no name dedupe).
pub fn add_tool(tool: CliTool) {
    let mut t = tools();
    t.push(tool);
    set_tools(&t);
}

/// Remove the tool at `index` (no-op when out of range).
pub fn remove_tool(index: usize) {
    let mut t = tools();
    if index < t.len() {
        t.remove(index);
        set_tools(&t);
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Expand a leading `~` or `~/` via [`dirs::home_dir`]; otherwise return the
/// path as given. Pure — does not touch settings.
pub fn expand_cwd(cwd: &str) -> PathBuf {
    if cwd == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = cwd.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(cwd)
}

/// Parse a raw JSON string into a tool list. Returns `None` when the payload
/// is not a valid JSON array of [`CliTool`]. Pure — used by [`tools`].
pub fn parse_tools(raw: &str) -> Option<Vec<CliTool>> {
    serde_json::from_str(raw).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tools_valid() {
        let raw = r#"[
            {"name":"Cleanup","command":"drop -d","cwd":"~/src","icon":"x"}
        ]"#;
        let tools = parse_tools(raw).expect("valid json");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Cleanup");
        assert_eq!(tools[0].command, "drop -d");
        assert_eq!(tools[0].cwd, "~/src");
        assert_eq!(tools[0].icon, "x");
    }

    #[test]
    fn parse_tools_malformed() {
        assert!(parse_tools("not-json").is_none());
        assert!(parse_tools(r#"{"name":"x"}"#).is_none());
        assert!(parse_tools(r#"[1,2,3]"#).is_none());
    }

    #[test]
    fn parse_tools_empty_array() {
        let tools = parse_tools("[]").expect("empty array is valid");
        assert!(tools.is_empty());
    }

    #[test]
    fn expand_cwd_tilde() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(expand_cwd("~"), home);
    }

    #[test]
    fn expand_cwd_tilde_slash() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(expand_cwd("~/src"), home.join("src"));
    }

    #[test]
    fn expand_cwd_absolute_unchanged() {
        assert_eq!(expand_cwd("/tmp/foo"), PathBuf::from("/tmp/foo"));
        assert_eq!(expand_cwd("relative/path"), PathBuf::from("relative/path"));
    }
}
