//! Terminal color presets: the ANSI palette the panes render with.
//!
//! Like chrome themes, terminal colors are chosen per appearance polarity on
//! Settings → Appearance (`"terminal.light"` / `"terminal.dark"`), and the
//! resolved mode decides which slot applies. The value `"default"` (also the
//! unset default, and any unknown name) means the adaptive scheme: wezterm's
//! stock ANSI table on the chrome theme's `term_bg` — exactly the pre-theming
//! look. Presets carry only the 16 ANSI slots plus fg/bg; the 240 extended
//! cube/gray entries are universal and stay stock.
//!
//! Every colour is editable. A polarity resolves in two steps: the **base**
//! (a preset, a user-saved custom theme from `terminal.custom`, or the
//! adaptive default) and then that polarity's **overrides**
//! (`terminal.light.overrides` / `terminal.dark.overrides`, `{key: "#rrggbb"}`,
//! holding only the colours that differ from the base). [`resolve`] is the one
//! pure function that does this — every input is explicit, so it is fully
//! unit-testable — and [`resolved`] / [`resolved_for`] are the thin
//! settings-reading wrappers the renderer, tile chrome and Settings preview
//! call. [`Palette20`] is the resolved value: `bg, fg, cursor, sel` plus the 16
//! ANSI slots under the stable keys in [`KEYS`].
//!
//! Two slots are derived rather than stated, and both keep the exact colour the
//! app painted before colours became editable: the cursor is a block of the
//! effective foreground, and the selection is the chrome accent — which
//! `renderer::selection_rects` composites at 30 % over the pane ground, so an
//! explicit override and the derived default are painted by the same code
//! path. A custom theme or an override that names either slot is explicit and
//! wins outright. Invariant: with no overrides and no custom themes the
//! resolved palette is byte-identical to [`build`] (pinned by tests for every
//! preset and for the adaptive default); that oracle ([`build`]) is kept for tests
//! only, since every painter now goes through [`resolve`].
//!
//! Settings JSON is user-editable, so parsing is tolerant: malformed entries,
//! unknown keys and wrong types are skipped — never a panic, never a launch
//! failure.

use std::collections::BTreeMap;

use serde_json::Value;
use wezterm_term::color::{ColorPalette, RgbColor};

/// sRGB u8 triples, same convention as `theme::Theme`.
/// `ansi` order: black, red, green, yellow, blue, magenta, cyan, white.
pub struct TermTheme {
    /// Settings value + stable identifier.
    pub name: &'static str,
    /// Slot label on the Appearance page.
    pub label: &'static str,
    /// Which appearance slot this scheme belongs to.
    pub dark: bool,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub ansi: [(u8, u8, u8); 8],
    pub brights: [(u8, u8, u8); 8],
}

pub const SOLARIZED_DARK: TermTheme = TermTheme {
    name: "solarized-dark",
    label: "Solarized Dark",
    dark: true,
    fg: (0x83, 0x94, 0x96),
    bg: (0x00, 0x2b, 0x36),
    ansi: [
        (0x07, 0x36, 0x42),
        (0xdc, 0x32, 0x2f),
        (0x85, 0x99, 0x00),
        (0xb5, 0x89, 0x00),
        (0x26, 0x8b, 0xd2),
        (0xd3, 0x36, 0x82),
        (0x2a, 0xa1, 0x98),
        (0xee, 0xe8, 0xd5),
    ],
    brights: [
        (0x00, 0x2b, 0x36),
        (0xcb, 0x4b, 0x16),
        (0x58, 0x6e, 0x75),
        (0x65, 0x7b, 0x83),
        (0x83, 0x94, 0x96),
        (0x6c, 0x71, 0xc4),
        (0x93, 0xa1, 0xa1),
        (0xfd, 0xf6, 0xe3),
    ],
};

/// Solarized's light half: same accents, inverted base tones.
pub const SOLARIZED_LIGHT: TermTheme = TermTheme {
    name: "solarized-light",
    label: "Solarized Light",
    dark: false,
    fg: (0x65, 0x7b, 0x83),
    bg: (0xfd, 0xf6, 0xe3),
    ansi: SOLARIZED_DARK.ansi,
    brights: SOLARIZED_DARK.brights,
};

