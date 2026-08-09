//! Checkbox — port of shadcn base-vega `ui/checkbox.tsx`.
//!
//! Controlled: the caller owns `checked` and receives the next value in
//! `on_change`. The check indicator follows the active icon library.
//! Aria-invalid styles are omitted.

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px, svg,
};

use crate::ui::motion;
use crate::ui::theme::{Theme, alpha};

type ChangeHandler = Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            disabled: false,
            on_change: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let checked = self.checked;

        // size-4 rounded-[4px] border border-input shadow-xs;
        // checked: border-primary bg-primary text-primary-foreground;
        // dark unchecked: bg-input/30
        div()
            .id(self.id)
            .flex()
            .flex_shrink_0()
            .size(px(16.))
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .shadow_xs()
            .map(|el| {
                if checked {
                    el.border_color(theme.primary).bg(theme.primary)
                } else if theme.dark {
                    el.border_color(theme.input).bg(alpha(theme.input, 0.3))
                } else {
                    el.border_color(theme.input)
                }
            })
            .when(self.disabled, |el| el.opacity(0.5))
            .when(!self.disabled, |el| {
                let ring = motion::focus_ring(&theme);
                el.tab_index(0)
                    .focus_visible(move |s| s.border_color(theme.ring).shadow(ring.clone()))
                    .when_some(self.on_change, |el, on_change| {
                        el.on_click(move |_, window, cx| on_change(&!checked, window, cx))
                    })
            })
            .when(checked, |el| {
                el.child(
                    svg()
                        .path(theme.icons.check())
                        .size(px(14.))
                        .text_color(theme.primary_foreground),
                )
            })
    }
}
