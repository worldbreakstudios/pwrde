//! Top-level page navigation + rebindable keyboard actions.
//!
//! pwrde has Arc-style *pages*: Sessions (the terminal workspace), one page
//! per user-registered CLI tool (see [`crate::cli_tools`]), and Settings.
//! Tool pages are opened from the folders card's pinned-tool rows and
//! Settings from the sessions header's gear chip (`crate::folders_ui`,
//! `crate::sidebar_ui`). ⌘⇧←/→ cycle pages with wraparound; ⌘⇧↑/↓ cycle the
//! sidebar's tabs (groups on Sessions, sections on Settings) the same way.
//!
//! Every ⌘ shortcut is an [`Action`] dispatched through a bindings table
//! resolved from the settings store (`"keyboard.<action>"` keys, falling back
//! to the defaults below), so the Settings → Keyboard page can rebind them.

use gpui::Keystroke;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Sessions,
    /// A user-registered CLI tool (index into [`crate::cli_tools::tools`]):
    /// a full-page, non-persisted terminal running that tool's command from
    /// its configured directory.
    Tool(usize),
    Settings,
}

impl Page {
    /// Page order for `n_tools` registered CLI tools; `cycle` (⌘⇧←/→)
    /// walks it. Tool pages sit between Sessions and Settings.
    pub fn all(n_tools: usize) -> Vec<Page> {
        let mut v = vec![Page::Sessions];
        v.extend((0..n_tools).map(Page::Tool));
        v.push(Page::Settings);
        v
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
    /// Registered CLI tool pages (see [`crate::cli_tools`]).
    Tools,
    Accessibility,
    Debug,
    FeatureFlags,
}

impl Section {
    pub const ALL: [Section; 8] = [
        Section::Sessions,
        Section::Keyboard,
        Section::Terminal,
        Section::Appearance,
        Section::Tools,
        Section::Accessibility,
        Section::Debug,
        Section::FeatureFlags,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Sessions => "Sessions",
            Section::Keyboard => "Keyboard",
            Section::Terminal => "Terminal",
            Section::Appearance => "Appearance",
            Section::Tools => "Tools",
            Section::Accessibility => "Accessibility",
            Section::Debug => "Debug",
            Section::FeatureFlags => "Feature Flags",
        }
    }
}

// ── Appearance page layout ──────────────────────────────────────────────

/// The terminal-color dropdown selectors on the Appearance page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppearanceDropdown {
    TermLight,
    TermDark,
}

impl AppearanceDropdown {
    /// Whether this dropdown controls the dark-polarity slot.
    pub fn dark(self) -> bool {
        matches!(self, AppearanceDropdown::TermDark)
    }
}

/// All terminal-scheme options for the given slot: `None` (the adaptive
/// "Default") first, then every preset — mixing polarities is allowed —
/// with the slot's own polarity sorted first for easier choosing.
pub fn term_options(dark: bool) -> Vec<Option<&'static crate::term_theme::TermTheme>> {
    let mut v: Vec<Option<&'static crate::term_theme::TermTheme>> = vec![None];
    for matching in [true, false] {
        let want = if matching { dark } else { !dark };
        v.extend(crate::term_theme::ALL.iter().copied().filter(|t| t.dark == want).map(Some));
    }
    v
}

// ── Rebindable actions ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SplitRight,
    SplitDown,
    NewTab,
    NewWebview,
    NewGroup,
    Copy,
    Paste,
    CloseTab,
    CloseGroup,
    TogglePin,
    Quit,
    PrevTile,
    NextTile,
    PrevTab,
    NextTab,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    ToggleCollapse,
    ToggleFocusOthers,
    PrevSidebarTab,
    NextSidebarTab,
    ToggleSidebar,
    ToggleFolders,
    PrevPage,
    NextPage,
    OpenSettings,
    CommandPalette,
    ToggleFlyover,
    FlyoverPopout,
    SaveWorkspace,
    OpenPrInGithub,
    ToggleFlow,
    IncreaseFontSize,
    DecreaseFontSize,
    ScreenshotToClipboard,
    ScreenshotToFile,
    NewSection,
    GoToSessions,
    GoToTool,
}

