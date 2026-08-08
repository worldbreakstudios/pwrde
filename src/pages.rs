//! Top-level page navigation + rebindable keyboard actions.
//!
//! pwrde has Arc-style *pages*: Sessions (the terminal workspace) and
//! Settings. The sidebar's bottom strip shows one slot per page — a subtle
//! dot that crossfades into the page's glyph on hover, and stays a glyph on
//! the active page. ⌘⇧←/→ cycle pages with wraparound; ⌘⇧↑/↓ cycle the
//! sidebar's tabs (groups on Sessions, sections on Settings) the same way.
//!
//! Every ⌘ shortcut is an [`Action`] dispatched through a bindings table
//! resolved from the settings store (`"keyboard.<action>"` keys, falling back
//! to the defaults below), so the Settings → Keyboard page can rebind them.

use gpui::Keystroke;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Sessions,
    Settings,
}

impl Page {
    /// Dot-strip order; `cycle` walks this.
    pub const ALL: [Page; 2] = [Page::Sessions, Page::Settings];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// Glyph shown in the page slot when active or hovered. The cog is a Nerd
    /// Font codepoint — the UI font guarantees coverage.
    pub fn glyph(self) -> &'static str {
        match self {
            Page::Sessions => "<>",
            Page::Settings => "\u{f013}",
        }
    }
}

/// Wrapping cycle: the index `delta` steps from `i` among `n` entries.
pub fn cycle(i: usize, n: usize, delta: isize) -> usize {
    if n == 0 {
        return 0;
    }
    let n = n as isize;
    (((i as isize + delta) % n + n) % n) as usize
}

/// Sections of the Settings page (sidebar tabs while it is active).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Sessions,
    Keyboard,
    Terminal,
    Appearance,
    Debug,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Sessions,
        Section::Keyboard,
        Section::Terminal,
        Section::Appearance,
        Section::Debug,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Sessions => "Sessions",
            Section::Keyboard => "Keyboard",
            Section::Terminal => "Terminal",
            Section::Appearance => "Appearance",
            Section::Debug => "Debug",
        }
    }
}

// ── Appearance page layout ──────────────────────────────────────────────

