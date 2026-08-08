//! Theme presets: every chrome color the renderer paints with.
//!
//! Themes are chosen per appearance polarity on Settings → Appearance: one
//! light theme (`"theme.light"`) and one dark theme (`"theme.dark"`), with a
//! mode setting (`"appearance.mode"`: system/dark/light) deciding which slot
//! applies. "System" follows macOS, fed into [`set_system_dark`] by main.rs
//! from gpui's window appearance. Everything is applied live — the renderer
//! re-reads its `theme` reference each frame it paints. Terminal ANSI colors
//! are themed separately in `term_theme`.

use std::sync::atomic::{AtomicBool, Ordering};

/// sRGB u8 triples; the renderer maps them to gpui colors with per-use alpha.
pub struct Theme {
    /// Settings value + stable identifier.
    pub name: &'static str,
    /// Row label on the Appearance page.
    pub label: &'static str,
    /// Which appearance slot this theme belongs to (Appearance-page grouping
    /// and the legacy `"theme"` key migration both key off this).
    pub dark: bool,
    /// Window gradient endpoints (painted under everything).
    pub gradient_from: (u8, u8, u8),
    pub gradient_to: (u8, u8, u8),
    /// Terminal / settings card fill.
    pub term_bg: (u8, u8, u8),
    /// Hairline under a card's tab strip.
    pub card_divider: (u8, u8, u8),
    /// Sidebar text on the gradient, and text on `card` surfaces — so `card`
    /// must stay close to the gradient in lightness for both to read.
    pub ink: (u8, u8, u8),
    pub ink_dim: (u8, u8, u8),
    /// Raised surfaces floating on the gradient: sidebar pills, popover
    /// panels, the new-group field.
    pub card: (u8, u8, u8),
    pub accent: (u8, u8, u8),
    /// Text inside dark cards (tab titles).
    pub text_bright: (u8, u8, u8),
    pub text_dim: (u8, u8, u8),
    /// Dimming scrim behind popovers.
    pub scrim: (u8, u8, u8),
    /// Drop-shadow ink under cards and pills.
    pub shadow: (u8, u8, u8),
}

/// The original Arc-style chrome (mockup 3a): dark cards on a warm gradient.
pub const ARC_LIGHT: Theme = Theme {
    name: "arc-light",
    label: "Arc Light",
    dark: false,
    gradient_from: (248, 221, 214),
    gradient_to: (236, 233, 230),
    term_bg: (32, 30, 29),
    card_divider: (51, 48, 46),
    ink: (32, 30, 29),
    ink_dim: (87, 83, 79),
    card: (255, 255, 255),
    accent: (236, 48, 19),
    text_bright: (243, 242, 242),
    text_dim: (138, 133, 128),
    scrim: (0, 0, 0),
    shadow: (32, 30, 29),
};

/// Cool gray-blue light variant.
pub const SLATE: Theme = Theme {
    name: "slate",
    label: "Slate",
    dark: false,
    gradient_from: (213, 224, 242),
    gradient_to: (231, 234, 239),
    term_bg: (27, 30, 36),
    card_divider: (45, 49, 57),
    ink: (28, 32, 40),
    ink_dim: (84, 91, 104),
    card: (255, 255, 255),
    accent: (37, 99, 235),
    text_bright: (240, 242, 246),
    text_dim: (129, 136, 148),
    scrim: (0, 0, 0),
    shadow: (28, 32, 40),
};

/// Dark theme: near-black cards on a deep gradient, light ink in the sidebar.
pub const MIDNIGHT: Theme = Theme {
    name: "midnight",
    label: "Midnight",
    dark: true,
    gradient_from: (34, 30, 44),
    gradient_to: (22, 21, 28),
    term_bg: (16, 15, 19),
    card_divider: (52, 48, 62),
    ink: (233, 229, 241),
    ink_dim: (157, 151, 171),
    card: (54, 50, 66),
    accent: (240, 84, 56),
    text_bright: (240, 238, 244),
    text_dim: (141, 136, 151),
    scrim: (0, 0, 0),
    shadow: (0, 0, 0),
};

/// Slate's dark counterpart: the same gray-blue family on a deep gradient,
/// with the blue accent brightened to read on dark surfaces.
pub const SLATE_DARK: Theme = Theme {
    name: "slate-dark",
    label: "Slate Dark",
    dark: true,
    gradient_from: (30, 36, 48),
    gradient_to: (20, 23, 29),
    term_bg: (15, 17, 22),
    card_divider: (42, 47, 57),
    ink: (222, 228, 238),
    ink_dim: (140, 148, 162),
    card: (44, 50, 62),
    accent: (96, 145, 240),
    text_bright: (235, 239, 245),
    text_dim: (130, 138, 152),
    scrim: (0, 0, 0),
    shadow: (0, 0, 0),
};

pub const ALL: [&Theme; 4] = [&ARC_LIGHT, &SLATE, &MIDNIGHT, &SLATE_DARK];

/// Look a theme up by its settings name.
fn find(name: &str) -> Option<&'static Theme> {
    ALL.iter().find(|t| t.name == name).copied()
}

