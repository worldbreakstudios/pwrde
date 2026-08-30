//! Flow: the embedded agent surface. This module is the *backend boundary* —
//! the UI (`main.rs` / `flow_ui.rs`) knows only the [`AgentBackend`] trait,
//! the [`FlowEvent`] stream, and the [`FlowState`] transcript model; it never
//! learns which agent is on the other end.
//!
//! Why the seam: the first backend drives the `claude` CLI in stream-json
//! mode, but it is expected to be swapped for other agents (an in-process
//! model, a different CLI, a remote service). Every agent-specific detail —
//! binary name, flags, NDJSON framing, event shapes — is therefore confined
//! to [`ClaudeCliBackend`] and [`parse_stream_line`]; nothing in `FlowEvent`
//! or the transcript model mentions it.
//!
//! Backends report progress asynchronously: the reader thread forwards
//! [`FlowEvent`]s as [`crate::term::TermEvent::Flow`] over the app's existing
//! event channel (the same pattern as `lfg.rs::spawn_event_stream`), so the
//! gpui foreground is never blocked and all UI updates arrive through the
//! regular event drain.
//!
//! Flow holds several conversations at once: each [`FlowChat`] has its own
//! transcript and its own backend (one agent process per chat, keyed by chat
//! id in `App::flow_backends`), and every event carries the id of the chat
//! it belongs to. [`FlowState::apply`] is the single reducer routing events
//! into the right chat's transcript; `flow_ui.rs` renders the chat list and
//! the open conversation.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;

use crate::term::TermEvent;

/// The only agent abstraction the UI is allowed to know about. Backends push
/// progress onto the app's event channel themselves (they hold a sender);
/// `send`/`shutdown` are the entire control surface.
/// True when the experimental Flow flag (`features.flow`) is on. Off ⇒ no
/// pill bar, `ToggleFlow` no-ops, `flow-send` is refused, no backend spawns.
pub fn enabled() -> bool {
    crate::features::enabled(crate::features::FLOW)
}

pub trait AgentBackend: Send {
    /// Hand one user request to the agent. Errors mean the agent could not
    /// accept the request (spawn failure, dead pipe) and the caller should
    /// surface that to the user.
    fn send(&mut self, text: &str) -> std::io::Result<()>;
    /// Stop the agent and release its resources. Called on app quit (and any
    /// future teardown path).
    fn shutdown(&mut self);
}

/// Backend-agnostic progress events. Never gains agent-specific variants —
/// backends translate their wire formats into these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowEvent {
    /// The backend finished initializing and will accept requests.
    Ready,
    /// A chunk of assistant prose. Consecutive chunks in one turn merge.
    Text(String),
    /// The agent started an action (tool call) in the workspace.
    ToolStarted { id: String, title: String, detail: String },
    /// A previously started action finished.
    ToolFinished { id: String, ok: bool, detail: String },
    /// The agent finished the whole turn; the composer may send again.
    TurnDone,
    /// Something failed at the agent level (spawn, crash, turn error).
    Error(String),
}

/// The agent-first backend: drives the `claude` CLI as a child process in
/// stream-json mode (one NDJSON frame per direction), pointed at a
/// system-prompt file that teaches it `pwrde-cli`. Kept private to this
/// module behind [`spawn_flow_backend`] so the concrete type never leaks.
pub struct ClaudeCliBackend {
    child: Child,
    stdin: std::process::ChildStdin,
    /// Set before killing the child so the reader thread can tell an
    /// intentional shutdown apart from a crash (no spurious `Error`).
    shutting_down: Arc<AtomicBool>,
}