/// One interactive (or header) item on the Appearance page.
#[derive(Clone, Copy)]
pub enum AppearanceItem {
    /// The System/Dark/Light segmented control.
    Mode,
    /// A section header ("Theme", "Terminal Colors").
    Header(&'static str),
    /// A chrome theme slot; clicking assigns it to its polarity's slot.
    Theme(&'static crate::theme::Theme),
    /// The adaptive terminal scheme; clicking resets both polarity slots.
    TermDefault,
    /// A terminal scheme slot; clicking assigns it to its polarity's slot.
    Term(&'static crate::term_theme::TermTheme),
}

impl AppearanceItem {
    /// Mode and headers own their whole row; theme/scheme slots pack in halves.
    pub fn full_width(self) -> bool {
        matches!(self, AppearanceItem::Mode | AppearanceItem::Header(_))
    }
}

/// The Appearance page as `(row, col, item)` triples — the single source of
/// truth walked by both the renderer (drawing) and main.rs (hit-testing), so
/// clicks always agree with pixels. `col` is 0 (left half) or 1 (right half);
/// full-width items sit alone in col 0. Slots pack two per row, light-
/// polarity entries before dark ones (mirroring the light/dark slot model).
pub fn appearance_layout() -> Vec<(usize, usize, AppearanceItem)> {
    struct Packer {
        out: Vec<(usize, usize, AppearanceItem)>,
        row: usize,
        col: usize,
    }
    impl Packer {
        /// Finish a half-filled row so the next item starts on a fresh one.
        fn settle(&mut self) {
            if self.col == 1 {
                self.row += 1;
                self.col = 0;
            }
        }
        fn push(&mut self, item: AppearanceItem) {
            if item.full_width() {
                self.settle();
                self.out.push((self.row, 0, item));
                self.row += 1;
            } else {
                self.out.push((self.row, self.col, item));
                if self.col == 1 {
                    self.row += 1;
                }
                self.col ^= 1;
            }
        }
    }

    let mut p = Packer { out: vec![(0, 0, AppearanceItem::Mode)], row: 2, col: 0 };

    p.push(AppearanceItem::Header("Theme"));
    for t in crate::theme::ALL.iter().filter(|t| !t.dark) {
        p.push(AppearanceItem::Theme(t));
    }
    for t in crate::theme::ALL.iter().filter(|t| t.dark) {
        p.push(AppearanceItem::Theme(t));
    }

    // A blank row between the sections.
    p.settle();
    p.row += 1;

    p.push(AppearanceItem::Header("Terminal Colors"));
    p.push(AppearanceItem::TermDefault);
    for t in crate::term_theme::ALL.iter().filter(|t| !t.dark) {
        p.push(AppearanceItem::Term(t));
    }
    for t in crate::term_theme::ALL.iter().filter(|t| t.dark) {
        p.push(AppearanceItem::Term(t));
    }
    p.out
}

/// Row index of the "Show frame stats" toggle on the Debug page. Rows 0..N
/// are read-only diagnostics; one blank row separates them from the toggle.
/// Shared by the renderer (drawing) and main.rs (hit-testing).
pub const DEBUG_TOGGLE_ROW: usize = 8;

/// Row index of the "Persist sessions" toggle on the Terminal page.
pub const PERSIST_TOGGLE_ROW: usize = 0;

// ── Rebindable actions ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SplitRight,
    SplitDown,
    NewTab,
    NewGroup,
    Copy,
    Paste,
    CloseTab,
    Quit,
    PrevTile,
    NextTile,
    PrevTab,
    NextTab,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    PrevSidebarTab,
    NextSidebarTab,
    PrevPage,
    NextPage,
    OpenSettings,
}

impl Action {
    /// Keyboard-page row order.
    pub const ALL: [Action; 21] = [
        Action::SplitRight,
        Action::SplitDown,
        Action::NewTab,
        Action::NewGroup,
        Action::Copy,
        Action::Paste,
        Action::CloseTab,
        Action::Quit,
        Action::PrevTile,
        Action::NextTile,
        Action::PrevTab,
        Action::NextTab,
        Action::FocusLeft,
        Action::FocusDown,
        Action::FocusUp,
        Action::FocusRight,
        Action::PrevSidebarTab,
        Action::NextSidebarTab,
        Action::PrevPage,
        Action::NextPage,
        Action::OpenSettings,
    ];

    /// Stable identifier used in the settings key (`keyboard.<name>`).
    pub fn name(self) -> &'static str {
        match self {
            Action::SplitRight => "split_right",
            Action::SplitDown => "split_down",
            Action::NewTab => "new_tab",
            Action::NewGroup => "new_group",
            Action::Copy => "copy",
            Action::Paste => "paste",
            Action::CloseTab => "close_tab",
            Action::Quit => "quit",
            Action::PrevTile => "prev_tile",
            Action::NextTile => "next_tile",
            Action::PrevTab => "prev_tab",
            Action::NextTab => "next_tab",
            Action::FocusLeft => "focus_left",
            Action::FocusDown => "focus_down",
            Action::FocusUp => "focus_up",
            Action::FocusRight => "focus_right",
            Action::PrevSidebarTab => "prev_sidebar_tab",
            Action::NextSidebarTab => "next_sidebar_tab",
            Action::PrevPage => "prev_page",
            Action::NextPage => "next_page",
            Action::OpenSettings => "open_settings",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::SplitRight => "Split side-by-side",
            Action::SplitDown => "Split stacked",
            Action::NewTab => "New tab",
            Action::NewGroup => "New group",
            Action::Copy => "Copy selection",
            Action::Paste => "Paste",
            Action::CloseTab => "Close tab",
            Action::Quit => "Quit",
            Action::PrevTile => "Focus previous tile",
            Action::NextTile => "Focus next tile",
            Action::PrevTab => "Previous tab",
            Action::NextTab => "Next tab",
            Action::FocusLeft => "Focus pane left",
            Action::FocusDown => "Focus pane down",
            Action::FocusUp => "Focus pane up",
            Action::FocusRight => "Focus pane right",
            Action::PrevSidebarTab => "Previous sidebar tab",
            Action::NextSidebarTab => "Next sidebar tab",
            Action::PrevPage => "Previous page",
            Action::NextPage => "Next page",
            Action::OpenSettings => "Open settings",
        }
    }

    pub fn setting_key(self) -> String {
        format!("keyboard.{}", self.name())
    }

    pub fn default_binding(self) -> Binding {
        let (shift, key) = match self {
            Action::SplitRight => (false, "d"),
            Action::SplitDown => (true, "d"),
            Action::NewTab => (false, "t"),
            Action::NewGroup => (true, "t"),
            Action::Copy => (false, "c"),
            Action::Paste => (false, "v"),
            Action::CloseTab => (false, "w"),
            Action::Quit => (false, "q"),
            Action::PrevTile => (false, "["),
            Action::NextTile => (false, "]"),
            Action::PrevTab => (true, "["),
            Action::NextTab => (true, "]"),
            Action::FocusLeft => (true, "h"),
            Action::FocusDown => (true, "j"),
            Action::FocusUp => (true, "k"),
            Action::FocusRight => (true, "l"),
            Action::PrevSidebarTab => (true, "up"),
            Action::NextSidebarTab => (true, "down"),
            Action::PrevPage => (true, "left"),
            Action::NextPage => (true, "right"),
            Action::OpenSettings => (false, ","),
        };
        Binding { shift, alt: false, ctrl: false, key: key.into() }
    }

    /// The user's binding from settings, or the default. Resolved per lookup —
    /// the store is in memory, so this stays cheap and always current.
    pub fn binding(self) -> Binding {
        crate::settings::get_str(&self.setting_key())
            .and_then(|s| Binding::parse(&s))
            .unwrap_or_else(|| self.default_binding())
    }
}

/// A ⌘ chord. Cmd is implicit — bare ctrl/alt chords belong to the terminal
/// apps, so every app shortcut requires ⌘ — the other modifiers are explicit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub key: String,
}

