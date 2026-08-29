//! The save-as-workspace modal as a gpui element tree.
//!
//! It used to be canvas-painted (`Renderer::save_overlay`) with its two text
//! fields edited by hand in `handle_save_key` and hit-tested in
//! `overlay_click`. The fields are the vendored rcn `Input` entities now
//! (`App::save_name` / `App::save_desc`, bare, inside the same tinted boxes
//! the canvas drew), observers mirror their text into the
//! `SaveWorkspaceModal` buffers, and the destination rows are click targets.
//! The panel shares `modal_ui::panel_style` (chrome scrim, card, shadow) and
//! sits in the window's upper third exactly where `save_layout` put it.
//!
//! Focus is reconciled once per render by `App::sync_save_focus`: keyboard
//! moves (Tab / Enter) set `SaveWorkspaceModal::field` and raise
//! `save_sync`, and the next render focuses the matching field; a click on
//! a field focuses it natively and the render reflects that back into
//! `field`. Entering the destination stage drops field focus so the arrow
//! keys reach `handle_save_key` cleanly.

use gpui::{
    AnyElement, App as GpuiApp, ClickEvent, Context, Focusable as _, InteractiveElement,
    IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, Window, div, px,
    prelude::FluentBuilder as _,
};

use crate::App;
use crate::modal_ui::panel_style;
use crate::ui::AlertDialog;
use crate::ui::theme::Theme;

/// Inner padding, matching the canvas `save_layout`.
const PAD: f32 = 12.0;
/// Field / row corner radius.
const RADIUS: f32 = 7.0;
/// Inset of the selection pill from a destination row's edges.
const PILL_INSET: f32 = 6.0;

impl App {
    /// Which field the modal wants focused (`None` in the destination
    /// stage), from the modal's own state.
    fn save_wants_field(&self) -> Option<usize> {
        let modal = self.save_ws.as_ref()?;
        wants_field(modal.field, modal.dest_selected)
    }

