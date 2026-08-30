// Shared by both binaries via `#[path]`; each uses only part of it.
#![allow(dead_code)]
//! Command bus protocol: NDJSON over a Unix domain socket.
//!
//! This module is deliberately crate-independent — it uses only `std`, `serde`,
//! and `serde_json`. A second binary can include it with
//! `#[path = "../bus.rs"] mod bus;` without pulling in the rest of pwrde.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A request sent from a client (CLI, hook, etc.) to a running pwrde instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Action {
        name: String,
    },
    NewSession {
        cwd: PathBuf,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        layout: Option<String>,
    },
    SendText {
        text: String,
        #[serde(default)]
        group: Option<String>,
    },
    Key {
        keys: Vec<String>,
    },
    FocusGroup {
        group: String,
    },
    NewSection {
        name: String,
    },
    MoveGroupToSection {
        group: String,
        section: String,
    },
    GoToPage {
        page: String,
    },
    ResizeWindow {
        width: f32,
        height: f32,
    },
    Screenshot {
        #[serde(default)]
        path: Option<PathBuf>,
        #[serde(default)]
        clipboard: bool,
    },
    ReadPane {
        session: u64,
        #[serde(default)]
        lines: Option<usize>,
        #[serde(default)]
        all: bool,
    },
    ReadPanes {
        #[serde(default)]
        query: Option<String>,
    },
    State,
    ListCommands,
    Ping,
    /// Hand a user message to the embedded Flow agent (spawned lazily by the
    /// app on first use; see `flow.rs` in the app crate).
    FlowSend {
        text: String,
    },
}

/// Response written back on the same connection, one line per request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn success(data: impl Into<Option<Value>>) -> Self {
        Self {
            ok: true,
            data: data.into(),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// One row of the human-readable command catalogue (CLI help + `ListCommands`).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub args: &'static str,
    pub help: &'static str,
    /// True when the command only reads state and never moves focus, the
    /// page, or a pane's scroll position (safe for agents to call freely).
    pub read_only: bool,
}

/// Specs for every [`Command`] variant, in a stable display order.
pub fn command_specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "action",
            args: "<name>",
            help: "Invoke a named pages::Action (e.g. split_right)",
            read_only: false,
        },
        CommandSpec {
            name: "new_session",
            args: "<cwd> [--base <ref>] [--layout <name>]",
            help: "Open a new session at the given working directory",
            read_only: false,
        },
        CommandSpec {
            name: "send_text",
            args: "<text> [--group <name>]",
            help: "Send raw keystrokes to the focused (or named) group's pane (--enter appends \\r)",
            read_only: false,
        },
        CommandSpec {
            name: "key",
            args: "<chord>...",
            help: "Press keystrokes through the app's key handler (gpui chord syntax: cmd-p, escape, cmd-shift-t, ctrl-c)",
            read_only: false,
        },
        CommandSpec {
            name: "focus_group",
            args: "<group>",
            help: "Focus a session group by name",
            read_only: false,
        },
        CommandSpec {
            name: "new_section",
            args: "<name>",
            help: "Create a new sidebar section/folder",
            read_only: false,
        },
        CommandSpec {
            name: "move_group_to_section",
            args: "<group> <section>",
            help: "Move a group into a section",
            read_only: false,
        },
        CommandSpec {
            name: "go_to_page",
            args: "<page>",
            help: "Navigate to a page (sessions, pull_requests, settings, tool:<n> or a tool's name)",
            read_only: false,
        },
        CommandSpec {
            name: "resize_window",
            args: "<width> <height>",
            help: "Resize the pwrde window",
            read_only: false,
        },
        CommandSpec {
            name: "screenshot",
            args: "[path] [--clipboard]",
            help: "Capture the window to a PNG file (default: a temp path) or, with --clipboard, the pasteboard",
            read_only: false,
        },
        CommandSpec {
            name: "read_pane",
            args: "<session> [--lines N|--all]",
            help: "Read a pane's text and metadata by session id (read-only; never changes focus or scroll)",
            read_only: true,
        },
        CommandSpec {
            name: "read_panes",
            args: "[query]",
            help: "List every pane (session id, group, title, foreground) across all groups, optionally filtered by a substring (read-only)",
            read_only: true,
        },
        CommandSpec {
            name: "state",
            args: "",
            help: "Dump current app state as JSON",
            read_only: true,
        },
        CommandSpec {
            name: "list_commands",
            args: "",
            help: "List available bus commands",
            read_only: true,
        },
        CommandSpec {
            name: "ping",
            args: "",
            help: "Liveness check",
            read_only: true,
        },
    ]
}