pub const DRACULA: TermTheme = TermTheme {
    name: "dracula",
    label: "Dracula",
    dark: true,
    fg: (0xf8, 0xf8, 0xf2),
    bg: (0x28, 0x2a, 0x36),
    ansi: [
        (0x21, 0x22, 0x2c),
        (0xff, 0x55, 0x55),
        (0x50, 0xfa, 0x7b),
        (0xf1, 0xfa, 0x8c),
        (0xbd, 0x93, 0xf9),
        (0xff, 0x79, 0xc6),
        (0x8b, 0xe9, 0xfd),
        (0xf8, 0xf8, 0xf2),
    ],
    brights: [
        (0x62, 0x72, 0xa4),
        (0xff, 0x6e, 0x6e),
        (0x69, 0xff, 0x94),
        (0xff, 0xff, 0xa5),
        (0xd6, 0xac, 0xff),
        (0xff, 0x92, 0xdf),
        (0xa4, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ],
};

pub const GRUVBOX_DARK: TermTheme = TermTheme {
    name: "gruvbox-dark",
    label: "Gruvbox Dark",
    dark: true,
    fg: (0xeb, 0xdb, 0xb2),
    bg: (0x28, 0x28, 0x28),
    ansi: [
        (0x28, 0x28, 0x28),
        (0xcc, 0x24, 0x1d),
        (0x98, 0x97, 0x1a),
        (0xd7, 0x99, 0x21),
        (0x45, 0x85, 0x88),
        (0xb1, 0x62, 0x86),
        (0x68, 0x9d, 0x6a),
        (0xa8, 0x99, 0x84),
    ],
    brights: [
        (0x92, 0x83, 0x74),
        (0xfb, 0x49, 0x34),
        (0xb8, 0xbb, 0x26),
        (0xfa, 0xbd, 0x2f),
        (0x83, 0xa5, 0x98),
        (0xd3, 0x86, 0x9b),
        (0x8e, 0xc0, 0x7c),
        (0xeb, 0xdb, 0xb2),
    ],
};

pub const GRUVBOX_LIGHT: TermTheme = TermTheme {
    name: "gruvbox-light",
    label: "Gruvbox Light",
    dark: false,
    fg: (0x3c, 0x38, 0x36),
    bg: (0xfb, 0xf1, 0xc7),
    ansi: [
        (0xfb, 0xf1, 0xc7),
        (0xcc, 0x24, 0x1d),
        (0x98, 0x97, 0x1a),
        (0xd7, 0x99, 0x21),
        (0x45, 0x85, 0x88),
        (0xb1, 0x62, 0x86),
        (0x68, 0x9d, 0x6a),
        (0x7c, 0x6f, 0x64),
    ],
    brights: [
        (0x92, 0x83, 0x74),
        (0x9d, 0x00, 0x06),
        (0x79, 0x74, 0x0e),
        (0xb5, 0x76, 0x14),
        (0x07, 0x66, 0x78),
        (0x8f, 0x3f, 0x71),
        (0x42, 0x7b, 0x58),
        (0x3c, 0x38, 0x36),
    ],
};

pub const NORD: TermTheme = TermTheme {
    name: "nord",
    label: "Nord",
    dark: true,
    fg: (0xd8, 0xde, 0xe9),
    bg: (0x2e, 0x34, 0x40),
    ansi: [
        (0x3b, 0x42, 0x52),
        (0xbf, 0x61, 0x6a),
        (0xa3, 0xbe, 0x8c),
        (0xeb, 0xcb, 0x8b),
        (0x81, 0xa1, 0xc1),
        (0xb4, 0x8e, 0xad),
        (0x88, 0xc0, 0xd0),
        (0xe5, 0xe9, 0xf0),
    ],
    brights: [
        (0x4c, 0x56, 0x6a),
        (0xbf, 0x61, 0x6a),
        (0xa3, 0xbe, 0x8c),
        (0xeb, 0xcb, 0x8b),
        (0x81, 0xa1, 0xc1),
        (0xb4, 0x8e, 0xad),
        (0x8f, 0xbc, 0xbb),
        (0xec, 0xef, 0xf4),
    ],
};

pub const ONE_DARK: TermTheme = TermTheme {
    name: "one-dark",
    label: "One Dark",
    dark: true,
    fg: (0xab, 0xb2, 0xbf),
    bg: (0x28, 0x2c, 0x34),
    ansi: [
        (0x28, 0x2c, 0x34),
        (0xe0, 0x6c, 0x75),
        (0x98, 0xc3, 0x79),
        (0xe5, 0xc0, 0x7b),
        (0x61, 0xaf, 0xef),
        (0xc6, 0x78, 0xdd),
        (0x56, 0xb6, 0xc2),
        (0xab, 0xb2, 0xbf),
    ],
    brights: [
        (0x5c, 0x63, 0x70),
        (0xe0, 0x6c, 0x75),
        (0x98, 0xc3, 0x79),
        (0xe5, 0xc0, 0x7b),
        (0x61, 0xaf, 0xef),
        (0xc6, 0x78, 0xdd),
        (0x56, 0xb6, 0xc2),
        (0xff, 0xff, 0xff),
    ],
};

pub const ONE_LIGHT: TermTheme = TermTheme {
    name: "one-light",
    label: "One Light",
    dark: false,
    fg: (0x38, 0x3a, 0x42),
    bg: (0xfa, 0xfa, 0xfa),
    ansi: [
        (0x38, 0x3a, 0x42),
        (0xe4, 0x56, 0x49),
        (0x50, 0xa1, 0x4f),
        (0xc1, 0x84, 0x01),
        (0x01, 0x84, 0xbc),
        (0xa6, 0x26, 0xa4),
        (0x09, 0x97, 0xb3),
        (0xfa, 0xfa, 0xfa),
    ],
    brights: [
        (0x4f, 0x52, 0x5e),
        (0xe4, 0x56, 0x49),
        (0x50, 0xa1, 0x4f),
        (0xc1, 0x84, 0x01),
        (0x01, 0x84, 0xbc),
        (0xa6, 0x26, 0xa4),
        (0x09, 0x97, 0xb3),
        (0xff, 0xff, 0xff),
    ],
};

pub const TOKYO_NIGHT: TermTheme = TermTheme {
    name: "tokyo-night",
    label: "Tokyo Night",
    dark: true,
    fg: (0xc0, 0xca, 0xf5),
    bg: (0x1a, 0x1b, 0x26),
    ansi: [
        (0x15, 0x16, 0x1e),
        (0xf7, 0x76, 0x8e),
        (0x9e, 0xce, 0x6a),
        (0xe0, 0xaf, 0x68),
        (0x7a, 0xa2, 0xf7),
        (0xbb, 0x9a, 0xf7),
        (0x7d, 0xcf, 0xff),
        (0xa9, 0xb1, 0xd6),
    ],
    brights: [
        (0x41, 0x48, 0x68),
        (0xf7, 0x76, 0x8e),
        (0x9e, 0xce, 0x6a),
        (0xe0, 0xaf, 0x68),
        (0x7a, 0xa2, 0xf7),
        (0xbb, 0x9a, 0xf7),
        (0x7d, 0xcf, 0xff),
        (0xc0, 0xca, 0xf5),
    ],
};

pub const CATPPUCCIN_MOCHA: TermTheme = TermTheme {
    name: "catppuccin-mocha",
    label: "Catppuccin Mocha",
    dark: true,
    fg: (0xcd, 0xd6, 0xf4),
    bg: (0x1e, 0x1e, 0x2e),
    ansi: [
        (0x45, 0x47, 0x5a),
        (0xf3, 0x8b, 0xa8),
        (0xa6, 0xe3, 0xa1),
        (0xf9, 0xe2, 0xaf),
        (0x89, 0xb4, 0xfa),
        (0xf5, 0xc2, 0xe7),
        (0x94, 0xe2, 0xd5),
        (0xba, 0xc2, 0xde),
    ],
    brights: [
        (0x58, 0x5b, 0x70),
        (0xf3, 0x8b, 0xa8),
        (0xa6, 0xe3, 0xa1),
        (0xf9, 0xe2, 0xaf),
        (0x89, 0xb4, 0xfa),
        (0xf5, 0xc2, 0xe7),
        (0x94, 0xe2, 0xd5),
        (0xa6, 0xad, 0xc8),
    ],
};

pub const GITHUB_DARK: TermTheme = TermTheme {
    name: "github-dark",
    label: "GitHub Dark",
    dark: true,
    fg: (0xc9, 0xd1, 0xd9),
    bg: (0x0d, 0x11, 0x17),
    ansi: [
        (0x48, 0x4f, 0x58),
        (0xff, 0x7b, 0x72),
        (0x3f, 0xb9, 0x50),
        (0xd2, 0x99, 0x22),
        (0x58, 0xa6, 0xff),
        (0xbc, 0x8c, 0xff),
        (0x39, 0xc5, 0xcf),
        (0xb1, 0xba, 0xc4),
    ],
    brights: [
        (0x6e, 0x76, 0x81),
        (0xff, 0xa1, 0x98),
        (0x56, 0xd3, 0x64),
        (0xe3, 0xb3, 0x41),
        (0x79, 0xc0, 0xff),
        (0xd2, 0xa8, 0xff),
        (0x56, 0xd4, 0xdd),
        (0xff, 0xff, 0xff),
    ],
};

pub const GITHUB_LIGHT: TermTheme = TermTheme {
    name: "github-light",
    label: "GitHub Light",
    dark: false,
    fg: (0x24, 0x29, 0x2f),
    bg: (0xff, 0xff, 0xff),
    ansi: [
        (0x24, 0x29, 0x2f),
        (0xcf, 0x22, 0x2e),
        (0x11, 0x63, 0x29),
        (0xb0, 0x88, 0x00),
        (0x09, 0x69, 0xda),
        (0x82, 0x50, 0xdf),
        (0x1b, 0x7c, 0x83),
        (0x6e, 0x77, 0x81),
    ],
    brights: [
        (0x57, 0x60, 0x6a),
        (0xa4, 0x0e, 0x26),
        (0x1a, 0x7f, 0x37),
        (0xd4, 0xa7, 0x2c),
        (0x21, 0x8b, 0xff),
        (0xa4, 0x75, 0xf9),
        (0x31, 0x92, 0xaa),
        (0x8c, 0x95, 0x9f),
    ],
};

pub const GITHUB_DARK_HC: TermTheme = TermTheme {
    name: "github-dark-hc",
    label: "GitHub Dark HC",
    dark: true,
    fg: (0xf0, 0xf3, 0xf6),
    bg: (0x0a, 0x0c, 0x10),
    ansi: [
        (0x7a, 0x82, 0x8e),
        (0xff, 0x94, 0x92),
        (0x26, 0xcd, 0x4d),
        (0xf0, 0xb7, 0x2f),
        (0x71, 0xb7, 0xff),
        (0xcb, 0x9e, 0xff),
        (0x39, 0xc5, 0xcf),
        (0xd9, 0xde, 0xe3),
    ],
    brights: [
        (0x9e, 0xa7, 0xb3),
        (0xff, 0xb1, 0xaf),
        (0x4a, 0xe1, 0x68),
        (0xf7, 0xc8, 0x43),
        (0x91, 0xcb, 0xff),
        (0xdb, 0xb7, 0xff),
        (0x56, 0xd4, 0xdd),
        (0xff, 0xff, 0xff),
    ],
};

pub const GITHUB_LIGHT_HC: TermTheme = TermTheme {
    name: "github-light-hc",
    label: "GitHub Light HC",
    dark: false,
    fg: (0x0e, 0x11, 0x16),
    bg: (0xff, 0xff, 0xff),
    ansi: [
        (0x0e, 0x11, 0x16),
        (0xa0, 0x11, 0x1f),
        (0x02, 0x4c, 0x1a),
        (0x3f, 0x22, 0x00),
        (0x03, 0x49, 0xb4),
        (0x62, 0x2c, 0xbc),
        (0x1b, 0x7c, 0x83),
        (0x66, 0x70, 0x7b),
    ],
    brights: [
        (0x4b, 0x53, 0x5d),
        (0x86, 0x06, 0x1d),
        (0x05, 0x5d, 0x20),
        (0x4e, 0x2c, 0x00),
        (0x11, 0x68, 0xe3),
        (0x84, 0x4a, 0xe7),
        (0x31, 0x92, 0xaa),
        (0x88, 0x92, 0x9d),
    ],
};

/// Appearance-page order: light schemes first, then dark (the page groups by
/// polarity, and packs slots in this sequence).
pub const ALL: [&TermTheme; 14] = [
    &SOLARIZED_LIGHT,
    &GRUVBOX_LIGHT,
    &ONE_LIGHT,
    &GITHUB_LIGHT,
    &GITHUB_LIGHT_HC,
    &SOLARIZED_DARK,
    &DRACULA,
    &GRUVBOX_DARK,
    &NORD,
    &ONE_DARK,
    &TOKYO_NIGHT,
    &CATPPUCCIN_MOCHA,
    &GITHUB_DARK,
    &GITHUB_DARK_HC,
];

/// Look a preset up by its settings name. `"default"` and unknown names yield
/// `None` — the adaptive default scheme.
pub fn find(name: &str) -> Option<&'static TermTheme> {
    ALL.iter().find(|t| t.name == name).copied()
}

