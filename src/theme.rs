//! Chrome colors: every non-terminal color the renderer paints with, derived
//! from the accent.
//!
//! There are no chrome presets. One [`Theme`] per appearance polarity is
//! built by [`from_accent`] the way the GANTRY mock's stylesheet does it —
//! the accent mixed into a near-white (light) or near-black (dark) ground for
//! the sidebar gradient, white/charcoal cards, fixed ink — so every surface
//! carries a hint of the chosen accent, as macOS apps do. The polarity comes
//! from the mode setting (`"appearance.mode"`: system/dark/light); "System"
//! follows macOS, fed into [`set_system_dark`] by main.rs from gpui's window
//! appearance. Everything is applied live — the renderer re-reads its `theme`
//! reference each frame it paints. Terminal ANSI colors are themed separately
//! in `term_theme`.
//!
//! The accent (`"accent"` setting) is chosen the same way: System follows
//! macOS's accent color (pushed in via [`set_system_accent`]), or a named
//! preset overrides it — see the Accent section.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// sRGB u8 triples; the renderer maps them to gpui colors with per-use alpha.
pub struct Theme {
    /// "Light" / "Dark" — shown on the Appearance preview and in diagnostics.
    pub label: &'static str,
    /// Polarity this chrome was derived for.
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

// ── Color helpers ───────────────────────────────────────────────────────

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

// ── Accent ─────────────────────────────────────────────────────────────
//
// The GANTRY mock's `--accent`: the sidebar's selected card and pinned
// avatar, unread dots, the focused pane's tab pill, and rcn's `primary`
// (Approve, the active segmented button). Distinct from a chrome theme's
// own `accent` field, which stays a per-theme tint the renderer paints
// canvas details with (link underlines, hint washes).
//
// Like appearance mode, it defaults to following macOS — System Settings →
// Appearance → Accent color, read by main.rs from `NSColor.controlAccentColor`
// into [`set_system_accent`] — and can be overridden with one of the mock's
// eight named colors (the same set macOS offers).

/// Accent choice (`"accent"` setting): follow the OS, or a fixed preset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accent {
    System,
    Blue,
    Purple,
    Pink,
    Red,
    Orange,
    Yellow,
    Green,
    Graphite,
}

impl Accent {
    /// Swatch order on the Appearance page: System first, then the mock's
    /// palette in its own order.
    pub const ALL: [Accent; 9] = [
        Accent::System,
        Accent::Blue,
        Accent::Purple,
        Accent::Pink,
        Accent::Red,
        Accent::Orange,
        Accent::Yellow,
        Accent::Green,
        Accent::Graphite,
    ];

    /// Stable settings value (`"accent"`).
    pub fn name(self) -> &'static str {
        match self {
            Accent::System => "system",
            Accent::Blue => "blue",
            Accent::Purple => "purple",
            Accent::Pink => "pink",
            Accent::Red => "red",
            Accent::Orange => "orange",
            Accent::Yellow => "yellow",
            Accent::Green => "green",
            Accent::Graphite => "graphite",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Accent::System => "System",
            Accent::Blue => "Blue",
            Accent::Purple => "Purple",
            Accent::Pink => "Pink",
            Accent::Red => "Red",
            Accent::Orange => "Orange",
            Accent::Yellow => "Yellow",
            Accent::Green => "Green",
            Accent::Graphite => "Graphite",
        }
    }

    /// Unknown values fall back to System, like [`Mode::parse`].
    pub fn parse(s: &str) -> Accent {
        Accent::ALL
            .into_iter()
            .find(|a| a.name() == s)
            .unwrap_or(Accent::System)
    }

    /// The preset's sRGB value — the mock's hex table verbatim. `None` for
    /// System, which resolves from the OS at runtime.
    pub fn rgb(self) -> Option<(u8, u8, u8)> {
        Some(match self {
            Accent::System => return None,
            Accent::Blue => (0x0a, 0x84, 0xff),
            Accent::Purple => (0xbf, 0x5a, 0xf2),
            Accent::Pink => (0xff, 0x2d, 0x75),
            Accent::Red => (0xff, 0x45, 0x3a),
            Accent::Orange => (0xff, 0x9f, 0x0a),
            Accent::Yellow => (0xff, 0xd6, 0x0a),
            Accent::Green => (0x30, 0xd1, 0x58),
            Accent::Graphite => (0x8e, 0x8e, 0x93),
        })
    }
}

