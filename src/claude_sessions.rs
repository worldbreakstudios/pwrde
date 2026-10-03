//! Best-effort reader for local Claude Code session transcripts.
//!
//! `~/.claude/projects/<project>/<session-id>.jsonl` is an **internal Claude
//! Code storage format**, not a published API: the directory names are slugs
//! derived from the original cwd, the record shape is unversioned, and both
//! may change between releases. Everything here therefore treats that storage
//! as hostile input:
//!
//! * parsing is best-effort — a malformed line, an unknown record type, a
//!   missing field or an unreadable file only ever drops that one transcript,
//!   never the whole picker;
//! * only metadata is read, plus the two things that can name a session: the
//!   `ai-title` record's `aiTitle` when the store carries one, and, failing
//!   that, a **capped excerpt of the first human-authored prompt**. That
//!   excerpt is the only message text that ever reaches the UI or a log, and a
//!   transcript body beyond it is never read out — transcripts can hold
//!   secrets;
//! * nothing under `~/.claude` is ever written, moved, or deleted;
//! * the cwd comes from the transcripts themselves, never from the encoded
//!   project-directory slug.
//!
//! [`ClaudeSession::resume_argv`] is what the launcher uses: it returns the
//! resume command as **separate arguments**, so a session id can never be
//! interpreted as shell syntax.
//!
//! # Grounded record shapes
//!
//! Observed in local `~/.claude/projects/*/*.jsonl` transcripts (2026-09):
//!
//! * the session name is not part of a user record — it is its own
//!   `{"type":"ai-title","aiTitle":"…","sessionId":…}` record, which Claude
//!   *regenerates* as the conversation grows, appending another copy, so the
//!   last one in the file is the current name;
//! * the initial human prompt is a `{"type":"user",…}` record whose
//!   `message.content` is either a plain string or an array of
//!   `{"type":"text","text":…}` parts, together with `isSidechain:false`, no
//!   `isMeta`, no `toolUseResult`, and usually `origin:{"kind":"human"}` and
//!   `promptSource:"typed"`;
//! * tool results are `type:"user"` records too, but carry `toolUseResult` and
//!   a `tool_result` content part — never prompt text;
//! * slash-command echoes and reminders are `type:"user"` records whose text
//!   is wrapped in known `<command-name>`/`<local-command-stdout>`/
//!   `<system-reminder>`-style tags, or flagged `isMeta:true`.
//!
//! The recorded `gitBranch` is deliberately *not* a title: the worktree
//! directory name it mirrors is already the cwd fallback below.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Longest transcript line we will even look at. A longer line is skipped
/// rather than allocated (a real transcript line is a few KB).
const MAX_LINE: u64 = 256 * 1024;
/// How many lines of one transcript we scan before giving up on its metadata.
/// Claude writes the cwd, session id and timestamp in a record near the top,
/// so this is generous; the cap keeps one huge transcript from stalling the
/// picker.
const MAX_LINES: usize = 400;
/// Bound on how many transcript files one scan opens, so a store with an
/// implausible number of projects cannot block the caller for long.
const MAX_TRANSCRIPTS: usize = 2_000;
/// How many sessions one scan returns, newest first.
pub const MAX_SESSIONS: usize = 200;
/// Longest title we build, in Unicode scalar values. A row's title is one short
/// line, so anything longer is cut at a character boundary and marked with an
/// ellipsis rather than split mid-code-point.
const TITLE_CHARS: usize = 80;
/// Bytes of a transcript's tail scanned for the newest `ai-title` record.
/// Claude appends the regenerated name near the end of the file; the cap keeps
/// one huge transcript from ever being read in full.
const TAIL_BYTES: u64 = 512 * 1024;
/// Cheap pre-filter before a tail line is parsed as JSON.
const AI_TITLE_MARKER: &str = "\"ai-title\"";
/// Angle-bracket tags that wrap Claude's own scaffolding — slash-command
/// echoes, local command output, reminders — rather than user prose. A prompt
/// beginning with one of these never becomes a title.
const INTERNAL_TAGS: [&str; 10] = [
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-caveat",
    "system-reminder",
    "bash-input",
    "bash-stdout",
    "user-prompt-submit-hook",
    "post-tool-use",
];

