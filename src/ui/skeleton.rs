//! Skeleton — port of shadcn base-vega `ui/skeleton.tsx`.
//!
//! A rounded-md bg-muted placeholder block shown while content is loading.

use gpui::{App, IntoElement, Pixels, RenderOnce, Styled, Window, div, px};

use crate::ui::theme::Theme;

#[derive(IntoElement)]
pub struct Skeleton {
    width: Pixels,
    height: Pixels,
    rounded_full: bool,
}

impl Skeleton {
    pub fn new() -> Self {
        Self {
            width: px(100.),
            height: px(20.),
            rounded_full: false,
        }
    }

    pub fn w(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    pub fn h(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    pub fn rounded_full(mut self) -> Self {
        self.rounded_full = true;
        self
    }
}

impl Default for Skeleton {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Skeleton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        // TODO(rcn): animate-pulse — gpui animation not wired
        // rounded-md bg-muted (or rounded-full when circular)
        let base = div()
            .flex_shrink_0()
            .w(self.width)
            .h(self.height)
            .bg(theme.muted);
        if self.rounded_full {
            base.rounded_full()
        } else {
            base.rounded(theme.radius_md())
        }
    }
}
