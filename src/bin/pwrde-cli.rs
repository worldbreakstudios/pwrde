//! pwrde-cli — talk to a running pwrde instance over the command bus.
//!
//! Hand-rolled argv parsing (no clap). Includes the crate-independent bus
//! protocol module via `#[path]` so this binary stays free of the app crate.

#[path = "../bus.rs"]
mod bus;

use bus::{Command, Reply};
use serde_json::Value;
use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => {}
        Err(CliError::Usage(msg)) => {
            eprintln!("error: {msg}");
            eprintln!();
            eprint!("{}", usage());
            process::exit(64);
        }
        Err(CliError::Reply(msg)) => {
            eprintln!("error: {msg}");
            process::exit(1);
        }
        Err(CliError::Connect(msg)) => {
            eprintln!("{msg}");
            process::exit(2);
        }
    }
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Reply(String),
    Connect(String),
}

fn run(args: &[String]) -> Result<(), CliError> {
    let mut socket: Option<PathBuf> = None;
    let mut timeout_secs: u64 = 10;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-h" || a == "--help" {
            print!("{}", usage());
            return Ok(());
        } else if a == "--socket" {
            i += 1;
            let p = args
                .get(i)
                .ok_or_else(|| CliError::Usage("--socket requires a path".into()))?;
            socket = Some(PathBuf::from(p));
        } else if let Some(p) = a.strip_prefix("--socket=") {
            socket = Some(PathBuf::from(p));
        } else if a == "--timeout" {
            i += 1;
            let t = args
                .get(i)
                .ok_or_else(|| CliError::Usage("--timeout requires seconds".into()))?;
            timeout_secs = t
                .parse()
                .map_err(|_| CliError::Usage(format!("invalid --timeout: {t}")))?;
        } else if let Some(t) = a.strip_prefix("--timeout=") {
            timeout_secs = t
                .parse()
                .map_err(|_| CliError::Usage(format!("invalid --timeout: {t}")))?;
        } else if a.starts_with('-') && a != "-" {
            // Subcommand-local flags are collected into `rest` and parsed later.
            rest.push(a.clone());
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }

    if rest.is_empty() {
        print!("{}", usage());
        return Ok(());
    }

    let sub = rest[0].as_str();
    let sub_args = &rest[1..];
    if sub == "read" && sub_args.is_empty() {
        print!("{}", read_usage());
        return Ok(());
    }
    // `--json` on a read-only command asks for the full reply object instead
    // of the plain-text rendering agents pipe by default.
    let json = sub == "read" && sub_args.iter().any(|a| a == "--json");
    let sub_args: Vec<String> =
        sub_args.iter().filter(|a| !(json && *a == "--json")).cloned().collect();
    let cmd = parse_command(sub, &sub_args)?;

    let path = socket.unwrap_or_else(bus::default_socket_path);
    let timeout = Duration::from_secs(timeout_secs);
    let reply = bus::request(&path, &cmd, timeout).map_err(CliError::Connect)?;
    emit_reply(&cmd, &reply, json)
}