impl Binding {
    /// Parse `"cmd-shift-left"` style strings (the settings encoding).
    /// Unknown tokens are the key; the last one wins; no key → `None`.
    pub fn parse(s: &str) -> Option<Binding> {
        let (mut shift, mut alt, mut ctrl, mut key) = (false, false, false, None);
        for token in s.split('-').filter(|t| !t.is_empty()) {
            match token {
                "cmd" => {},
                "shift" => shift = true,
                "alt" | "opt" => alt = true,
                "ctrl" => ctrl = true,
                k => key = Some(normalize_key(k)),
            }
        }
        key.map(|key| Binding { shift, alt, ctrl, key })
    }

    /// The settings encoding; `parse` inverts it.
    pub fn serialize(&self) -> String {
        let mut out = String::from("cmd");
        if self.ctrl {
            out.push_str("-ctrl");
        }
        if self.alt {
            out.push_str("-alt");
        }
        if self.shift {
            out.push_str("-shift");
        }
        out.push('-');
        out.push_str(&self.key);
        out
    }

    /// Human display, macOS style: `⇧⌘←`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push('⌃');
        }
        if self.alt {
            out.push('⌥');
        }
        if self.shift {
            out.push('⇧');
        }
        out.push('⌘');
        out.push_str(&match self.key.as_str() {
            "left" => "←".into(),
            "right" => "→".into(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            k => k.to_uppercase(),
        });
        out
    }

    /// Capture a binding from a keystroke: requires ⌘ and a non-modifier key.
    pub fn from_keystroke(ks: &Keystroke) -> Option<Binding> {
        let m = ks.modifiers;
        if !m.platform {
            return None;
        }
        let key = normalize_key(&ks.key);
        if key.is_empty()
            || matches!(key.as_str(), "cmd" | "ctrl" | "control" | "alt" | "opt" | "shift" | "fn" | "platform" | "function")
        {
            return None;
        }
        Some(Binding { shift: m.shift, alt: m.alt, ctrl: m.control, key })
    }

    pub fn matches(&self, ks: &Keystroke) -> bool {
        let m = ks.modifiers;
        m.platform
            && m.shift == self.shift
            && m.alt == self.alt
            && m.control == self.ctrl
            && normalize_key(&ks.key) == self.key
    }
}

/// gpui may report shifted punctuation for bracket chords (`{` for ⇧[), and
/// named keys are matched case-insensitively — fold both so a stored `[`
/// matches either report.
fn normalize_key(key: &str) -> String {
    match key {
        "{" => "[".into(),
        "}" => "]".into(),
        k => k.to_lowercase(),
    }
}

