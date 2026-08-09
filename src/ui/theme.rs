//! shadcn's design-token system, ported from the base-vega style's CSS
//! variables (ui.shadcn.com, neutral palette). Every color token from
//! `:root` / `.dark` in shadcn's globals.css maps to a field here; values are
//! sRGB conversions of the original oklch values.
//!
//! The active theme lives in the gpui `Global` store: read it in a component
//! with `Theme::of(cx)`, swap it with `cx.set_global(Theme::dark())`.

use gpui::{App, Global, Hsla, Pixels, Rgba, SharedString, px, rgb, rgba};

use crate::ui::assets::IconLibrary;

/// Design tokens for one color scheme (shadcn `:root` or `.dark`).
#[derive(Clone, Debug)]
pub struct Theme {
    /// True when this is the dark scheme; components use it where shadcn
    /// styles carry `dark:` overrides that aren't pure token swaps.
    pub dark: bool,

    pub background: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub card_foreground: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub destructive: Hsla,
    pub destructive_foreground: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub ring: Hsla,

    /// Chart series colors (shadcn `--chart-1`..`--chart-5`).
    pub chart: [Hsla; 5],

    /// Base radius (shadcn `--radius: 0.625rem` = 10px). The sm/md/lg/xl
    /// scale derives from it, mirroring shadcn's calc() chain.
    pub radius: Pixels,

    /// Body font family (shadcn `--font-sans`); `None` uses gpui's default.
    pub font_sans: Option<SharedString>,
    /// Heading font family (shadcn `--font-heading`); falls back to
    /// [`Self::font_sans`].
    pub font_heading: Option<SharedString>,
    /// The icon set components draw from.
    pub icons: IconLibrary,
}

impl Global for Theme {}

impl Theme {
    pub fn of(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    /// The heading font family, falling back to the body font.
    pub fn heading_font(&self) -> Option<SharedString> {
        self.font_heading.clone().or_else(|| self.font_sans.clone())
    }

    /// shadcn `--radius-sm` = radius × 0.6
    pub fn radius_sm(&self) -> Pixels {
        self.radius * 0.6
    }

    /// shadcn `--radius-md` = radius × 0.8
    pub fn radius_md(&self) -> Pixels {
        self.radius * 0.8
    }

    /// shadcn `--radius-lg` = radius × 1.0
    pub fn radius_lg(&self) -> Pixels {
        self.radius
    }

    /// shadcn `--radius-xl` = radius × 1.4
    pub fn radius_xl(&self) -> Pixels {
        self.radius * 1.4
    }

    /// shadcn `:root` (light), neutral palette.
    pub fn light() -> Self {
        Self {
            dark: false,
            background: rgb(0xffffff).into(),
            foreground: rgb(0x000000).into(),
            card: rgb(0xffffff).into(),
            card_foreground: rgb(0x000000).into(),
            popover: rgb(0xffffff).into(),
            popover_foreground: rgb(0x000000).into(),
            primary: rgb(0x000000).into(),
            primary_foreground: rgb(0xfafafa).into(),
            secondary: rgb(0xf5f5f5).into(),
            secondary_foreground: rgb(0x171717).into(),
            muted: rgb(0xf5f5f5).into(),
            muted_foreground: rgb(0x737373).into(),
            accent: rgb(0xf5f5f5).into(),
            accent_foreground: rgb(0x171717).into(),
            destructive: rgb(0xe7000b).into(),
            destructive_foreground: rgb(0xfcf3f3).into(),
            border: rgb(0xe5e5e5).into(),
            input: rgb(0xe5e5e5).into(),
            ring: rgb(0xa1a1a1).into(),
            chart: [
                oklch(0.809, 0.105, 251.8),
                oklch(0.623, 0.214, 259.8),
                oklch(0.546, 0.245, 262.9),
                oklch(0.488, 0.243, 264.4),
                oklch(0.424, 0.199, 265.6),
            ],
            radius: px(10.),
            font_sans: None,
            font_heading: None,
            icons: IconLibrary::Lucide,
        }
    }

    /// shadcn `.dark`, neutral palette. Border and input are translucent
    /// white, exactly as in the source (`oklch(1 0 0 / 10%)` / `15%`).
    pub fn dark() -> Self {
        Self {
            dark: true,
            background: rgb(0x0a0a0a).into(),
            foreground: rgb(0xfafafa).into(),
            card: rgb(0x171717).into(),
            card_foreground: rgb(0xfafafa).into(),
            popover: rgb(0x171717).into(),
            popover_foreground: rgb(0xfafafa).into(),
            primary: rgb(0xe5e5e5).into(),
            primary_foreground: rgb(0x171717).into(),
            secondary: rgb(0x262626).into(),
            secondary_foreground: rgb(0xfafafa).into(),
            muted: rgb(0x262626).into(),
            muted_foreground: rgb(0xa1a1a1).into(),
            accent: rgb(0x404040).into(),
            accent_foreground: rgb(0xfafafa).into(),
            destructive: rgb(0xff6467).into(),
            destructive_foreground: rgb(0xdf2225).into(),
            border: rgba(0xffffff1a).into(),
            input: rgba(0xffffff26).into(),
            ring: rgb(0x737373).into(),
            chart: [
                oklch(0.809, 0.105, 251.8),
                oklch(0.623, 0.214, 259.8),
                oklch(0.546, 0.245, 262.9),
                oklch(0.488, 0.243, 264.4),
                oklch(0.424, 0.199, 265.6),
            ],
            radius: px(10.),
            font_sans: None,
            font_heading: None,
            icons: IconLibrary::Lucide,
        }
    }
}

/// shadcn's `color/NN` opacity modifier: the token color at the given alpha.
pub fn alpha(mut color: Hsla, a: f32) -> Hsla {
    color.a = a;
    color
}

/// Convert an oklch color (shadcn's native token space; hue in degrees) to a
/// gpui color, clamped into sRGB.
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Hsla {
    let h = h_deg.to_radians();
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let (l3, m3, s3) = (l_.powi(3), m_.powi(3), s_.powi(3));
    let r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
    let g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
    let b = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;
    let enc = |x: f32| {
        let x = x.clamp(0., 1.);
        if x <= 0.0031308 {
            12.92 * x
        } else {
            1.055 * x.powf(1. / 2.4) - 0.055
        }
    };
    Rgba {
        r: enc(r),
        g: enc(g),
        b: enc(b),
        a: 1.,
    }
    .into()
}

/// The gray family behind the neutral tokens — shadcn's "base color" choice.
/// Each tints the whole background/border/muted ramp toward its hue.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BaseColor {
    #[default]
    Neutral,
    Stone,
    Zinc,
    Gray,
    Slate,
}