/// One resumable Claude Code session, reduced to what the picker needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeSession {
    /// The session UUID (Claude's session id, also the transcript's stem).
    pub id: String,
    /// The cwd Claude recorded for the session. Only sessions whose cwd still
    /// exists as a directory are ever returned.
    pub cwd: PathBuf,
    /// A short, non-sensitive display title, in strict priority: the recorded
    /// `ai-title` session name when the transcript carries one, else a capped
    /// excerpt of the first human-authored prompt, else the cwd's directory
    /// name. Never more than [`TITLE_CHARS`] characters of anything.
    pub title: String,
    /// Recency: the newest transcript timestamp we could parse, falling back
    /// to the transcript file's modified time.
    pub updated: SystemTime,
}

impl ClaudeSession {
    /// The resume command as argv: `claude`, `--resume`, `<session id>`.
    /// Always three elements — the id is passed as its own argument, so
    /// nothing read out of a transcript can become shell syntax.
    pub fn resume_argv(&self) -> [&str; 3] {
        ["claude", "--resume", self.id.as_str()]
    }

    /// The same command as one display line. Used both for status text and as
    /// the line typed into a resumed pane's shell: any character outside the
    /// safe set is single-quoted, so an id carrying shell metacharacters
    /// (`x; touch /tmp/pwned`, or anything else a planted transcript or
    /// filename can produce) stays one inert word. Never use `join` on
    /// [`ClaudeSession::resume_argv`] for that line — that would give the
    /// shell exactly the syntax this quoting removes.
    pub fn resume_display(&self) -> String {
        format!("claude --resume {}", shell_quote(&self.id))
    }

    /// The line to *type* into a pane's shell to resume this session: the
    /// same text as [`ClaudeSession::resume_display`], kept as one named
    /// concept so a caller cannot accidentally build it by joining argv.
    pub fn typed_command_line(&self) -> String {
        self.resume_display()
    }
}

/// Minimal single-quote shell quoting, for display strings only.
fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `~/.claude/projects`, or `None` when there is no home directory.
pub fn projects_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join("projects"))
}

/// The recent Claude sessions on this machine, newest first, one entry per
/// session UUID. Never fails: a missing or unreadable store is simply empty.
pub fn load_recent() -> Vec<ClaudeSession> {
    match projects_dir() {
        Some(root) => load_recent_under(&root),
        None => Vec::new(),
    }
}

/// [`load_recent`] against an explicit store root — the test seam, and what a
/// caller uses when the store lives somewhere other than `~/.claude/projects`.
pub fn load_recent_under(root: &Path) -> Vec<ClaudeSession> {
    let Ok(projects) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut sessions: HashMap<String, ClaudeSession> = HashMap::new();
    let mut scanned = 0usize;
    for project in projects.flatten() {
        if scanned >= MAX_TRANSCRIPTS {
            break;
        }
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            if scanned >= MAX_TRANSCRIPTS {
                break;
            }
            if !file.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            scanned += 1;
            let Some(session) = read_transcript(&path) else {
                continue;
            };
            // One entry per session UUID: keep the newest metadata we saw.
            match sessions.get(&session.id) {
                Some(existing) if existing.updated >= session.updated => {}
                _ => {
                    sessions.insert(session.id.clone(), session);
                }
            }
        }
    }
    let mut out: Vec<ClaudeSession> = sessions
        .into_values()
        .filter(|session| session.cwd.is_dir())
        .collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    out.truncate(MAX_SESSIONS);
    out
}

/// Lifts the metadata out of one transcript file. `None` when the file cannot
/// be read or carries no usable session id and cwd — the caller skips it.
fn read_transcript(path: &Path) -> Option<ClaudeSession> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let fallback_id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty());
    let file_meta = std::fs::metadata(path).ok();
    let len = file_meta.as_ref().map(|meta| meta.len()).unwrap_or(0);
    let mut meta = Meta::default();
    let mut line = Vec::new();
    for _ in 0..MAX_LINES {
        line.clear();
        match read_capped_line(&mut reader, &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            // A read error part-way through: keep whatever we already have.
            Err(_) => break,
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        meta.absorb(text);
        if meta.complete() {
            break;
        }
    }
    // The newest name sits at the end of the file, past the head scan's window.
    meta.absorb_tail_name(&mut reader, len);
    let id = meta.id.or(fallback_id)?;
    let cwd = meta.cwd?;
    let updated = meta
        .timestamp
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64))
        .or_else(|| file_meta.and_then(|meta| meta.modified().ok()))
        .unwrap_or(UNIX_EPOCH);
    let title = meta.name.or(meta.prompt).unwrap_or_else(|| dir_label(&cwd));
    Some(ClaudeSession {
        id,
        cwd,
        title,
        updated,
    })
}

