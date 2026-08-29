//! The modal overlays — the confirm dialog and the one-line message panel —
//! as a gpui element tree above everything else.
//!
//! Both used to be canvas-painted (`Renderer::confirm_overlay` /
//! `message_overlay`) with their buttons hit-tested in `main.rs`'s
//! `overlay_click`. They are now built from the vendored rcn `AlertDialog`
//! (a deferred, full-viewport occluding scrim with a centered panel) and rcn
//! `Button`s, styled with the exact chrome tokens the canvas used: the
//! chrome `scrim` at 30%, a `card` panel at 96% with the tile-card radius and
//! shadow, chrome-font text, and an accent accept button. Mounted last in
//! `App::render` so it sits over every page overlay and the canvas flyover;
//! being `deferred`, it paints after its siblings regardless.
//!
//! Keyboard handling (Enter accepts, Escape cancels; any key dismisses a
//! dismissable message) stays in `main.rs`'s `on_key_down` — the state is
//! `App::confirm` / `App::message`, and this tree only renders it.

use gpui::{
    AnyElement, App as GpuiApp, BoxShadow, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, point, px,
};

use crate::App;
use crate::ui::theme::Theme;
use crate::ui::{AlertDialog, AlertDialogFooter, Button, ButtonSize, ButtonVariant};

/// Padding inside the panel, matching the canvas dialog.
const PAD: f32 = 16.0;
/// Gap between the message line and the buttons, and between buttons.
const GAP: f32 = 10.0;

impl App {
    /// The open modal, if any: the confirm dialog wins over a message.
    pub fn render_modals(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(confirm) = self.confirm.as_ref() {
            return self.render_confirm(&confirm.text, confirm.accept_label(), cx);
        }
        if let Some((text, dismissable)) = self.message.as_ref() {
            return self.render_message(text, *dismissable, cx);
        }
        div().into_any_element()
    }

    /// Centered confirm dialog: a message line over Cancel / accept buttons.
    /// A click outside the panel cancels — the safe default for a
    /// destructive action.
    fn render_confirm(&self, text: &str, accept: &'static str, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let font = crate::renderer::chrome_font();
        let entity = cx.entity().downgrade();

        let cancel = {
            let entity = entity.clone();
            Button::new("confirm-cancel")
                .variant(ButtonVariant::Secondary)
                .size(ButtonSize::Sm)
                .child("Cancel")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.confirm = None;
                            this.request_redraw();
                            cx.notify();
                        });
                    }
                })
        };
        let accept_btn = {
            let entity = entity.clone();
            Button::new("confirm-accept")
                .variant(ButtonVariant::Default)
                .size(ButtonSize::Sm)
                .child(accept)
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.confirm_accept();
                            this.request_redraw();
                            cx.notify();
                        });
                    }
                })
        };

        let backdrop_entity = entity.clone();
        panel_style(AlertDialog::new("confirm-dialog").open(true), &theme, chrome)
            .scrim(crate::renderer::color(chrome.scrim, 0.30))
            .on_backdrop_click(move |_ev, _win, app| {
                if let Some(entity) = backdrop_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.confirm = None;
                        this.request_redraw();
                        cx.notify();
                    });
                }
            })
            .child(
                div()
                    .font_family(crate::renderer::FONT_FAMILY)
                    .text_size(px(font))
                    .text_color(theme.foreground)
                    .whitespace_nowrap()
                    .child(text.to_string()),
            )
            .child(
                AlertDialogFooter::new()
                    .gap(px(GAP))
                    .child(cancel)
                    .child(accept_btn),
            )
            .into_any_element()
    }

    /// Centered one-line message panel (worktree provisioning / failure
    /// note). A dismissable one clears on any click; a modal one — work in
    /// flight — swallows it.
    fn render_message(&self, text: &str, dismissable: bool, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let font = crate::renderer::chrome_font();
        let entity = cx.entity().downgrade();

        let dismiss = move |app: &mut GpuiApp| {
            if let Some(entity) = entity.upgrade() {
                entity.update(app, |this, cx| {
                    if this.message.as_ref().is_some_and(|(_, d)| *d) {
                        this.message = None;
                        this.request_redraw();
                        cx.notify();
                    }
                });
            }
        };
        let backdrop_dismiss = dismiss.clone();

        let mut line = div()
            .id("message-panel-text")
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(font))
            .text_color(theme.foreground)
            .whitespace_nowrap()
            .child(text.to_string());
        if dismissable {
            line = line.on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                dismiss(app)
            });
        }

        panel_style(AlertDialog::new("message-panel").open(true), &theme, chrome)
            .scrim(crate::renderer::color(chrome.scrim, 0.30))
            .on_backdrop_click(move |_ev, _win, app| backdrop_dismiss(app))
            .child(line)
            .into_any_element()
    }
}

/// The panel treatment both modals share: the canvas painted `th.card` at
/// 96% with the tile-card radius and `Shadow::Card`, content-sized.
fn panel_style(dialog: AlertDialog, theme: &Theme, chrome: &crate::theme::Theme) -> AlertDialog {
    dialog
        .w_auto()
        .p(px(PAD))
        .gap(px(GAP))
        .rounded(px(crate::renderer::CARD_RADIUS))
        .border_0()
        .bg(theme.card.opacity(0.96))
        .shadow(vec![BoxShadow {
            color: crate::renderer::color(chrome.shadow, 0.22),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(28.0),
            spread_radius: px(0.0),
            inset: false,
        }])
}