/// Socket path under `config_dir` for an optional worktree scope.
///
/// - `None` → `<config_dir>/bus.sock`
/// - `Some(scope)` → `<config_dir>/worktrees/<scope>/bus.sock`
///
/// Mirrors `settings::path_in` layout.
pub fn socket_path(config_dir: &Path, scope: Option<&str>) -> PathBuf {
    match scope {
        Some(slug) => config_dir.join("worktrees").join(slug).join("bus.sock"),
        None => config_dir.join("bus.sock"),
    }
}

/// Default socket: `$PWRDE_SOCKET` if set, else the socket under
/// `~/.pwrde` resolved by [`resolve_socket_path`].
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("PWRDE_SOCKET") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    resolve_socket_path(&PathBuf::from(home).join(".pwrde"))
}

/// Pick the socket a CLI run outside the app should talk to: the unscoped
/// `<config_dir>/bus.sock` when it exists; otherwise the most recently
/// modified `<config_dir>/worktrees/*/bus.sock` (an app launched from a
/// linked git worktree listens there); otherwise the unscoped path, so the
/// connection error names where the app would listen.
pub fn resolve_socket_path(config_dir: &Path) -> PathBuf {
    let base = socket_path(config_dir, None);
    if base.exists() {
        return base;
    }
    let mut scoped: Vec<(std::time::SystemTime, PathBuf)> =
        std::fs::read_dir(config_dir.join("worktrees"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path().join("bus.sock"))
                    .filter(|p| p.exists())
                    .map(|p| {
                        let modified = p
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH);
                        (modified, p)
                    })
                    .collect()
            })
            .unwrap_or_default();
    scoped.sort();
    scoped.pop().map(|(_, p)| p).unwrap_or(base)
}

/// Encode a command as one NDJSON line (JSON + trailing newline).
pub fn encode_command(cmd: &Command) -> String {
    let mut s = serde_json::to_string(cmd).expect("Command always serializes");
    s.push('\n');
    s
}

/// Decode one NDJSON line into a [`Command`].
///
/// On unknown `cmd` tag the error names the tag. Other serde failures return
/// the underlying error text.
pub fn decode_command(line: &str) -> Result<Command, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty command line".into());
    }
    match serde_json::from_str::<Command>(line) {
        Ok(cmd) => Ok(cmd),
        Err(e) => {
            // Prefer naming an unknown tag when present.
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(tag) = v.get("cmd").and_then(|t| t.as_str()) {
                    // If the tag is not one we know, surface it explicitly.
                    let known: Vec<&str> = command_specs().iter().map(|s| s.name).collect();
                    if !known.iter().any(|k| *k == tag) {
                        return Err(format!("unknown cmd tag: {tag}"));
                    }
                }
            }
            Err(e.to_string())
        }
    }
}

/// Serve commands on a Unix domain socket at `path`.
///
/// Creates the parent directory, removes any stale socket file, binds a
/// [`UnixListener`], and spawns a thread named `"pwrde-bus"` that accepts
/// forever. Each connection is handled on its own thread: read lines, decode,
/// call `handler`, write one reply line. Decode errors yield `Reply::err` and
/// the connection stays open. Never panics on a bad client.
pub fn serve(
    path: PathBuf,
    handler: impl Fn(Command) -> Reply + Send + Sync + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket file that still answers belongs to a running instance; don't
    // steal its name. One that doesn't is a leftover from a crash.
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("another pwrde instance is listening on {}", path.display()),
            ));
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    // The bus can type into live shells: owner-only.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let handler = std::sync::Arc::new(handler);
    let handle = std::thread::Builder::new()
        .name("pwrde-bus".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let handler = std::sync::Arc::clone(&handler);
                let _ = std::thread::spawn(move || handle_connection(stream, handler));
            }
        })?;
    Ok(handle)
}