/// Reads one line, capped at [`MAX_LINE`] bytes. A line longer than the cap is
/// discarded (and its tail skipped over) instead of being allocated.
fn read_capped_line<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> std::io::Result<usize> {
    let read = reader.take(MAX_LINE).read_until(b'\n', out)?;
    if read as u64 == MAX_LINE && !out.ends_with(b"\n") {
        let mut sink = Vec::new();
        let _ = reader.read_until(b'\n', &mut sink);
        out.clear();
    }
    Ok(read)
}

/// The handful of fields we lift out of a transcript's records. Every one is
/// optional: whatever the records happen to carry is what we get.
#[derive(Default)]
struct Meta {
    id: Option<String>,
    cwd: Option<PathBuf>,
    /// The recorded `ai-title` session name, normalized and capped. The tail
    /// scan runs last, so the newest name overwrites whatever the head saw.
    name: Option<String>,
    /// The first human-authored prompt, flattened and capped. Only this
    /// excerpt is ever kept — never the full prompt text.
    prompt: Option<String>,
    /// The newest parseable `timestamp` seen, in epoch seconds.
    timestamp: Option<i64>,
}

impl Meta {
    /// Folds one raw JSONL line in, ignoring anything that does not parse or
    /// does not look like the record we know about. Unknown record types are
    /// fine: only the fields we recognize are read.
    fn absorb(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return;
        };
        let Some(object) = value.as_object() else {
            return;
        };
        match object.get("type").and_then(|value| value.as_str()) {
            // Claude regenerates the session name and appends another copy, so
            // the newest record wins (the tail scan overwrites this one).
            Some("ai-title") => {
                if let Some(name) = text_field(object.get("aiTitle"))
                    && let Some(name) = title_excerpt(&name)
                {
                    self.name = Some(name);
                }
            }
            // No explicit name: the first prompt the user actually typed names
            // the session. Scaffolding records are skipped inside.
            Some("user") if self.prompt.is_none() => {
                self.prompt = first_prompt(object).as_deref().and_then(title_excerpt);
            }
            _ => {}
        }
        if let Some(id) = text_field(object.get("sessionId")) {
            self.id = Some(id);
        }
        if let Some(cwd) = text_field(object.get("cwd")) {
            let path = PathBuf::from(&cwd);
            // A relative cwd would resolve against our own process cwd.
            if path.is_absolute() {
                self.cwd = Some(path);
            }
        }
        if let Some(secs) = text_field(object.get("timestamp")).and_then(|s| parse_rfc3339_secs(&s))
        {
            self.timestamp = Some(self.timestamp.map_or(secs, |current| current.max(secs)));
        }
    }

    /// True once everything the head scan wants is in hand — the prompt
    /// included, so the scan keeps reading until it has the first user record
    /// (or hits the line cap).
    fn complete(&self) -> bool {
        self.id.is_some() && self.cwd.is_some() && self.timestamp.is_some() && self.prompt.is_some()
    }

    /// Scans a bounded window at the end of the transcript for the newest
    /// `ai-title` record — the session's current name — overwriting whatever
    /// the head scan saw. Only lines that mention `ai-title` are parsed, so
    /// this stays cheap on a large transcript.
    fn absorb_tail_name<R: BufRead + Seek>(&mut self, reader: &mut R, len: u64) {
        if len == 0 {
            return;
        }
        let start = len.saturating_sub(TAIL_BYTES);
        if reader.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if reader.take(TAIL_BYTES).read_to_end(&mut buf).is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&buf);
        let mut lines = text.split('\n');
        // A window that does not start at byte 0 begins mid-line: drop it.
        if start > 0 {
            lines.next();
        }
        for line in lines {
            if !line.contains(AI_TITLE_MARKER) {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            let Some(object) = value.as_object() else {
                continue;
            };
            if object.get("type").and_then(|value| value.as_str()) != Some("ai-title") {
                continue;
            }
            if let Some(name) = text_field(object.get("aiTitle"))
                && let Some(name) = title_excerpt(&name)
            {
                self.name = Some(name);
            }
        }
    }
}

