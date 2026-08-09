//! Kbd — port of shadcn base-vega `ui/kbd.tsx`.
//!
//! Inline keyboard-key chips and a grouping wrapper. Tooltip-context styles
//! (`in-data-[slot=tooltip-content]:…`) are omitted — no tooltip slot yet.
//! SVG child sizing and `pointer-events-none` / `select-none` are also omitted.

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px,
};

use crate::ui::theme::Theme;

#[derive(IntoElement)]
pub struct Kbd {
    children: Vec<AnyElement>,
}

impl Kbd {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for Kbd {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Kbd {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        // inline-flex h-5 w-fit min-w-5 items-center justify-center gap-1
        // rounded-sm bg-muted px-1 font-sans text-xs font-medium text-muted-foreground
        div()
            .flex()
            .flex_row()
            .h(px(20.))
            .min_w(px(20.))
            .items_center()
            .justify_center()
            .gap(px(4.))
            .rounded(theme.radius_sm())
            .bg(theme.muted)
            .px(px(4.))
            .text_size(px(12.))
            .line_height(px(16.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .children(self.children)
    }
}

#[derive(IntoElement)]
pub struct KbdGroup {
    children: Vec<AnyElement>,
}

impl KbdGroup {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for KbdGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for KbdGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for KbdGroup {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // inline-flex items-center gap-1
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .children(self.children)
    }
}