// ── Appearance mode ─────────────────────────────────────────────────────

/// How the dark/light polarity is chosen: follow macOS, or force one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    System,
    Dark,
    Light,
}

impl Mode {
    /// Segmented-control order on the Appearance page.
    pub const ALL: [Mode; 3] = [Mode::System, Mode::Dark, Mode::Light];

    /// Stable settings value (`"appearance.mode"`).
    pub fn name(self) -> &'static str {
        match self {
            Mode::System => "system",
            Mode::Dark => "dark",
            Mode::Light => "light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::System => "System",
            Mode::Dark => "Dark",
            Mode::Light => "Light",
        }
    }

    /// Unknown values fall back to System so a hand-edited settings file
    /// can't wedge the appearance.
    pub fn parse(s: &str) -> Mode {
        match s {
            "dark" => Mode::Dark,
            "light" => Mode::Light,
            _ => Mode::System,
        }
    }
}

/// The mode selected in settings (System when unset).
pub fn mode() -> Mode {
    Mode::parse(&crate::settings::get_str("appearance.mode").unwrap_or_default())
}

/// The OS appearance, pushed in by main.rs (seeded at window open, updated by
/// gpui's appearance observer). A global so `current()` stays callable from
/// anywhere the settings store is (renderer, terminal config).
static SYSTEM_DARK: AtomicBool = AtomicBool::new(false);

pub fn set_system_dark(dark: bool) {
    SYSTEM_DARK.store(dark, Ordering::Relaxed);
}

pub fn system_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}

/// Pure mode resolution: which polarity applies given the OS state.
pub fn resolve_dark(mode: Mode, system_dark: bool) -> bool {
    match mode {
        Mode::Dark => true,
        Mode::Light => false,
        Mode::System => system_dark,
    }
}

/// The polarity currently in effect (mode setting + OS appearance).
pub fn dark_active() -> bool {
    resolve_dark(mode(), system_dark())
}

/// Settings key holding the theme for one polarity slot.
pub fn setting_key(dark: bool) -> &'static str {
    if dark { "theme.dark" } else { "theme.light" }
}

/// Built-in slot defaults: Arc Light / Midnight.
pub fn slot_default(dark: bool) -> &'static Theme {
    if dark { &MIDNIGHT } else { &ARC_LIGHT }
}

/// Pure fallback chain for one slot: the slot's own value, then the legacy
/// single `"theme"` key from before per-mode slots existed (honored only when
/// its polarity matches, so an old dark pick doesn't hijack the light slot),
/// then the built-in default. Unknown names fall through so a hand-edited
/// settings file can't blank the UI.
pub fn resolve_slot(dark: bool, slot: Option<&str>, legacy: Option<&str>) -> &'static Theme {
    slot.and_then(find)
        .or_else(|| legacy.and_then(find).filter(|t| t.dark == dark))
        .unwrap_or(slot_default(dark))
}

/// The theme configured for a polarity slot (used by the Appearance page to
/// mark both slots' selections).
pub fn selected(dark: bool) -> &'static Theme {
    resolve_slot(
        dark,
        crate::settings::get_str(setting_key(dark)).as_deref(),
        crate::settings::get_str("theme").as_deref(),
    )
}

/// The theme in effect right now.
pub fn current() -> &'static Theme {
    selected(dark_active())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_locates_every_preset() {
        for t in ALL {
            assert_eq!(find(t.name).map(|f| f.name), Some(t.name));
        }
        assert!(find("no-such-theme").is_none());
    }

    #[test]
    fn each_polarity_has_at_least_two_themes() {
        assert!(ALL.iter().filter(|t| t.dark).count() >= 2);
        assert!(ALL.iter().filter(|t| !t.dark).count() >= 2);
    }

    #[test]
    fn mode_parse_round_trips_and_defaults_to_system() {
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.name()), m);
        }
        assert_eq!(Mode::parse(""), Mode::System);
        assert_eq!(Mode::parse("purple"), Mode::System);
    }

    #[test]
    fn resolve_dark_forces_or_follows_system() {
        assert!(resolve_dark(Mode::Dark, false));
        assert!(!resolve_dark(Mode::Light, true));
        assert!(resolve_dark(Mode::System, true));
        assert!(!resolve_dark(Mode::System, false));
    }

    #[test]
    fn slot_resolution_prefers_slot_then_matching_legacy_then_default() {
        // Explicit slot value wins.
        assert_eq!(resolve_slot(true, Some("slate-dark"), Some("midnight")).name, "slate-dark");
        // Legacy key fills an unset slot only when its polarity matches.
        assert_eq!(resolve_slot(true, None, Some("midnight")).name, "midnight");
        assert_eq!(resolve_slot(false, None, Some("midnight")).name, "arc-light");
        assert_eq!(resolve_slot(false, None, Some("slate")).name, "slate");
        // Unknown names fall through to the built-in defaults.
        assert_eq!(resolve_slot(true, Some("bogus"), None).name, "midnight");
        assert_eq!(resolve_slot(false, None, None).name, "arc-light");
    }
}
