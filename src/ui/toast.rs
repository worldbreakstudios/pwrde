//! Toast — port of shadcn base-vega `ui/toast.tsx`.
//!
//! A notification card plus a viewport pinned to the window's bottom-right
//! corner. Controlled: the caller owns which toasts are visible and closes
//! them from the card's close button. Timers, swipe-to-dismiss, and stack
//! animations are omitted. Sizing and shape overrides come from the caller
//! via [`Styled`].

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement,
    Refineable as _, RenderOnce, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
    anchored, deferred, div, point, prelude::FluentBuilder as _, px, svg,
};

use crate::ui::motion;
use crate::ui::theme::{Theme, alpha};

type CloseHandler = Rc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>;

/// The bottom-right viewport that stacks visible toasts over the app.
/// Sizing and shape overrides via [`Styled`] target the viewport stack root
/// (the flex container), not the deferred/anchored plumbing.
#[derive(IntoElement)]
pub struct ToastViewport {
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl ToastViewport {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }
}

impl Default for ToastViewport {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for ToastViewport {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for ToastViewport {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ToastViewport {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if self.children.is_empty() {
            let mut empty = div();
            empty.style().refine(&self.style);
            return empty.into_any_element();
        }
        let viewport = window.viewport_size();
        let mut root = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .flex_col()
            .items_end()
            .justify_end()
            .p(px(16.))
            .gap(px(8.))
            .children(self.children);
        root.style().refine(&self.style);
        deferred(anchored().position(point(px(0.), px(0.))).child(root)).into_any_element()
    }
}

/// One notification card: title, optional description, optional action,
/// and a close button. Sizing and shape overrides via [`Styled`] target the
/// toast card root (bg/rounded/padding), not the pop-in motion wrapper.
#[derive(IntoElement)]
pub struct Toast {
    id: ElementId,
    title: gpui::SharedString,
    description: Option<gpui::SharedString>,
    action: Option<AnyElement>,
    on_close: Option<CloseHandler>,
    style: StyleRefinement,
}

impl Toast {
    pub fn new(id: impl Into<ElementId>, title: impl Into<gpui::SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            action: None,
            on_close: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn description(mut self, description: impl Into<gpui::SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Trailing action element (usually an xs outline Button).
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    pub fn on_close(
        mut self,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

impl Styled for Toast {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        // Card: w-full max-w-sm rounded-md bg-popover p-4 shadow-lg ring-1,
        // sliding in from the bottom like the source's enter animation.
        let mut card = div()
            .id(self.id)
            .occlude()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(12.))
            .w(px(360.))
            .rounded(theme.radius_md())
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .p(px(16.))
            .shadow_lg()
            .border_1()
            .border_color(alpha(theme.foreground, 0.1))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title),
                    )
                    .when_some(self.description, |el, description| {
                        el.child(
                            div()
                                .text_size(px(14.))
                                .line_height(px(20.))
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    }),
            )
            .children(self.action)
            .when_some(self.on_close, |el, close| {
                el.child({
                    let ring = motion::focus_ring(&theme);
                    div()
                        .id("toast-close")
                        .flex_shrink_0()
                        .rounded(theme.radius_sm())
                        .p(px(2.))
                        .tab_index(0)
                        .focus_visible(move |s| s.border_color(theme.ring).shadow(ring.clone()))
                        .hover(|s| s.bg(alpha(theme.muted, 0.8)))
                        .on_click(move |event, window, cx| close(event, window, cx))
                        .child(
                            svg()
                                .path(theme.icons.x())
                                .size(px(14.))
                                .text_color(theme.muted_foreground),
                        )
                })
            });
        card.style().refine(&self.style);
        crate::ui::motion::pop_in("toast-in", card)
    }
}