impl BaseColor {
    pub const ALL: [BaseColor; 5] = [
        BaseColor::Neutral,
        BaseColor::Stone,
        BaseColor::Zinc,
        BaseColor::Gray,
        BaseColor::Slate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BaseColor::Neutral => "neutral",
            BaseColor::Stone => "stone",
            BaseColor::Zinc => "zinc",
            BaseColor::Gray => "gray",
            BaseColor::Slate => "slate",
        }
    }

    /// The family's (chroma, hue) tint, approximating the tailwind ramps
    /// (e.g. slate-500 ≈ oklch(0.554 0.041 257)).
    fn tint(self) -> (f32, f32) {
        match self {
            BaseColor::Neutral => (0., 0.),
            BaseColor::Stone => (0.005, 58.),
            BaseColor::Zinc => (0.010, 286.),
            BaseColor::Gray => (0.020, 264.),
            BaseColor::Slate => (0.038, 257.),
        }
    }
}

impl Theme {
    /// Parse a shadcn theme stylesheet — the `:root { --token: … }` /
    /// `.dark { … }` CSS you copy from shadcn.com/design or a `globals.css` —
    /// into a (light, dark) theme pair. Tokens the library doesn't use
    /// (charts, sidebar) are ignored; missing tokens keep their stock neutral
    /// values. Returns `None` when no `:root` block with at least one known
    /// token is found.
    pub fn from_shadcn_css(css: &str) -> Option<(Theme, Theme)> {
        let root = css_block(css, ":root")?;
        let (light, applied) = themed_from_block(Theme::light(), root);
        if applied == 0 {
            return None;
        }
        let dark = match css_block(css, ".dark") {
            Some(block) => themed_from_block(Theme::dark(), block).0,
            None => Theme::dark(),
        };
        Some((light, dark))
    }
}

