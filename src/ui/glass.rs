//! Liquid-glass surface recipes for pwrde's overlay chrome.
//!
//! gpui has no backdrop blur, so "liquid glass" is faked with four cues: a
//! translucent gradient fill, a 1px white hairline rim, an inset top
//! highlight ([`BoxShadow`] with `inset: true`, offset y=1, blur 0), and a
//! deep outer drop shadow. Each constructor returns a pure [`Glass`] recipe
//! for one surface family (panel / bar / card / chat bubbles) in either
//! polarity; [`Glass::apply`] paints the recipe onto a `Div`.
//!
//! Values come from the GANTRY Workspace Claude Design mocks (dark + light).
//! Where a mock's big-surface alpha sits low (0.3-0.55) it is raised here:
//! with no blur available, terminal text behind the surface must not read
//! through.

use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, Corners, Div, Hsla, IntoElement, Pixels, RenderImage, Size, Styled, Background,
    BoxShadow, canvas, linear_color_stop, linear_gradient, point, px, rgba,
};

/// A "liquid glass" surface recipe: a translucent gradient fill, a hairline
/// rim color, and the shadow stack (outer drop shadow + inset top
/// highlight). Pure data — apply it with [`Glass::apply`].
#[derive(Debug, Clone)]
pub struct Glass {
    pub fill: Background,
    pub rim: Hsla,
    pub shadows: Vec<BoxShadow>,
}

/// Opaque white/black tints at a given alpha (the mock's rim/highlight ink).
fn white(a: f32) -> Hsla {
    Hsla { h: 0., s: 0., l: 1., a }
}

fn black(a: f32) -> Hsla {
    Hsla { h: 0., s: 0., l: 0., a }
}

/// Mix `c` toward white by `frac` (mock's "accent washed with white" lead
/// stop for the user bubble), keeping the hue.
fn washed(c: Hsla, frac: f32) -> Hsla {
    Hsla {
        h: c.h,
        s: c.s * (1. - frac),
        l: c.l + (1. - c.l) * frac,
        a: c.a,
    }
}

/// Deep outer drop shadow, offset straight down.
fn drop_shadow(y: f32, blur: f32, color: Hsla) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.),
        inset: false,
    }
}

/// 1px inset top highlight — the "light catching the top edge" cue.
fn top_highlight(alpha: f32) -> BoxShadow {
    BoxShadow {
        color: white(alpha),
        offset: point(px(0.), px(1.)),
        blur_radius: px(0.),
        spread_radius: px(0.),
        inset: true,
    }
}

/// Translucent gradient fill running top-to-bottom-ish at `angle` degrees.
fn gradient(angle: f32, from: Hsla, to: Hsla) -> Background {
    linear_gradient(
        angle,
        linear_color_stop(from, 0.),
        linear_color_stop(to, 1.),
    )
}

impl Glass {
    /// Big overlay surface: PR/tool panel + Flow chat sheet.
    pub fn panel(dark: bool) -> Self {
        if dark {
            Self {
                fill: gradient(
                    165.,
                    rgba(0x2c2c34e6).into(),
                    rgba(0x24242cdb).into(),
                ),
                rim: white(0.16),
                shadows: vec![
                    drop_shadow(18., 50., black(0.65)),
                    top_highlight(0.22),
                ],
            }
        } else {
            Self {
                fill: gradient(
                    165.,
                    rgba(0xffffffeb).into(),
                    rgba(0xffffffdb).into(),
                ),
                rim: white(0.55),
                shadows: vec![
                    drop_shadow(18., 50., rgba(0x00001e38).into()),
                    top_highlight(0.80),
                ],
            }
        }
    }

    /// Slim overlay surface: Flow composer bar + Flow chat-list rows.
    pub fn bar(dark: bool) -> Self {
        if dark {
            Self {
                fill: gradient(
                    165.,
                    rgba(0x2c2c34e6).into(),
                    rgba(0x24242cdb).into(),
                ),
                rim: white(0.16),
                shadows: vec![
                    drop_shadow(14., 40., black(0.55)),
                    top_highlight(0.14),
                ],
            }
        } else {
            Self {
                fill: gradient(
                    165.,
                    rgba(0xffffffe6).into(),
                    rgba(0xffffffd6).into(),
                ),
                rim: white(0.55),
                shadows: vec![
                    drop_shadow(12., 40., rgba(0x00001e38).into()),
                    top_highlight(0.70),
                ],
            }
        }
    }

    /// Small inset surface: PR comment / diff-file cards, Flow action cards.
    pub fn card(dark: bool) -> Self {
        if dark {
            Self {
                fill: gradient(
                    150.,
                    rgba(0xffffff1a).into(),
                    rgba(0xffffff0f).into(),
                ),
                rim: white(0.14),
                shadows: vec![
                    drop_shadow(2., 10., black(0.25)),
                    top_highlight(0.16),
                ],
            }
        } else {
            Self {
                fill: gradient(
                    150.,
                    rgba(0xffffff80).into(),
                    rgba(0xffffff2e).into(),
                ),
                rim: white(0.50),
                shadows: vec![
                    drop_shadow(2., 10., rgba(0x00001e12).into()),
                    top_highlight(0.90),
                ],
            }
        }
    }

    /// Chat transcript bubble for assistant replies (rimless).
    pub fn bubble_assistant(dark: bool) -> Self {
        if dark {
            Self {
                fill: gradient(
                    150.,
                    rgba(0xffffff24).into(),
                    rgba(0xffffff0f).into(),
                ),
                rim: white(0.),
                shadows: vec![
                    drop_shadow(2., 10., black(0.25)),
                    top_highlight(0.18),
                ],
            }
        } else {
            Self {
                fill: gradient(
                    150.,
                    rgba(0xffffff8c).into(),
                    rgba(0xffffff38).into(),
                ),
                rim: white(0.),
                shadows: vec![
                    drop_shadow(2., 10., rgba(0x00001e1a).into()),
                    top_highlight(0.90),
                ],
            }
        }
    }

