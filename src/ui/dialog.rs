//! Dialog — port of shadcn base-nova `ui/dialog.tsx`.
//!
//! A modal window over a dimmed backdrop, centered in the viewport.
//! Controlled via `open` + `on_open_change`, or uncontrolled via
//! `default_open` + keyed state and an optional `trigger`. Backdrop click,
//! Escape, and the close button all share the same close path.
//!
//! Sizing and shape overrides come from the caller via [`Styled`] and apply
//! to the floating panel (the element carrying background, border, and shadow),
//! not the full-viewport backdrop. Header/Title/Description/Footer also accept
//! caller styles via [`Styled`].
//!
//! # Omitted / TODO(rcn)
//! - Exit animations (unportable on unmount in gpui)
//! - Overlay `supports-backdrop-filter:backdrop-blur-xs` (unportable in gpui)
//! - Focus trap (Base UI keeps Tab inside the popup; initial focus IS ported —
//!   the panel is focused on open so Escape works immediately)
//! - RTL layout
//! - Description link styles (`*:[a]:underline`)

use std::rc::Rc;

use gpui::{
    AnimationExt as _, AnyElement, App, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, Refineable as _, RenderOnce, StatefulInteractiveElement as _,
    StyleRefinement, Styled, Window, anchored, deferred, div, point, prelude::FluentBuilder as _,
    px, svg,
};

use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::motion;
use crate::ui::theme::{Theme, alpha};

pub type OpenChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

/// The modal root: optional inline trigger + centered content panel over a
/// dimmed backdrop when open. Sizing and shape overrides via [`Styled`] target
/// the floating panel root (bg/border/shadow), not the backdrop.
#[derive(IntoElement)]
pub struct Dialog {
    id: ElementId,
    /// `None` = uncontrolled (resolve via keyed state); `Some` = controlled.
    open: Option<bool>,
    default_open: bool,
    on_open_change: Option<OpenChangeHandler>,
    /// Top-right close button (shadcn `showCloseButton`, default true).
    show_close_button: bool,
    /// Override content max width (default `sm:max-w-md` = 448px).
    max_w: Option<gpui::Pixels>,
    /// Optional inline trigger element (uncontrolled open pattern).
    trigger: Option<AnyElement>,
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl Dialog {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            open: None,
            default_open: false,
            on_open_change: None,
            show_close_button: true,
            max_w: None,
            trigger: None,
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    /// Controlled open state. Distinguishes "never set" (`None`) from
    /// `open(false)` so uncontrolled keyed state can take over.
    pub fn open(mut self, open: bool) -> Self {
        self.open = Some(open);
        self
    }

    /// Initial open state when uncontrolled (default `false`).
    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    /// Inline trigger element. Clicking it toggles open in uncontrolled
    /// mode (and notifies `on_open_change` when set).
    pub fn trigger(mut self, trigger: impl IntoElement) -> Self {
        self.trigger = Some(trigger.into_any_element());
        self
    }

    pub fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }

    /// Show the top-right close button (default `true`, matching shadcn).
    pub fn show_close_button(mut self, show: bool) -> Self {
        self.show_close_button = show;
        self
    }

    /// Override the content panel max width (default 448px / `sm:max-w-md`).
    pub fn max_w(mut self, width: gpui::Pixels) -> Self {
        self.max_w = Some(width);
        self
    }
}

impl ParentElement for Dialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Dialog {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Dialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let viewport = window.viewport_size();
        let show_close_button = self.show_close_button;
        let content_w = self.max_w.unwrap_or(px(448.)).min(viewport.width - px(32.));

        // Controlled snapshot vs keyed-state (accordion pattern).
        let (is_open, uncontrolled_state): (bool, Option<Entity<bool>>) =
            if let Some(open) = self.open {
                (open, None)
            } else {
                let default_open = self.default_open;
                let state_key: ElementId = (self.id.clone(), "open").into();
                let state = window.use_keyed_state(state_key, cx, move |_, _| default_open);
                (*state.read(cx), Some(state))
            };

        let on_open_change = self.on_open_change;

        // Shared close path: set uncontrolled state false + notify handler.
        let notify_open = {
            let on_open_change = on_open_change.clone();
            let uncontrolled_state = uncontrolled_state.clone();
            Rc::new(move |open: bool, window: &mut Window, cx: &mut App| {
                if let Some(state) = uncontrolled_state.as_ref() {
                    state.update(cx, |v, cx| {
                        *v = open;
                        cx.notify();
                    });
                }
                if let Some(handler) = on_open_change.as_ref() {
                    handler(&open, window, cx);
                }
            }) as Rc<dyn Fn(bool, &mut Window, &mut App)>
        };

        let root = div().id(self.id.clone()).relative();

        let root = if let Some(trigger) = self.trigger {
            let notify = notify_open.clone();
            let next = !is_open;
            root.child(
                div()
                    .id("dialog-trigger")
                    .on_click(move |_, window, cx| notify(next, window, cx))
                    .child(trigger),
            )
        } else {
            root
        };

        // Open-transition tracker, so focus moves into the panel once per open.
        let was_open_key: ElementId = (self.id.clone(), "was-open").into();
        let was_open = window.use_keyed_state(was_open_key, cx, |_, _| false);

        if !is_open {
            if *was_open.read(cx) {
                was_open.update(cx, |v, _| *v = false);
            }
            return root.into_any_element();
        }

        // Initial focus (Base UI default): focus the panel when it opens so
        // Escape lands on its key surface immediately.
        let focus_key: ElementId = (self.id.clone(), "focus").into();
        let focus_state = window.use_keyed_state(focus_key, cx, |_, cx| cx.focus_handle());
        let focus_handle = focus_state.read(cx).clone();
        if !*was_open.read(cx) {
            was_open.update(cx, |v, _| *v = true);
            let handle = focus_handle.clone();
            window.defer(cx, move |window, cx| window.focus(&handle, cx));
        }

