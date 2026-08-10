//! Label — port of shadcn base-vega `ui/label.tsx`.
//!
//! A flex-row text label for associating copy with controls. Peer/group
//! disabled cascade is approximated via an explicit `.disabled(bool)` builder
//! (opacity 0.5); pointer-events and cursor variants are omitted.

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px,
};

use crate::ui::theme::Theme;

#[derive(IntoElement)]
pub struct Label {
    disabled: bool,
    children: Vec<AnyElement>,
}

impl Label {
    pub fn new() -> Self {
        Self {
            disabled: false,
            children: Vec::new(),
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Default for Label {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Label {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        // flex items-center gap-2 text-sm leading-none font-medium select-none
        // group-data-[disabled=true]:opacity-50 peer-disabled:opacity-50
        let mut base = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .text_size(px(14.))
            .line_height(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.foreground);

        if self.disabled {
            base = base.opacity(0.5);
        }

        base.children(self.children)
    }
}