/// The body of the first `selector { … }` block in `css`.
fn css_block<'a>(css: &'a str, selector: &str) -> Option<&'a str> {
    let start = css.find(selector)?;
    let rest = &css[start + selector.len()..];
    let open = rest.find('{')?;
    let body = &rest[open + 1..];
    Some(&body[..body.find('}')?])
}

/// Apply every recognized `--token: value` declaration in `block` onto
/// `theme`; returns the theme and how many declarations applied.
fn themed_from_block(mut theme: Theme, block: &str) -> (Theme, usize) {
    let mut applied = 0;
    for decl in block.split(';') {
        let Some((name, value)) = decl.split_once(':') else {
            continue;
        };
        let name = name.trim().trim_start_matches("--");
        let value = value.trim();
        if name == "radius" {
            if let Some(radius) = parse_css_length(value) {
                theme.radius = radius;
                applied += 1;
            }
            continue;
        }
        if name == "font-sans" || name == "font-heading" {
            if let Some(family) = parse_css_font(value) {
                if name == "font-sans" {
                    theme.font_sans = Some(family);
                } else {
                    theme.font_heading = Some(family);
                }
                applied += 1;
            }
            continue;
        }
        if let Some(chart_index) = name
            .strip_prefix("chart-")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| (1..=5).contains(n))
        {
            if let Some(color) = parse_css_color(value) {
                theme.chart[chart_index - 1] = color;
                applied += 1;
            }
            continue;
        }
        let Some(slot) = token_slot(&mut theme, name) else {
            continue;
        };
        if let Some(color) = parse_css_color(value) {
            *slot = color;
            applied += 1;
        }
    }
    (theme, applied)
}

fn token_slot<'a>(theme: &'a mut Theme, name: &str) -> Option<&'a mut Hsla> {
    Some(match name {
        "background" => &mut theme.background,
        "foreground" => &mut theme.foreground,
        "card" => &mut theme.card,
        "card-foreground" => &mut theme.card_foreground,
        "popover" => &mut theme.popover,
        "popover-foreground" => &mut theme.popover_foreground,
        "primary" => &mut theme.primary,
        "primary-foreground" => &mut theme.primary_foreground,
        "secondary" => &mut theme.secondary,
        "secondary-foreground" => &mut theme.secondary_foreground,
        "muted" => &mut theme.muted,
        "muted-foreground" => &mut theme.muted_foreground,
        "accent" => &mut theme.accent,
        "accent-foreground" => &mut theme.accent_foreground,
        "destructive" => &mut theme.destructive,
        "destructive-foreground" => &mut theme.destructive_foreground,
        "border" => &mut theme.border,
        "input" => &mut theme.input,
        "ring" => &mut theme.ring,
        _ => return None,
    })
}

/// `0`, `0.625rem`, or `10px` → pixels.
fn parse_css_length(value: &str) -> Option<Pixels> {
    if let Some(rem) = value.strip_suffix("rem") {
        return Some(px(rem.trim().parse::<f32>().ok()? * 16.));
    }
    if let Some(pixels) = value.strip_suffix("px") {
        return Some(px(pixels.trim().parse::<f32>().ok()?));
    }
    Some(px(value.parse::<f32>().ok()?))
}

/// A number that may carry a `%` suffix, normalized so `50%` → 0.5.
fn parse_css_number(value: &str) -> Option<f32> {
    match value.strip_suffix('%') {
        Some(pct) => Some(pct.trim().parse::<f32>().ok()? / 100.),
        None => value.parse().ok(),
    }
}