/// Settings key holding the scheme for one polarity slot.
pub fn setting_key(dark: bool) -> &'static str {
    if dark { "terminal.dark" } else { "terminal.light" }
}

/// Pure slot resolution over a raw setting value (unit-testable).
pub fn resolve_slot(slot: Option<&str>) -> Option<&'static TermTheme> {
    slot.and_then(find)
}

/// The scheme configured for a polarity slot; `None` is the adaptive default.
pub fn selected(dark: bool) -> Option<&'static TermTheme> {
    resolve_slot(crate::settings::get_str(setting_key(dark)).as_deref())
}

/// The theme choices for one polarity slot as `(key, label)` pairs: the
/// adaptive "Default" first, then that polarity's presets, then that
/// polarity's custom themes — the order the Appearance select lists them in.
pub fn slot_options(dark: bool) -> Vec<(String, String)> {
    let mut out = vec![("default".to_string(), "Default".to_string())];
    for t in ALL.iter().filter(|t| t.dark == dark) {
        out.push((t.name.to_string(), t.label.to_string()));
    }
    for c in load_customs().iter().filter(|c| c.dark == dark) {
        out.push((c.name.clone(), c.label.clone()));
    }
    out
}

fn srgba(rgb: (u8, u8, u8)) -> wezterm_term::color::SrgbaTuple {
    RgbColor::new_8bpc(rgb.0, rgb.1, rgb.2).into()
}

/// Build a wezterm palette from a preset (or the adaptive default when
/// `scheme` is `None`).
///
/// This is the **pre-editing mapping**, kept as the reference oracle the
/// resolution tests compare against (DoD 4: uncustomised resolution must be
/// byte-identical to it for every preset and for the adaptive default).
/// Nothing paints from it any more — live panes go through [`resolved`] — so
/// it is compiled only for tests.
#[cfg(test)]
pub fn build(scheme: Option<&TermTheme>, chrome_bg: (u8, u8, u8)) -> ColorPalette {
    let mut p = ColorPalette::default();
    match scheme {
        // Adaptive default: stock ANSI table, pane ground from the chrome
        // theme — the exact pre-theming look.
        None => p.background = srgba(chrome_bg),
        Some(t) => {
            for (i, c) in t.ansi.iter().chain(t.brights.iter()).enumerate() {
                p.colors.0[i] = srgba(*c);
            }
            p.foreground = srgba(t.fg);
            p.background = srgba(t.bg);
            // A plain fg-colored block cursor (the renderer draws the block
            // itself from `foreground`; these keep OSC color reports honest).
            p.cursor_bg = srgba(t.fg);
            p.cursor_border = srgba(t.fg);
            p.cursor_fg = srgba(t.bg);
        },
    }
    p
}


