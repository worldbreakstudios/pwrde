//! Separator — port of shadcn base-vega `ui/separator.tsx`.
//!
//! A 1px border-colored rule; horizontal fills the row, vertical stretches
//! to its flex container's height.

use gpui::{App, IntoElement, RenderOnce, Styled, Window, div, px};

use crate::ui::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SeparatorOrientation {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(IntoElement)]
pub struct Separator {
    orientation: SeparatorOrientation,
}

impl Separator {
    pub fn new() -> Self {
        Self {
            orientation: SeparatorOrientation::default(),
        }
    }

    pub fn orientation(mut self, orientation: SeparatorOrientation) -> Self {
        self.orientation = orientation;
        self
    }

    pub fn vertical() -> Self {
        Self::new().orientation(SeparatorOrientation::Vertical)
    }
}

impl Default for Separator {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Separator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        // shrink-0 bg-border; data-horizontal:h-px w-full,
        // data-vertical:w-px self-stretch
        let base = div().flex_shrink_0().bg(theme.border);
        match self.orientation {
            SeparatorOrientation::Horizontal => base.h(px(1.)).w_full(),
            SeparatorOrientation::Vertical => base.w(px(1.)).self_stretch(),
        }
    }
}
