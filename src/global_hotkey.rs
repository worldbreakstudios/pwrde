//! System-wide hotkey for the command palette.
//!
//! The palette lives in its own window so it can be summoned with pwrde in the
//! background — the Spotlight/Raycast flow. That needs a hotkey the OS routes
//! to us regardless of which app is frontmost, which gpui's own key bindings
//! cannot do: they only fire on a focused pwrde window.
//!
//! `global-hotkey` wraps Carbon's `RegisterEventHotKey` on macOS and delivers
//! presses on its own event loop, so it runs on a dedicated thread here and
//! every press is forwarded over the app's existing `TermEvent` channel. The
//! drain in `App::drain_events` then flips the palette window's visibility on
//! the main thread — the same path every other background signal takes, so no
//! gpui type is ever touched off-thread.
//!
//! The default binding is ⌥⌘P (⌃⌘Space is the fallback if the machine already
//! owns ⌥⌘P); `keyboard.global_palette` in settings overrides it, so the rest
//! of the global command table has somewhere to grow. Registration is
//! best-effort: a hotkey macOS refuses (or that another app owns) warns and the
//! in-app ⌘P binding keeps working.

use std::sync::mpsc::Sender;

use crate::term::TermEvent;

#[cfg(target_os = "macos")]
pub fn spawn(tx: Sender<TermEvent>) {
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

    let chord = crate::settings::get_str("keyboard.global_palette")
        .unwrap_or_else(|| DEFAULT_CHORD.into());
    let Some(hotkey) = parse(&chord) else {
        eprintln!("global hotkey: `{chord}` is not a binding I understand — skipped");
        return;
    };

    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!("global hotkey: unavailable ({error}) — ⌘P still works in-app");
            return;
        },
    };
    if let Err(error) = manager.register(hotkey) {
        eprintln!(
            "global hotkey: {chord} was refused ({error}) — another app may own it; \
             ⌘P still works in-app"
        );
        return;
    }

    std::thread::Builder::new()
        .name("pwrde-global-hotkey".into())
        .spawn(move || {
            // The manager is owned by this thread: dropping it unregisters the
            // hotkey, so it has to outlive the loop.
            let _manager = manager;
            let id = hotkey.id();
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                let Ok(event) = receiver.recv() else { break };
                if event.id() != id || event.state() != HotKeyState::Pressed {
                    continue;
                }
                let _ = tx.send(TermEvent::GlobalPalette);
            }
        })
        .ok();
}

/// Other platforms have no global hotkey: the in-app ⌘P binding is the palette's
/// only summon affordance.
#[cfg(not(target_os = "macos"))]
pub fn spawn(_tx: Sender<TermEvent>) {}

/// The binding registered when `keyboard.global_palette` is unset.
#[cfg(target_os = "macos")]
pub const DEFAULT_CHORD: &str = "alt+meta+KeyP";

/// Parse a chord into a `HotKey`.
///
/// The grammar is deliberately close to gpui's own keystrokes (`⌘P` is
/// `"meta-p"`, ⇧⌘T is `"shift-meta-t"`) so the value a user writes in settings
/// reads like every other binding in the app: `alt+meta+KeyP`, `ctrl-meta-space`
/// and `cmd-shift-p` all mean what they look like. Recognized modifier tokens
/// are `alt`/`option`, `ctrl`/`control`, `meta`/`cmd`/`command`/`super`, and
/// `shift`; the remaining token is the key — a known key name, or a single
/// letter/digit, which always means the physical key so the binding does not
/// depend on the active keyboard layout.
#[cfg(target_os = "macos")]
fn parse(spec: &str) -> Option<global_hotkey::hotkey::HotKey> {
    use global_hotkey::hotkey::{HotKey, Modifiers};

    let mut modifiers = Modifiers::empty();
    let mut key: Option<String> = None;
    let mut shift = false;
    let mut alt = false;
    for token in spec
        .split(['+', '-'])
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    {
        match token.to_ascii_lowercase().as_str() {
            "alt" | "option" | "opt" => alt = true,
            "shift" => shift = true,
            "ctrl" | "control" => {
                modifiers |= Modifiers::CONTROL;
            },
            "meta" | "cmd" | "command" | "super" => {
                modifiers |= Modifiers::META;
            },
            _ => key = key.or_else(|| Some(token.to_owned())),
        }
    }
    let key = key?;
    // `shift+p` and `P` are the same physical key with the same implicit
    // shift, so spelling the letter in either case means one binding.
    let upper = key.to_ascii_uppercase();
    if !shift && upper.len() == 1 {
        shift = matches!(
            key.as_bytes()[0],
            b'A'..=b'Z' | b'!' | b'@' | b'#' | b'$' | b'%' | b'^' | b'&' | b'*' | b'(' | b')'
        );
    }
    let code = parse_key(&key)?;
    if shift {
        modifiers |= Modifiers::SHIFT;
    }
    if alt {
        modifiers |= Modifiers::ALT;
    }
    // A bare key would swallow typing system-wide, so a chord must carry at
    // least one modifier — the same rule the crate's own `FromStr` applies.
    (modifiers != Modifiers::empty()).then(|| HotKey::new(Some(modifiers), code))
}

/// A single letter or digit is the physical key (so the binding survives a
/// layout switch); everything else is a named key, normalized to `Key` +
/// upper-case letter the same way the crate's own `FromStr` does.
#[cfg(target_os = "macos")]
fn parse_key(key: &str) -> Option<global_hotkey::hotkey::Code> {
    let upper = key.to_ascii_uppercase();
    let one = upper.as_bytes().first().copied();
    let normalized = if upper == "SPACE" {
        "Space".to_string()
    } else if upper.len() == 1 && one.is_some_and(|b| b.is_ascii_alphabetic()) {
        format!("Key{upper}")
    } else if upper.len() == 1 && one.is_some_and(|b| b.is_ascii_digit()) {
        format!("Digit{upper}")
    } else if let Some(rest) = upper.strip_prefix("KEY")
        && rest.len() == 1
    {
        format!("Key{rest}")
    } else if let Some(rest) = upper.strip_prefix("DIGIT")
        && rest.len() == 1
    {
        format!("Digit{rest}")
    } else {
        // Named keys (`F5`, `ArrowUp`, …) go through as written; the crate
        // rejects anything it does not know, which is how a typo in the
        // setting surfaces as "binding skipped" instead of silently binding
        // the wrong key.
        upper
    };
    normalized.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_default_chord_parses_with_both_modifiers() {
        use global_hotkey::hotkey::{Code, HotKey, Modifiers};

        assert_eq!(
            parse(DEFAULT_CHORD),
            Some(HotKey::new(Some(Modifiers::ALT | Modifiers::META), Code::KeyP)),
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn chords_read_either_way_and_letters_are_physical_keys() {
        use global_hotkey::hotkey::{Code, HotKey, Modifiers};

        // `ctrl-meta-space` is the documented fallback for a machine that
        // already owns ⇧⌘P; `CMD-SHIFT-P` is the same binding as `meta+shift+p`.
        assert_eq!(
            parse("ctrl-meta-space"),
            Some(HotKey::new(Some(Modifiers::CONTROL | Modifiers::META), Code::Space)),
        );
        assert_eq!(parse("cmd-shift-p"), parse("META+SHIFT+KeyP"));
        assert_eq!(parse("control+KeyP"), parse("ctrl+p"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nonsense_bindings_are_rejected_not_panicked_on() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("meta+"), None);
        assert_eq!(parse("meta+NotAKey"), None);
    }
}