/// The accent selected in settings (System when unset).
pub fn accent_setting() -> Accent {
    Accent::parse(&crate::settings::get_str("accent").unwrap_or_default())
}

/// The OS accent color, pushed in by main.rs (seeded at window open, refreshed
/// when the window regains focus or its appearance changes — there is no
/// AppKit observer wired for it). `None` until seeded, or off macOS.
static SYSTEM_ACCENT: RwLock<Option<(u8, u8, u8)>> = RwLock::new(None);

/// Record a fresh OS reading. A failed read (`None`) keeps the last known
/// value rather than clearing it, so a transient AppKit nil can't flash the
/// chrome back to the Blue fallback for a frame.
pub fn set_system_accent(rgb: Option<(u8, u8, u8)>) {
    let Some(rgb) = rgb else { return };
    if let Ok(mut slot) = SYSTEM_ACCENT.write() {
        *slot = Some(rgb);
    }
}

pub fn system_accent() -> Option<(u8, u8, u8)> {
    SYSTEM_ACCENT.read().ok().and_then(|s| *s)
}

/// Pure accent resolution: a preset wins outright; System takes the OS color,
/// falling back to the mock's default Blue when none has been read.
pub fn resolve_accent(setting: Accent, system: Option<(u8, u8, u8)>) -> (u8, u8, u8) {
    setting
        .rgb()
        .or(system)
        .unwrap_or_else(|| Accent::Blue.rgb().expect("Blue is a preset"))
}