/// The color syntaxes shadcn themes have shipped with: `oklch(l c h [/ a])`
/// (v4), `hsl(h s% l%)` and the bare `h s% l%` triple (v3), and hex.
fn parse_css_color(value: &str) -> Option<Hsla> {
    let value = value.trim();
    if let Some(inner) = value
        .strip_prefix("oklch(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let (color, a) = split_css_alpha(inner);
        let parts: Vec<_> = color.split_whitespace().collect();
        let [l, c, h] = parts.as_slice() else {
            return None;
        };
        let mut color = oklch(
            parse_css_number(l)?,
            parse_css_number(c)?,
            parse_css_number(h)?,
        );
        color.a = a?;
        return Some(color);
    }
    if let Some(hex) = value.strip_prefix('#') {
        let expand = |v: &str| u32::from_str_radix(v, 16).ok();
        return match hex.len() {
            3 => {
                let v = expand(hex)?;
                let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                Some(rgb((r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11)).into())
            }
            6 => Some(rgb(expand(hex)?).into()),
            8 => Some(rgba(expand(hex)?).into()),
            _ => None,
        };
    }
    let inner = value
        .strip_prefix("hsl(")
        .and_then(|v| v.strip_suffix(')'))
        .unwrap_or(value);
    let (color, a) = split_css_alpha(inner);
    let parts: Vec<_> = color.split_whitespace().collect();
    let [h, s, l] = parts.as_slice() else {
        return None;
    };
    // Bare triples must look like the hsl shorthand ("240 10% 3.9%") to
    // avoid parsing arbitrary values as colors.
    if !s.ends_with('%') || !l.ends_with('%') {
        return None;
    }
    Some(Hsla {
        h: parse_css_number(h)? / 360.,
        s: parse_css_number(s)?,
        l: parse_css_number(l)?,
        a: a?,
    })
}

/// The first concrete family in a CSS font stack: `'Outfit', sans-serif` →
/// `Outfit`. Generic families and var() references are skipped.
fn parse_css_font(value: &str) -> Option<SharedString> {
    value
        .split(',')
        .map(|family| family.trim().trim_matches(|c| c == '\'' || c == '"'))
        .find(|family| {
            !family.is_empty()
                && !family.starts_with("var(")
                && ![
                    "sans-serif",
                    "serif",
                    "monospace",
                    "system-ui",
                    "ui-sans-serif",
                    "ui-serif",
                    "ui-monospace",
                    "cursive",
                    "fantasy",
                ]
                .contains(family)
        })
        .map(|family| SharedString::from(family.to_owned()))
}

