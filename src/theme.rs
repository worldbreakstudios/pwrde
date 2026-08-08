//! Theme presets: every chrome color the renderer paints with.
//!
//! The active theme is chosen on the Settings → Themes page, persisted under
//! the `"theme"` settings key, and applied live (the renderer re-reads its
//! `theme` reference each frame it paints). Terminal ANSI colors come from the
//! wezterm palette and are not themed here — only pwrde's own chrome is.

/// sRGB u8 triples; the renderer maps them to gpui colors with per-use alpha.
pub struct Theme {
    /// Settings value + stable identifier.
    pub name: &'static str,
    /// Row label on the Themes page.
    pub label: &'static str,
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

pub const ALL: [&Theme; 3] = [&ARC_LIGHT, &SLATE, &MIDNIGHT];

/// Look a theme up by its settings name; unknown names fall back to Arc Light
/// so a hand-edited settings file can't blank the UI.
pub fn by_name(name: &str) -> &'static Theme {
    ALL.iter().find(|t| t.name == name).copied().unwrap_or(&ARC_LIGHT)
}

/// The theme selected in settings (Arc Light when unset).
pub fn current() -> &'static Theme {
    by_name(&crate::settings::get_str("theme").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_finds_every_preset() {
        for t in ALL {
            assert_eq!(by_name(t.name).name, t.name);
        }
    }

    #[test]
    fn unknown_name_falls_back_to_arc_light() {
        assert_eq!(by_name("no-such-theme").name, ARC_LIGHT.name);
        assert_eq!(by_name("").name, ARC_LIGHT.name);
    }
}