impl ClaudeCliBackend {
    /// Spawn the CLI in `cwd` and start its stdout reader thread, tagging
    /// every event with `chat` so the reducer routes it to the right
    /// transcript. The prompt file is (re)written first so an updated skill
    /// doc is picked up on the next session.
    pub(crate) fn new(cwd: &Path, tx: Sender<TermEvent>, chat: u64) -> std::io::Result<Self> {
        let prompt = write_prompt_file()?;
        let mut cmd = crate::git::augmented_command("claude");
        cmd.arg("-p")
            .arg("--verbose")
            .args(["--input-format", "stream-json"])
            .args(["--output-format", "stream-json"])
            .arg("--append-system-prompt-file")
            .arg(prompt)
            .args(["--allowedTools", "Bash(pwrde-cli:*)"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The app exports its bus socket to child sessions; the agent needs
        // the same pointer so `pwrde-cli` targets this window.
        if let Ok(sock) = std::env::var("PWRDE_SOCKET") {
            cmd.env("PWRDE_SOCKET", sock);
        }
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "no stdout")
            })?;
        let shutting_down = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutting_down);
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                for ev in parse_stream_line(&line) {
                    if tx.send(TermEvent::Flow { chat, ev }).is_err() {
                        return; // app gone
                    }
                }
            }
            if !flag.load(Ordering::Relaxed) {
                let _ = tx
                    .send(TermEvent::Flow { chat, ev: FlowEvent::Error("agent exited".into()) });
            }
        });
        Ok(Self { child, stdin, shutting_down })
    }
}

impl AgentBackend for ClaudeCliBackend {
    fn send(&mut self, text: &str) -> std::io::Result<()> {
        // serde_json does the escaping; one NDJSON frame per request.
        let frame = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{ "type": "text", "text": text }]
            }
        });
        writeln!(self.stdin, "{frame}")?;
        self.stdin.flush()
    }

    fn shutdown(&mut self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The single construction site for the concrete backend. `main.rs` calls
/// this and holds only the trait object; if the backend is swapped later,
/// this is the only function that changes.
pub fn spawn_flow_backend(
    cwd: &Path,
    tx: Sender<TermEvent>,
    chat: u64,
) -> Option<Box<dyn AgentBackend>> {
    match ClaudeCliBackend::new(cwd, tx, chat) {
        Ok(backend) => Some(Box::new(backend)),
        Err(e) => {
            eprintln!("pwrde: flow backend failed to spawn: {e}");
            None
        }
    }
}

/// Fixed preamble ahead of the `pwrde-cli` skill doc in the agent's system
/// prompt: tells the agent who it is, that it owns *this* window, and how to
/// behave (act, confirm, report briefly).
const PREAMBLE: &str = "\
# You are Flow, the agent embedded in the pwrde terminal workspace.
You control the pwrde window that spawned you. `PWRDE_SOCKET` in your environment points at its command bus; `pwrde-cli` targets it automatically. Always run `pwrde-cli state` before acting so you know the current groups, tiles and tabs, and re-check `state` (or `screenshot`) after acting to confirm the effect. Keep replies short: one or two sentences, then the actions you took. Never ask for permission — act, then report. The reference below explains every command.";

/// Write `~/.pwrde/flow-prompt.md` (the agent's system prompt: preamble +
/// the full `pwrde-cli` skill reference) and return its path. Called once at
/// startup so the file exists before any backend spawns.
pub fn write_prompt_file() -> std::io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    let dir = Path::new(&home).join(".pwrde");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("flow-prompt.md");
    std::fs::write(&path, format!("{PREAMBLE}\n\n{}", include_str!("../.claude/skills/pwrde-cli/SKILL.md")))?;
    Ok(path)
}