/// A non-empty string field, or `None` (numbers, objects and booleans are
/// ignored rather than coerced).
fn text_field(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

/// Collapses every run of whitespace — spaces, tabs, newlines — to a single
/// space and trims the ends, so a multi-line prompt becomes one display line.
fn one_line(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Truncates `text` to at most `max` Unicode scalar values, appending an
/// ellipsis when it cuts — a code point is never split.
fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A one-line, capped display title built from transcript text, or `None` when
/// nothing usable is left of it.
fn title_excerpt(text: &str) -> Option<String> {
    let flat = one_line(text);
    if flat.is_empty() {
        return None;
    }
    Some(cap_chars(&flat, TITLE_CHARS))
}

/// Whether `flat` is Claude's own scaffolding rather than user prose: a leading
/// angle-bracket tag from the known internal set.
fn is_internal_text(flat: &str) -> bool {
    let Some(rest) = flat.strip_prefix('<') else {
        return false;
    };
    let Some(end) = rest.find('>') else {
        return false;
    };
    let tag = &rest[..end];
    !tag.is_empty() && !tag.contains('<') && INTERNAL_TAGS.contains(&tag)
}

/// The text of the first human-authored prompt in a `user` record, or `None`
/// when the record is sidechain, meta, tool-result or command scaffolding
/// rather than something the user typed.
fn first_prompt(object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if object.get("isSidechain").and_then(|value| value.as_bool()) == Some(true)
        || object.get("isMeta").and_then(|value| value.as_bool()) == Some(true)
        || object
            .get("toolUseResult")
            .is_some_and(|value| !value.is_null())
    {
        return None;
    }
    // A record that says where it came from only counts when a human wrote it.
    if let Some(kind) = object
        .get("origin")
        .and_then(|value| value.as_object())
        .and_then(|origin| origin.get("kind"))
        .and_then(|value| value.as_str())
        && kind != "human"
    {
        return None;
    }
    let content = object.get("message")?.as_object()?.get("content")?;
    let flat = match content {
        serde_json::Value::String(text) => one_line(text),
        serde_json::Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                let Some(part) = part.as_object() else {
                    continue;
                };
                if part.get("type").and_then(|value| value.as_str()) != Some("text") {
                    continue;
                }
                let Some(chunk) = part.get("text").and_then(|value| value.as_str()) else {
                    continue;
                };
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(chunk);
            }
            one_line(&text)
        }
        _ => return None,
    };
    if flat.is_empty() || is_internal_text(&flat) {
        return None;
    }
    Some(flat)
}

