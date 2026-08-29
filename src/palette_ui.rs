//! Search-list modals as gpui element trees: the command palette (⌘P) here,
//! and — through [`SearchModal`] — the directory / fork-source /
//! workspace-profile pickers in `picker_ui`.
//!
//! These used to be canvas-painted (`Renderer::palette_overlay` and the three
//! `*_overlay` picker painters) with their queries edited by hand in
//! `on_key_down` and their rows hit-tested in `overlay_click`. The search
//! field is now the shared rcn `Input` entity `App::modal_search` (bare, at
//! the chrome font), whose text an observer feeds into the open model's
//! `set_query`; the rows are click targets; the panel shares
//! `modal_ui::panel_style` and is laid out with the same numbers the canvas
//! used (panel width, padding, search and row heights, the visible-row
//! window around the selection).
//!
//! Keyboard navigation (Escape / Enter / ↑ / ↓ and the palette's own chord)
//! stays in `main.rs`: the root key listener still fires while the field is
//! focused, and the field owns editing.

use std::rc::Rc;

use gpui::{
    AnyElement, App as GpuiApp, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
    prelude::FluentBuilder as _,
};

use crate::App;
use crate::modal_ui::panel_style;
use crate::picker::{PANEL_PAD, PANEL_W, PickerLayout, ROW_H, SEARCH_H};
use crate::ui::AlertDialog;
use crate::ui::theme::Theme;

/// Placeholder the palette's field shows while the query is empty.
pub(crate) const PALETTE_PLACEHOLDER: &str = "Run a command…";
/// Text inset inside the search box and the rows.
pub(crate) const TEXT_PAD: f32 = 12.0;
/// Inset of the selection / hover pill from a row's edges.
const PILL_INSET: f32 = 6.0;
/// Pill and search-box corner radius.
const RADIUS: f32 = 7.0;
/// Gold of the pinned star, as the canvas drew it.
const STAR: (u8, u8, u8) = (205, 150, 35);

/// One row of a [`SearchModal`].
pub(crate) struct ListRow {
    pub label: SharedString,
    /// Dim, right-aligned annotation (a profile's `description · source`).
    pub detail: Option<SharedString>,
    /// A dim section caption: painted, never selectable.
    pub header: bool,
    /// Show the git glyph in the right-hand gutter.
    pub git: bool,
    /// Show the pinned star in the far-right gutter.
    pub pinned: bool,
}

impl ListRow {
    pub(crate) fn entry(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), detail: None, header: false, git: false, pinned: false }
    }

    pub(crate) fn header(label: impl Into<SharedString>) -> Self {
        Self { header: true, ..Self::entry(label) }
    }
}

/// A row-index handler shared by the row and gutter click targets.
pub(crate) type RowHandler = Rc<dyn Fn(usize, &mut GpuiApp)>;

/// The panel every search-list modal shares: chrome scrim + card, an optional
/// header line, the shared search field in a tinted box, then the visible
/// window of rows with the accent selection pill.
pub(crate) struct SearchModal {
    pub id: &'static str,
    /// Panel width, logical px.
    pub panel_w: f32,
    /// `Some(y)` pins the panel's top edge (the fork picker sits in the
    /// upper third); `None` centers it like the dir picker and palette.
    pub top: Option<f32>,
    /// A dim caption line above the search box ("fork <name> from…").
    pub header: Option<SharedString>,
    pub search_h: f32,
    pub row_h: f32,
    pub rows: Vec<ListRow>,
    pub selected: usize,
    /// The visible window of `rows`, as the canvas scrolled it.
    pub first_visible: usize,
    pub visible: usize,
    /// Reserve the two right-hand gutters (git glyph, star) on every row so
    /// labels clip short of them, and make the star gutter a click target.
    pub gutters: bool,
    pub on_select: RowHandler,
    pub on_star: Option<RowHandler>,
    pub on_dismiss: Rc<dyn Fn(&mut GpuiApp)>,
}