impl Action {
    /// Keyboard-page row order.
    pub const ALL: [Action; 41] = [
        Action::SplitRight,
        Action::SplitDown,
        Action::NewTab,
        Action::NewWebview,
        Action::NewGroup,
        Action::Copy,
        Action::Paste,
        Action::CloseTab,
        Action::CloseGroup,
        Action::TogglePin,
        Action::Quit,
        Action::PrevTile,
        Action::NextTile,
        Action::PrevTab,
        Action::NextTab,
        Action::FocusLeft,
        Action::FocusDown,
        Action::FocusUp,
        Action::FocusRight,
        Action::ToggleCollapse,
        Action::ToggleFocusOthers,
        Action::PrevSidebarTab,
        Action::NextSidebarTab,
        Action::ToggleSidebar,
        Action::ToggleFolders,
        Action::PrevPage,
        Action::NextPage,
        Action::OpenSettings,
        Action::CommandPalette,
        Action::ToggleFlyover,
        Action::FlyoverPopout,
        Action::SaveWorkspace,
        Action::OpenPrInGithub,
        Action::ToggleFlow,
        Action::IncreaseFontSize,
        Action::DecreaseFontSize,
        Action::ScreenshotToClipboard,
        Action::ScreenshotToFile,
        Action::NewSection,
        Action::GoToSessions,
        Action::GoToTool,
    ];