/// The accent in effect right now (setting + OS accent).
pub fn accent_color() -> (u8, u8, u8) {
    resolve_accent(accent_setting(), system_accent())
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

// ── Derived chrome ──────────────────────────────────────────────────────

/// Build one polarity's chrome from the accent, the way the GANTRY mock's
/// stylesheet does: the sidebar gradient is the accent mixed into a
/// near-white ground (`#fbfcfe`) at 13% → 5%, cards are white with hairline
/// dividers, ink is the mock's `#1c1c22` / `#8a9099`. Dark mirrors the recipe
/// over vitrine's dark material (`#1e1e24`), tinted a touch stronger so the
/// accent still reads on the darker ground.
pub fn from_accent(accent: (u8, u8, u8), dark: bool) -> Theme {
    if dark {
        let ground = (0x1e, 0x1e, 0x24);
        let term_bg = (0x14, 0x14, 0x18);
        let ink = (0xf0, 0xf0, 0xf5);
        let text_bright = (0xea, 0xea, 0xf0);
        Theme {
            label: "Dark",
            dark: true,
            gradient_from: mix(ground, accent, 0.16),
            gradient_to: mix(ground, accent, 0.06),
            term_bg,
            card_divider: mix(term_bg, text_bright, 0.12),
            ink,
            ink_dim: mix(ink, ground, 0.45),
            card: mix(ground, (0xff, 0xff, 0xff), 0.08),
            accent,
            text_bright,
            text_dim: mix(text_bright, term_bg, 0.45),
            scrim: (0, 0, 0),
            shadow: (0, 0, 0),
        }
    } else {
        let ground = (0xfb, 0xfc, 0xfe);
        let term_bg = (0xff, 0xff, 0xff);
        let ink = (0x1c, 0x1c, 0x22);
        Theme {
            label: "Light",
            dark: false,
            gradient_from: mix(ground, accent, 0.13),
            gradient_to: mix(ground, accent, 0.05),
            term_bg,
            card_divider: mix(term_bg, ink, 0.08),
            ink,
            ink_dim: (0x8a, 0x90, 0x99),
            card: (0xff, 0xff, 0xff),
            accent,
            text_bright: ink,
            text_dim: (0x6e, 0x77, 0x81),
            scrim: (0, 0, 0),
            shadow: ink,
        }
    }
}

/// Derived chrome per polarity, cached by the accent it was built from. Each
/// distinct accent leaks one boxed `Theme` (~100 bytes) so the pervasive
/// `&'static Theme` references stay valid.
static DERIVED: [RwLock<Option<((u8, u8, u8), &'static Theme)>>; 2] =
    [RwLock::new(None), RwLock::new(None)];

/// One polarity's chrome for the accent in effect (the Appearance preview
/// asks for the polarity it is previewing).
pub fn selected(dark: bool) -> &'static Theme {
    let accent = accent_color();
    let cache = &DERIVED[dark as usize];
    if let Some((cached, th)) = cache.read().ok().and_then(|c| *c) {
        if cached == accent {
            return th;
        }
    }
    let th: &'static Theme = Box::leak(Box::new(from_accent(accent, dark)));
    if let Ok(mut c) = cache.write() {
        *c = Some((accent, th));
    }
    th
}

/// The theme in effect right now.
pub fn current() -> &'static Theme {
    selected(dark_active())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_round_trips_and_defaults_to_system() {
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.name()), m);
        }
        assert_eq!(Mode::parse(""), Mode::System);
        assert_eq!(Mode::parse("purple"), Mode::System);
    }

    #[test]
    fn accent_parse_round_trips_and_defaults_to_system() {
        for a in Accent::ALL {
            assert_eq!(Accent::parse(a.name()), a);
        }
        assert_eq!(Accent::parse(""), Accent::System);
        assert_eq!(Accent::parse("magenta"), Accent::System);
    }

    #[test]
    fn resolve_accent_prefers_preset_then_system_then_blue() {
        let os = Some((1, 2, 3));
        assert_eq!(resolve_accent(Accent::Purple, os), Accent::Purple.rgb().unwrap());
        assert_eq!(resolve_accent(Accent::System, os), (1, 2, 3));
        assert_eq!(resolve_accent(Accent::System, None), Accent::Blue.rgb().unwrap());
    }

    #[test]
    fn failed_os_reading_keeps_the_last_known_accent() {
        set_system_accent(Some((9, 9, 9)));
        set_system_accent(None);
        assert_eq!(system_accent(), Some((9, 9, 9)));
    }

    #[test]
    fn every_preset_but_system_has_a_color() {
        for a in Accent::ALL {
            assert_eq!(a.rgb().is_none(), a == Accent::System, "{}", a.name());
        }
    }

    #[test]
    fn resolve_dark_forces_or_follows_system() {
        assert!(resolve_dark(Mode::Dark, false));
        assert!(!resolve_dark(Mode::Light, true));
        assert!(resolve_dark(Mode::System, true));
        assert!(!resolve_dark(Mode::System, false));
    }

    #[test]
    fn from_accent_tints_the_gradient_and_carries_the_accent() {
        let pink = (0xff, 0x2d, 0x75);
        for dark in [false, true] {
            let th = from_accent(pink, dark);
            assert_eq!(th.dark, dark);
            assert_eq!(th.accent, pink);
            // The gradient's top is tinted harder than its bottom.
            let dist = |c: (u8, u8, u8)| {
                (c.0 as i32 - pink.0 as i32).abs()
                    + (c.1 as i32 - pink.1 as i32).abs()
                    + (c.2 as i32 - pink.2 as i32).abs()
            };
            assert!(dist(th.gradient_from) < dist(th.gradient_to), "{dark}");
            // Two accents give two chromes.
            assert_ne!(th.gradient_from, from_accent((0x30, 0xd1, 0x58), dark).gradient_from);
        }
    }

    #[test]
    fn from_accent_keeps_ink_legible_on_its_surfaces() {
        for dark in [false, true] {
            let th = from_accent((0xff, 0xd6, 0x0a), dark);
            assert_eq!(is_dark_color(th.gradient_to), dark);
            assert_eq!(is_dark_color(th.card), dark);
            assert_eq!(is_dark_color(th.term_bg), dark);
            assert_ne!(is_dark_color(th.ink), dark);
            assert_ne!(is_dark_color(th.text_bright), dark);
        }
    }

    #[test]
    fn selected_is_cached_per_polarity() {
        let a = selected(false);
        assert!(std::ptr::eq(a, selected(false)));
        assert!(selected(true).dark);
        assert!(!a.dark);
    }
}