// ── Editable 20-colour palette (Settings → Appearance) ─────────────────────
//
// Every terminal colour is editable. The model is one flat [`Palette20`]
// addressed by the stable string keys in [`KEYS`]; the settings store keeps
// only the *differences* from the base slot (per polarity) plus any custom
// themes the user saved. [`resolve`] is the single pure resolution path —
// base slot (preset | custom theme | adaptive default) with the per-polarity
// overrides on top — and live panes and the settings preview both go through
// it, so there is exactly one place a colour can come from.

/// Stable keys of the 20 editable colours, in display order: the four core
/// slots, then ANSI 0–7, then the brights 8–15.
pub const KEYS: [&str; 20] = [
    "bg", "fg", "cursor", "sel", "ansi0", "ansi1", "ansi2", "ansi3", "ansi4", "ansi5", "ansi6",
    "ansi7", "ansi8", "ansi9", "ansi10", "ansi11", "ansi12", "ansi13", "ansi14", "ansi15",
];

/// Display labels, index-aligned with [`KEYS`].
pub const LABELS: [&str; 20] = [
    "Background",
    "Foreground",
    "Cursor",
    "Selection",
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "White",
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "White",
];

/// Settings key holding a polarity's per-colour overrides (`{key: "#rrggbb"}`).
pub fn overrides_key(dark: bool) -> &'static str {
    if dark { "terminal.dark.overrides" } else { "terminal.light.overrides" }
}

/// Settings key holding the user's saved custom themes (JSON array).
pub const CUSTOM_THEMES_KEY: &str = "terminal.custom";

/// Index of a colour key in [`KEYS`]; `None` for anything unknown.
pub fn key_index(key: &str) -> Option<usize> {
    KEYS.iter().position(|k| *k == key)
}

/// Parse `#rrggbb` (case-insensitive). Anything else — wrong length, missing
/// `#`, non-hex characters — is rejected.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some((((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8))
}

/// `(r, g, b)` → `"#rrggbb"`, always lowercase — the stored spelling.
pub fn fmt_hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// The 20 resolved colours of one polarity slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Palette20 {
    pub bg: (u8, u8, u8),
    pub fg: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub sel: (u8, u8, u8),
    /// ANSI 0–15 in key order (`ansi0` … `ansi15`).
    pub ansi: [(u8, u8, u8); 16],
}

impl Palette20 {
    /// Colour at a [`KEYS`] index (`0..=3` core, `4..=19` ANSI 0–15).
    pub fn get_at(&self, i: usize) -> (u8, u8, u8) {
        match i {
            0 => self.bg,
            1 => self.fg,
            2 => self.cursor,
            3 => self.sel,
            _ => self.ansi[(i - 4).min(15)],
        }
    }

    /// Set the colour at a [`KEYS`] index.
    pub fn set_at(&mut self, i: usize, c: (u8, u8, u8)) {
        match i {
            0 => self.bg = c,
            1 => self.fg = c,
            2 => self.cursor = c,
            3 => self.sel = c,
            _ => self.ansi[(i - 4).min(15)] = c,
        }
    }

    /// Colour for a key, or `None` for an unknown key.
    pub fn get(&self, key: &str) -> Option<(u8, u8, u8)> {
        key_index(key).map(|i| self.get_at(i))
    }

    /// Hex spelling of a key's colour.
    pub fn hex(&self, key: &str) -> Option<String> {
        self.get(key).map(fmt_hex)
    }

    /// All 20 keys as `key → "#rrggbb"`, the persisted form.
    pub fn keys_map(&self) -> BTreeMap<String, String> {
        KEYS.iter()
            .enumerate()
            .map(|(i, k)| (k.to_string(), fmt_hex(self.get_at(i))))
            .collect()
    }
}

/// A theme the user saved from the Appearance page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomTheme {
    /// Stable identifier (also the settings value naming the slot).
pub name: String,
    /// Display label.
pub label: String,
    /// Which polarity slot it belongs to.
pub dark: bool,
    /// The 20 colours, as `key → "#rrggbb"`; missing/unknown keys fall back.
    pub colors: BTreeMap<String, String>,
}

impl CustomTheme {
    /// The persisted JSON shape: `{name, label, dark, colors: {key: "#rrggbb"}}`.
    pub fn to_value(&self) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("name".into(), Value::String(self.name.clone()));
        o.insert("label".into(), Value::String(self.label.clone()));
        o.insert("dark".into(), Value::Bool(self.dark));
        let mut colors = serde_json::Map::new();
        for k in KEYS {
            if let Some(v) = self.colors.get(k) {
                colors.insert(k.to_string(), Value::String(v.clone()));
            }
        }
        o.insert("colors".into(), Value::Object(colors));
        Value::Object(o)
    }
}

/// One polarity slot, fully resolved: the 20 colours plus where they came
/// from (base slot, the effective overrides, and whether the cursor/selection
/// colours were stated explicitly or are today's derived defaults).
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The colours that should be painted.
pub colors: Palette20,
    /// Settings value of the base slot (`"default"`, a preset name, or a
    /// custom theme's name).
pub base_key: String,
    /// Human label of the base slot.
pub base_label: String,
    /// Effective overrides (`key → "#rrggbb"`), already validated/normalised.
pub overrides: BTreeMap<String, String>,
    /// The cursor colour is explicit (custom theme key or an override), not
    /// the derived foreground.
pub cursor_explicit: bool,
    /// The selection colour is explicit, not the chrome accent. The renderer
    /// composites whatever this palette carries at 30 % over the pane ground
    /// (unchanged from before this module grew editable colours), so the
    /// non-explicit value is the raw accent — that is what makes an
    /// uncustomised selection byte-identical to today.
    #[allow(dead_code)] // read by the resolution tests; the renderer composites instead
    pub sel_explicit: bool,
    /// The base slot paints cursor/selection itself (a preset or custom
    /// theme), so the wezterm cursor fields are set from them.
    cursor_from_base: bool,
}

impl Resolved {
    /// How many colours differ from the base slot.
    pub fn modified(&self) -> usize {
        self.overrides.len()
    }

    /// True when at least one colour differs from the base slot.
    pub fn is_modified(&self) -> bool {
        !self.overrides.is_empty()
    }

