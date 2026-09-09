//! Background subscription to `lfg`'s cache-updated SSE stream.
//!
//! This is the pwrde half of h20's instant-then-fresh trick. Reads issued with
//! `lfg -A` paint whatever is cached immediately and kick off a background
//! `gh` refresh; when that refresh actually changes an entity, `lfgd` emits a
//! `cache-updated` event over its SSE stream. Here we tail that stream (via
//! `lfg events`, which handles the daemon connection + reconnection for us) on
//! a dedicated thread and forward each event onto the app's `TermEvent` channel
//! so the sidebar's PR card rollups re-fetch and repaint with fresh data.
//!
//! Only meaningful when the async path is active (`git.async` on and the CLI is
//! `lfg`); otherwise there is no daemon to talk to and this does nothing.

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::term::TermEvent;

/// Start tailing `lfg events` on a background thread, forwarding each
/// `cache-updated` frame as a [`TermEvent::PrCacheUpdated`]. Returns the child
/// process handle (kept alive by the caller) or `None` when the async path is
/// off or `lfg` can't be launched.
pub fn spawn_event_stream(tx: Sender<TermEvent>) -> Option<Child> {
    if !crate::gh::use_async() {
        return None;
    }
    let mut cmd = crate::git::augmented_command("lfg");
    cmd.args(["events"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            // The first frame is a `# tailing …` comment; data frames are JSON.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(ev) = parse_event(line) {
                if tx.send(ev).is_err() {
                    break; // app gone
                }
            }
        }
    });
    Some(child)
}

/// Parse one SSE data line into a `TermEvent`. Non-`cache-updated` frames
/// (heartbeats, status) and malformed JSON yield `None`.
fn parse_event(line: &str) -> Option<TermEvent> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("event")?.as_str()? != "cache-updated" {
        return None;
    }
    // The frame names the entry (`kind`, `number`), but the sidebar re-fetches
    // every rollup regardless, so only a well-formed frame matters here.
    v.get("data")?.get("kind")?.as_str()?;
    Some(TermEvent::PrCacheUpdated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cache_updated_frame() {
        let line = r#"{"id":282,"event":"cache-updated","data":{"kind":"pr","repo":"o/r","number":94,"reason":"evict","ts":1}}"#;
        assert!(matches!(parse_event(line), Some(TermEvent::PrCacheUpdated)));
    }

    #[test]
    fn ignores_heartbeats_and_junk() {
        assert!(parse_event(r#"{"event":"heartbeat"}"#).is_none());
        assert!(parse_event("not json").is_none());
        assert!(parse_event(r#"{"event":"cache-updated"}"#).is_none());
    }
}