/// Split a CSS color body on its `/ alpha` tail. Returns the color part and
/// the alpha (1.0 when absent, `None` when present but malformed).
fn split_css_alpha(inner: &str) -> (&str, Option<f32>) {
    match inner.split_once('/') {
        Some((color, alpha)) => (color.trim(), parse_css_number(alpha.trim())),
        None => (inner.trim(), Some(1.)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(color: Hsla, expected: Hsla) {
        let (a, b): (Rgba, Rgba) = (color.into(), expected.into());
        for (x, y) in [(a.r, b.r), (a.g, b.g), (a.b, b.b)] {
            assert!((x - y).abs() < 0.01, "{color:?} != {expected:?}");
        }
    }

    /// The oklch conversion must reproduce the hand-converted token values
    /// the themes are built from.
    #[test]
    fn oklch_matches_shadcn_tokens() {
        assert_close(oklch(0.577, 0.245, 27.325), rgb(0xe7000b).into()); // destructive
        assert_close(oklch(0.97, 0., 0.), rgb(0xf5f5f5).into()); // secondary
        assert_close(oklch(0.145, 0., 0.), rgb(0x0a0a0a).into()); // dark background
        assert_close(oklch(1., 0., 0.), rgb(0xffffff).into());
    }

    #[test]
    fn neutral_base_is_the_stock_theme() {
        for dark in [false, true] {
            let stock = if dark { Theme::dark() } else { Theme::light() };
            let based = Theme::with_base(BaseColor::Neutral, dark);
            assert_eq!(based.background, stock.background);
            assert_eq!(based.primary, stock.primary);
            assert_eq!(based.dark, stock.dark);
        }
    }

    /// A trimmed copy of a real theme copied from shadcn.com/design
    /// (including its single-line `.dark` tail).
    const DESIGN_CSS: &str = r#"
:root {
  --background: oklch(1 0 0);
  --foreground: oklch(0.147 0.004 49.3);
  --primary: oklch(0.553 0.195 38.402);
  --primary-foreground: oklch(0.98 0.016 73.684);
  --muted-foreground: oklch(0.547 0.021 43.1);
  --chart-1: oklch(0.837 0.128 66.29);
  --radius: 0;
  --sidebar: oklch(0.986 0.002 67.8);
}

.dark {
  --background: oklch(0.147 0.004 49.3);
  --primary: oklch(0.47 0.157 37.304);
  --border: oklch(1 0 0 / 10%);  --ring: oklch(0.547 0.021 43.1);  --sidebar-ring: oklch(0.547 0.021 43.1);}
"#;

    #[test]
    fn imports_design_css() {
        let (light, dark) = Theme::from_shadcn_css(DESIGN_CSS).expect("should parse");
        assert_close(light.primary, oklch(0.553, 0.195, 38.402));
        assert_close(light.foreground, oklch(0.147, 0.004, 49.3));
        assert_eq!(light.radius, px(0.));
        // Unlisted tokens keep their stock values.
        assert_eq!(light.secondary, Theme::light().secondary);
        assert_close(dark.background, oklch(0.147, 0.004, 49.3));
        assert_close(dark.primary, oklch(0.47, 0.157, 37.304));
        assert!((dark.border.a - 0.1).abs() < 0.001, "translucent border");
        // Dark blocks don't inherit light values for unlisted tokens.
        assert_eq!(dark.secondary, Theme::dark().secondary);
    }

    #[test]
    fn imports_v3_hsl_and_hex_colors() {
        let css = ":root { --background: 20 14.3% 4.1%; --primary: hsl(24 90% 50%); --ring: #e7000b; --radius: 0.625rem; }";
        let (light, _) = Theme::from_shadcn_css(css).expect("should parse");
        assert!((light.background.h - 20. / 360.).abs() < 0.001);
        assert!((light.background.l - 0.041).abs() < 0.001);
        assert!((light.primary.s - 0.9).abs() < 0.001);
        assert_eq!(light.ring, rgb(0xe7000b).into());
        assert_eq!(light.radius, px(10.));
    }

    #[test]
    fn imports_font_declarations() {
        let css = ":root { --primary: #000000; --font-sans: 'Outfit', ui-sans-serif, sans-serif; --font-heading: var(--font-raleway), Raleway, serif; }";
        let (light, _) = Theme::from_shadcn_css(css).expect("should parse");
        assert_eq!(light.font_sans.as_deref(), Some("Outfit"));
        assert_eq!(light.font_heading.as_deref(), Some("Raleway"));
    }

    #[test]
    fn rejects_css_without_tokens() {
        assert!(Theme::from_shadcn_css("body { margin: 0 }").is_none());
        assert!(Theme::from_shadcn_css(":root { --thing: 12; }").is_none());
        assert!(Theme::from_shadcn_css("not css at all").is_none());
    }

    /// Tinted bases keep the neutral lightness structure: light backgrounds
    /// stay white, dark backgrounds stay near-black.
    #[test]
    fn tinted_bases_preserve_lightness() {
        for base in BaseColor::ALL {
            let light = Theme::with_base(base, false);
            assert!(light.background.l > 0.95, "{base:?} light background");
            assert!(light.foreground.l < 0.25, "{base:?} light foreground");
            let dark = Theme::with_base(base, true);
            assert!(dark.background.l < 0.25, "{base:?} dark background");
            assert!(dark.foreground.l > 0.9, "{base:?} dark foreground");
        }
    }
}

impl Theme {
    /// The neutral theme re-tinted toward a base gray family, like choosing a
    /// base color in shadcn's init/create flow. `BaseColor::Neutral` returns
    /// [`Theme::light`]/[`Theme::dark`] exactly.
    pub fn with_base(base: BaseColor, dark: bool) -> Self {
        let neutral = if dark { Self::dark() } else { Self::light() };
        if base == BaseColor::Neutral {
            return neutral;
        }
        let (chroma, hue) = base.tint();
        // Tailwind's gray ramps hold chroma roughly constant except near
        // white, where it fades out.
        let shade = |l: f32| oklch(l, chroma * ((1. - l) * 8.).clamp(0., 1.), hue);
        if dark {
            Self {
                background: shade(0.141),
                foreground: shade(0.985),
                card: shade(0.205),
                card_foreground: shade(0.985),
                popover: shade(0.205),
                popover_foreground: shade(0.985),
                primary: shade(0.922),
                primary_foreground: shade(0.205),
                secondary: shade(0.269),
                secondary_foreground: shade(0.985),
                muted: shade(0.269),
                muted_foreground: shade(0.708),
                accent: shade(0.371),
                accent_foreground: shade(0.985),
                ring: shade(0.556),
                ..neutral
            }
        } else {
            Self {
                background: shade(1.),
                foreground: shade(0.141),
                card: shade(1.),
                card_foreground: shade(0.141),
                popover: shade(1.),
                popover_foreground: shade(0.141),
                primary: shade(0.205),
                primary_foreground: shade(0.985),
                secondary: shade(0.97),
                secondary_foreground: shade(0.205),
                muted: shade(0.97),
                muted_foreground: shade(0.556),
                accent: shade(0.97),
                accent_foreground: shade(0.205),
                destructive: neutral.destructive,
                destructive_foreground: neutral.destructive_foreground,
                border: shade(0.922),
                input: shade(0.922),
                ring: shade(0.708),
                ..neutral
            }
        }
    }
}

// ---------------------------------------------------------------------------
// pwrde local additions (not in rcn). Kept as one contiguous tail block so
// `rcn diff theme` shows upstream drift above and only this block as local.

impl Theme {
    /// Map pwrde chrome tokens (sRGB u8 triples) onto the shadcn token set so
    /// vendored components pick up the live appearance theme.
    pub fn from_chrome(th: &crate::theme::Theme) -> Self {
        let mut t = if th.dark { Theme::dark() } else { Theme::light() };
        t.dark = th.dark;
        t.background = srgb(th.gradient_to);
        t.foreground = srgb(th.ink);
        t.card = srgb(th.card);
        t.card_foreground = srgb(th.ink);
        t.popover = srgb(th.card);
        t.popover_foreground = srgb(th.ink);
        t.primary = srgb(th.accent);
        t.primary_foreground = if crate::theme::is_dark_color(th.accent) {
            srgb((0xff, 0xff, 0xff))
        } else {
            srgb((0x17, 0x17, 0x17))
        };
        let muted_surface = mix(th.card, th.ink, 0.08);
        t.secondary = srgb(muted_surface);
        t.muted = srgb(muted_surface);
        t.accent = srgb(muted_surface);
        t.secondary_foreground = srgb(th.ink);
        t.accent_foreground = srgb(th.ink);
        t.muted_foreground = srgb(th.ink_dim);
        let line = mix(th.card, th.ink, 0.16);
        t.border = srgb(line);
        t.input = srgb(line);
        t.ring = srgb(th.ink_dim);
        // destructive, destructive_foreground, chart, radius, fonts, icons
        // stay on the light/dark base.
        t
    }
}

/// Convert an opaque sRGB triple to a gpui [`Hsla`].
fn srgb((r, g, b): (u8, u8, u8)) -> Hsla {
    rgb(((r as u32) << 16) | ((g as u32) << 8) | (b as u32)).into()
}

/// Per-channel linear blend, `f` of the way from `a` to `b`.
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), f: f32) -> (u8, u8, u8) {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

#[cfg(test)]
mod pwrde_tests {
    use super::*;

    fn assert_close(color: Hsla, expected: Hsla) {
        let (a, b): (Rgba, Rgba) = (color.into(), expected.into());
        for (x, y) in [(a.r, b.r), (a.g, b.g), (a.b, b.b)] {
            assert!((x - y).abs() < 0.01, "{color:?} != {expected:?}");
        }
    }

    #[test]
    fn from_chrome_polarity_follows_dark() {
        let light = Theme::from_chrome(&crate::theme::ARC_LIGHT);
        let dark = Theme::from_chrome(&crate::theme::MIDNIGHT);
        assert!(!light.dark);
        assert!(dark.dark);
    }

    #[test]
    fn from_chrome_maps_card_and_primary() {
        let midnight = Theme::from_chrome(&crate::theme::MIDNIGHT);
        assert_close(midnight.card, srgb(crate::theme::MIDNIGHT.card));
        assert_close(midnight.primary, srgb(crate::theme::MIDNIGHT.accent));
        assert_close(midnight.background, srgb(crate::theme::MIDNIGHT.gradient_to));
        assert_close(midnight.foreground, srgb(crate::theme::MIDNIGHT.ink));

        let arc = Theme::from_chrome(&crate::theme::ARC_LIGHT);
        assert_close(arc.card, srgb(crate::theme::ARC_LIGHT.card));
        assert_close(arc.primary, srgb(crate::theme::ARC_LIGHT.accent));
        assert_close(arc.background, srgb(crate::theme::ARC_LIGHT.gradient_to));
        assert_close(arc.foreground, srgb(crate::theme::ARC_LIGHT.ink));
    }
}