    /// Colour for a [`KEYS`] key.
    pub fn hex(&self, key: &str) -> Option<String> {
        self.colors.hex(key)
    }

    /// The wezterm palette the panes render with. Byte-identical to [`build`]
    /// when nothing has been customised (the invariant test pins this): the
    /// extended 240 cube/gray entries always stay stock and the cursor fields
    /// are only touched when a base theme states them.
    pub fn to_color_palette(&self) -> ColorPalette {
        let mut p = ColorPalette::default();
        for (i, c) in self.colors.ansi.iter().enumerate() {
            p.colors.0[i] = srgba(*c);
        }
        p.foreground = srgba(self.colors.fg);
        p.background = srgba(self.colors.bg);
        if self.cursor_from_base || self.cursor_explicit {
            p.cursor_bg = srgba(self.colors.cursor);
            p.cursor_border = srgba(self.colors.cursor);
            p.cursor_fg = srgba(self.colors.bg);
        }
        p
    }
}

/// sRGB u8 triple of a wezterm colour — the same conversion the renderer
/// paints with, so palette comparisons are painted-pixel comparisons.
fn tuple_u8(c: wezterm_term::color::SrgbaTuple) -> (u8, u8, u8) {
    let (r, g, b, _) = c.to_srgb_u8();
    (r, g, b)
}

/// Composite `over` at `alpha` over an opaque `base` — how a translucent
/// quad (the selection highlight) lands on the pane ground.
pub fn blend(base: (u8, u8, u8), over: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let mix = |b: u8, o: u8| {
        (b as f32 * (1.0 - alpha) + o as f32 * alpha).round().clamp(0.0, 255.0) as u8
    };
    (mix(base.0, over.0), mix(base.1, over.1), mix(base.2, over.2))
}

/// Resolve one polarity slot. Pure — every input is explicit, so tests can
/// exercise the whole resolution without touching settings.
///
/// Order: base slot (preset → custom theme → adaptive default, unknown names
/// falling back to the adaptive default) → per-polarity overrides on top.
pub fn resolve(
    slot: Option<&str>,
    overrides: &BTreeMap<String, String>,
    customs: &[CustomTheme],
    dark: bool,
    chrome_bg: (u8, u8, u8),
    chrome_accent: (u8, u8, u8),
) -> Resolved {
    // Adaptive default: wezterm's stock ANSI table on the chrome ground.
    let stock = ColorPalette::default();
    let stock_fg = tuple_u8(stock.foreground);
    let mut ansi = [(0u8, 0u8, 0u8); 16];
    for (i, slot_c) in ansi.iter_mut().enumerate() {
        *slot_c = tuple_u8(stock.colors.0[i]);
    }
    let mut colors = Palette20 {
        bg: chrome_bg,
        fg: stock_fg,
        cursor: stock_fg,
        sel: blend(chrome_bg, chrome_accent, 0.30),
        ansi,
    };

    let name = slot.unwrap_or("default");
    let mut base_key = "default".to_string();
    let mut base_label = "Default".to_string();
    let mut cursor_explicit = false;
    let mut sel_explicit = false;
    let mut cursor_from_base = false;

    if let Some(t) = find(name) {
        base_key = t.name.to_string();
        base_label = t.label.to_string();
        colors.fg = t.fg;
        colors.bg = t.bg;
        for i in 0..8 {
            colors.ansi[i] = t.ansi[i];
            colors.ansi[i + 8] = t.brights[i];
        }
        cursor_from_base = true;
    } else if let Some(c) = customs.iter().find(|c| c.name == name && c.dark == dark) {
        base_key = c.name.clone();
        base_label = c.label.clone();
        for (i, k) in KEYS.iter().enumerate() {
            if let Some(v) = c.colors.get(*k).and_then(|s| parse_hex(s)) {
                colors.set_at(i, v);
            }
        }
        cursor_from_base = true;
        cursor_explicit = c.colors.get("cursor").and_then(|s| parse_hex(s)).is_some();
        sel_explicit = c.colors.get("sel").and_then(|s| parse_hex(s)).is_some();
    }

    // Derived defaults for the two slots a preset does not state: the cursor
    // is a block of the effective foreground and the selection is the chrome
    // accent at 30 % over the pane ground — exactly what the renderer paints
    // today when nothing has been customised.
    if !cursor_explicit {
        colors.cursor = colors.fg;
    }
    if !sel_explicit {
        // Raw accent: `selection_rects` still composites it at 0.30, so an
        // explicit override and a preset/custom colour are painted the same
        // way, and the uncustomised paint is exactly today's.
        colors.sel = chrome_accent;
    }

    let mut applied = BTreeMap::new();
    for (k, v) in overrides {
        let (Some(i), Some(c)) = (key_index(k), parse_hex(v)) else {
            continue;
        };
        colors.set_at(i, c);
        applied.insert(k.clone(), fmt_hex(c));
        match k.as_str() {
            "cursor" => cursor_explicit = true,
            "sel" => sel_explicit = true,
            _ => {},
        }
    }
    // A cursor nobody stated keeps tracking the foreground, overridden or not.
    if !cursor_explicit {
        colors.cursor = colors.fg;
    }

    Resolved {
        colors,
        base_key,
        base_label,
        overrides: applied,
        cursor_explicit,
        sel_explicit,
        cursor_from_base,
    }
}

/// Read a polarity's overrides out of the settings store, ignoring anything
/// malformed (unknown keys, non-object JSON, non-hex or wrongly-cased
/// values are dropped, never fatal).
pub fn load_overrides(dark: bool) -> BTreeMap<String, String> {
    parse_overrides(setting_json(overrides_key(dark)).as_deref())
}

/// A structured setting as JSON text for the pure parsers. Objects/arrays are
/// what this module writes; a string holding JSON is accepted too.
fn setting_json(key: &str) -> Option<String> {
    crate::settings::get_value(key).map(|v| match v {
        Value::String(s) => s,
        other => other.to_string(),
    })
}

/// Pure override parsing — the settings JSON is user-editable, so a junk
/// value must degrade to "no overrides" rather than panic.
pub fn parse_overrides(raw: Option<&str>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(raw) = raw else { return out };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return out;
    };
    let Some(obj) = v.as_object() else { return out };
    for (k, val) in obj {
        if key_index(k).is_none() {
            continue;
        }
        if let Some(c) = val.as_str().and_then(parse_hex) {
            out.insert(k.clone(), fmt_hex(c));
        }
    }
    out
}

/// Read the saved custom themes, skipping malformed entries.
pub fn load_customs() -> Vec<CustomTheme> {
    parse_customs(setting_json(CUSTOM_THEMES_KEY).as_deref())
}

