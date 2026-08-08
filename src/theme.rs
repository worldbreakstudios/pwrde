//! Theme presets: every chrome color the renderer paints with.
//!
//! Themes are chosen per appearance polarity on Settings → Appearance: one
//! light theme (`"theme.light"`) and one dark theme (`"theme.dark"`), with a
//! mode setting (`"appearance.mode"`: system/dark/light) deciding which slot
//! applies. "System" follows macOS, fed into [`set_system_dark`] by main.rs
//! from gpui's window appearance. Everything is applied live — the renderer
//! re-reads its `theme` reference each frame it paints. Terminal ANSI colors
//! are themed separately in `term_theme`.
//!
//! Beyond the built-in presets, a theme compresses to seven shareable tokens
//! (comma-separated `#rrggbb` hex) that expand back into a full [`Theme`] —
//! see the token section below. An imported token string lives under
//! `"theme.custom.<polarity>"` and resolves through the usual slot chain as
//! `"custom-light"` / `"custom-dark"`.

use std::sync::RwLock;
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
    /// Terminal card fill.
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

// ── Shareable token strings ─────────────────────────────────────────────
//
// The whole chrome derives from seven tokens: background, background 2,
// surface, panel, text, panel text, accent. The remaining `Theme` fields are
// blends computed in `expand_tokens`, so a single pasted string rebuilds a
// complete theme. Terminal ANSI schemes are out of scope — they stay in
// `term_theme`.

/// The seven tokens in string order: gradient_from, gradient_to, card,
/// term_bg, ink, text_bright, accent.
pub type Tokens = [(u8, u8, u8); 7];

/// One `#rrggbb` (or bare `rrggbb`) hex color, case-insensitive.
fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Parse a shared theme string: exactly seven comma-separated hex colors,
/// whitespace around entries tolerated. Anything else is `None`.
pub fn parse_tokens(s: &str) -> Option<Tokens> {
    let mut out = [(0, 0, 0); 7];
    let mut n = 0;
    for part in s.split(',') {
        if n == out.len() {
            return None;
        }
        out[n] = parse_hex(part.trim())?;
        n += 1;
    }
    (n == out.len()).then_some(out)
}

/// The canonical share encoding (lowercase, `#`-prefixed); `parse_tokens`
/// inverts it exactly.
pub fn serialize_tokens(t: &Tokens) -> String {
    t.iter()
        .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A theme's seven tokens — presets export through here too, so any built-in
/// makes a shareable starting point.
pub fn tokens_of(t: &Theme) -> Tokens {
    [t.gradient_from, t.gradient_to, t.card, t.term_bg, t.ink, t.text_bright, t.accent]
}

/// Relative luminance below 50%: decides an imported theme's polarity from
/// its background token.
pub fn is_dark_color((r, g, b): (u8, u8, u8)) -> bool {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0 < 0.5
}

/// Per-channel linear blend, `frac` of the way from `a` to `b`.
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), frac: f32) -> (u8, u8, u8) {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * frac).round() as u8;
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Expand seven tokens into a full theme. Dim text sinks toward its backing
/// surface, the divider lifts slightly off the panel, and light themes cast
/// ink-tinted shadows like the built-in presets do.
pub fn expand_tokens(t: &Tokens) -> Theme {
    let [gradient_from, gradient_to, card, term_bg, ink, text_bright, accent] = *t;
    let dark = is_dark_color(gradient_from);
    Theme {
        name: custom_name(dark),
        label: if dark { "Custom Dark" } else { "Custom Light" },
        dark,
        gradient_from,
        gradient_to,
        term_bg,
        card_divider: mix(term_bg, text_bright, 0.12),
        ink,
        ink_dim: mix(ink, gradient_from, 0.4),
        card,
        accent,
        text_bright,
        text_dim: mix(text_bright, term_bg, 0.45),
        scrim: (0, 0, 0),
        shadow: if dark { (0, 0, 0) } else { ink },
    }
}

/// Settings key holding one polarity's imported token string.
pub fn custom_key(dark: bool) -> &'static str {
    if dark { "theme.custom.dark" } else { "theme.custom.light" }
}