        let close_backdrop = notify_open.clone();
        let close_button = notify_open.clone();
        let close_escape = notify_open.clone();

        // Overlay: bg-black/10 duration-100 fade-in (backdrop-blur omitted — see module TODO)
        let overlay = div()
            .id("dialog-overlay")
            .occlude()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0., 0., 0., 0.10))
            .on_click(move |_, window, cx| close_backdrop(false, window, cx))
            .with_animation("dialog-overlay-in", motion::enter_fast(), |el, delta| {
                el.opacity(delta)
            })
            .child({
                // Content: popover surface, radius-xl, ring-foreground/10, gap-6,
                // max-w-[calc(100%-2rem)] sm:max-w-md; duration-100 fade+zoom-in-95
                // (zoom approximated as opacity + small settle — see dialog_in).
                // Escape closes in-component (focus trap / initial focus = TODO).
                let mut content = div()
                    .id("dialog-content")
                    .occlude()
                    .track_focus(&focus_handle)
                    .tab_index(0)
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .w(content_w)
                    .rounded(theme.radius_xl())
                    .border_1()
                    .border_color(alpha(theme.foreground, 0.10))
                    .bg(theme.popover)
                    .p(px(24.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(theme.popover_foreground)
                    .on_key_down(move |event, window, cx| {
                        if event.keystroke.key == "escape" {
                            close_escape(false, window, cx);
                        }
                    })
                    .children(self.children)
                    // Close: ghost icon-sm Button, absolute top-4 right-4
                    .when(show_close_button, |el| {
                        el.child(
                            div().absolute().top(px(16.)).right(px(16.)).child(
                                Button::new("dialog-close")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::IconSm)
                                    .on_click(move |_, window, cx| close_button(false, window, cx))
                                    .child(
                                        // base-nova ghost has no own text color —
                                        // the X inherits the content's foreground.
                                        svg()
                                            .path(theme.icons.x())
                                            .size(px(16.))
                                            .text_color(theme.popover_foreground),
                                    ),
                            ),
                        )
                    });
                content.style().refine(&self.style);
                content.with_animation("dialog-in", motion::enter_fast(), |el, delta| {
                    el.opacity(delta).mt(px(8. * (1. - delta)))
                })
            });

        root.child(deferred(
            anchored().position(point(px(0.), px(0.))).child(overlay),
        ))
        .into_any_element()
    }
}

/// flex flex-col gap-2 text-center sm:text-left.
/// Sizing and shape overrides come from the caller via [`Styled`].
#[derive(IntoElement)]
pub struct DialogHeader {
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl DialogHeader {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }
}

impl Default for DialogHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for DialogHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for DialogHeader {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for DialogHeader {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let mut root = div().flex().flex_col().gap(px(8.)).children(self.children);
        root.style().refine(&self.style);
        root
    }
}

/// leading-none font-medium — inherits content's 14px text size.
/// Sizing and shape overrides come from the caller via [`Styled`].
#[derive(IntoElement)]
pub struct DialogTitle {
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl DialogTitle {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }
}

impl Default for DialogTitle {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for DialogTitle {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for DialogTitle {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for DialogTitle {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let mut root = div()
            .text_size(px(14.))
            .line_height(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .children(self.children);
        root.style().refine(&self.style);
        root
    }
}

/// text-sm text-muted-foreground.
/// Sizing and shape overrides come from the caller via [`Styled`].
#[derive(IntoElement)]
pub struct DialogDescription {
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl DialogDescription {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }
}

impl Default for DialogDescription {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for DialogDescription {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for DialogDescription {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for DialogDescription {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let mut root = div()
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(theme.muted_foreground)
            .children(self.children);
        root.style().refine(&self.style);
        root
    }
}

/// flex flex-row justify-end gap-2 (desktop). Optional outline Close via
/// `show_close_button` (default false, matching shadcn DialogFooter).
/// Sizing and shape overrides come from the caller via [`Styled`].
#[derive(IntoElement)]
pub struct DialogFooter {
    children: Vec<AnyElement>,
    /// Append outline "Close" button (shadcn `showCloseButton`, default false).
    show_close_button: bool,
    /// Close action for the optional footer Close button.
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// `sm:justify-start` override (default is `justify-end`).
    justify_start: bool,
    style: StyleRefinement,
}

impl DialogFooter {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            show_close_button: false,
            on_close: None,
            justify_start: false,
            style: StyleRefinement::default(),
        }
    }

    /// Append an outline "Close" button after children (default false).
    pub fn show_close_button(mut self, show: bool) -> Self {
        self.show_close_button = show;
        self
    }

    /// Handler invoked when the footer's Close button is clicked.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Use `justify-start` instead of the default `justify-end`.
    pub fn justify_start(mut self) -> Self {
        self.justify_start = true;
        self
    }
}

impl Default for DialogFooter {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for DialogFooter {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for DialogFooter {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for DialogFooter {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let justify_start = self.justify_start;
        let show_close = self.show_close_button;
        let on_close = self.on_close;
        let mut root = div()
            .flex()
            .flex_row()
            .when(justify_start, |el| el.justify_start())
            .when(!justify_start, |el| el.justify_end())
            .gap(px(8.))
            .children(self.children)
            .when(show_close, |el| {
                let handler = on_close.clone();
                el.child(
                    Button::new("dialog-footer-close")
                        .variant(ButtonVariant::Outline)
                        .child("Close")
                        .when_some(handler, |btn, cb| {
                            btn.on_click(move |_, window, cx| cb(window, cx))
                        }),
                )
            });
        root.style().refine(&self.style);
        root
    }
}