    /// Reconcile the two save-field entities with the modal each render:
    /// seed and focus them on `save_sync`, reflect a click-driven focus back
    /// into `field`, and release focus when the modal is gone or in its
    /// destination stage.
    pub(crate) fn sync_save_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name_focused = self.save_name.read(cx).focus_handle(cx).is_focused(window);
        let desc_focused = self.save_desc.read(cx).focus_handle(cx).is_focused(window);
        let wants = self.save_wants_field();
        let Some(modal) = self.save_ws.as_ref() else {
            if name_focused || desc_focused {
                window.focus(&self.focus_handle, cx);
            }
            if !self.save_name.read(cx).text().is_empty()
                || !self.save_desc.read(cx).text().is_empty()
            {
                self.save_name.update(cx, |i, cx| i.set_text("", cx));
                self.save_desc.update(cx, |i, cx| i.set_text("", cx));
            }
            return;
        };
        if self.save_sync {
            self.save_sync = false;
            let (name, desc) = (modal.name.clone(), modal.description.clone());
            if self.save_name.read(cx).text() != name {
                self.save_name.update(cx, |i, cx| i.set_text(name, cx));
            }
            if self.save_desc.read(cx).text() != desc {
                self.save_desc.update(cx, |i, cx| i.set_text(desc, cx));
            }
            match wants {
                Some(0) => window.focus(&self.save_name.read(cx).focus_handle(cx), cx),
                Some(_) => window.focus(&self.save_desc.read(cx).focus_handle(cx), cx),
                None => window.focus(&self.focus_handle, cx),
            }
            return;
        }
        // A click focused a field directly: follow it.
        if wants.is_some()
            && let Some(modal) = self.save_ws.as_mut()
        {
            if name_focused && modal.field != 0 {
                modal.field = 0;
            } else if desc_focused && modal.field != 1 {
                modal.field = 1;
            }
        }
    }

    /// The save-as-workspace modal, or an empty element while it is closed.
    pub fn render_save(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(modal) = self.save_ws.as_ref() else {
            return div().into_any_element();
        };
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let font = crate::renderer::chrome_font();
        let scale = self.scale();
        let line = self.renderer.chrome_cell_height / scale;
        let entity = cx.entity().downgrade();

        // Geometry, as `save_layout` computed it (logical px).
        let (surface_w, surface_h) = self.renderer.surface_size();
        let (win_w, win_h) = (surface_w as f32 / scale, surface_h as f32 / scale);
        let row_h = (line + 8.0).round();
        let caption_h = (line + 4.0).round();
        let panel_w = (win_w * 0.5).min(560.0).round();
        let dest_rows = if modal.dest_selected.is_some() { modal.dest_labels.len() } else { 0 };
        let dest_h = if dest_rows > 0 { caption_h + dest_rows as f32 * row_h } else { 0.0 };
        let panel_h = (PAD * 2.0 + row_h + 2.0 * (caption_h + row_h) + dest_h).round();
        let panel_y = ((win_h - panel_h) / 3.0).round().max(PAD);

        let editing_fields = modal.dest_selected.is_none();
        let text_size = px(font);
        for input in [&self.save_name, &self.save_desc] {
            input.update(cx, |i, _| i.set_text_size(Some(text_size)));
        }

        let field = |i: usize, caption: &'static str, input: gpui::Entity<crate::ui::Input>| {
            let focused = editing_fields && modal.field == i;
            let entity = entity.clone();
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(
                    div()
                        .h(px(caption_h))
                        .flex()
                        .items_end()
                        .pb(px(2.0))
                        .text_color(theme.muted_foreground)
                        .child(caption),
                )
                .child(
                    div()
                        .id(("save-field", i))
                        .h(px(row_h))
                        .w_full()
                        .rounded(px(RADIUS))
                        .bg(theme.foreground.opacity(if focused { 0.10 } else { 0.06 }))
                        .hover(|s| s.bg(theme.foreground.opacity(0.10)))
                        .flex()
                        .items_center()
                        .px(px(PAD))
                        .text_color(theme.foreground)
                        // A click focuses the field (the Input's tracked focus
                        // handle does that); it also steps back out of the
                        // destination stage, as the canvas did.
                        .on_mouse_down(MouseButton::Left, move |_ev, _win, app: &mut GpuiApp| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, cx| {
                                    if let Some(m) = this.save_ws.as_mut() {
                                        m.field = i;
                                        m.dest_selected = None;
                                    }
                                    this.request_redraw();
                                    cx.notify();
                                });
                            }
                        })
                        .child(input),
                )
        };

        let mut panel = panel_style(AlertDialog::new("save-workspace").open(true), &theme, chrome)
            .w(px(panel_w))
            .p(px(PAD))
            .gap(px(0.0))
            .top(px(panel_y))
            .scrim(crate::renderer::color(chrome.scrim, 0.30))
            .on_backdrop_click({
                let entity = entity.clone();
                move |_ev, _win, app| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.save_ws = None;
                            this.request_redraw();
                            cx.notify();
                        });
                    }
                }
            })
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(text_size)
            .child(
                div()
                    .h(px(row_h))
                    .flex()
                    .items_start()
                    .text_color(theme.muted_foreground)
                    .child("Save as workspace"),
            )
            .child(field(0, "Name", self.save_name.clone()))
            .child(field(1, "Description", self.save_desc.clone()));

        if let Some(selected) = modal.dest_selected {
            panel = panel.child(
                div()
                    .h(px(caption_h))
                    .flex()
                    .items_end()
                    .pb(px(2.0))
                    .text_color(theme.muted_foreground)
                    .child("Save to"),
            );
            // Rows span the panel's full width, so they pull back out of
            // the padding the fields sit in.
            let mut rows = div().flex().flex_col().w(px(panel_w)).ml(px(-PAD));
            for (i, label) in modal.dest_labels.iter().enumerate() {
                let entity = entity.clone();
                let is_selected = i == selected;
                rows = rows.child(
                    div()
                        .id(("save-dest", i))
                        .h(px(row_h))
                        .w_full()
                        .px(px(PILL_INSET))
                        .cursor_pointer()
                        .child(
                            div()
                                .size_full()
                                .rounded(px(RADIUS))
                                .when(is_selected, |d| d.bg(theme.primary.opacity(0.10)))
                                .when(!is_selected, |d| {
                                    d.hover(|s| s.bg(theme.foreground.opacity(0.06)))
                                })
                                .flex()
                                .items_center()
                                .px(px(PAD - PILL_INSET))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(theme.foreground)
                                .child(label.clone()),
                        )
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, cx| {
                                    if let Some(m) = this.save_ws.as_mut() {
                                        m.dest_selected = Some(i);
                                    }
                                    this.commit_save_workspace();
                                    this.save_sync = true;
                                    cx.notify();
                                });
                            }
                        }),
                );
            }
            panel = panel.child(rows);
        }

        panel.into_any_element()
    }
}

/// The field the modal wants focused: `field` while the text fields are being
/// edited, none once a destination row is selected (arrow keys own the list).
fn wants_field(field: usize, dest_selected: Option<usize>) -> Option<usize> {
    if dest_selected.is_some() { None } else { Some(field) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Focus follows the field index until the destination stage, which
    /// releases it so ↑/↓ reach the row list instead of a text field.
    #[test]
    fn destination_stage_releases_field_focus() {
        assert_eq!(wants_field(0, None), Some(0));
        assert_eq!(wants_field(1, None), Some(1));
        assert_eq!(wants_field(1, Some(0)), None);
    }
}