/// Slot value a custom theme is selected under.
pub fn custom_name(dark: bool) -> &'static str {
    if dark { "custom-dark" } else { "custom-light" }
}

/// Expanded custom themes, cached per polarity and keyed by their source
/// string. Each distinct import leaks one boxed `Theme` (~100 bytes) so the
/// pervasive `&'static Theme` references stay valid.
static CUSTOM: [RwLock<Option<(String, &'static Theme)>>; 2] =
    [RwLock::new(None), RwLock::new(None)];

/// The custom theme for a polarity, when a valid token string is stored. A
/// string whose derived polarity mismatches its slot is ignored, so a
/// hand-edited settings file can't put a dark theme in the light slot.
pub fn custom(dark: bool) -> Option<&'static Theme> {
    let src = crate::settings::get_str(custom_key(dark))?;
    let cache = &CUSTOM[dark as usize];
    if let Some((cached, th)) = cache.read().ok()?.as_ref() {
        if *cached == src {
            return (th.dark == dark).then_some(*th);
        }
    }
    let th: &'static Theme = Box::leak(Box::new(expand_tokens(&parse_tokens(&src)?)));
    *cache.write().ok()? = Some((src, th));
    (th.dark == dark).then_some(th)
}

/// The active theme as a shareable token string (Appearance-page export).
pub fn export_current() -> String {
    serialize_tokens(&tokens_of(current()))
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
    let slot = crate::settings::get_str(setting_key(dark));
    // The custom slot resolves through its stored token string; a missing or
    // invalid string falls through the normal chain to the built-in default.
    if slot.as_deref() == Some(custom_name(dark)) {
        if let Some(t) = custom(dark) {
            return t;
        }
    }
    resolve_slot(dark, slot.as_deref(), crate::settings::get_str("theme").as_deref())
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
    fn tokens_round_trip_through_parse_and_serialize() {
        let t = tokens_of(&ARC_LIGHT);
        let s = serialize_tokens(&t);
        assert_eq!(parse_tokens(&s), Some(t));
        // Uppercase, bare hex, and stray whitespace all still parse.
        assert_eq!(parse_tokens(&s.to_uppercase().replace('#', " ")), Some(t));
    }

    #[test]
    fn parse_rejects_malformed_strings() {
        assert_eq!(parse_tokens(""), None);
        let six = ["#111111"; 6].join(",");
        let eight = ["#111111"; 8].join(",");
        assert_eq!(parse_tokens(&six), None);
        assert_eq!(parse_tokens(&eight), None);
        assert_eq!(parse_tokens(&["#11111g"; 7].join(",")), None);
        assert_eq!(parse_tokens(&["#1111"; 7].join(",")), None);
    }

    #[test]
    fn expand_derives_polarity_and_round_trips_the_tokens() {
        let light = tokens_of(&ARC_LIGHT);
        let t = expand_tokens(&light);
        assert!(!t.dark);
        assert_eq!(t.name, "custom-light");
        assert_eq!(tokens_of(&t), light);
        let dark = tokens_of(&MIDNIGHT);
        let t = expand_tokens(&dark);
        assert!(t.dark);
        assert_eq!(t.name, "custom-dark");
        assert_eq!(tokens_of(&t), dark);
    }

    #[test]
    fn expand_derives_distinct_minor_colors() {
        let t = expand_tokens(&tokens_of(&MIDNIGHT));
        assert_ne!(t.ink_dim, t.ink);
        assert_ne!(t.text_dim, t.text_bright);
        assert_ne!(t.card_divider, t.term_bg);
    }

    /// `resolve_slot` knows nothing of customs; with no token string stored
    /// the custom names fall through to the built-in defaults instead of
    /// blanking the UI.
    #[test]
    fn custom_names_fall_back_until_a_string_is_stored() {
        assert_eq!(resolve_slot(true, Some("custom-dark"), None).name, "midnight");
        assert_eq!(resolve_slot(false, Some("custom-light"), None).name, "arc-light");
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
