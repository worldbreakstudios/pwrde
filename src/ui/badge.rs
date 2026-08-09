//! Badge — port of shadcn base-vega `ui/badge.tsx`.
//!
//! Variants: Default, Secondary, Destructive, Outline, Ghost, Link.
//! The source's `rounded-4xl` renders as a pill at badge height, so the port
//! uses a full radius. Focus-visible and aria-invalid styles are omitted.

use gpui::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    Styled, Window, div, px,
};

use crate::ui::theme::{Theme, alpha};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum BadgeVariant {
    #[default]
    Default,
    Secondary,
    Destructive,
    Outline,
    Ghost,
    Link,
}

#[derive(IntoElement)]
pub struct Badge {
    variant: BadgeVariant,
    children: Vec<AnyElement>,
}

impl Badge {
    pub fn new() -> Self {
        Self {
            variant: BadgeVariant::default(),
            children: Vec::new(),
        }
    }

    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }
}

impl Default for Badge {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Badge {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();

        // inline-flex h-5 w-fit items-center justify-center gap-1 rounded-4xl
        // border border-transparent px-2 py-0.5 text-xs font-medium
        let base = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .h(px(20.))
            .items_center()
            .justify_center()
            .gap(px(4.))
            .overflow_hidden()
            .rounded_full()
            .border_1()
            .border_color(gpui::transparent_black())
            .px(px(8.))
            .text_size(px(12.))
            .line_height(px(16.))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap();

        match self.variant {
            BadgeVariant::Default => base.bg(theme.primary).text_color(theme.primary_foreground),
            BadgeVariant::Secondary => base
                .bg(theme.secondary)
                .text_color(theme.secondary_foreground),
            // bg-destructive/10 (dark: /20) text-destructive
            BadgeVariant::Destructive => base
                .bg(alpha(theme.destructive, if theme.dark { 0.2 } else { 0.1 }))
                .text_color(theme.destructive),
            BadgeVariant::Outline => base.border_color(theme.border).text_color(theme.foreground),
            // hover:bg-muted hover:text-muted-foreground dark:hover:bg-muted/50
            BadgeVariant::Ghost => {
                let dark = theme.dark;
                let muted = theme.muted;
                let muted_foreground = theme.muted_foreground;
                base.text_color(theme.foreground).hover(move |s| {
                    let bg = if dark { alpha(muted, 0.5) } else { muted };
                    s.bg(bg).text_color(muted_foreground)
                })
            }
            // text-primary hover:underline
            BadgeVariant::Link => base.text_color(theme.primary).hover(|s| s.underline()),
        }
        .children(self.children)
    }
}