/// Translate one NDJSON line from the agent's stdout into zero or more
/// [`FlowEvent`]s. Pure so it can be unit-tested against realistic frames.
/// Unparseable and unknown lines are ignored (the CLI may interleave its own
/// control frames).
pub(crate) fn parse_stream_line(line: &str) -> Vec<FlowEvent> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "system" if v.get("subtype").and_then(|s| s.as_str()) == Some("init") => {
            vec![FlowEvent::Ready]
        }
        "assistant" => {
            let mut out = Vec::new();
            let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                return out;
            };
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                out.push(FlowEvent::Text(text.to_string()));
                            }
                        }
                    }
                    Some("tool_use") => {
                        let id = str_field(block, "id");
                        let name = str_field(block, "name");
                        let title = tool_title(&name, block.get("input"));
                        out.push(FlowEvent::ToolStarted { id, title, detail: name });
                    }
                    _ => {}
                }
            }
            out
        }
        "user" => {
            // The CLI delivers tool results as a user message holding
            // `tool_result` blocks; plain text frames we *send* never come
            // back on stdout, so anything without a tool_result is ignored.
            let mut out = Vec::new();
            let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                return out;
            };
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                    continue;
                }
                let is_error = block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                out.push(FlowEvent::ToolFinished {
                    id: str_field(block, "tool_use_id"),
                    ok: !is_error,
                    detail: truncate_chars(&one_line(&result_text(block.get("content"))), 120),
                });
            }
            out
        }
        "result" => {
            let mut out = Vec::new();
            if v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) {
                let msg = v
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("agent turn failed");
                out.push(FlowEvent::Error(msg.to_string()));
            }
            out.push(FlowEvent::TurnDone);
            out
        }
        _ => Vec::new(),
    }
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Card title for a tool call: for shell commands show the command itself
/// (first line, ≤80 chars) since that is what the user cares about; for
/// everything else the tool name is the title.
fn tool_title(name: &str, input: Option<&serde_json::Value>) -> String {
    if name == "Bash" {
        if let Some(cmd) = input.and_then(|i| i.get("command")).and_then(|c| c.as_str()) {
            let first = cmd.lines().next().unwrap_or("").trim();
            if !first.is_empty() {
                return truncate_chars(first, 80);
            }
        }
    }
    name.to_string()
}

/// Flatten a tool-result `content` (a plain string, or an array of
/// `{type:"text",text}` blocks) into display text.
fn result_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => {
            let mut out = String::new();
            for item in items {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(text);
                    }
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Collapse a tool result onto one line (action cards show a single
/// ellipsized line; multi-line JSON or logs would stack otherwise).
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Char-boundary-safe truncation (never splits a multi-byte character).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// One row of the Flow transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowMsg {
    User(String),
    Assistant(String),
    Action { id: String, title: String, detail: String, status: ActionStatus },
}

/// Lifecycle of an [`FlowMsg::Action`] card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStatus {
    Running,
    Done,
    Failed,
}

/// Which surface the panel shows: the chat list, or one open conversation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FlowView {
    #[default]
    List,
    Chat,
}

/// One conversation: its transcript plus the per-chat invariants (`busy`,
/// `ready`, `draft_error`) and the list-card fields (`title`, `unseen`,
/// `last_activity`). `title` is derived from the first user message;
/// `last_activity` is epoch seconds the caller feeds in — this module never
/// reads the clock, so reducers stay pure and tests stay deterministic.
#[derive(Debug, Default, Clone)]
pub struct FlowChat {
    pub id: u64,
    pub title: Option<String>,
    pub messages: Vec<FlowMsg>,
    pub busy: bool,
    pub ready: bool,
    pub draft_error: Option<String>,
    pub last_activity: i64,
    pub unseen: bool,
}

/// The whole Flow UI state: panel visibility, which surface is shown, the
/// chat list and the active one. Updated only via the methods below.
#[derive(Debug, Default, Clone)]
pub struct FlowState {
    pub open: bool,
    pub view: FlowView,
    pub chats: Vec<FlowChat>,
    pub active: usize,
    /// Set when the panel opens so the next render focuses the composer
    /// (focusing needs the window, which only render has).
    pub wants_focus: bool,
}

