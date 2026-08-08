//! Auto-installed Claude Code hooks that signal pane attention.
//!
//! On startup pwrde writes a tiny hook script to `~/.pwrde/claude-hook.sh` and
//! idempotently merges entries for the attention-worthy Claude Code hook events
//! into `~/.claude/settings.json`. The script emits an OSC 9 toast to
//! `/dev/tty`, which rides the pane's own PTY back to pwrde (through shpool
//! too) where wezterm-term surfaces it as `Alert::ToastNotification` and the
//! tab gains an unread dot. The script no-ops outside pwrde: directly spawned
//! panes carry `PWRDE=1`, and shpool-backed panes carry a `pwrde-`-prefixed
//! `SHPOOL_SESSION_NAME` set by the daemon.
//!
//! The settings merge is additive only — existing entries are never removed or
//! reordered, and a settings file that fails to parse is left untouched.

use serde_json::{Value, json};

/// Hook events that mean "Claude needs the user's attention": permission
/// prompts and idle notifications, end of turn, approval requests, and
/// AskUserQuestion (Elicitation).
const HOOK_EVENTS: [&str; 4] = ["Notification", "Stop", "PermissionRequest", "Elicitation"];

const HOOK_SCRIPT: &str = "#!/bin/sh\n\
# Installed by pwrde. Signals attention to the hosting pwrde pane via OSC 9.\n\
# No-ops outside pwrde: PWRDE is set in panes pwrde spawns directly, and\n\
# shpool-backed panes carry a pwrde- prefixed SHPOOL_SESSION_NAME.\n\
if [ -n \"${PWRDE:-}\" ] || [ \"${SHPOOL_SESSION_NAME#pwrde-}\" != \"${SHPOOL_SESSION_NAME:-}\" ]; then\n\
  printf '\\033]9;pwrde:attention\\007' 2>/dev/null > /dev/tty || :\n\
fi\n";

/// Ensure every event in [`HOOK_EVENTS`] has an entry running `script_path`.
/// Returns the (possibly updated) root and whether anything changed. Purely
/// additive: never removes or reorders existing configuration.
pub fn merge_hooks(mut root: Value, script_path: &str) -> (Value, bool) {
    // A non-object root (e.g. a bare array) is not a settings file we can
    // safely extend; leave it alone.
    if !root.is_object() {
        return (root, false);
    }
    let mut changed = false;
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return (root, false);
    }
    for event in HOOK_EVENTS {
        let entries = hooks
            .as_object_mut()
            .unwrap()
            .entry(event)
            .or_insert_with(|| json!([]));
        let Some(list) = entries.as_array_mut() else { continue };
        let present = list.iter().any(|entry| {
            entry["hooks"].as_array().is_some_and(|cmds| {
                cmds.iter().any(|c| {
                    c["command"].as_str().is_some_and(|cmd| cmd.contains(script_path))
                })
            })
        });
        if !present {
            list.push(json!({
                "hooks": [{ "type": "command", "command": script_path }]
            }));
            changed = true;
        }
    }
    (root, changed)
}

/// Write the hook script and merge the hook entries into the user's Claude
/// Code settings. All failures warn and continue — hook installation must
/// never prevent launch.
pub fn install() {
    let Some(home) = dirs::home_dir() else { return };

    // ── The script ──────────────────────────────────────────────────────
    let script_path = home.join(".pwrde").join("claude-hook.sh");
    if let Some(dir) = script_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Rewrite unconditionally so script updates ship with the app.
    if let Err(e) = std::fs::write(&script_path, HOOK_SCRIPT) {
        eprintln!("claude_hooks: failed to write {}: {e}", script_path.display());
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }

    // ── The settings merge ──────────────────────────────────────────────
    let settings_path = home.join(".claude").join("settings.json");
    let root = match std::fs::read_to_string(&settings_path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => v,
            Err(e) => {
                // Never clobber a file we could not parse.
                eprintln!(
                    "claude_hooks: {} is not valid JSON ({e}); skipping hook install",
                    settings_path.display()
                );
                return;
            },
        },
        Err(_) => json!({}),
    };
    let (root, changed) = merge_hooks(root, &script_path.to_string_lossy());
    if !changed {
        return;
    }
    if let Some(dir) = settings_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(&root).unwrap_or_default();
    if text.is_empty() {
        return;
    }
    if let Err(e) = std::fs::write(&settings_path, text) {
        eprintln!("claude_hooks: failed to write {}: {e}", settings_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "/home/u/.pwrde/claude-hook.sh";

    #[test]
    fn merge_into_empty_adds_all_events() {
        let (root, changed) = merge_hooks(json!({}), SCRIPT);
        assert!(changed);
        for event in HOOK_EVENTS {
            let list = root["hooks"][event].as_array().unwrap();
            assert_eq!(list.len(), 1, "{event} should have one entry");
            assert_eq!(list[0]["hooks"][0]["command"], SCRIPT);
        }
    }

    #[test]
    fn merge_is_idempotent() {
        let (root, changed) = merge_hooks(json!({}), SCRIPT);
        assert!(changed);
        let (again, changed) = merge_hooks(root.clone(), SCRIPT);
        assert!(!changed, "second merge must be a no-op");
        assert_eq!(root, again);
    }

    #[test]
    fn merge_preserves_unrelated_keys_and_existing_entries() {
        let existing = json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": {
                "Stop": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "say done" }] }
                ],
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "echo pre" }] }
                ]
            }
        });
        let (root, changed) = merge_hooks(existing, SCRIPT);
        assert!(changed);
        // Unrelated keys and hook events untouched.
        assert_eq!(root["model"], "opus");
        assert_eq!(root["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(root["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        // The user's Stop entry stays first; ours is appended.
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "say done");
        assert_eq!(stop[1]["hooks"][0]["command"], SCRIPT);
    }

    #[test]
    fn merge_leaves_unusable_shapes_alone() {
        // Root not an object → untouched.
        let (root, changed) = merge_hooks(json!([1, 2]), SCRIPT);
        assert!(!changed);
        assert_eq!(root, json!([1, 2]));

        // "hooks" not an object → untouched.
        let odd = json!({ "hooks": "what" });
        let (root, changed) = merge_hooks(odd.clone(), SCRIPT);
        assert!(!changed);
        assert_eq!(root, odd);

        // An event key holding a non-array → that event skipped, others added.
        let odd = json!({ "hooks": { "Stop": 5 } });
        let (root, changed) = merge_hooks(odd, SCRIPT);
        assert!(changed);
        assert_eq!(root["hooks"]["Stop"], 5);
        assert_eq!(root["hooks"]["Notification"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn script_is_posix_and_guarded() {
        assert!(HOOK_SCRIPT.starts_with("#!/bin/sh\n"));
        assert!(HOOK_SCRIPT.contains("PWRDE"));
        assert!(HOOK_SCRIPT.contains("SHPOOL_SESSION_NAME#pwrde-"));
        assert!(HOOK_SCRIPT.contains("]9;"), "must emit OSC 9");
        assert!(HOOK_SCRIPT.contains("/dev/tty"));
    }
}
