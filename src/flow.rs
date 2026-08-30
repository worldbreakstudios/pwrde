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
//! [`FlowState::apply`] is the single reducer turning events into transcript
//! entries; `flow_ui.rs` renders that transcript.

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
    /// Spawn the CLI in `cwd` and start its stdout reader thread. The prompt
    /// file is (re)written first so an updated skill doc is picked up on the
    /// next session.
    pub(crate) fn new(cwd: &Path, tx: Sender<TermEvent>) -> std::io::Result<Self> {
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
                    if tx.send(TermEvent::Flow(ev)).is_err() {
                        return; // app gone
                    }
                }
            }
            if !flag.load(Ordering::Relaxed) {
                let _ = tx.send(TermEvent::Flow(FlowEvent::Error("agent exited".into())));
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
pub fn spawn_flow_backend(cwd: &Path, tx: Sender<TermEvent>) -> Option<Box<dyn AgentBackend>> {
    match ClaudeCliBackend::new(cwd, tx) {
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

/// The whole Flow UI state: panel visibility, transcript, and the composer
/// invariants (`busy`, `ready`, `draft_error`). Updated only via
/// [`FlowState::apply`] / [`FlowState::push_user`].
#[derive(Debug, Default, Clone)]
pub struct FlowState {
    pub open: bool,
    pub busy: bool,
    pub ready: bool,
    pub messages: Vec<FlowMsg>,
    pub draft_error: Option<String>,
    /// Set when the panel opens so the next render focuses the composer
    /// (focusing needs the window, which only render has).
    pub wants_focus: bool,
}

impl FlowState {
    /// Reduce one backend event into transcript state. Text chunks arriving
    /// mid-turn append to the open Assistant bubble; a new turn starts a new
    /// one.
    pub fn apply(&mut self, ev: FlowEvent) {
        match ev {
            FlowEvent::Ready => self.ready = true,
            FlowEvent::Text(text) => {
                if self.busy {
                    if let Some(FlowMsg::Assistant(last)) = self.messages.last_mut() {
                        last.push_str(&text);
                        return;
                    }
                }
                self.messages.push(FlowMsg::Assistant(text));
            }
            FlowEvent::ToolStarted { id, title, detail } => {
                self.messages
                    .push(FlowMsg::Action { id, title, detail, status: ActionStatus::Running });
            }
            FlowEvent::ToolFinished { id, ok, detail } => {
                if let Some(FlowMsg::Action { status, detail: d, .. }) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|m| matches!(m, FlowMsg::Action { id: i, .. } if *i == id))
                {
                    *status = if ok { ActionStatus::Done } else { ActionStatus::Failed };
                    *d = detail;
                }
            }
            FlowEvent::TurnDone => self.busy = false,
            FlowEvent::Error(e) => {
                self.busy = false;
                self.draft_error = Some(e);
            }
        }
    }

    /// Record the user's request and mark the turn in flight.
    pub fn push_user(&mut self, text: String) {
        self.messages.push(FlowMsg::User(text));
        self.busy = true;
        self.draft_error = None;
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
        st.push_user("spawn a shell".into());
        assert!(st.busy);
        st.apply(FlowEvent::ToolStarted { id: "t1".into(), title: "pwrde-cli state".into(), detail: "Bash".into() });
        st.apply(FlowEvent::ToolFinished { id: "t1".into(), ok: true, detail: "ok".into() });
        assert_eq!(st.messages.len(), 2);
        match &st.messages[1] {
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
        st.apply(FlowEvent::ToolStarted { id: "t1".into(), title: "a".into(), detail: "Bash".into() });
        st.apply(FlowEvent::ToolFinished { id: "nope".into(), ok: true, detail: "x".into() });
        st.apply(FlowEvent::ToolFinished { id: "t1".into(), ok: false, detail: "bad".into() });
        match &st.messages[0] {
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
        st.push_user("hi".into());
        st.apply(FlowEvent::TurnDone);
        assert!(!st.busy);
        st.push_user("again".into());
        st.apply(FlowEvent::Error("agent exited".into()));
        assert!(!st.busy);
        assert_eq!(st.draft_error.as_deref(), Some("agent exited"));
        st.apply(FlowEvent::Ready);
        assert!(st.ready);
    }

    #[test]
    fn text_chunks_merge_into_one_bubble_within_a_turn() {
        let mut st = FlowState::default();
        st.push_user("go".into());
        st.apply(FlowEvent::Text("a".into()));
        st.apply(FlowEvent::Text("b".into()));
        assert_eq!(st.messages.len(), 2); // User + one merged Assistant
        assert_eq!(st.messages[1], FlowMsg::Assistant("ab".into()));
    }

    #[test]
    fn a_new_turn_starts_a_fresh_assistant_bubble() {
        let mut st = FlowState::default();
        st.push_user("one".into());
        st.apply(FlowEvent::Text("a".into()));
        st.apply(FlowEvent::TurnDone);
        st.push_user("two".into());
        st.apply(FlowEvent::Text("b".into()));
        assert_eq!(
            st.messages,
            vec![
                FlowMsg::User("one".into()),
                FlowMsg::Assistant("a".into()),
                FlowMsg::User("two".into()),
                FlowMsg::Assistant("b".into()),
            ]
        );
    }
}