fn parse_command(sub: &str, args: &[String]) -> Result<Command, CliError> {
    match sub {
        "action" => {
            let name = args
                .first()
                .ok_or_else(|| CliError::Usage("action requires <name>".into()))?
                .clone();
            Ok(Command::Action { name })
        }
        "new-session" => {
            let mut cwd: Option<PathBuf> = None;
            let mut base: Option<String> = None;
            let mut layout: Option<String> = None;
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--base" {
                    i += 1;
                    base = Some(
                        args.get(i)
                            .ok_or_else(|| CliError::Usage("--base requires a value".into()))?
                            .clone(),
                    );
                } else if let Some(v) = a.strip_prefix("--base=") {
                    base = Some(v.to_string());
                } else if a == "--layout" {
                    i += 1;
                    layout = Some(
                        args.get(i)
                            .ok_or_else(|| CliError::Usage("--layout requires a value".into()))?
                            .clone(),
                    );
                } else if let Some(v) = a.strip_prefix("--layout=") {
                    layout = Some(v.to_string());
                } else if a.starts_with('-') {
                    return Err(CliError::Usage(format!("unknown flag: {a}")));
                } else if cwd.is_none() {
                    cwd = Some(PathBuf::from(a));
                } else {
                    return Err(CliError::Usage(format!("unexpected argument: {a}")));
                }
                i += 1;
            }
            let cwd = cwd.ok_or_else(|| CliError::Usage("new-session requires <dir>".into()))?;
            let cwd = canonicalize_dir(&cwd)?;
            Ok(Command::NewSession { cwd, base, layout })
        }
        "new-webview" => {
            let mut url: Option<String> = None;
            let mut group: Option<String> = None;
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--group" {
                    i += 1;
                    group = Some(
                        args.get(i)
                            .ok_or_else(|| CliError::Usage("--group requires a value".into()))?
                            .clone(),
                    );
                } else if let Some(value) = a.strip_prefix("--group=") {
                    group = Some(value.to_string());
                } else if a.starts_with('-') {
                    return Err(CliError::Usage(format!("unknown flag: {a}")));
                } else if url.is_none() {
                    url = Some(a.clone());
                } else {
                    return Err(CliError::Usage(format!("unexpected argument: {a}")));
                }
                i += 1;
            }
            let url = url.ok_or_else(|| CliError::Usage("new-webview requires <url>".into()))?;
            let url = bus::validate_webview_url(&url).map_err(CliError::Usage)?;
            Ok(Command::NewWebview { url, group })
        }
        "send-text" => {
            let mut text: Option<String> = None;
            let mut group: Option<String> = None;
            let mut enter = false;
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--group" {
                    i += 1;
                    group = Some(
                        args.get(i)
                            .ok_or_else(|| CliError::Usage("--group requires a value".into()))?
                            .clone(),
                    );
                } else if let Some(v) = a.strip_prefix("--group=") {
                    group = Some(v.to_string());
                } else if a == "--enter" {
                    enter = true;
                } else if a.starts_with('-') && a != "-" {
                    return Err(CliError::Usage(format!("unknown flag: {a}")));
                } else if text.is_none() {
                    text = Some(a.clone());
                } else {
                    return Err(CliError::Usage(format!("unexpected argument: {a}")));
                }
                i += 1;
            }
            let mut text = match text {
                Some(t) if t == "-" => read_stdin()?,
                Some(t) => t,
                None => {
                    return Err(CliError::Usage(
                        "send-text requires <text> (or - for stdin)".into(),
                    ));
                }
            };
            if enter {
                text.push('\r');
            }
            Ok(Command::SendText { text, group })
        }
        "key" => {
            if args.is_empty() {
                return Err(CliError::Usage(
                    "key requires at least one <chord>".into(),
                ));
            }
            Ok(Command::Key {
                keys: args.to_vec(),
            })
        }
        "focus" => {
            let group = args
                .first()
                .ok_or_else(|| CliError::Usage("focus requires <group>".into()))?
                .clone();
            Ok(Command::FocusGroup { group })
        }
        "new-section" => {
            let name = args
                .first()
                .ok_or_else(|| CliError::Usage("new-section requires <name>".into()))?
                .clone();
            Ok(Command::NewSection { name })
        }
        "move" => {
            if args.len() < 2 {
                return Err(CliError::Usage("move requires <group> <section>".into()));
            }
            Ok(Command::MoveGroupToSection {
                group: args[0].clone(),
                section: args[1].clone(),
            })
        }
        "page" => {
            let page = args
                .first()
                .ok_or_else(|| CliError::Usage("page requires <name>".into()))?
                .clone();
            // The app validates the name (see bus_exec::page_from_name).
            Ok(Command::GoToPage { page })
        }
        "resize" => {
            if args.len() < 2 {
                return Err(CliError::Usage("resize requires <width> <height>".into()));
            }
            let width: f32 = args[0]
                .parse()
                .map_err(|_| CliError::Usage(format!("invalid width: {}", args[0])))?;
            let height: f32 = args[1]
                .parse()
                .map_err(|_| CliError::Usage(format!("invalid height: {}", args[1])))?;
            Ok(Command::ResizeWindow { width, height })
        }
        "screenshot" => {
            let mut path: Option<PathBuf> = None;
            let mut clipboard = false;
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--clipboard" {
                    clipboard = true;
                } else if a.starts_with('-') {
                    return Err(CliError::Usage(format!("unknown flag: {a}")));
                } else if path.is_none() {
                    path = Some(absolute_path(Path::new(a)));
                } else {
                    return Err(CliError::Usage(format!("unexpected argument: {a}")));
                }
                i += 1;
            }
            if clipboard && path.is_some() {
                return Err(CliError::Usage(
                    "screenshot: give a path or --clipboard, not both".into(),
                ));
            }
            Ok(Command::Screenshot { path, clipboard })
        }
        "read" => parse_read(args),
        "flow-send" => {
            let text = if args.is_empty() {
                return Err(CliError::Usage(
                    "flow-send requires <text> (or - for stdin)".into(),
                ));
            } else if args.len() > 1 {
                // Join words so `flow-send open the logs` reads naturally.
                args.join(" ")
            } else {
                let t = &args[0];
                if t == "-" {
                    read_stdin()?
                } else {
                    t.clone()
                }
            };
            Ok(Command::FlowSend { text })
        }
        "state" => Ok(Command::State),
        "commands" => Ok(Command::ListCommands),
        "ping" => Ok(Command::Ping),
        "raw" => {
            let line = args
                .first()
                .ok_or_else(|| CliError::Usage("raw requires '<json line>'".into()))?;
            bus::decode_command(line).map_err(|e| CliError::Usage(format!("invalid raw json: {e}")))
        }
        other => Err(CliError::Usage(format!("unknown subcommand: {other}"))),
    }
}

