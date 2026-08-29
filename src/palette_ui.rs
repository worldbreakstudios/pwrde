//! The command palette (⌘P) as a gpui element tree.
//!
//! It used to be canvas-painted (`Renderer::palette_overlay`) with its query
//! edited by hand in `on_key_down` and its rows hit-tested in
//! `overlay_click`. The search field is now the shared rcn `Input` entity
//! `App::modal_search` (bare, at the chrome font), whose text an observer
//! feeds into [`crate::palette::Palette::set_query`]; the rows are click
//! targets; the panel shares `modal_ui::panel_style` and is laid out with
//! the same numbers `picker::PickerLayout` used (panel width, padding,
//! search and row heights, the visible-row window around the selection).
//!
//! Keyboard navigation (Escape / Enter / ↑ / ↓ and the palette's own chord)
//! stays in `main.rs`: the root key listener still fires while the field is
//! focused, and the field owns editing.

use gpui::{
    AnyElement, App as GpuiApp, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, px,
    prelude::FluentBuilder as _,
};

use crate::App;
use crate::modal_ui::panel_style;
use crate::picker::{PANEL_PAD, PANEL_W, PickerLayout, ROW_H, SEARCH_H};
use crate::ui::AlertDialog;
use crate::ui::theme::Theme;

/// Placeholder the field shows while the query is empty.
pub(crate) const PALETTE_PLACEHOLDER: &str = "Run a command…";
/// Text inset inside the search box and the rows.
const TEXT_PAD: f32 = 12.0;
/// Inset of the selection / hover pill from a row's edges.
const PILL_INSET: f32 = 6.0;
/// Pill and search-box corner radius.
const RADIUS: f32 = 7.0;

impl App {
    /// The command palette, or an empty element while it is closed.
    pub fn render_palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(palette) = self.palette.as_ref() else {
            return div().into_any_element();
        };
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let font = crate::renderer::chrome_font();
        let scale = self.scale();
        let entity = cx.entity().downgrade();

        // The visible window of rows, exactly as the canvas scrolled it.
        let (surface_w, surface_h) = self.renderer.surface_size();
        let layout =
            PickerLayout::compute(surface_w, surface_h, scale, palette.rows.len(), palette.selected);
        let win_w = surface_w as f32 / scale;
        let panel_w = PANEL_W.min(win_w - 2.0 * PANEL_PAD).max(ROW_H);

        self.modal_search.update(cx, |i, _| i.set_text_size(Some(px(font))));

        let search = div()
            .h(px(SEARCH_H))
            .w_full()
            .rounded(px(RADIUS))
            .bg(theme.foreground.opacity(0.06))
            .flex()
            .items_center()
            .px(px(TEXT_PAD))
            .text_color(theme.foreground)
            .child(self.modal_search.clone());

        let mut rows = div().flex().flex_col().w_full();
        for i in layout.first_visible..(layout.first_visible + layout.visible) {
            let Some(action) = palette.rows.get(i) else { break };
            let selected = i == palette.selected;
            let entity = entity.clone();
            rows = rows.child(
                div()
                    .id(("palette-row", i))
                    .h(px(ROW_H))
                    .w_full()
                    .px(px(PILL_INSET))
                    .cursor_pointer()
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(app, |this, cx| {
                                let action = this.palette.as_mut().and_then(|p| {
                                    p.select(i);
                                    p.selected_action()
                                });
                                this.palette = None;
                                if let Some(action) = action {
                                    this.run_action(action);
                                }
                                this.request_redraw();
                                cx.notify();
                            });
                        }
                    })
                    .child(
                        div()
                            .size_full()
                            .rounded(px(RADIUS))
                            .when(selected, |d| d.bg(theme.primary.opacity(0.10)))
                            .when(!selected, |d| {
                                d.hover(|s| s.bg(theme.foreground.opacity(0.06)))
                            })
                            .flex()
                            .items_center()
                            .px(px(TEXT_PAD - PILL_INSET))
                            .gap(px(TEXT_PAD))
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(theme.foreground)
                                    .child(action.label()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.muted_foreground)
                                    .child(action.binding().display()),
                            ),
                    ),
            );
        }

        panel_style(AlertDialog::new("command-palette").open(true), &theme, chrome)
            .w(px(panel_w))
            .p(px(PANEL_PAD))
            .gap(px(0.0))
            .scrim(crate::renderer::color(chrome.scrim, 0.30))
            .on_backdrop_click(move |_ev, _win, app| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.palette = None;
                        this.request_redraw();
                        cx.notify();
                    });
                }
            })
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(font))
            .child(search)
            .child(rows)
            .into_any_element()
    }
}