impl SearchModal {
    pub(crate) fn render(self, app: &App, cx: &mut Context<App>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let font = crate::renderer::chrome_font();
        app.modal_search.update(cx, |i, _| i.set_text_size(Some(px(font))));

        let search = div()
            .h(px(self.search_h))
            .w_full()
            .rounded(px(RADIUS))
            .bg(theme.foreground.opacity(0.06))
            .flex()
            .items_center()
            .px(px(TEXT_PAD))
            .text_color(theme.foreground)
            .child(app.modal_search.clone());

        // Rows span the panel's full width (the canvas laid them out at
        // `panel.x`), so the column pulls back out of the padding.
        let mut rows = div().flex().flex_col().w(px(self.panel_w)).ml(px(-PANEL_PAD));
        let end = (self.first_visible + self.visible).min(self.rows.len());
        for (i, row) in self.rows.iter().enumerate().take(end).skip(self.first_visible) {
            if row.header {
                rows = rows.child(
                    div()
                        .h(px(self.row_h))
                        .w_full()
                        .flex()
                        .items_center()
                        .px(px(TEXT_PAD))
                        .text_color(theme.muted_foreground)
                        .child(row.label.clone()),
                );
                continue;
            }
            let selected = i == self.selected;
            let on_select = self.on_select.clone();
            let mut pill = div()
                .size_full()
                .rounded(px(RADIUS))
                .when(selected, |d| d.bg(theme.primary.opacity(0.10)))
                .when(!selected, |d| d.hover(|s| s.bg(theme.foreground.opacity(0.06))))
                .flex()
                .items_center()
                .pl(px(TEXT_PAD - PILL_INSET))
                .when(!self.gutters, |d| d.pr(px(TEXT_PAD - PILL_INSET)).gap(px(TEXT_PAD)))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(theme.foreground)
                        .child(row.label.clone()),
                );
            if let Some(detail) = row.detail.clone() {
                pill = pill.child(div().flex_none().text_color(theme.muted_foreground).child(detail));
            }
            if self.gutters {
                // Git glyph cell, then the star cell, each one row tall.
                let gutter = self.row_h;
                pill = pill.child(
                    div()
                        .flex_none()
                        .w(px(gutter))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.primary)
                        .when(row.git, |d| d.child("\u{e0a0}")),
                );
                let on_star = self.on_star.clone();
                pill = pill.child(
                    div()
                        .id(("search-star", i))
                        .flex_none()
                        // The star cell straddles the pill's inset, matching
                        // the canvas `star_rect` at the row's right edge.
                        .w(px(gutter - PILL_INSET))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(crate::renderer::color(STAR, 1.0))
                        .when(row.pinned, |d| d.child("★"))
                        .when_some(on_star, |d, on_star| {
                            d.on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                app.stop_propagation();
                                on_star(i, app);
                            })
                        }),
                );
            }
            rows = rows.child(
                div()
                    .id(("search-row", i))
                    .h(px(self.row_h))
                    .w_full()
                    .px(px(PILL_INSET))
                    .cursor_pointer()
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        on_select(i, app)
                    })
                    .child(pill),
            );
        }

        let on_dismiss = self.on_dismiss.clone();
        let mut panel = panel_style(AlertDialog::new(self.id).open(true), &theme, chrome)
            .w(px(self.panel_w))
            .p(px(PANEL_PAD))
            .gap(px(0.0))
            .scrim(crate::renderer::color(chrome.scrim, 0.30))
            .on_backdrop_click(move |_ev, _win, app| on_dismiss(app))
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(font));
        if let Some(top) = self.top {
            panel = panel.top(px(top));
        }
        if let Some(header) = self.header {
            panel = panel.child(
                div()
                    .h(px(self.row_h))
                    .w_full()
                    .flex()
                    .items_center()
                    .text_color(theme.muted_foreground)
                    .child(header),
            );
        }
        panel.child(search).child(rows).into_any_element()
    }
}

impl App {
    /// The command palette, or an empty element while it is closed.
    pub fn render_palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(palette) = self.palette.as_ref() else {
            return div().into_any_element();
        };
        let scale = self.scale();
        let (surface_w, surface_h) = self.renderer.surface_size();
        let layout =
            PickerLayout::compute(surface_w, surface_h, scale, palette.rows.len(), palette.selected);
        let win_w = surface_w as f32 / scale;
        let entity = cx.entity().downgrade();
        let select_entity = entity.clone();

        SearchModal {
            id: "command-palette",
            panel_w: PANEL_W.min(win_w - 2.0 * PANEL_PAD).max(ROW_H),
            top: None,
            header: None,
            search_h: SEARCH_H,
            row_h: ROW_H,
            rows: palette
                .rows
                .iter()
                .map(|a| ListRow { detail: Some(a.binding().display().into()), ..ListRow::entry(a.label()) })
                .collect(),
            selected: palette.selected,
            first_visible: layout.first_visible,
            visible: layout.visible,
            gutters: false,
            on_select: Rc::new(move |i, app| {
                if let Some(entity) = select_entity.upgrade() {
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
            }),
            on_star: None,
            on_dismiss: Rc::new(move |app| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.palette = None;
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
        }
        .render(self, cx)
    }
}