/// Pure custom-theme parsing. Entries without a usable name are skipped;
/// duplicate names keep the first; a colour key that is missing, unknown or
/// not a hex triple is dropped (resolution falls back for it).
pub fn parse_customs(raw: Option<&str>) -> Vec<CustomTheme> {
    let mut out: Vec<CustomTheme> = Vec::new();
    let Some(raw) = raw else { return out };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return out;
    };
    let Some(arr) = v.as_array() else { return out };
    for e in arr {
        let Some(o) = e.as_object() else { continue };
        let Some(name) = o.get("name").and_then(|v| v.as_str()) else { continue };
        if name.is_empty() || find(name).is_some() || out.iter().any(|c| c.name == name) {
            continue;
        }
        let label = o
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_string();
        let dark = o.get("dark").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut colors = BTreeMap::new();
        if let Some(c) = o.get("colors").and_then(|v| v.as_object()) {
            for (k, val) in c {
                if key_index(k).is_none() {
                    continue;
                }
                if let Some(c) = val.as_str().and_then(parse_hex) {
                    colors.insert(k.clone(), fmt_hex(c));
                }
            }
        }
        out.push(CustomTheme {
            name: name.to_string(),
            label,
            dark,
            colors,
        });
    }
    out
}

/// Resolved colours for a polarity slot, with the chrome ground/accent the
/// caller observed — the settings-reading wrapper around [`resolve`].
pub fn resolved_for(
    dark: bool,
    chrome_bg: (u8, u8, u8),
    chrome_accent: (u8, u8, u8),
) -> Resolved {
    let slot = crate::settings::get_str(setting_key(dark));
    resolve(
        slot.as_deref(),
        &load_overrides(dark),
        &load_customs(),
        dark,
        chrome_bg,
        chrome_accent,
    )
}

/// Resolved colours for a polarity slot against the chrome theme that serves
/// it right now.
pub fn resolved(dark: bool) -> Resolved {
    let th = crate::theme::selected(dark);
    resolved_for(dark, th.term_bg, th.accent)
}

/// The base slot with the overrides stripped — what a swatch compares against
/// to decide whether it is modified, and what "Reset" goes back to.
pub fn base_resolved(dark: bool) -> Resolved {
    let th = crate::theme::selected(dark);
    let slot = crate::settings::get_str(setting_key(dark));
    resolve(
        slot.as_deref(),
        &BTreeMap::new(),
        &load_customs(),
        dark,
        th.term_bg,
        th.accent,
    )
}

fn write_overrides(dark: bool, map: &BTreeMap<String, String>) {
    let obj = map
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect::<serde_json::Map<String, Value>>();
    crate::settings::set(overrides_key(dark), Value::Object(obj));
}

/// Pure override-map update: an invalid key or value leaves the map alone,
/// a colour equal to the base slot drops the key (only differences are
/// stored), anything else stores the normalised lowercase hex.
pub fn update_overrides(
    base: &Palette20,
    current: &BTreeMap<String, String>,
    key: &str,
    hex: &str,
) -> BTreeMap<String, String> {
    let mut map = current.clone();
    if key_index(key).is_none() {
        return map;
    }
    let Some(c) = parse_hex(hex) else { return map };
    if base.get(key) == Some(c) {
        map.remove(key);
    } else {
        map.insert(key.to_string(), fmt_hex(c));
    }
    map
}

/// Commit one colour for a polarity slot (invalid input is ignored).
pub fn set_override(dark: bool, key: &str, hex: &str) {
    let base = base_resolved(dark);
    let map = update_overrides(&base.colors, &load_overrides(dark), key, hex);
    write_overrides(dark, &map);
}

/// Drop every override for a polarity slot.
pub fn clear_overrides(dark: bool) {
    write_overrides(dark, &BTreeMap::new());
}

/// Point a polarity slot at a base theme (`"default"`, a preset or a custom
/// theme) and clear that polarity's overrides.
pub fn select_base(dark: bool, name: &str) {
    crate::settings::set(setting_key(dark), Value::String(name.to_string()));
    clear_overrides(dark);
}

/// `"Dracula Custom 2"` → `"Dracula"`; a label that carries no ` Custom[ n]`
/// suffix is returned unchanged.
pub fn strip_custom_suffix(label: &str) -> String {
    let Some(rest) = label.strip_suffix(" Custom") else {
        if let Some((root, n)) = label.rsplit_once(" Custom ") {
            if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
                return root.to_string();
            }
        }
        return label.to_string();
    };
    rest.to_string()
}

