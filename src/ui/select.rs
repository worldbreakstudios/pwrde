//! Select — port of shadcn base-vega `ui/select.tsx`.
//!
//! A picker: an input-styled trigger showing the current value, opening a
//! popover panel of options with a check on the selected one. Controlled:
//! the caller owns `value` (option index) and `open`, receiving changes
//! via `on_change` / `on_open_change`. Option groups, scroll buttons, and
//! typeahead are omitted.

use std::rc::Rc;

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, anchored, deferred, div,
    prelude::FluentBuilder as _, px, relative, svg,
};

use crate::ui::theme::{Theme, alpha};

type ChangeHandler = Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>;
type OpenChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    options: Vec<SharedString>,
    value: Option<usize>,
    placeholder: SharedString,
    open: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
    on_open_change: Option<OpenChangeHandler>,
}

impl Select {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            options: Vec::new(),
            value: None,
            placeholder: "Select an option".into(),
            open: false,
            disabled: false,
            on_change: None,
            on_open_change: None,
        }
    }

    pub fn option(mut self, option: impl Into<SharedString>) -> Self {
        self.options.push(option.into());
        self
    }

    pub fn options(mut self, options: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.options.extend(options.into_iter().map(Into::into));
        self
    }

    /// The selected option index.
    pub fn value(mut self, value: Option<usize>) -> Self {
        self.value = value;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    pub fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Select {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let open = self.open;
        let toggle = self.on_open_change.clone();
        let close = self.on_open_change.clone();
        let selected_label = self
            .value
            .and_then(|index| self.options.get(index).cloned());

        // Trigger: h-9 rounded-md border-input bg-transparent px-3 py-2
        // text-sm shadow-xs, muted placeholder, trailing chevron.
        let trigger = div()
            .id(self.id)
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .h(px(36.))
            .w_full()
            .rounded(theme.radius_md())
            .border_1()
            .border_color(theme.input)
            .when(theme.dark, |el| el.bg(alpha(theme.input, 0.3)))
            .px(px(12.))
            .text_size(px(14.))
            .line_height(px(20.))
            .shadow_xs()
            .when(self.disabled, |el| el.opacity(0.5))
            .when(!self.disabled, |el| {
                el.when_some(toggle, |el, toggle| {
                    el.on_click(move |_, window, cx| toggle(&!open, window, cx))
                })
            })
            .child(match selected_label {
                Some(label) => div().text_color(theme.foreground).child(label),
                None => div()
                    .text_color(theme.muted_foreground)
                    .child(self.placeholder.clone()),
            })
            .child(
                svg()
                    .path(theme.icons.chevron_down())
                    .size(px(16.))
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground),
            );

        // Panel: popover list of options, check on the selected one.
        let panel = open.then(|| {
            let on_change = self.on_change.clone();
            let value = self.value;
            div()
                .occlude()
                .flex()
                .flex_col()
                .min_w(px(128.))
                .max_h(px(384.))
                .rounded(theme.radius_md())
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .p(px(4.))
                .shadow_md()
                .border_1()
                .border_color(alpha(theme.foreground, 0.1))
                .when_some(close, |el, close| {
                    el.on_mouse_down_out(move |_, window, cx| close(&false, window, cx))
                })
                .children(self.options.into_iter().enumerate().map(|(index, label)| {
                    let selected = value == Some(index);
                    let on_change = on_change.clone();
                    let finish = self.on_open_change.clone();
                    div()
                        .id(("select-option", index))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .rounded(theme.radius_sm())
                        .pl(px(8.))
                        .pr(px(32.))
                        .py(px(6.))
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .hover(|s| s.bg(theme.accent).text_color(theme.accent_foreground))
                        .on_click(move |_, window, cx| {
                            if let Some(on_change) = &on_change {
                                on_change(&index, window, cx);
                            }
                            if let Some(finish) = &finish {
                                finish(&false, window, cx);
                            }
                        })
                        .child(div().w(px(14.)).flex_shrink_0().when(selected, |el| {
                            el.child(
                                svg()
                                    .path(theme.icons.check())
                                    .size(px(14.))
                                    .text_color(theme.popover_foreground),
                            )
                        }))
                        .child(label)
                }))
        });

        div()
            .relative()
            .flex()
            .flex_col()
            .w(px(180.))
            .child(trigger)
            .when_some(panel, |el, panel| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(relative(1.))
                        .pt(px(4.))
                        .w_full()
                        .child(deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.))
                                .child(crate::ui::motion::pop_in("select-in", panel)),
                        )),
                )
            })
    }
}