fn handle_connection(
    stream: UnixStream,
    handler: std::sync::Arc<dyn Fn(Command) -> Reply + Send + Sync>,
) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let reply = match decode_command(&line) {
            Ok(cmd) => handler(cmd),
            Err(e) => Reply::err(e),
        };
        let mut out = match serde_json::to_string(&reply) {
            Ok(s) => s,
            Err(_) => continue,
        };
        out.push('\n');
        if writer.write_all(out.as_bytes()).is_err() {
            break;
        }
        if writer.flush().is_err() {
            break;
        }
    }
}

/// Send one command and read one reply, with a read timeout.
///
/// Connection failures include the socket path and the hint `"is pwrde running?"`.
pub fn request(path: &Path, cmd: &Command, timeout: Duration) -> Result<Reply, String> {
    let mut stream = UnixStream::connect(path).map_err(|e| {
        format!(
            "failed to connect to {}: {e} (is pwrde running?)",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    let encoded = encode_command(cmd);
    stream
        .write_all(encoded.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read: {e}"))?;
    if line.is_empty() {
        return Err("empty reply from server".into());
    }
    serde_json::from_str::<Reply>(line.trim()).map_err(|e| format!("bad reply: {e}"))
}

/// Serde tag string for a command (the `cmd` field value).
fn command_tag(cmd: &Command) -> &'static str {
    match cmd {
        Command::Action { .. } => "action",
        Command::NewSession { .. } => "new_session",
        Command::SendText { .. } => "send_text",
        Command::Key { .. } => "key",
        Command::FocusGroup { .. } => "focus_group",
        Command::NewSection { .. } => "new_section",
        Command::MoveGroupToSection { .. } => "move_group_to_section",
        Command::GoToPage { .. } => "go_to_page",
        Command::ResizeWindow { .. } => "resize_window",
        Command::Screenshot { .. } => "screenshot",
        Command::ReadPane { .. } => "read_pane",
        Command::ReadPanes { .. } => "read_panes",
        Command::State => "state",
        Command::ListCommands => "list_commands",
        Command::Ping => "ping",
        Command::FlowSend { .. } => "flow_send",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::time::Duration;

    fn all_variants() -> Vec<Command> {
        vec![
            Command::Action {
                name: "split_right".into(),
            },
            Command::NewSession {
                cwd: PathBuf::from("/tmp/proj"),
                base: Some("main".into()),
                layout: Some("dev".into()),
            },
            Command::NewSession {
                cwd: PathBuf::from("/tmp/proj"),
                base: None,
                layout: None,
            },
            Command::SendText {
                text: "hello".into(),
                group: Some("a".into()),
            },
            Command::SendText {
                text: "hello".into(),
                group: None,
            },
            Command::Key {
                keys: vec!["cmd-p".into(), "escape".into()],
            },
            Command::Key { keys: vec!["enter".into()] },
            Command::FocusGroup { group: "g1".into() },
            Command::NewSection {
                name: "Work".into(),
            },
            Command::MoveGroupToSection {
                group: "g1".into(),
                section: "Work".into(),
            },
            Command::GoToPage {
                page: "sessions".into(),
            },
            Command::ResizeWindow {
                width: 800.0,
                height: 600.0,
            },
            Command::Screenshot {
                path: Some(PathBuf::from("/tmp/s.png")),
                clipboard: true,
            },
            Command::Screenshot {
                path: None,
                clipboard: false,
            },
            Command::ReadPane {
                session: 7,
                lines: Some(2),
                all: false,
            },
            Command::ReadPane {
                session: 9,
                lines: None,
                all: true,
            },
            Command::ReadPanes { query: None },
            Command::ReadPanes {
                query: Some("zsh".into()),
            },
            Command::State,
            Command::ListCommands,
            Command::Ping,
        ]
    }

    #[test]
    fn serde_round_trip_every_variant() {
        for cmd in all_variants() {
            let line = encode_command(&cmd);
            assert!(line.ends_with('\n'), "encode must end with newline");
            let got = decode_command(line.trim()).expect("decode");
            assert_eq!(got, cmd);
        }
    }

    #[test]
    fn screenshot_omitted_fields_default() {
        let cmd = decode_command(r#"{"cmd":"screenshot"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::Screenshot {
                path: None,
                clipboard: false,
            }
        );
    }

    #[test]
    fn new_session_omitted_optionals_default() {
        let cmd = decode_command(r#"{"cmd":"new_session","cwd":"/tmp"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::NewSession {
                cwd: PathBuf::from("/tmp"),
                base: None,
                layout: None,
            }
        );
    }

    #[test]
    fn read_pane_omitted_optionals_default() {
        let cmd = decode_command(r#"{"cmd":"read_pane","session":42}"#).unwrap();
        assert_eq!(
            cmd,
            Command::ReadPane {
                session: 42,
                lines: None,
                all: false,
            }
        );
    }

    /// Only inspection commands are marked read-only; everything that could
    /// move focus, send keys, or resize the window must stay `false`.
    #[test]
    fn read_only_flag_marks_only_inspection_commands() {
        let read_only: Vec<&str> = command_specs()
            .into_iter()
            .filter(|s| s.read_only)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            read_only,
            vec!["read_pane", "read_panes", "state", "list_commands", "ping"]
        );
    }

    #[test]
    fn unknown_tag_names_the_tag() {
        let err = decode_command(r#"{"cmd":"nope"}"#).unwrap_err();
        assert!(
            err.contains("nope"),
            "error should name the unknown tag, got: {err}"
        );
    }

    #[test]
    fn command_specs_cover_every_variant_tag() {
        let specs = command_specs();
        let tags: std::collections::HashSet<&str> = specs.iter().map(|s| s.name).collect();
        // One representative per variant.
        let reps = [
            Command::Action { name: "x".into() },
            Command::NewSession {
                cwd: PathBuf::from("."),
                base: None,
                layout: None,
            },
            Command::SendText {
                text: String::new(),
                group: None,
            },
            Command::Key { keys: vec![] },
            Command::FocusGroup {
                group: String::new(),
            },
            Command::NewSection {
                name: String::new(),
            },
            Command::MoveGroupToSection {
                group: String::new(),
                section: String::new(),
            },
            Command::GoToPage {
                page: String::new(),
            },
            Command::ResizeWindow {
                width: 0.0,
                height: 0.0,
            },
            Command::Screenshot {
                path: None,
                clipboard: false,
            },
            Command::ReadPane {
                session: 1,
                lines: None,
                all: false,
            },
            Command::ReadPanes { query: None },
            Command::State,
            Command::ListCommands,
            Command::Ping,
        ];
        assert_eq!(
            specs.len(),
            reps.len(),
            "spec count must equal number of Command variants"
        );
        for cmd in &reps {
            let tag = command_tag(cmd);
            // Round-trip and assert tag appears in specs.
            let enc = encode_command(cmd);
            let v: Value = serde_json::from_str(enc.trim()).unwrap();
            let ser_tag = v.get("cmd").and_then(|t| t.as_str()).unwrap();
            assert_eq!(ser_tag, tag);
            assert!(tags.contains(tag), "tag {tag} missing from command_specs");
        }
    }

    #[test]
    fn socket_path_with_and_without_scope() {
        let base = PathBuf::from("/tmp/cfg");
        assert_eq!(socket_path(&base, None), PathBuf::from("/tmp/cfg/bus.sock"));
        assert_eq!(
            socket_path(&base, Some("wt-abc")),
            PathBuf::from("/tmp/cfg/worktrees/wt-abc/bus.sock")
        );
    }

    /// Helper: write `line` (with newline if missing), read one reply line, parse.
    fn write_line_read_reply(stream: &mut UnixStream, line: &str) -> Reply {
        let mut msg = line.to_string();
        if !msg.ends_with('\n') {
            msg.push('\n');
        }
        stream.write_all(msg.as_bytes()).expect("write");
        stream.flush().expect("flush");
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut resp = String::new();
        reader.read_line(&mut resp).expect("read reply");
        serde_json::from_str(resp.trim()).expect("parse reply")
    }

    #[test]
    fn serve_request_integration() {
        let dir = std::env::temp_dir().join(format!(
            "pwrde-bus-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("bus.sock");

        let _handle = serve(sock.clone(), |cmd| {
            let tag = command_tag(&cmd);
            Reply::success(Some(json!({"got": tag})))
        })
        .expect("serve");

        // Wait briefly for the listener to bind.
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let timeout = Duration::from_secs(2);

        let r = request(&sock, &Command::Ping, timeout).expect("ping");
        assert!(r.ok);
        assert_eq!(r.data, Some(json!({"got": "ping"})));

        let r = request(
            &sock,
            &Command::Screenshot {
                path: None,
                clipboard: false,
            },
            timeout,
        )
        .expect("screenshot");
        assert!(r.ok);
        assert_eq!(r.data, Some(json!({"got": "screenshot"})));

        // Garbage line yields ok:false and does not kill the connection.
        let mut stream = UnixStream::connect(&sock).expect("connect");
        stream.set_read_timeout(Some(timeout)).expect("timeout");
        let bad = write_line_read_reply(&mut stream, "not-json-at-all");
        assert!(!bad.ok, "garbage should yield ok:false");
        assert!(bad.error.is_some());

        // Same stream: a valid command still works.
        let good = write_line_read_reply(&mut stream, r#"{"cmd":"ping"}"#);
        assert!(good.ok, "connection must survive a bad line");
        assert_eq!(good.data, Some(json!({"got": "ping"})));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_missing_socket_mentions_path_and_hint() {
        let path = PathBuf::from("/tmp/pwrde-bus-definitely-missing.sock");
        let err = request(&path, &Command::Ping, Duration::from_millis(200)).unwrap_err();
        assert!(
            err.contains("pwrde-bus-definitely-missing.sock"),
            "error should include path: {err}"
        );
        assert!(
            err.contains("is pwrde running?"),
            "error should hint: {err}"
        );
    }

    #[test]
    fn reply_constructors() {
        let ok = Reply::success(Some(json!(42)));
        assert!(ok.ok);
        assert_eq!(ok.data, Some(json!(42)));
        assert!(ok.error.is_none());

        let ok_none = Reply::success(None);
        assert!(ok_none.ok);
        assert!(ok_none.data.is_none());

        let err = Reply::err("boom");
        assert!(!err.ok);
        assert_eq!(err.error.as_deref(), Some("boom"));
        assert!(err.data.is_none());
    }

    // Silence unused-import warning if Read is not used above on some rustc.
    #[allow(dead_code)]
    fn _use_read<R: Read>(_: R) {}
    #[test]
    fn resolve_socket_path_prefers_unscoped_then_worktree_scoped() {
        let dir = std::env::temp_dir().join(format!("pwrde-bus-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Nothing exists: the unscoped path is reported.
        assert_eq!(resolve_socket_path(&dir), dir.join("bus.sock"));
        // One worktree socket: it wins.
        let wt = dir.join("worktrees").join("abc");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("bus.sock"), b"").unwrap();
        assert_eq!(resolve_socket_path(&dir), wt.join("bus.sock"));
        // The unscoped socket beats any worktree one.
        std::fs::write(dir.join("bus.sock"), b"").unwrap();
        assert_eq!(resolve_socket_path(&dir), dir.join("bus.sock"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