/// The read-only `read <what>` namespace: `pane <session> [--lines N|--all]`
/// and `panes [query]`. Nothing here can change focus, page, or scroll.
fn parse_read(args: &[String]) -> Result<Command, CliError> {
    let Some(what) = args.first() else {
        return Err(CliError::Usage("read requires a subcommand: pane | panes".into()));
    };
    let args = &args[1..];
    match what.as_str() {
        "pane" => {
            let mut session: Option<u64> = None;
            let mut lines: Option<usize> = None;
            let mut all = false;
            let mut i = 0;
            while i < args.len() {
                let a = &args[i];
                if a == "--all" {
                    all = true;
                } else if a == "--lines" {
                    i += 1;
                    let n = args
                        .get(i)
                        .ok_or_else(|| CliError::Usage("--lines requires a count".into()))?;
                    lines = Some(parse_lines(n)?);
                } else if let Some(n) = a.strip_prefix("--lines=") {
                    lines = Some(parse_lines(n)?);
                } else if a.starts_with('-') {
                    return Err(CliError::Usage(format!("unknown flag: {a}")));
                } else if session.is_none() {
                    session = Some(
                        a.parse()
                            .map_err(|_| CliError::Usage(format!("invalid session id: {a}")))?,
                    );
                } else {
                    return Err(CliError::Usage(format!("unexpected argument: {a}")));
                }
                i += 1;
            }
            let session = session
                .ok_or_else(|| CliError::Usage("read pane requires <session-id>".into()))?;
            if all && lines.is_some() {
                return Err(CliError::Usage("read pane: give --lines or --all, not both".into()));
            }
            Ok(Command::ReadPane { session, lines, all })
        }
        "panes" => {
            if let Some(flag) = args.iter().find(|a| a.starts_with('-')) {
                return Err(CliError::Usage(format!("unknown flag: {flag}")));
            }
            let query = (!args.is_empty()).then(|| args.join(" "));
            Ok(Command::ReadPanes { query })
        }
        other => Err(CliError::Usage(format!("unknown read subcommand: {other}"))),
    }
}

fn parse_lines(n: &str) -> Result<usize, CliError> {
    n.parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| CliError::Usage(format!("invalid --lines count: {n}")))
}

fn read_usage() -> String {
    let mut out = String::new();
    out.push_str("Usage: pwrde-cli read <what> [args] [--json]\n\n");
    out.push_str("Read-only inspection (never changes focus, page, or scroll):\n");
    out.push_str("  pane <session-id> [--lines N|--all]   A pane's text (visible screen by default, --lines N / --all for scrollback); --json adds title, size, cursor, group, foreground\n");
    out.push_str("  panes [query]                          One line per pane across every group: <session>\\t<group>\\t<title>\\t<foreground>, focused marked *; --json for the full rows\n");
    out
}

fn emit_reply(cmd: &Command, reply: &Reply, json: bool) -> Result<(), CliError> {
    if !reply.ok {
        let msg = reply
            .error
            .clone()
            .unwrap_or_else(|| "unknown error".into());
        return Err(CliError::Reply(msg));
    }
    match &reply.data {
        None => Ok(()),
        Some(data) if !json && matches!(cmd, Command::ReadPane { .. }) => {
            for line in data.get("lines").and_then(Value::as_array).into_iter().flatten() {
                println!("{}", line.as_str().unwrap_or_default());
            }
            Ok(())
        }
        Some(data) if !json && matches!(cmd, Command::ReadPanes { .. }) => {
            for row in data.as_array().into_iter().flatten() {
                println!("{}", pane_row(row));
            }
            Ok(())
        }
        Some(data) => {
            // screenshot and ping: print a JSON string value plainly
            let plain = matches!(cmd, Command::Screenshot { .. } | Command::Ping);
            if plain {
                if let Some(s) = data.as_str() {
                    println!("{s}");
                    return Ok(());
                }
            }
            match serde_json::to_string_pretty(data) {
                Ok(s) => {
                    println!("{s}");
                    Ok(())
                }
                Err(e) => Err(CliError::Reply(format!("failed to format data: {e}"))),
            }
        }
    }
}

