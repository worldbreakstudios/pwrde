//! HoverCard — port of shadcn base-vega `ui/hover-card.tsx`.
//!
//! Rich content revealed on hover, built on gpui's hoverable-tooltip
//! machinery (the panel stays open while the pointer is over it). The
//! panel matches the source: `w-64 rounded-md bg-popover p-4 shadow-md
//! ring-1 ring-foreground/10`. Open/close animations are omitted.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, InteractiveElement as _, IntoElement,
    ParentElement, Render, RenderOnce, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::ui::theme::{Theme, alpha};

type ContentBuilder = Rc<dyn Fn(&mut App) -> AnyElement + 'static>;

/// The floating panel view.
struct HoverCardView {
    content: ContentBuilder,
}

impl Render for HoverCardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        div().m(px(4.)).child(
            div()
                .w(px(256.))
                .rounded(theme.radius_md())
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .p(px(16.))
                .shadow_md()
                .border_1()
                .border_color(alpha(theme.foreground, 0.1))
                .text_size(px(14.))
                .line_height(px(20.))
                .child((self.content)(cx)),
        )
    }
}

/// A hover-card wrapper: trigger children + a content builder.
#[derive(IntoElement)]
pub struct HoverCard {
    id: ElementId,
    content: Option<ContentBuilder>,
    children: Vec<AnyElement>,
}

impl HoverCard {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            content: None,
            children: Vec::new(),
        }
    }

    /// Builds the panel content each time the card opens.
    pub fn content(mut self, content: impl Fn(&mut App) -> AnyElement + 'static) -> Self {
        self.content = Some(Rc::new(content));
        self
    }
}

impl ParentElement for HoverCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for HoverCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .id(self.id)
            .when_some(self.content, |el, content| {
                el.hoverable_tooltip(move |_, cx| {
                    let content = content.clone();
                    cx.new(|_| HoverCardView { content }).into()
                })
            })
            .children(self.children)
    }
}