    /// Stable identifier used in the settings key (`keyboard.<name>`).
    pub fn name(self) -> &'static str {
        match self {
            Action::SplitRight => "split_right",
            Action::SplitDown => "split_down",
            Action::NewTab => "new_tab",
            Action::NewWebview => "new_webview",
            Action::NewGroup => "new_group",
            Action::Copy => "copy",
            Action::Paste => "paste",
            Action::CloseTab => "close_tab",
            Action::CloseGroup => "close_group",
            Action::TogglePin => "toggle_pin",
            Action::Quit => "quit",
            Action::PrevTile => "prev_tile",
            Action::NextTile => "next_tile",
            Action::PrevTab => "prev_tab",
            Action::NextTab => "next_tab",
            Action::FocusLeft => "focus_left",
            Action::FocusDown => "focus_down",
            Action::FocusUp => "focus_up",
            Action::FocusRight => "focus_right",
            Action::ToggleCollapse => "toggle_collapse",
            Action::ToggleFocusOthers => "toggle_focus_others",
            Action::PrevSidebarTab => "prev_sidebar_tab",
            Action::NextSidebarTab => "next_sidebar_tab",
            Action::ToggleSidebar => "toggle_sidebar",
            Action::ToggleFolders => "toggle_folders",
            Action::PrevPage => "prev_page",
            Action::NextPage => "next_page",
            Action::OpenSettings => "open_settings",
            Action::CommandPalette => "command_palette",
            Action::ToggleFlyover => "toggle_flyover",
            Action::FlyoverPopout => "flyover_popout",
            Action::SaveWorkspace => "save_workspace",
            Action::OpenPrInGithub => "open_pr_in_github",
            Action::ToggleFlow => "toggle_flow",
            Action::IncreaseFontSize => "increase_font_size",
            Action::DecreaseFontSize => "decrease_font_size",
            Action::ScreenshotToClipboard => "screenshot_to_clipboard",
            Action::ScreenshotToFile => "screenshot_to_file",
            Action::NewSection => "new_section",
            Action::GoToSessions => "go_to_sessions",
            Action::GoToTool => "go_to_tool",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::SplitRight => "Split side-by-side",
            Action::SplitDown => "Split stacked",
            Action::NewTab => "New tab",
            Action::NewWebview => "New webview…",
            Action::NewGroup => "New session…",
            Action::Copy => "Copy selection",
            Action::Paste => "Paste",
            Action::CloseTab => "Close tab",
            Action::CloseGroup => "Close group",
            Action::TogglePin => "Pin/unpin group",
            Action::Quit => "Quit",
            Action::PrevTile => "Focus previous tile",
            Action::NextTile => "Focus next tile",
            Action::PrevTab => "Previous tab",
            Action::NextTab => "Next tab",
            Action::FocusLeft => "Focus pane left",
            Action::FocusDown => "Focus pane down",
            Action::FocusUp => "Focus pane up",
            Action::FocusRight => "Focus pane right",
            Action::ToggleCollapse => "Collapse/expand pane",
            Action::ToggleFocusOthers => "Collapse/expand other panes",
            Action::PrevSidebarTab => "Previous sidebar tab",
            Action::NextSidebarTab => "Next sidebar tab",
            Action::ToggleSidebar => "Focus terminals",
            Action::ToggleFolders => "Toggle folders",
            Action::PrevPage => "Previous page",
            Action::NextPage => "Next page",
            Action::OpenSettings => "Open settings",
            Action::CommandPalette => "Command palette",
            Action::ToggleFlyover => "Toggle Flyover Terminal",
            Action::FlyoverPopout => "Flyover: panel ↔ window",
            Action::SaveWorkspace => "Save as workspace",
            Action::OpenPrInGithub => "Open PR in GitHub",
            Action::ToggleFlow => "Toggle Flow agent",
            Action::IncreaseFontSize => "Increase font size",
            Action::DecreaseFontSize => "Decrease font size",
            Action::ScreenshotToClipboard => "Screenshot to clipboard",
            Action::ScreenshotToFile => "Screenshot to file",
            Action::NewSection => "New folder",
            Action::GoToSessions => "Go to Sessions",
            Action::GoToTool => "Go to first tool page",
        }
    }

    pub fn setting_key(self) -> String {
        format!("keyboard.{}", self.name())
    }

    pub fn default_binding(self) -> Binding {
        // (shift, alt, ctrl, key) — most app chords are cmd+key; alt/ctrl let a
        // few actions avoid macOS system shortcuts and existing defaults.
        let (shift, alt, ctrl, key) = match self {
            Action::SplitRight => (false, false, false, "d"),
            Action::SplitDown => (true, false, false, "d"),
            Action::NewTab => (false, false, false, "t"),
            // Deliberately uncommon: the command palette is the primary entry.
            Action::NewWebview => (false, true, false, "b"),
            Action::NewGroup => (true, false, false, "t"),
            Action::Copy => (false, false, false, "c"),
            Action::Paste => (false, false, false, "v"),
            Action::CloseTab => (false, false, false, "w"),
            Action::CloseGroup => (true, false, false, "w"),
            Action::TogglePin => (true, false, false, "p"),
            Action::Quit => (false, false, false, "q"),
            Action::PrevTile => (false, false, false, "["),
            Action::NextTile => (false, false, false, "]"),
            Action::PrevTab => (true, false, false, "["),
            Action::NextTab => (true, false, false, "]"),
            Action::FocusLeft => (true, false, false, "h"),
            Action::FocusDown => (true, false, false, "j"),
            Action::FocusUp => (true, false, false, "k"),
            Action::FocusRight => (true, false, false, "l"),
            Action::ToggleCollapse => (true, false, false, "m"),
            Action::ToggleFocusOthers => (true, false, false, "f"),
            Action::PrevSidebarTab => (true, false, false, "up"),
            Action::NextSidebarTab => (true, false, false, "down"),
            Action::ToggleSidebar => (false, false, false, "s"),
            Action::PrevPage => (true, false, false, "left"),
            Action::NextPage => (true, false, false, "right"),
            Action::OpenSettings => (false, false, false, ","),
            Action::CommandPalette => (false, false, false, "p"),
            Action::ToggleFlyover => (false, false, false, "`"),
            Action::FlyoverPopout => (true, false, false, "`"),
            Action::SaveWorkspace => (true, false, false, "s"),
            Action::OpenPrInGithub => (true, false, false, "g"),
            Action::ToggleFlow => (false, false, false, "j"),
            Action::IncreaseFontSize => (false, false, false, "="),
            // "minus" (not "-") because "-" is the binding token separator.
            Action::DecreaseFontSize => (false, false, false, "minus"),
            // ctrl+alt chords: avoid macOS system screenshot and existing defaults.
            Action::ScreenshotToClipboard => (false, true, true, "c"),
            Action::ScreenshotToFile => (false, true, true, "s"),
            Action::NewSection => (false, true, true, "n"),
            Action::ToggleFolders => (false, false, false, "\\"),
            Action::GoToSessions => (false, true, true, "1"),
            Action::GoToTool => (false, true, true, "3"),
        };
        Binding { shift, alt, ctrl, key: key.into() }
    }

    /// Look up an action by its stable `name()` string (e.g. `"split_right"`).
    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.name() == name)
    }

    /// The user's binding from settings, or the default. Resolved per lookup —
    /// the store is in memory, so this stays cheap and always current.
    pub fn binding(self) -> Binding {
        crate::settings::get_str(&self.setting_key())
            .and_then(|s| Binding::parse(&s))
            // `go_to_tool` replaced `go_to_cleanup` when the Cleanup page
            // became the first CLI tool page; honor the old key if it was
            // customized so a rebinding survives the rename. (The retired
            // `keyboard.toggle_tool_panel` is deliberately *not* carried over
            // to `open_pr_in_github`: opening a browser tab is a different
            // effect than toggling a panel, so an old rebinding shouldn't
            // silently acquire it.)
            .or_else(|| {
                (self == Action::GoToTool)
                    .then(|| crate::settings::get_str("keyboard.go_to_cleanup"))
                    .flatten()
                    .and_then(|s| Binding::parse(&s))
            })
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
            "minus" => "-".into(),
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

/// gpui may report shifted punctuation for a chord (`{` for ⇧[, `+` for ⇧=,
/// `_` for ⇧-), and named keys are matched case-insensitively — fold both so a
/// stored key matches either report. `-` folds to the token `minus` because a
/// bare `-` is the binding-string separator (see [`Binding::serialize`]).
fn normalize_key(key: &str) -> String {
    match key {
        "{" => "[".into(),
        "}" => "]".into(),
        "+" => "=".into(),
        "-" | "_" => "minus".into(),
        k => k.to_lowercase(),
    }
}

/// The action bound to `ks`, if any (first match in `Action::ALL` order).
pub fn match_action(ks: &Keystroke) -> Option<Action> {
    Action::ALL.iter().copied().find(|a| a.binding().matches(ks))
}

// ── Settings search index ───────────────────────────────────────────────

/// One searchable entry in the settings index.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsEntry {
    pub section: Section,
    pub label: &'static str,
    pub keywords: &'static str,
}