/// Next free custom-theme name for a root label: `"<root> Custom"`, then
/// `"<root> Custom 2"`, `" Custom 3"`… skipping presets and names already
/// taken.
pub fn custom_theme_name(root: &str, existing: &[String]) -> String {
    let taken = |n: &str| find(n).is_some() || existing.iter().any(|e| e == n);
    let first = format!("{root} Custom");
    if !taken(&first) {
        return first;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{root} Custom {n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Save the slot's resolved 20 colours as a custom theme, select it for that
/// polarity and clear the overrides. Returns the new theme's name.
pub fn save_as_theme(dark: bool) -> String {
    let r = resolved(dark);
    let mut customs = load_customs();
    let root = strip_custom_suffix(&r.base_label);
    let name = custom_theme_name(
        &root,
        &customs.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
    );
    customs.push(CustomTheme {
        name: name.clone(),
        label: name.clone(),
        dark,
        colors: r.colors.keys_map(),
    });
    crate::settings::set(
        CUSTOM_THEMES_KEY,
        Value::Array(customs.iter().map(|c| c.to_value()).collect()),
    );
    crate::settings::set(setting_key(dark), Value::String(name.clone()));
    clear_overrides(dark);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_locates_every_preset_and_default_is_none() {
        for t in ALL {
            assert_eq!(find(t.name).map(|f| f.name), Some(t.name));
        }
        assert!(find("default").is_none());
        assert!(find("no-such-scheme").is_none());
        assert!(resolve_slot(None).is_none());
    }

    #[test]
    fn preset_names_are_unique() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn build_maps_ansi_slots_and_base_tones() {
        let p = build(Some(&DRACULA), (0, 0, 0));
        assert_eq!(p.colors.0[1], srgba(DRACULA.ansi[1]), "red");
        assert_eq!(p.colors.0[9], srgba(DRACULA.brights[1]), "bright red");
        assert_eq!(p.foreground, srgba(DRACULA.fg));
        assert_eq!(p.background, srgba(DRACULA.bg));
        // Extended entries stay stock (index 21 is pure blue in the cube).
        assert_eq!(p.colors.0[16..], ColorPalette::default().colors.0[16..]);
    }

    // ── the editable 20-colour palette ─────────────────────────────────

    const CHROME_BG: (u8, u8, u8) = (32, 30, 29);
    const CHROME_ACCENT: (u8, u8, u8) = (10, 132, 255);

    fn no_overrides() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Field-by-field palette equality (wezterm's ColorPalette is not
    /// guaranteed to expose PartialEq, and the four fields are what paint).
    fn assert_same_palette(a: &ColorPalette, b: &ColorPalette) {
        assert_eq!(a.colors.0[..], b.colors.0[..], "ansi table");
        assert_eq!(a.foreground, b.foreground, "foreground");
        assert_eq!(a.background, b.background, "background");
        assert_eq!(a.cursor_bg, b.cursor_bg, "cursor_bg");
        assert_eq!(a.cursor_border, b.cursor_border, "cursor_border");
        assert_eq!(a.cursor_fg, b.cursor_fg, "cursor_fg");
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        assert_eq!(parse_hex("#0a84ff"), Some((0x0a, 0x84, 0xff)));
        assert_eq!(parse_hex("#0A84FF"), Some((0x0a, 0x84, 0xff)), "case-insensitive");
        assert_eq!(fmt_hex((0x0a, 0x84, 0xff)), "#0a84ff", "lowercase out");
        assert_eq!(fmt_hex(parse_hex("#0A84FF").unwrap()), "#0a84ff");
        assert_eq!(fmt_hex(parse_hex(&fmt_hex((1, 2, 3))).unwrap()), "#010203");
        for bad in ["0a84ff", "#0a84f", "#0a84fff", "#0a84fg", "#", ""] {
            assert!(parse_hex(bad).is_none(), "{bad} must be rejected");
        }
    }

    #[test]
    fn keys_are_twenty_unique_and_label_aligned() {
        assert_eq!(KEYS.len(), 20);
        assert_eq!(LABELS.len(), 20);
        let mut ks: Vec<&str> = KEYS.to_vec();
        ks.sort_unstable();
        let n = ks.len();
        ks.dedup();
        assert_eq!(ks.len(), n, "duplicate colour key");
        assert_eq!(KEYS[0], "bg");
        assert_eq!(KEYS[3], "sel");
        assert_eq!(KEYS[19], "ansi15");
        for (i, k) in KEYS.iter().enumerate() {
            assert_eq!(key_index(k), Some(i));
            let mut p = Palette20::default();
            p.set_at(i, (1, 2, 3));
            assert_eq!(p.get_at(i), (1, 2, 3));
            assert_eq!(p.get(k), Some((1, 2, 3)));
            assert_eq!(p.hex(k).as_deref(), Some("#010203"));
        }
        assert_eq!(key_index("nope"), None);
        assert_eq!(Palette20::default().get("nope"), None);
    }

    #[test]
    fn uncustomised_resolution_equals_build_for_every_preset() {
        for t in ALL {
            let r = resolve(Some(t.name), &no_overrides(), &[], t.dark, CHROME_BG, CHROME_ACCENT);
            assert_eq!(r.base_key, t.name);
            assert_eq!(r.base_label, t.label);
            assert!(!r.is_modified());
            assert_eq!(r.colors.fg, t.fg, "{} fg", t.name);
            assert_eq!(r.colors.bg, t.bg, "{} bg", t.name);
            for i in 0..8 {
                assert_eq!(r.colors.ansi[i], t.ansi[i], "{} ansi{i}", t.name);
                assert_eq!(r.colors.ansi[i + 8], t.brights[i], "{} ansi{}", t.name, i + 8);
            }
            assert_same_palette(&r.to_color_palette(), &build(Some(t), CHROME_BG));
        }
    }

    #[test]
    fn uncustomised_resolution_equals_build_for_the_adaptive_default() {
        for slot in [None, Some("default"), Some("no-such-theme")] {
            let r = resolve(slot, &no_overrides(), &[], true, CHROME_BG, CHROME_ACCENT);
            assert_eq!(r.base_key, "default", "{slot:?} falls back to the adaptive default");
            assert_same_palette(&r.to_color_palette(), &build(None, CHROME_BG));
        }
        // The derived cursor/selection are today's painted colours.
        let r = resolve(None, &no_overrides(), &[], true, CHROME_BG, CHROME_ACCENT);
        assert_eq!(r.colors.cursor, r.colors.fg, "today's cursor is the foreground");
        assert_eq!(
            r.colors.sel,
            CHROME_ACCENT,
            "raw accent — the renderer composites it at 30%"
        );
        assert_eq!(
            blend(CHROME_BG, CHROME_ACCENT, 0.30),
            blend(r.colors.bg, r.colors.sel, 0.30),
            "the composite the preview paints matches the pane's"
        );
        assert!(!r.cursor_explicit && !r.sel_explicit);
    }

    #[test]
    fn overrides_layer_on_top_and_equal_to_base_is_dropped() {
        let mut ov = no_overrides();
        ov.insert("fg".into(), "#112233".into());
        ov.insert("ansi3".into(), "#445566".into());
        let customs: Vec<CustomTheme> = vec![];
        let r = resolve(Some("dracula"), &ov, &customs, true, CHROME_BG, CHROME_ACCENT);
        assert_eq!(r.colors.fg, (0x11, 0x22, 0x33));
        assert_eq!(r.colors.ansi[3], (0x44, 0x55, 0x66));
        assert_eq!(r.colors.ansi[4], DRACULA.ansi[4], "untouched slots keep the base");
        assert_eq!(r.modified(), 2);
        assert_eq!(r.overrides["fg"], "#112233");

        // Setting a colour back to its base value removes the key.
        let base = base_of(Some("dracula"), true);
        let cur = update_overrides(&base, &no_overrides(), "fg", &fmt_hex(DRACULA.fg));
        assert!(cur.is_empty(), "equal-to-base is not stored");
        let cur = update_overrides(&base, &cur, "fg", "#abcdef");
        assert_eq!(cur["fg"], "#abcdef");
        let cur = update_overrides(&base, &cur, "fg", "#ABCDEF");
        assert_eq!(cur["fg"], "#abcdef", "stored lowercase");
        let cur = update_overrides(&base, &cur, "fg", "not-a-colour");
        assert_eq!(cur["fg"], "#abcdef", "invalid input leaves the map alone");
        let cur = update_overrides(&base, &cur, "nope", "#ffffff");
        assert_eq!(cur.len(), 1, "unknown key is ignored");
    }

    /// The base slot of a resolution — the same pure path with no overrides.
    fn base_of(slot: Option<&str>, dark: bool) -> Palette20 {
        resolve(slot, &no_overrides(), &[], dark, CHROME_BG, CHROME_ACCENT).colors
    }

    #[test]
    fn explicit_cursor_and_selection_ride_the_palette() {
        let mut ov = no_overrides();
        ov.insert("cursor".into(), "#ff0000".into());
        ov.insert("sel".into(), "#00ff00".into());
        let r = resolve(Some("dracula"), &ov, &[], true, CHROME_BG, CHROME_ACCENT);
        assert!(r.cursor_explicit && r.sel_explicit);
        let p = r.to_color_palette();
        assert_eq!(tuple_u8(p.cursor_bg), (0xff, 0x00, 0x00));
        assert_eq!(tuple_u8(p.cursor_border), (0xff, 0x00, 0x00));
        assert_eq!(tuple_u8(p.cursor_fg), DRACULA.bg, "cursor ink stays the pane ground");
    }

    #[test]
    fn custom_theme_lookup_resolves_and_records_explicitness() {
        let customs = vec![CustomTheme {
            name: "my-theme".into(),
            label: "My Theme".into(),
            dark: true,
            colors: Palette20 {
                bg: (1, 1, 1),
                fg: (2, 2, 2),
                cursor: (3, 3, 3),
                sel: (4, 4, 4),
                ansi: [(9, 9, 9); 16],
            }
            .keys_map(),
        }];
        let r = resolve(Some("my-theme"), &no_overrides(), &customs, true, CHROME_BG, CHROME_ACCENT);
        assert_eq!(r.base_key, "my-theme");
        assert_eq!(r.base_label, "My Theme");
        assert_eq!(r.colors.bg, (1, 1, 1));
        assert_eq!(r.colors.cursor, (3, 3, 3));
        assert_eq!(r.colors.ansi[15], (9, 9, 9));
        assert!(r.cursor_explicit && r.sel_explicit, "a custom theme states both");

        // A custom theme belonging to the other polarity is not selected.
        let r = resolve(Some("my-theme"), &no_overrides(), &customs, false, CHROME_BG, CHROME_ACCENT);
        assert_eq!(r.base_key, "default");
    }

    #[test]
    fn save_as_theme_naming_includes_collision_suffixes() {
        assert_eq!(strip_custom_suffix("Dracula"), "Dracula");
        assert_eq!(strip_custom_suffix("Dracula Custom"), "Dracula");
        assert_eq!(strip_custom_suffix("Dracula Custom 2"), "Dracula");
        assert_eq!(strip_custom_suffix("Dracula Custom x"), "Dracula Custom x");
        assert_eq!(custom_theme_name("Dracula", &[]), "Dracula Custom");
        let existing = vec!["Dracula Custom".to_string()];
        assert_eq!(custom_theme_name("Dracula", &existing), "Dracula Custom 2");
        let existing = vec![
            "Dracula Custom".to_string(),
            "Dracula Custom 2".to_string(),
            "Dracula Custom 3".to_string(),
        ];
        assert_eq!(custom_theme_name("Dracula", &existing), "Dracula Custom 4");
        // A preset name can never be reused.
        assert_eq!(custom_theme_name("Dracula", &["x".into()]), "Dracula Custom");
        assert_eq!(custom_theme_name("Dracula Custom", &[]), "Dracula Custom Custom");
    }

    #[test]
    fn malformed_settings_values_are_ignored() {
        for bad in ["not json", "[1,2,3]", "\"str\"", "null"] {
            assert!(parse_overrides(Some(bad)).is_empty(), "overrides: {bad}");
            assert!(parse_customs(Some(bad)).is_empty(), "customs: {bad}");
        }
        assert!(parse_overrides(None).is_empty());
        assert!(parse_customs(None).is_empty());

        let ov = parse_overrides(Some(
            r##"{"fg":"#AABBCC","nope":"#ffffff","bg":"#123","sel":42}"##,
        ));
        assert_eq!(ov.len(), 1, "only the valid known key survives");
        assert_eq!(ov["fg"], "#aabbcc", "normalised lowercase");

        let cs = parse_customs(Some(
            r##"[{"name":"a","label":"A","dark":true,"colors":{"bg":"#000000","nope":"#ffffff","fg":"zzz"}},
               {"label":"no name"}, "junk", {"name":"dracula","colors":{}}, {"name":"a","colors":{}}]"##,
        ));
        assert_eq!(cs.len(), 1, "unnamed, non-object, preset-clashing and duplicate entries skipped");
        assert_eq!(cs[0].name, "a");
        assert_eq!(cs[0].dark, true);
        assert_eq!(cs[0].colors.len(), 1);
        assert_eq!(cs[0].colors["bg"], "#000000");
        // A custom theme with junk colours still resolves (falls back per slot).
        let r = resolve(Some("a"), &no_overrides(), &cs, true, CHROME_BG, CHROME_ACCENT);
        assert_eq!(r.colors.bg, (0, 0, 0));
        assert_eq!(r.colors.fg, tuple_u8(ColorPalette::default().foreground));
    }

    #[test]
    fn custom_theme_round_trips_through_its_persisted_shape() {
        let t = CustomTheme {
            name: "n".into(),
            label: "N".into(),
            dark: false,
            colors: Palette20 {
                bg: (10, 11, 12),
                fg: (20, 21, 22),
                cursor: (30, 31, 32),
                sel: (40, 41, 42),
                ansi: [(1, 2, 3); 16],
            }
            .keys_map(),
        };
        let raw = serde_json::to_string(&vec![t.to_value()]).unwrap();
        let back = parse_customs(Some(&raw));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].colors, t.colors);
        assert_eq!(back[0].name, "n");
        assert_eq!(back[0].dark, false);
    }

    #[test]
    fn preset_resolution_sets_the_cursor_fields_like_build() {
        // Uncustised presets carry today's wezterm cursor fields (OSC reports).
        let r = resolve(Some("dracula"), &no_overrides(), &[], true, CHROME_BG, CHROME_ACCENT);
        let p = r.to_color_palette();
        assert_eq!(p.cursor_bg, srgba(DRACULA.fg));
        assert_eq!(p.cursor_fg, srgba(DRACULA.bg));
        // The adaptive default leaves them stock, exactly as `build` does.
        let r = resolve(None, &no_overrides(), &[], true, CHROME_BG, CHROME_ACCENT);
        let p = r.to_color_palette();
        assert_eq!(p.cursor_bg, ColorPalette::default().cursor_bg);
    }

    #[test]
    fn adaptive_default_keeps_stock_ansi_on_chrome_ground() {
        let stock = ColorPalette::default();
        let p = build(None, (32, 30, 29));
        assert_eq!(p.background, srgba((32, 30, 29)));
        assert_eq!(p.foreground, stock.foreground);
        assert_eq!(p.colors.0[..], stock.colors.0[..]);
    }
}
