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