/// Build the full settings index, covering every setting in every section.
/// Mirrors the single-source-of-truth pattern of [`appearance_layout`].
pub fn settings_index() -> Vec<SettingsEntry> {
    let mut out = Vec::new();

    // Sessions
    out.push(SettingsEntry {
        section: Section::Sessions,
        label: "Primary command",
        keywords: "runs in the primary pane when a group opens",
    });
    out.push(SettingsEntry {
        section: Section::Sessions,
        label: "Pull request CLI",
        keywords: "git cli lfg gh pull request pr diff tool source control",
    });
    out.push(SettingsEntry {
        section: Section::Sessions,
        label: "Async streaming (lfg -A)",
        keywords: "git async streaming lfg force-async sse cache refresh",
    });

    // Keyboard — one entry per action
    for action in &Action::ALL {
        out.push(SettingsEntry {
            section: Section::Keyboard,
            label: action.label(),
            keywords: action.name(),
        });
    }

    // Terminal
    out.push(SettingsEntry {
        section: Section::Terminal,
        label: "Persist sessions",
        keywords: "persist sessions restore",
    });

    // Appearance
    out.push(SettingsEntry {
        section: Section::Appearance,
        label: "Appearance mode",
        keywords: "system dark light",
    });
    out.push(SettingsEntry {
        section: Section::Appearance,
        label: "Accent color",
        keywords: "accent color colour tint highlight system blue purple pink red orange yellow green graphite",
    });
    out.push(SettingsEntry {
        section: Section::Appearance,
        label: "Terminal colors",
        keywords: "terminal colors palette",
    });

    // Tools
    out.push(SettingsEntry {
        section: Section::Tools,
        label: "CLI tool pages",
        keywords: "tool tools cli command page sidebar icon cwd drop cleanup register",
    });

    // Accessibility
    out.push(SettingsEntry {
        section: Section::Accessibility,
        label: "Terminal text size",
        keywords: "font size terminal zoom larger smaller accessibility text",
    });
    out.push(SettingsEntry {
        section: Section::Accessibility,
        label: "App text size",
        keywords: "font size app chrome ui zoom larger smaller accessibility text",
    });

    // Debug
    out.push(SettingsEntry {
        section: Section::Debug,
        label: "Show frame stats",
        keywords: "frame stats fps debug performance",
    });

    // Feature Flags — the section itself, then one entry per experimental
    // flag (so the section stays searchable even when no flags are defined).
    out.push(SettingsEntry {
        section: Section::FeatureFlags,
        label: "Experimental features",
        keywords: "feature flags experimental toggles beta",
    });
    for flag in crate::features::ALL {
        out.push(SettingsEntry {
            section: Section::FeatureFlags,
            label: flag.label,
            keywords: flag.description,
        });
    }

    out
}