/// Wall-clock epoch seconds for the reducer's `now` argument. The reducer
/// itself never reads the clock (tests feed times in); call sites use this.
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Age of a chat for the list card: "now" (< 60s), then floored "Nm" /
/// "Nh" / "Nd" — deliberately terser than `pr_ui.rs`'s `format_ago` (no
/// rounding, no " ago") because it sits in a narrow card corner; no chrono,
/// just epoch seconds in.
pub fn format_age(now: i64, then: i64) -> String {
    let s = (now - then).max(0);
    if s < 60 {
        "now".to_string()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

impl FlowState {
    /// Start a fresh chat, make it the active one and switch to it. Ids are
    /// one past the highest seen so they stay stable across view switches.
    pub fn new_chat(&mut self, now: i64) -> u64 {
        let id = self.chats.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.chats
            .push(FlowChat { id, last_activity: now, ..FlowChat::default() });
        self.active = self.chats.len() - 1;
        self.view = FlowView::Chat;
        id
    }

    /// Switch to a chat by id and clear its unread marker (it is on screen).
    pub fn open_chat(&mut self, id: u64) {
        if let Some(i) = self.chats.iter().position(|c| c.id == id) {
            self.active = i;
            self.chats[i].unseen = false;
            self.view = FlowView::Chat;
        }
    }

    pub fn active_chat(&self) -> Option<&FlowChat> {
        self.chats.get(self.active)
    }

    pub fn active_chat_mut(&mut self) -> Option<&mut FlowChat> {
        self.chats.get_mut(self.active)
    }

    /// The chat an event is about — events are routed by id, not by which
    /// chat is on screen, so background chats keep receiving their turns.
    fn chat_mut(&mut self, id: u64) -> Option<&mut FlowChat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    /// Reduce one backend event into the transcript of chat `id` (unknown
    /// ids are ignored). Text chunks arriving mid-turn append to the open
    /// Assistant bubble; a new turn starts a new one. A turn finishing in a
    /// chat that is not currently visible marks it unseen for the list
    /// badge.
    pub fn apply(&mut self, id: u64, ev: FlowEvent, now: i64) {
        let visible = self.open
            && self.view == FlowView::Chat
            && self.active_chat().is_some_and(|c| c.id == id);
        let Some(chat) = self.chat_mut(id) else { return };
        match ev {
            FlowEvent::Ready => chat.ready = true,
            FlowEvent::Text(text) => {
                let merged = chat.busy && matches!(chat.messages.last(), Some(FlowMsg::Assistant(_)));
                if merged {
                    if let Some(FlowMsg::Assistant(last)) = chat.messages.last_mut() {
                        last.push_str(&text);
                    }
                } else {
                    chat.messages.push(FlowMsg::Assistant(text));
                }
            }
            FlowEvent::ToolStarted { id, title, detail } => {
                chat.messages
                    .push(FlowMsg::Action { id, title, detail, status: ActionStatus::Running });
            }
            FlowEvent::ToolFinished { id, ok, detail } => {
                if let Some(FlowMsg::Action { status, detail: d, .. }) = chat
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|m| matches!(m, FlowMsg::Action { id: i, .. } if *i == id))
                {
                    *status = if ok { ActionStatus::Done } else { ActionStatus::Failed };
                    *d = detail;
                }
            }
            FlowEvent::TurnDone => {
                chat.busy = false;
                if !visible {
                    chat.unseen = true;
                }
            }
            FlowEvent::Error(e) => {
                chat.busy = false;
                chat.draft_error = Some(e);
                // A background failure deserves the badge at least as much
                // as a background success — a died agent must not look idle.
                if !visible {
                    chat.unseen = true;
                }
            }
        }
        chat.last_activity = now;
    }

    /// Record the user's request in chat `id` and mark the turn in flight.
    /// The first user message also names the chat (the list-card title).
    pub fn push_user(&mut self, id: u64, text: String, now: i64) {
        let Some(chat) = self.chat_mut(id) else { return };
        let first = text.lines().next().unwrap_or("").trim();
        if chat.title.is_none() && !first.is_empty() {
            chat.title = Some(truncate_chars(first, 40));
        }
        chat.messages.push(FlowMsg::User(text));
        chat.busy = true;
        chat.draft_error = None;
        chat.last_activity = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_line_yields_ready() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp/x","session_id":"s1","tools":["Bash"],"model":"x"}"#;
        assert_eq!(parse_stream_line(line), vec![FlowEvent::Ready]);
    }

    #[test]
    fn assistant_text_and_bash_tool_use_map_to_text_and_tool_started() {
        let line = r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"Checking the workspace."},{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"pwrde-cli state\n# inspect groups"}}]}}"#;
        let evs = parse_stream_line(line);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0], FlowEvent::Text("Checking the workspace.".into()));
        assert_eq!(
            evs[1],
            FlowEvent::ToolStarted {
                id: "toolu_01".into(),
                title: "pwrde-cli state".into(),
                detail: "Bash".into(),
            }
        );
    }

    #[test]
    fn non_bash_tool_use_uses_tool_name_as_title() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_02","name":"Read","input":{"file_path":"/tmp/a"}}]}}"#;
        assert_eq!(
            parse_stream_line(line),
            vec![FlowEvent::ToolStarted {
                id: "toolu_02".into(),
                title: "Read".into(),
                detail: "Read".into(),
            }]
        );
    }

    #[test]
    fn bash_title_is_first_line_trimmed_to_80_chars() {
        let cmd = format!("echo {}\necho second", "x".repeat(120));
        let line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t","name":"Bash","input":{{"command":{}}}}}]}}}}"#,
            serde_json::json!(cmd)
        );
        match parse_stream_line(&line).first() {
            Some(FlowEvent::ToolStarted { title, .. }) => {
                assert_eq!(title.chars().count(), 80);
                assert!(title.starts_with("echo xxxx"));
            }
            other => panic!("expected ToolStarted, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_with_string_content_maps_to_tool_finished() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","is_error":false,"content":"{\"groups\":2}"}]}}"#;
        assert_eq!(
            parse_stream_line(line),
            vec![FlowEvent::ToolFinished {
                id: "toolu_01".into(),
                ok: true,
                detail: r#"{"groups":2}"#.into(),
            }]
        );
    }

    #[test]
    fn tool_result_with_block_content_and_error_maps_to_failed() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_03","is_error":true,"content":[{"type":"text","text":"boom"},{"type":"text","text":"bang"}]}]}}"#;
        assert_eq!(
            parse_stream_line(line),
            vec![FlowEvent::ToolFinished { id: "toolu_03".into(), ok: false, detail: "boom bang".into() }]
        );
    }

    #[test]
    fn tool_result_detail_collapses_to_one_line() {
        let content = serde_json::json!("{\n  \"page\": \"sessions\",\n  \"groups\": []\n}").to_string();
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","is_error":false,"content":{content}}}]}}}}"#
        );
        match parse_stream_line(&line).first() {
            Some(FlowEvent::ToolFinished { detail, .. }) => {
                assert_eq!(detail, "{ \"page\": \"sessions\", \"groups\": [] }");
            }
            other => panic!("expected ToolFinished, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_detail_is_truncated_to_120_chars() {
        let content = serde_json::json!("y".repeat(400)).to_string();
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","is_error":false,"content":{content}}}]}}}}"#
        );
        match parse_stream_line(&line).first() {
            Some(FlowEvent::ToolFinished { detail, .. }) => {
                assert_eq!(detail.chars().count(), 120);
            }
            other => panic!("expected ToolFinished, got {other:?}"),
        }
    }

    #[test]
    fn success_result_yields_turn_done_only() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done."}"#;
        assert_eq!(parse_stream_line(line), vec![FlowEvent::TurnDone]);
    }

    #[test]
    fn errored_result_yields_error_then_turn_done() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"nope"}"#;
        assert_eq!(
            parse_stream_line(line),
            vec![FlowEvent::Error("nope".into()), FlowEvent::TurnDone]
        );
    }

    #[test]
    fn junk_and_unknown_lines_are_ignored() {
        assert!(parse_stream_line("not json").is_empty());
        assert!(parse_stream_line(r#"{"type":"other"}"#).is_empty());
        assert!(parse_stream_line(r#"{"type":"system","subtype":"other"}"#).is_empty());
    }

    #[test]
    fn apply_pairs_tool_start_and_finish() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.push_user(1, "spawn a shell".into(), 0);
        assert!(st.active_chat().unwrap().busy);
        st.apply(1, FlowEvent::ToolStarted { id: "t1".into(), title: "pwrde-cli state".into(), detail: "Bash".into() }, 1);
        st.apply(1, FlowEvent::ToolFinished { id: "t1".into(), ok: true, detail: "ok".into() }, 2);
        let chat = st.active_chat().unwrap();
        assert_eq!(chat.messages.len(), 2);
        match &chat.messages[1] {
            FlowMsg::Action { id, status, detail, .. } => {
                assert_eq!(id, "t1");
                assert_eq!(*status, ActionStatus::Done);
                assert_eq!(detail, "ok");
            }
            other => panic!("expected Action, got {other:?}"),
        }
    }

    #[test]
    fn apply_marks_unmatched_tool_finish_failed_and_ignores_unknown_ids() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.apply(1, FlowEvent::ToolStarted { id: "t1".into(), title: "a".into(), detail: "Bash".into() }, 0);
        st.apply(1, FlowEvent::ToolFinished { id: "nope".into(), ok: true, detail: "x".into() }, 0);
        st.apply(1, FlowEvent::ToolFinished { id: "t1".into(), ok: false, detail: "bad".into() }, 0);
        match &st.active_chat().unwrap().messages[0] {
            FlowMsg::Action { status, detail, .. } => {
                assert_eq!(*status, ActionStatus::Failed);
                assert_eq!(detail, "bad");
            }
            other => panic!("expected Action, got {other:?}"),
        }
    }

    #[test]
    fn apply_turn_done_clears_busy_and_error_sets_draft_error() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.push_user(1, "hi".into(), 0);
        st.apply(1, FlowEvent::TurnDone, 1);
        assert!(!st.active_chat().unwrap().busy);
        st.push_user(1, "again".into(), 2);
        st.apply(1, FlowEvent::Error("agent exited".into()), 3);
        let chat = st.active_chat().unwrap();
        assert!(!chat.busy);
        assert_eq!(chat.draft_error.as_deref(), Some("agent exited"));
        st.apply(1, FlowEvent::Ready, 4);
        assert!(st.active_chat().unwrap().ready);
    }

    #[test]
    fn text_chunks_merge_into_one_bubble_within_a_turn() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.push_user(1, "go".into(), 0);
        st.apply(1, FlowEvent::Text("a".into()), 1);
        st.apply(1, FlowEvent::Text("b".into()), 1);
        let chat = st.active_chat().unwrap();
        assert_eq!(chat.messages.len(), 2); // User + one merged Assistant
        assert_eq!(chat.messages[1], FlowMsg::Assistant("ab".into()));
    }

    #[test]
    fn a_new_turn_starts_a_fresh_assistant_bubble() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.push_user(1, "one".into(), 0);
        st.apply(1, FlowEvent::Text("a".into()), 1);
        st.apply(1, FlowEvent::TurnDone, 2);
        st.push_user(1, "two".into(), 3);
        st.apply(1, FlowEvent::Text("b".into()), 4);
        assert_eq!(
            st.active_chat().unwrap().messages,
            vec![
                FlowMsg::User("one".into()),
                FlowMsg::Assistant("a".into()),
                FlowMsg::User("two".into()),
                FlowMsg::Assistant("b".into()),
            ]
        );
    }

    #[test]
    fn events_route_by_chat_id() {
        let mut st = FlowState::default();
        st.new_chat(0); // chat 1, active
        st.new_chat(1); // chat 2, active now
        st.push_user(1, "for A".into(), 2);
        st.push_user(2, "for B".into(), 2);
        // Chat 1's turn streams while chat 2 is on screen: A must not leak
        // into B, and events for an unknown id are dropped entirely.
        st.apply(1, FlowEvent::Text("A reply".into()), 3);
        st.apply(1, FlowEvent::ToolStarted { id: "t1".into(), title: "ls".into(), detail: "Bash".into() }, 4);
        st.apply(99, FlowEvent::Text("ghost".into()), 5);
        assert_eq!(
            st.chats[0].messages,
            vec![
                FlowMsg::User("for A".into()),
                FlowMsg::Assistant("A reply".into()),
                FlowMsg::Action { id: "t1".into(), title: "ls".into(), detail: "Bash".into(), status: ActionStatus::Running },
            ]
        );
        assert_eq!(st.chats[1].messages, vec![FlowMsg::User("for B".into())]);
        assert!(st
            .chats
            .iter()
            .all(|c| !c.messages.iter().any(|m| matches!(m, FlowMsg::Assistant(t) if t == "ghost"))));
    }

    #[test]
    fn busy_is_per_chat() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.new_chat(1);
        st.push_user(1, "A's turn".into(), 1); // A turns busy even though B is active
        assert!(st.chats[0].busy);
        assert!(!st.chats[1].busy);
        // B still accepts a send while A is mid-turn.
        st.push_user(2, "B's turn".into(), 1);
        assert!(st.chats[1].busy);
        st.apply(1, FlowEvent::TurnDone, 2);
        assert!(!st.chats[0].busy);
        assert!(st.chats[1].busy);
    }

    #[test]
    fn background_turn_done_marks_unseen_and_open_chat_clears_it() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.new_chat(1);
        st.open = true; // panel open, chat 2 on screen
        st.push_user(1, "A's turn".into(), 1);
        st.apply(1, FlowEvent::TurnDone, 2); // finished in the background
        assert!(st.chats[0].unseen);
        st.apply(2, FlowEvent::TurnDone, 2); // the visible chat is never unseen
        assert!(!st.chats[1].unseen);
        st.open_chat(1); // switching to it clears the marker
        assert!(!st.chats[0].unseen);
        assert_eq!(st.active, 0);
        assert_eq!(st.view, FlowView::Chat);
        // A closed panel means even the active chat's turn is unseen.
        let mut closed = FlowState::default();
        closed.new_chat(0);
        closed.push_user(1, "hi".into(), 1);
        closed.apply(1, FlowEvent::TurnDone, 2);
        assert!(closed.chats[0].unseen);
    }

    #[test]
    fn background_error_marks_unseen_too() {
        // A background agent failure must badge the chat — a died agent
        // should be at least as visible in the list as a finished turn.
        let mut st = FlowState::default();
        st.new_chat(0);
        st.new_chat(1);
        st.open = true; // chat 2 on screen; chat 1 fails in the background
        st.push_user(1, "go".into(), 1);
        st.apply(1, FlowEvent::Error("agent exited".into()), 2);
        assert!(st.chats[0].unseen);
        assert!(!st.chats[0].busy);
        assert_eq!(st.chats[0].draft_error.as_deref(), Some("agent exited"));
        // The visible chat's failure is on screen already — no badge.
        st.apply(2, FlowEvent::Error("boom".into()), 3);
        assert!(!st.chats[1].unseen);
    }

    #[test]
    fn title_comes_from_first_user_message_and_truncates() {
        let mut st = FlowState::default();
        st.new_chat(0);
        st.push_user(1, "  Review the open PR\nsecond line ignored".into(), 1);
        assert_eq!(st.active_chat().unwrap().title.as_deref(), Some("Review the open PR"));
        // Only the first user message names the chat.
        st.push_user(1, "and now this".into(), 2);
        assert_eq!(st.active_chat().unwrap().title.as_deref(), Some("Review the open PR"));
        // Long first lines truncate to 40 chars on char boundaries.
        let mut st2 = FlowState::default();
        st2.new_chat(0);
        st2.push_user(1, format!("🦀{}", "x".repeat(60)).into(), 1);
        let title = st2.active_chat().unwrap().title.clone().unwrap();
        assert_eq!(title.chars().count(), 40);
        assert!(title.is_char_boundary(title.len()));
    }

    #[test]
    fn format_age_buckets() {
        let t = 1_700_000_000;
        assert_eq!(format_age(t, t), "now");
        assert_eq!(format_age(t, t - 59), "now");
        assert_eq!(format_age(t, t - 60), "1m");
        assert_eq!(format_age(t, t - 3_599), "59m");
        assert_eq!(format_age(t, t - 3_600), "1h");
        assert_eq!(format_age(t, t - 86_399), "23h");
        assert_eq!(format_age(t, t - 86_400), "1d");
        assert_eq!(format_age(t, t - 7 * 86_400), "7d");
    }
}