fn usage() -> String {
    let mut out = String::new();
    out.push_str("Usage: pwrde-cli [options] <command> [args]\n\n");
    out.push_str("Options:\n");
    out.push_str("  --socket <path>     Bus socket (default: $PWRDE_SOCKET, ~/.pwrde/bus.sock, or a ~/.pwrde/worktrees/*/bus.sock)\n");
    out.push_str("  --timeout <secs>    Request timeout in seconds (default: 10)\n");
    out.push_str("  -h, --help          Show this help\n\n");
    out.push_str("Commands:\n");
    // Friendly CLI names (hyphenated) mapped from the bus snake_case tags.
    let cli_names: &[(&str, &str)] = &[
        ("action", "action"),
        ("new_session", "new-session"),
        ("new_webview", "new-webview"),
        ("send_text", "send-text"),
        ("flow_send", "flow-send"),
        ("key", "key"),
        ("focus_group", "focus"),
        ("new_section", "new-section"),
        ("move_group_to_section", "move"),
        ("go_to_page", "page"),
        ("resize_window", "resize"),
        ("screenshot", "screenshot"),
        ("read_pane", "read pane"),
        ("read_panes", "read panes"),
        ("state", "state"),
        ("list_commands", "commands"),
        ("ping", "ping"),
    ];
    let specs = bus::command_specs();
    let row = |out: &mut String, spec: &bus::CommandSpec| {
        let cli = cli_names
            .iter()
            .find(|(tag, _)| *tag == spec.name)
            .map(|(_, c)| *c)
            .unwrap_or(spec.name);
        if spec.args.is_empty() {
            out.push_str(&format!("  {cli:<16}  {}\n", spec.help));
        } else {
            let head = format!("{cli} {}", spec.args);
            out.push_str(&format!("  {head:<28}  {}\n", spec.help));
        }
    };
    for spec in &specs {
        if spec.read_only {
            continue;
        }
        row(&mut out, spec);
    }
    out.push_str("  raw '<json line>'             Send a raw NDJSON command line\n");
    out.push_str("\nRead-only commands (never change focus, page, or scroll):\n");
    for spec in &specs {
        if !spec.read_only {
            continue;
        }
        row(&mut out, spec);
    }
    out.push_str("\nSend-text extras: --enter appends CR; <text> of - reads stdin.\n");
    out.push_str("Key chords use gpui syntax (cmd-p, escape, cmd-shift-t, ctrl-c, enter).\n");
    out
}

fn canonicalize_dir(path: &Path) -> Result<PathBuf, CliError> {
    path.canonicalize()
        .map_err(|e| CliError::Usage(format!("cannot resolve directory {}: {e}", path.display())))
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// One `read panes` line: `<session>\t<group-index>:<group-name>\t<title>\t<foreground or ->`,
/// with a trailing `*` on the focused pane.
fn pane_row(row: &Value) -> String {
    fn s(v: Option<&Value>) -> &str {
        v.and_then(Value::as_str).unwrap_or("-")
    }
    let group = row.get("group");
    let index = group
        .and_then(|g| g.get("index"))
        .and_then(Value::as_u64)
        .map(|i| i.to_string())
        .unwrap_or_else(|| "-".into());
    let focused = row.get("focused").and_then(Value::as_bool).unwrap_or(false);
    format!(
        "{}\t{index}:{}\t{}\t{}{}",
        row.get("session").and_then(Value::as_u64).map(|i| i.to_string()).unwrap_or_else(|| "-".into()),
        s(group.and_then(|g| g.get("name"))),
        s(row.get("title")),
        s(row.get("foreground")),
        if focused { "*" } else { "" },
    )
}

fn read_stdin() -> Result<String, CliError> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| CliError::Usage(format!("failed to read stdin: {e}")))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_new_webview_with_optional_group() {
        let command = parse_command(
            "new-webview",
            &args(&["https://example.com/docs", "--group", "work"]),
        )
        .unwrap();
        assert_eq!(
            command,
            Command::NewWebview {
                url: "https://example.com/docs".into(),
                group: Some("work".into()),
            }
        );
    }

    #[test]
    fn rejects_non_http_webview_urls() {
        assert!(parse_command("new-webview", &args(&["javascript:alert(1)"])).is_err());
        assert!(parse_command("new-webview", &args(&["https:///missing-host"])).is_err());
    }
}