/// Search the settings index for entries matching `query`.
/// Returns an empty `Vec` when `query` is empty or all whitespace.
/// Matching is case-insensitive substring on the entry's `label` OR `keywords`.
pub fn search_settings(query: &str) -> Vec<SettingsEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    settings_index()
        .into_iter()
        .filter(|e| {
            e.label.to_lowercase().contains(&q)
                || e.keywords.to_lowercase().contains(&q)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    /// Tool pages sit between Sessions and Settings, one per registered
    /// tool.
    #[test]
    fn tool_pages_slot_between_sessions_and_settings() {
        assert_eq!(
            Page::all(2),
            vec![Page::Sessions, Page::Tool(0), Page::Tool(1), Page::Settings]
        );
        assert_eq!(Page::all(0), vec![Page::Sessions, Page::Settings]);
    }

    #[test]
    fn appearance_dropdown_dark_polarity() {
        assert!(!AppearanceDropdown::TermLight.dark());
        assert!(AppearanceDropdown::TermDark.dark());
    }

    #[test]
    fn term_options_start_with_default_then_matching_polarity() {
        for dark in [false, true] {
            let opts = super::term_options(dark);
            assert_eq!(opts.len(), crate::term_theme::ALL.len() + 1, "missing presets");
            assert!(opts[0].is_none(), "term_options({dark}) must start with Default");
            let matching = opts[1..]
                .iter()
                .take_while(|t| t.is_some_and(|t| t.dark == dark))
                .count();
            assert_eq!(
                matching,
                crate::term_theme::ALL.iter().filter(|t| t.dark == dark).count(),
                "term_options({dark}) must sort its own polarity first"
            );
        }
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

    /// ⌘⇧P (pin/unpin) must survive the settings string round-trip.
    #[test]
    fn toggle_pin_binding_roundtrips() {
        let b = Action::TogglePin.default_binding();
        assert_eq!(b.serialize(), "cmd-shift-p");
        assert_eq!(Binding::parse(&b.serialize()), Some(b));
    }

    /// ⌘P must reach the palette through the same lookup every hotkey uses.
    #[test]
    fn cmd_p_dispatches_command_palette() {
        let ks = Keystroke {
            modifiers: Modifiers { platform: true, ..Default::default() },
            key: "p".into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks), Some(Action::CommandPalette));
    }

    /// Two actions sharing a default chord would make the second unreachable
    /// (`match_action` returns the first hit in `ALL` order).
    #[test]
    fn default_bindings_do_not_collide() {
        for (i, a) in Action::ALL.iter().enumerate() {
            for b in &Action::ALL[i + 1..] {
                assert_ne!(
                    a.default_binding(),
                    b.default_binding(),
                    "{} and {} share a default binding",
                    a.name(),
                    b.name()
                );
            }
        }
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
    fn zoom_bindings_resolve() {
        // ⌘= grows, ⌘- shrinks. `-` folds to the `minus` token so it survives
        // the binding-string separator (see normalize_key / Binding::serialize).
        let ks = |key: &str| Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: false,
                control: false,
                alt: false,
                function: false,
            },
            key: key.into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks("=")), Some(Action::IncreaseFontSize));
        assert_eq!(match_action(&ks("-")), Some(Action::DecreaseFontSize));
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
    fn close_group_binding_resolves() {
        let ks = Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: true,
                control: false,
                alt: false,
                function: false,
            },
            key: "w".into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks), Some(Action::CloseGroup));
    }

    /// ⌘\ (folders toggle) must resolve through the shared lookup and
    /// round-trip its settings encoding.
    #[test]
    fn toggle_folders_binds_cmd_backslash() {
        let b = Action::ToggleFolders.default_binding();
        assert_eq!(b, Binding { shift: false, alt: false, ctrl: false, key: "\\".into() });
        assert_eq!(b.serialize(), "cmd-\\");
        assert_eq!(Binding::parse(&b.serialize()), Some(b));
        let ks = Keystroke {
            modifiers: Modifiers { platform: true, ..Default::default() },
            key: "\\".into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks), Some(Action::ToggleFolders));
    }

    /// ⌘S must reach the sidebar toggle through the same lookup every hotkey
    /// uses, and its default must round-trip as a plain (shiftless) chord.
    #[test]
    fn toggle_sidebar_binds_cmd_s() {
        let b = Action::ToggleSidebar.default_binding();
        assert_eq!(b, Binding { shift: false, alt: false, ctrl: false, key: "s".into() });
        let ks = Keystroke {
            modifiers: Modifiers { platform: true, ..Default::default() },
            key: "s".into(),
            key_char: None,
        };
        assert_eq!(match_action(&ks), Some(Action::ToggleSidebar));
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

    #[test]
    fn toggle_focus_others_binding() {
        let b = Action::ToggleFocusOthers.default_binding();
        assert!(b.shift, "ToggleFocusOthers should require shift");
        assert_eq!(b.key, "f", "ToggleFocusOthers key should be 'f'");
    }

    #[test]
    fn all_actions_have_unique_default_bindings() {
        let mut seen = std::collections::HashSet::new();
        for action in &Action::ALL {
            let b = action.default_binding();
            let key = (b.shift, b.alt, b.ctrl, b.key.clone());
            assert!(
                seen.insert(key.clone()),
                "duplicate default binding {:?} on {:?}",
                key,
                action,
            );
        }
    }

    #[test]
    fn from_name_roundtrips_all_and_rejects_unknown() {
        for action in Action::ALL {
            assert_eq!(Action::from_name(action.name()), Some(action));
        }
        assert_eq!(Action::from_name("nope"), None);
    }

    #[test]
    fn settings_index_covers_all_sections() {
        let index = settings_index();
        for section in &Section::ALL {
            assert!(
                index.iter().any(|e| &e.section == section),
                "settings_index missing entries for section {:?}",
                section,
            );
        }
    }

    #[test]
    fn search_settings_finds_terminal_entry() {
        let results = search_settings("persist");
        assert!(
            results.iter().any(|e| e.section == Section::Terminal),
            "search_settings(\"persist\") should find a Terminal entry",
        );
    }

    #[test]
    fn search_settings_case_insensitive_keyboard() {
        let results = search_settings("SPLIT");
        assert!(
            !results.is_empty(),
            "search_settings(\"SPLIT\") should return results",
        );
        assert!(
            results.iter().all(|e| e.section == Section::Keyboard),
            "all SPLIT results should be in the Keyboard section",
        );
    }

    #[test]
    fn search_settings_empty_query_returns_empty() {
        assert!(search_settings("").is_empty(), "empty query should return empty vec");
        assert!(search_settings("   ").is_empty(), "whitespace query should return empty vec");
    }
}