/// Last path component of `path`, for use as a fallback title.
fn dir_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Parses the RFC 3339 stamps Claude writes (`2026-09-19T20:39:00.686Z`) into
/// epoch seconds. Accepts a `Z` or `±HH:MM` offset and an optional fractional
/// part; anything else is `None`, and the caller then falls back to the file's
/// modified time rather than guessing.
fn parse_rfc3339_secs(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    if bytes[4] != b'-' {
        return None;
    }
    let month: i64 = text.get(5..7)?.parse().ok()?;
    if bytes[7] != b'-' {
        return None;
    }
    let day: i64 = text.get(8..10)?.parse().ok()?;
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    if bytes[13] != b':' {
        return None;
    }
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    if bytes[16] != b':' {
        return None;
    }
    let second: i64 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut rest = text.get(19..)?;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        rest = fraction.get(digits..)?;
    }
    let offset = match rest.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let zone = rest.get(1..)?.trim_end_matches(['Z', 'z']);
            let (hours, minutes) = zone.split_once(':')?;
            let hours: i64 = hours.parse().ok()?;
            let minutes: i64 = minutes.parse().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'-' { -magnitude } else { magnitude }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days from 1970-01-01 for a proleptic-Gregorian civil date (Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway store root, removed when the test ends.
    struct TempStore(PathBuf);

    impl TempStore {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "pwrde-claude-sessions-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create temp store");
            Self(dir)
        }

        fn dir(&self) -> &Path {
            &self.0
        }

        /// Writes `<root>/<project>/<name>` and returns its path.
        fn write(&self, project: &str, name: &str, body: &str) -> PathBuf {
            let dir = self.0.join(project);
            std::fs::create_dir_all(&dir).expect("create project dir");
            let path = dir.join(name);
            std::fs::write(&path, body).expect("write transcript");
            path
        }

        /// A real directory under the root, usable as a recorded cwd.
        fn live_dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).expect("create cwd");
            path
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn extracts_metadata_from_a_valid_transcript() {
        let store = TempStore::new("valid");
        let cwd = store.live_dir("cwd-alpha");
        let id = "11111111-2222-3333-4444-555555555555";
        let body = format!(
            "not json at all\n\
             {{\"type\":\"summary\",\"summary\":\"ignored\"}}\n\
             {{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\",\"gitBranch\":\"tw-tangshan-rhoai\",\"timestamp\":\"2026-09-19T20:39:00.686Z\",\"isSidechain\":false,\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"role\":\"user\",\"content\":\"  wire   up\\n the session reader  \"}}}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"content\":\"never surfaced\"}}}}\n",
            cwd = cwd.display()
        );
        store.write("project-alpha", &format!("{id}.jsonl"), &body);

        let sessions = load_recent_under(store.dir());
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.id, id);
        assert_eq!(session.cwd, cwd);
        assert_eq!(
            session.title, "wire up the session reader",
            "no ai-title: the first user prompt is flattened and used"
        );
        let stamp = parse_rfc3339_secs("2026-09-19T20:39:00.686Z").expect("stamp parses");
        assert_eq!(
            session.updated,
            UNIX_EPOCH + Duration::from_secs(stamp.max(0) as u64)
        );
    }

    #[test]
    fn malformed_and_unknown_records_are_skipped() {
        let store = TempStore::new("malformed");
        let cwd = store.live_dir("cwd-beta");
        // Only garbage: no id, no cwd — dropped.
        store.write(
            "p1",
            "garbage.jsonl",
            "{oops\n[1,2,3]\n\"just a string\"\n{\"type\":\"summary\"}\n",
        );
        // Valid JSON, but no cwd — dropped.
        store.write(
            "p1",
            "nocwd.jsonl",
            "{\"type\":\"user\",\"sessionId\":\"aaaaaaaa-0000-0000-0000-000000000001\",\"timestamp\":\"2026-09-18T00:00:00Z\"}\n",
        );
        // A record type from some future Claude, carrying usable metadata.
        let id = "aaaaaaaa-0000-0000-0000-000000000002";
        store.write(
            "p2",
            "future.jsonl",
            &format!(
                "{{\"type\":\"future/thing\",\"sessionId\":\"{id}\",\"cwd\":\"{}\",\"timestamp\":\"2026-09-17T00:00:00Z\"}}\n",
                cwd.display()
            ),
        );

        let sessions = load_recent_under(store.dir());
        assert_eq!(
            sessions.len(),
            1,
            "only the record with an id and a cwd survives"
        );
        assert_eq!(sessions[0].id, id);
        assert_eq!(
            sessions[0].title, "cwd-beta",
            "no name and no prompt: the cwd name stands in"
        );
    }

    #[test]
    fn dedupe_keeps_the_newest_metadata_per_session_id() {
        let store = TempStore::new("dedupe");
        let old_cwd = store.live_dir("cwd-old");
        let new_cwd = store.live_dir("cwd-new");
        let id = "bbbbbbbb-0000-0000-0000-000000000001";
        store.write(
            "p-old",
            &format!("{id}.jsonl"),
            &format!(
                "{{\"sessionId\":\"{id}\",\"cwd\":\"{}\",\"gitBranch\":\"old\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n",
                old_cwd.display()
            ),
        );
        store.write(
            "p-new",
            &format!("{id}.jsonl"),
            &format!(
                "{{\"sessionId\":\"{id}\",\"cwd\":\"{}\",\"gitBranch\":\"new\",\"timestamp\":\"2026-02-01T00:00:00Z\"}}\n",
                new_cwd.display()
            ),
        );

        let sessions = load_recent_under(store.dir());
        assert_eq!(sessions.len(), 1, "one entry per session UUID");
        assert_eq!(sessions[0].cwd, new_cwd);
        assert_eq!(
            sessions[0].title, "cwd-new",
            "no name and no prompt: the cwd's own name stands in"
        );
    }

    #[test]
    fn sessions_whose_cwd_is_gone_are_not_listed() {
        let store = TempStore::new("missing-cwd");
        let gone = store.dir().join("cwd-gone");
        store.write(
            "p",
            "cccccccc-0000-0000-0000-000000000001.jsonl",
            &format!(
                "{{\"sessionId\":\"cccccccc-0000-0000-0000-000000000001\",\"cwd\":\"{}\",\"timestamp\":\"2026-03-01T00:00:00Z\"}}\n",
                gone.display()
            ),
        );
        // A relative cwd is never trusted.
        store.write(
            "p",
            "dddddddd-0000-0000-0000-000000000002.jsonl",
            "{\"sessionId\":\"dddddddd-0000-0000-0000-000000000002\",\"cwd\":\"relative/path\",\"timestamp\":\"2026-03-01T00:00:00Z\"}\n",
        );

        assert!(load_recent_under(store.dir()).is_empty());
    }

    #[test]
    fn recency_falls_back_to_the_file_mtime_and_sorts_newest_first() {
        let store = TempStore::new("mtime");
        let cwd = store.live_dir("cwd-gamma");
        let id = "eeeeeeee-0000-0000-0000-000000000001";
        let path = store.write(
            "p",
            &format!("{id}.jsonl"),
            &format!("{{\"sessionId\":\"{id}\",\"cwd\":\"{}\"}}\n", cwd.display()),
        );
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let sessions = load_recent_under(store.dir());
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].updated, mtime,
            "no timestamp: the file's mtime is the recency"
        );

        let store = TempStore::new("order");
        let cwd = store.live_dir("cwd-delta");
        for (id, stamp) in [
            (
                "ffffffff-0000-0000-0000-000000000001",
                "2026-01-01T00:00:00Z",
            ),
            (
                "ffffffff-0000-0000-0000-000000000002",
                "2026-05-01T00:00:00Z",
            ),
        ] {
            store.write(
                "p",
                &format!("{id}.jsonl"),
                &format!(
                    "{{\"sessionId\":\"{id}\",\"cwd\":\"{}\",\"timestamp\":\"{stamp}\"}}\n",
                    cwd.display()
                ),
            );
        }
        let ordered = load_recent_under(store.dir());
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].id, "ffffffff-0000-0000-0000-000000000002");
        assert_eq!(ordered[1].id, "ffffffff-0000-0000-0000-000000000001");
    }

    #[test]
    fn rfc3339_timestamps_parse_with_and_without_offsets() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:00Z"), Some(0));
        let base = parse_rfc3339_secs("2026-09-19T20:39:00Z").expect("plain stamp parses");
        assert_eq!(parse_rfc3339_secs("2026-09-19T20:39:00.686Z"), Some(base));
        assert_eq!(parse_rfc3339_secs("2026-09-19T21:39:00+01:00"), Some(base));
        assert_eq!(parse_rfc3339_secs("2026-09-19T19:39:00-01:00"), Some(base));
        assert!(
            parse_rfc3339_secs("2024-02-29T00:00:00Z").is_some(),
            "leap day is a real date"
        );
        assert_eq!(parse_rfc3339_secs("not a timestamp"), None);
        assert_eq!(parse_rfc3339_secs("2026-13-01T00:00:00Z"), None);
        assert_eq!(
            parse_rfc3339_secs("2026-09-19T20:39:00"),
            None,
            "a stamp without a zone is rejected"
        );
        assert_eq!(parse_rfc3339_secs(""), None);
    }

    #[test]
    fn resume_argv_keeps_the_session_id_as_one_argument() {
        let session = ClaudeSession {
            id: "aaaa; rm -rf $HOME".into(),
            cwd: PathBuf::from("/tmp"),
            title: "t".into(),
            updated: UNIX_EPOCH,
        };
        let argv = session.resume_argv();
        assert_eq!(argv.len(), 3, "claude, --resume, the id — nothing splices");
        assert_eq!(argv, ["claude", "--resume", "aaaa; rm -rf $HOME"]);
        assert_eq!(
            session.resume_display(),
            "claude --resume 'aaaa; rm -rf $HOME'"
        );

        let plain = ClaudeSession {
            id: "11111111-2222-3333-4444-555555555555".into(),
            ..session.clone()
        };
        assert_eq!(
            plain.resume_display(),
            "claude --resume 11111111-2222-3333-4444-555555555555"
        );

        // What the launcher types into the pane is the quoted display line —
        // never `resume_argv().join(" ")`, which would hand the shell the
        // id's metacharacters as syntax.
        assert_eq!(
            session.typed_command_line(),
            session.resume_display(),
            "the typed line is the quoted one"
        );
        assert_ne!(
            session.typed_command_line(),
            session.resume_argv().join(" "),
            "joining argv would splice `; rm -rf $HOME` into shell syntax"
        );
    }

    #[test]
    fn a_missing_store_root_is_an_empty_list_not_a_failure() {
        assert!(load_recent_under(Path::new("/definitely/not/a/claude/store")).is_empty());
    }

    #[test]
    fn the_newest_explicit_session_name_beats_the_first_prompt() {
        let store = TempStore::new("name");
        let cwd = store.live_dir("cwd-name");
        let id = "12121212-0000-0000-0000-000000000001";
        let body = format!(
            "{{\"type\":\"ai-title\",\"sessionId\":\"{id}\",\"aiTitle\":\"Stale name\"}}\n\
             {{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\",\"timestamp\":\"2026-09-19T20:39:00Z\",\"isSidechain\":false,\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"role\":\"user\",\"content\":\"help me install modal\"}}}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"content\":\"sure\"}}}}\n\
             {{\"type\":\"ai-title\",\"sessionId\":\"{id}\",\"aiTitle\":\"Modal installation\"}}\n",
            cwd = cwd.display()
        );
        store.write("p", &format!("{id}.jsonl"), &body);

        let sessions = load_recent_under(store.dir());
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].title, "Modal installation",
            "the newest explicit name wins over the prompt"
        );
    }

    #[test]
    fn the_first_prompt_names_the_session_and_is_capped() {
        let store = TempStore::new("prompt");
        let cwd = store.live_dir("cwd-prompt");
        let id = "13131313-0000-0000-0000-000000000001";
        let prompt = "Réorganise each picker row so the session is obvious ".repeat(5);
        let flat: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.chars().count() > TITLE_CHARS * 2,
            "the fixture has to be long enough to need truncation"
        );
        let body = format!(
            "{{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\",\"timestamp\":\"2026-09-19T20:39:00Z\",\"isSidechain\":false,\"origin\":{{\"kind\":\"human\"}},\"promptSource\":\"typed\",\"message\":{{\"role\":\"user\",\"content\":\"{prompt}\"}}}}\n",
            cwd = cwd.display()
        );
        store.write("p", &format!("{id}.jsonl"), &body);

        let sessions = load_recent_under(store.dir());
        let title = &sessions[0].title;
        assert_eq!(
            title,
            &{
                let mut cut: String = flat.chars().take(TITLE_CHARS - 1).collect();
                cut.push('…');
                cut
            },
            "exactly the capped excerpt — no more of the prompt than that"
        );
        assert_eq!(title.chars().count(), TITLE_CHARS, "the row stays short");
        assert!(title.ends_with('…'), "truncation is marked: {title}");
        assert!(
            flat.starts_with(title.trim_end_matches('…')),
            "the excerpt is the prompt's own opening: {title}"
        );
    }

    #[test]
    fn whitespace_is_flattened_and_scaffolding_records_are_skipped() {
        let store = TempStore::new("scaffolding");
        let cwd = store.live_dir("cwd-scaffold");
        let id = "14141414-0000-0000-0000-000000000001";
        let head = format!(
            "\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\"",
            cwd = cwd.display()
        );
        let body = format!(
            // A tool result, a slash-command echo, a meta reminder, an empty
            // record and a sidechain prompt all precede the real one.
            "{{\"type\":\"user\",{head},\"timestamp\":\"2026-09-19T20:39:00Z\",\"toolUseResult\":{{\"stdout\":\"tool noise\"}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"content\":\"tool noise\"}}]}}}}\n\
             {{\"type\":\"user\",{head},\"timestamp\":\"2026-09-19T20:39:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"<command-name>/model</command-name>\\n            <command-message>model</command-message>\"}}}}\n\
             {{\"type\":\"user\",{head},\"timestamp\":\"2026-09-19T20:39:02Z\",\"isMeta\":true,\"message\":{{\"role\":\"user\",\"content\":\"<system-reminder>remember this</system-reminder>\"}}}}\n\
             {{\"type\":\"user\",{head},\"timestamp\":\"2026-09-19T20:39:03Z\",\"isSidechain\":true,\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"role\":\"user\",\"content\":\"subagent chore\"}}}}\n\
             {{\"type\":\"user\",{head},\"timestamp\":\"2026-09-19T20:39:04Z\",\"message\":{{\"role\":\"user\",\"content\":\"   \\n\\t  \"}}}}\n\
             {{\"type\":\"user\",{head},\"timestamp\":\"2026-09-19T20:39:05Z\",\"isSidechain\":false,\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"  make the rows\\n\\treadable across   lines  \"}}]}}}}\n"
        );
        store.write("p", &format!("{id}.jsonl"), &body);

        let sessions = load_recent_under(store.dir());
        assert_eq!(
            sessions[0].title, "make the rows readable across lines",
            "the scaffolding is skipped and the real prompt is flattened"
        );
    }

    #[test]
    fn the_cwd_basename_is_the_title_when_no_prompt_exists() {
        let store = TempStore::new("fallback");
        let cwd = store.live_dir("cwd-fallback");
        let id = "15151515-0000-0000-0000-000000000001";
        let body = format!(
            "{{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\",\"gitBranch\":\"tw-fallback\",\"timestamp\":\"2026-09-19T20:39:00Z\",\"toolUseResult\":{{\"stdout\":\"only tool output\"}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"content\":\"only tool output\"}}]}}}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"content\":\"no human prompt here\"}}}}\n",
            cwd = cwd.display()
        );
        store.write("p", &format!("{id}.jsonl"), &body);

        let sessions = load_recent_under(store.dir());
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].title, "cwd-fallback",
            "no name and no prompt: the cwd's basename stands in"
        );
    }

    #[test]
    fn a_name_past_the_head_window_is_still_found() {
        let store = TempStore::new("tail-name");
        let cwd = store.live_dir("cwd-tail");
        let id = "16161616-0000-0000-0000-000000000001";
        let mut body = format!(
            "{{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"{cwd}\",\"timestamp\":\"2026-09-19T20:39:00Z\",\"isSidechain\":false,\"message\":{{\"role\":\"user\",\"content\":\"first prompt\"}}}}\n",
            cwd = cwd.display()
        );
        for _ in 0..MAX_LINES {
            body.push_str("{\"type\":\"assistant\",\"message\":{\"content\":\"filler\"}}\n");
        }
        body.push_str(&format!(
            "{{\"type\":\"ai-title\",\"sessionId\":\"{id}\",\"aiTitle\":\"Late name\"}}\n"
        ));
        store.write("p", &format!("{id}.jsonl"), &body);

        let sessions = load_recent_under(store.dir());
        assert_eq!(
            sessions[0].title, "Late name",
            "the name lives past the head window and the tail scan still finds it"
        );
    }

    #[test]
    fn titles_are_one_line_capped_and_never_scaffolding() {
        assert_eq!(one_line("  a\n\tb   c "), "a b c");
        assert_eq!(title_excerpt("   \n\t "), None);
        let crab = "🦀".repeat(TITLE_CHARS + 5);
        let cut = title_excerpt(&crab).expect("a title");
        assert_eq!(cut.chars().count(), TITLE_CHARS, "capped by characters");
        assert!(cut.ends_with('…'), "truncation is marked: {cut}");
        assert!(!cut.contains('\u{fffd}'), "no code point was split");
        assert!(is_internal_text("<command-name>/model</command-name>"));
        assert!(is_internal_text(
            "<local-command-stdout>ok</local-command-stdout>"
        ));
        assert!(
            !is_internal_text("<div>my own markup</div>"),
            "an unknown tag is user text, not scaffolding"
        );
        assert!(!is_internal_text("plain prose"));
    }
}