/// The action bound to `ks`, if any (first match in `Action::ALL` order).
pub fn match_action(ks: &Keystroke) -> Option<Action> {
    Action::ALL.iter().copied().find(|a| a.binding().matches(ks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    #[test]
    fn appearance_layout_covers_everything_once_without_collisions() {
        let layout = appearance_layout();
        let mut themes = 0;
        let mut terms = 0;
        let mut defaults = 0;
        let mut slots: Vec<(usize, usize)> = Vec::new();
        for (row, col, item) in layout {
            assert!(col < 2, "col out of range");
            assert!(!(item.full_width() && col != 0), "full-width items sit in col 0");
            assert!(!slots.contains(&(row, col)), "slot ({row},{col}) used twice");
            slots.push((row, col));
            match item {
                AppearanceItem::Theme(_) => themes += 1,
                AppearanceItem::Term(_) => terms += 1,
                AppearanceItem::TermDefault => defaults += 1,
                _ => {},
            }
        }
        assert_eq!(themes, crate::theme::ALL.len());
        assert_eq!(terms, crate::term_theme::ALL.len());
        assert_eq!(defaults, 1);
    }

    #[test]
    fn cycle_wraps_both_ways() {
        assert_eq!(cycle(0, 2, 1), 1);
        assert_eq!(cycle(1, 2, 1), 0);
        assert_eq!(cycle(0, 2, -1), 1);
        assert_eq!(cycle(1, 2, -1), 0);
        assert_eq!(cycle(0, 3, -1), 2);
        assert_eq!(cycle(0, 0, 1), 0);
    }

    #[test]
    fn binding_serialize_parse_roundtrip() {
        for action in Action::ALL {
            let b = action.default_binding();
            assert_eq!(Binding::parse(&b.serialize()).as_ref(), Some(&b), "{}", action.name());
        }
        let full = Binding { shift: true, alt: true, ctrl: true, key: "left".into() };
        assert_eq!(Binding::parse(&full.serialize()), Some(full));
    }

    #[test]
    fn parse_rejects_keyless_strings() {
        assert_eq!(Binding::parse("cmd-shift"), None);
        assert_eq!(Binding::parse(""), None);
    }

    #[test]
    fn matches_folds_shifted_brackets() {
        let b = Binding { shift: true, alt: false, ctrl: false, key: "[".into() };
        let ks = Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: true,
                control: false,
                alt: false,
                function: false,
            },
            key: "{".into(),
            key_char: None,
        };
        assert!(b.matches(&ks));
    }

    #[test]
    fn sidebar_tab_bindings_resolve() {
        let ks = |key: &str| Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: true,
                control: false,
                alt: false,
                function: false,
            },
            key: key.into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks("up")), Some(Action::PrevSidebarTab));
        assert_eq!(match_action(&ks("down")), Some(Action::NextSidebarTab));
    }

    #[test]
    fn focus_dir_bindings_resolve() {
        let ks = |key: &str| Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: true,
                control: false,
                alt: false,
                function: false,
            },
            key: key.into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks("h")), Some(Action::FocusLeft));
        assert_eq!(match_action(&ks("j")), Some(Action::FocusDown));
        assert_eq!(match_action(&ks("k")), Some(Action::FocusUp));
        assert_eq!(match_action(&ks("l")), Some(Action::FocusRight));
    }

    #[test]
    fn open_settings_binds_cmd_comma() {
        let ks = Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: false,
                control: false,
                alt: false,
                function: false,
            },
            key: ",".into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks), Some(Action::OpenSettings));
    }

    #[test]
    fn from_keystroke_requires_cmd_and_a_real_key() {
        let cmd = Modifiers {
            platform: true,
            shift: false,
            control: false,
            alt: false,
            function: false,
        };
        let no_cmd = Modifiers { platform: false, ..cmd };
        let ks = |key: &str, m: Modifiers| Keystroke { modifiers: m, key: key.into(), key_char: None };
        assert!(Binding::from_keystroke(&ks("d", no_cmd)).is_none());
        assert!(Binding::from_keystroke(&ks("shift", cmd)).is_none());
        let got = Binding::from_keystroke(&ks("D", cmd)).unwrap();
        assert_eq!(got.key, "d");
    }
}
