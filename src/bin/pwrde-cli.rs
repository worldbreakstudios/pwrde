//! pwrde-cli — talk to a running pwrde instance over the command bus.
//!
//! Hand-rolled argv parsing (no clap). Includes the crate-independent bus
//! protocol module via `#[path]` so this binary stays free of the app crate.

#[path = "../bus.rs"]
mod bus;

use bus::{Command, Reply};
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
    let cmd = parse_command(sub, sub_args)?;

    let path = socket.unwrap_or_else(bus::default_socket_path);
    let timeout = Duration::from_secs(timeout_secs);
    let reply = bus::request(&path, &cmd, timeout).map_err(CliError::Connect)?;
    emit_reply(&cmd, &reply)
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

fn emit_reply(cmd: &Command, reply: &Reply) -> Result<(), CliError> {
    if !reply.ok {
        let msg = reply
            .error
            .clone()
            .unwrap_or_else(|| "unknown error".into());
        return Err(CliError::Reply(msg));
    }
    match &reply.data {
        None => Ok(()),
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
        ("send_text", "send-text"),
        ("flow_send", "flow-send"),
        ("key", "key"),
        ("focus_group", "focus"),
        ("new_section", "new-section"),
        ("move_group_to_section", "move"),
        ("go_to_page", "page"),
        ("resize_window", "resize"),
        ("screenshot", "screenshot"),
        ("state", "state"),
        ("list_commands", "commands"),
        ("ping", "ping"),
    ];
    let specs = bus::command_specs();
    for spec in &specs {
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
    }
    out.push_str("  raw '<json line>'             Send a raw NDJSON command line\n");
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

fn read_stdin() -> Result<String, CliError> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| CliError::Usage(format!("failed to read stdin: {e}")))?;
    Ok(buf)
}