    /// Chat transcript bubble for user messages: the accent, washed with
    /// white at the top and fading to the accent itself (rimless).
    pub fn bubble_user(dark: bool, accent: Hsla) -> Self {
        if dark {
            Self {
                fill: gradient(160., washed(accent, 0.20), Hsla { a: 0.72, ..accent }),
                rim: white(0.),
                shadows: vec![
                    drop_shadow(4., 16., Hsla { a: 0.40, ..accent }),
                    top_highlight(0.35),
                ],
            }
        } else {
            Self {
                fill: gradient(160., washed(accent, 0.22), Hsla { a: 0.68, ..accent }),
                rim: white(0.),
                shadows: vec![
                    drop_shadow(4., 16., Hsla { a: 0.40, ..accent }),
                    top_highlight(0.35),
                ],
            }
        }
    }

    /// Paint the recipe onto a div: gradient fill, 1px hairline rim, and the
    /// shadow stack (outer drop + inset top highlight).
    /// [`Glass::panel`] at the mock's own translucency — only for a surface
    /// that paints a blurred [`backdrop`] beneath it, where the low alpha
    /// reads as depth instead of as raw terminal text.
    pub fn panel_blurred(dark: bool) -> Self {
        let mut g = Self::panel(dark);
        g.fill = if dark {
            gradient(165., rgba(0x2c2c348c).into(), rgba(0x1c1c2261).into())
        } else {
            gradient(165., rgba(0xffffff6b).into(), rgba(0xffffff38).into())
        };
        g
    }

    /// [`Glass::bar`] at the mock's translucency; see [`Glass::panel_blurred`].
    pub fn bar_blurred(dark: bool) -> Self {
        let mut g = Self::bar(dark);
        g.fill = if dark {
            gradient(165., rgba(0x2c2c3473).into(), rgba(0x1e1e2447).into())
        } else {
            gradient(165., rgba(0xffffff52).into(), rgba(0xffffff1f).into())
        };
        g
    }

    pub fn apply(self, el: Div) -> Div {
        el.bg(self.fill)
            .border_1()
            .border_color(self.rim)
            .shadow(self.shadows)
    }
}

/// The blurred impression of the canvas (`backdrop.rs`) cropped to this
/// element's own bounds — drop it in as the first, `absolute().inset_0()`
/// child of a glass container and it becomes the container's backdrop.
/// `window` is the logical window size the image covers, `corners` the
/// container's own corner radii so the crop rounds exactly where it does —
/// a docked edge (sheet bottom / composer top) passes zero so the two crops
/// meet in one continuous pane of glass.
pub fn backdrop(image: Arc<RenderImage>, window: (f32, f32), corners: Corners<Pixels>) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, _, win, _| {
            let image_bounds = Bounds {
                origin: point(px(0.), px(0.)),
                size: Size { width: px(window.0), height: px(window.1) },
            };
            let _ = win.paint_image(bounds, image_bounds, corners, image, 0, false);
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_accent() -> Hsla {
        Hsla { h: 0.6, s: 0.8, l: 0.5, a: 1. }
    }

    #[test]
    fn panel_rim_alphas_match_mocks() {
        assert!((Glass::panel(true).rim.a - 0.16).abs() < 1e-3);
        assert!((Glass::panel(false).rim.a - 0.55).abs() < 1e-3);
        assert!((Glass::bar(true).rim.a - 0.16).abs() < 1e-3);
        assert!((Glass::card(false).rim.a - 0.50).abs() < 1e-3);
    }

    #[test]
    fn every_recipe_yields_two_shadows_exactly_one_inset() {
        let accent = test_accent();
        let recipes = [
            Glass::panel(true),
            Glass::panel(false),
            Glass::bar(true),
            Glass::bar(false),
            Glass::card(true),
            Glass::card(false),
            Glass::bubble_assistant(true),
            Glass::bubble_assistant(false),
            Glass::bubble_user(true, accent),
            Glass::bubble_user(false, accent),
        ];
        for glass in recipes {
            assert_eq!(glass.shadows.len(), 2, "expected [outer drop, inset highlight]");
            assert_eq!(glass.shadows.iter().filter(|s| s.inset).count(), 1);
            let inset = glass.shadows.iter().find(|s| s.inset).unwrap();
            assert_eq!(inset.blur_radius, px(0.));
            assert_eq!(inset.offset.y, px(1.));
        }
    }

    #[test]
    fn bubbles_are_rimless_and_user_keeps_accent() {
        for dark in [true, false] {
            assert_eq!(Glass::bubble_assistant(dark).rim.a, 0.);
            assert_eq!(Glass::bubble_user(dark, test_accent()).rim.a, 0.);
        }
        let accent = test_accent();
        let outer = Glass::bubble_user(true, accent)
            .shadows
            .into_iter()
            .find(|s| !s.inset)
            .unwrap();
        assert!((outer.color.a - 0.40).abs() < 1e-3, "outer glow is accent @ .40");
        assert_eq!(outer.color.h, accent.h, "glow keeps the accent hue");
    }

    #[test]
    fn polarities_differ() {
        let accent = test_accent();
        assert!(Glass::panel(true).rim.a != Glass::panel(false).rim.a);
        assert!(Glass::card(true).rim.a != Glass::card(false).rim.a);
        assert!(Glass::bubble_user(true, accent).fill != Glass::bubble_user(false, accent).fill);
    }
}
