//! Motion primitives matching shadcn's animation stack (tw-animate-css).
//!
//! tw-animate-css ground truth (from `node_modules/tw-animate-css`):
//! - `animate-in`/`animate-out` (enter/exit keyframes): default **150ms**,
//!   CSS `ease` timing — interpolating opacity (`fade-in-*`), scale
//!   (`zoom-in-*`), and translation (`slide-in-from-*`).
//! - `accordion-down/up`, `collapsible-down/up`: **200ms `ease-out`**,
//!   animating height between 0 and the content height.
//! - Tailwind `transition-*` utilities (hover states etc.) default to
//!   **150ms `cubic-bezier(0.4, 0, 0.2, 1)`**.
//!
//! gpui animates on element mount via `with_animation`, so open
//! transitions ("animate-in") are faithful; exit animations would need the
//! element to outlive its state and are omitted (TODO(rcn)). Scale
//! transforms only exist for svg in gpui, so `zoom-in-95` is approximated
//! by the fade + slide pair.

use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt as _, BoxShadow, ElementId, Hsla, IntoElement,
    Styled, point, px,
};

use crate::ui::theme::{Theme, alpha};

/// CSS `ease` — cubic-bezier(0.25, 0.1, 0.25, 1).
pub fn ease() -> impl Fn(f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.)
}

/// CSS `ease-out` — cubic-bezier(0, 0, 0.58, 1).
pub fn ease_out() -> impl Fn(f32) -> f32 {
    cubic_bezier(0., 0., 0.58, 1.)
}

/// Tailwind's default transition curve — cubic-bezier(0.4, 0, 0.2, 1).
pub fn ease_transition() -> impl Fn(f32) -> f32 {
    cubic_bezier(0.4, 0., 0.2, 1.)
}

/// `animate-in` (tw-animate-css enter keyframes): 150ms `ease`.
pub fn enter() -> Animation {
    Animation::new(Duration::from_millis(150)).with_easing(ease())
}

/// The popover family's `duration-100` override of [`enter`].
pub fn enter_fast() -> Animation {
    Animation::new(Duration::from_millis(100)).with_easing(ease())
}

/// `accordion-down` / `collapsible-down`: 200ms `ease-out`.
pub fn expand() -> Animation {
    Animation::new(Duration::from_millis(200)).with_easing(ease_out())
}

/// Wrap an overlay panel with the popover family's enter animation —
/// `animate-in fade-in-0 slide-in-from-top-2 duration-100`: opacity 0→1
/// and an 8px downward settle over 100ms `ease`. (zoom-in-95 has no div
/// transform in gpui; the fade+slide pair carries the effect.)
pub fn pop_in<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    panel: E,
) -> AnimationElement<E> {
    panel.with_animation(id, enter_fast(), |el, delta| {
        el.opacity(delta).mt(px(-8. * (1. - delta)))
    })
}

/// Tailwind `animate-pulse`: opacity breathing 1 → 0.5 → 1 over 2s,
/// repeating.
pub fn pulse<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    element: E,
) -> AnimationElement<E> {
    let animation = Animation::new(std::time::Duration::from_secs(2))
        .repeat()
        .with_easing(gpui::pulsating_between(0.5, 1.));
    element.with_animation(id, animation, |el, delta| el.opacity(delta))
}

/// The dialog family's `duration-200` enter (fade + small settle).
pub fn dialog_in<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    panel: E,
) -> AnimationElement<E> {
    let animation = Animation::new(std::time::Duration::from_millis(200)).with_easing(ease());
    panel.with_animation(id, animation, |el, delta| {
        el.opacity(delta).mt(px(8. * (1. - delta)))
    })
}

/// Sheet/drawer slide: 500ms `ease-in-out`, sliding `distance` px along the
/// axis toward rest (positive = from the right/bottom).
pub fn slide_in<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    horizontal: bool,
    distance: f32,
    panel: E,
) -> AnimationElement<E> {
    let animation = Animation::new(std::time::Duration::from_millis(500))
        .with_easing(cubic_bezier(0.42, 0., 0.58, 1.));
    panel.with_animation(id, animation, move |el, delta| {
        let offset = px(distance * (1. - delta));
        if horizontal {
            el.ml(offset)
        } else {
            el.mt(offset)
        }
    })
}

/// Evaluate a CSS cubic-bezier timing function at progress `t`.
///
/// Solves the parametric curve x(s) = t for s (Newton–Raphson with a
/// bisection fallback), then returns y(s) — the same algorithm browsers
/// use.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 {
    move |t: f32| {
        let t = t.clamp(0., 1.);
        if t == 0. || t == 1. {
            return t;
        }
        let curve = |a: f32, b: f32, s: f32| {
            // One-dimensional cubic Bézier with P0=0, P3=1.
            3. * a * s * (1. - s) * (1. - s) + 3. * b * s * s * (1. - s) + s * s * s
        };
        let derivative = |a: f32, b: f32, s: f32| {
            3. * a * (1. - s) * (1. - s) + 6. * (b - a) * s * (1. - s) + 3. * (1. - b) * s * s
        };

        // Newton–Raphson for s where x(s) == t.
        let mut s = t;
        for _ in 0..8 {
            let x = curve(x1, x2, s) - t;
            let d = derivative(x1, x2, s);
            if x.abs() < 1e-5 {
                return curve(y1, y2, s).clamp(0., 1.);
            }
            if d.abs() < 1e-6 {
                break;
            }
            s -= x / d;
        }
        // Bisection fallback for flat derivatives.
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        s = t;
        for _ in 0..20 {
            let x = curve(x1, x2, s);
            if (x - t).abs() < 1e-5 {
                break;
            }
            if x < t {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.;
        }
        curve(y1, y2, s).clamp(0., 1.)
    }
}

/// The shadcn focus ring: `focus-visible:border-ring focus-visible:ring-3
/// focus-visible:ring-ring/50` — a 3px zero-blur spread shadow in the ring
/// color at 50%.
pub fn focus_ring(theme: &Theme) -> Vec<BoxShadow> {
    ring_shadow(alpha(theme.ring, 0.5))
}

/// The destructive variant of the ring (`ring-destructive/20`).
pub fn focus_ring_destructive(theme: &Theme) -> Vec<BoxShadow> {
    ring_shadow(alpha(theme.destructive, if theme.dark { 0.4 } else { 0.2 }))
}

fn ring_shadow(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(3.),
        inset: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_monotonicity() {
        let curves = [
            cubic_bezier(0.25, 0.1, 0.25, 1.),
            cubic_bezier(0., 0., 0.58, 1.),
            cubic_bezier(0.4, 0., 0.2, 1.),
        ];
        for ease in &curves {
            assert_eq!(ease(0.), 0.);
            assert_eq!(ease(1.), 1.);
            let mut last = 0.;
            for step in 1..=20 {
                let value = ease(step as f32 / 20.);
                assert!(value >= last - 1e-4, "easing must be monotonic");
                last = value;
            }
        }
    }

    #[test]
    fn css_ease_reference_values() {
        // Browser-verified samples of cubic-bezier(0.25, 0.1, 0.25, 1).
        let e = ease();
        assert!((e(0.25) - 0.4085).abs() < 0.01, "got {}", e(0.25));
        assert!((e(0.5) - 0.8024).abs() < 0.01, "got {}", e(0.5));
        assert!((e(0.75) - 0.9603).abs() < 0.01, "got {}", e(0.75));
    }
}
