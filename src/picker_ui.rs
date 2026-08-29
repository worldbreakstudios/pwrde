//! The three pickers — directory (new group / tab / flyover target),
//! fork-source, and workspace-profile — as gpui element trees on the shared
//! [`crate::palette_ui::SearchModal`].
//!
//! They used to be the canvas "picker layer" (`Renderer::picker_overlay` /
//! `fork_overlay` / `profile_overlay`) with queries typed into their models
//! by hand and rows hit-tested through `picker::PickerLayout`. The query is
//! the shared rcn `Input` entity now (`App::modal_search`, fed into the open
//! model's `set_query` by an observer); rows, the dir picker's star gutter,
//! and the scrim are click targets. Geometry reproduces the canvas: the dir
//! and profile pickers use `PickerLayout`'s centered panel and visible-row
//! window, the fork picker its wider panel in the window's upper third.
//! Escape / Enter / ↑ / ↓ stay in `main.rs`'s `on_key_down`.

use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, div};

use crate::App;
use crate::palette_ui::{ListRow, SearchModal};
use crate::picker::{PANEL_PAD, PANEL_W, PickerLayout, PickerRow, ROW_H, SEARCH_H};

/// The dir picker's search placeholder.
pub(crate) const DIR_PLACEHOLDER: &str = "Search repos…";
/// The fork picker's filter placeholder.
pub(crate) const FORK_PLACEHOLDER: &str = "Filter branches…";

/// The profile picker's placeholder, naming the group being launched.
pub(crate) fn profile_placeholder(name: &str) -> String {
    format!("Launch {name} with…")
}
/// The fork panel's inner padding (the canvas used 12px here, not the
/// picker's 10px).
const FORK_PAD: f32 = 12.0;
/// Most fork rows the panel shows.
const FORK_MAX_ROWS: usize = 12;

impl App {
    /// Whichever picker is open — profile over fork over directory, the
    /// order the canvas painted them — or an empty element.
    pub fn render_pickers(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.profile_picker.is_some() {
            return self.render_profile_picker(cx);
        }
        if self.fork.is_some() {
            return self.render_fork_picker(cx);
        }
        if self.picker.is_some() {
            return self.render_dir_picker(cx);
        }
        div().into_any_element()
    }

    fn render_dir_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = self.picker.as_ref() else { return div().into_any_element() };
        let scale = self.scale();
        let (surface_w, surface_h) = self.renderer.surface_size();
        let layout =
            PickerLayout::compute(surface_w, surface_h, scale, picker.rows.len(), picker.selected);
        let win_w = surface_w as f32 / scale;
        let entity = cx.entity().downgrade();
        let (select_entity, star_entity) = (entity.clone(), entity.clone());

        SearchModal {
            id: "dir-picker",
            panel_w: PANEL_W.min(win_w - 2.0 * PANEL_PAD).max(ROW_H),
            top: None,
            header: None,
            search_h: SEARCH_H,
            row_h: ROW_H,
            rows: picker
                .rows
                .iter()
                .map(|row| match row {
                    PickerRow::Header(title) => ListRow::header(*title),
                    PickerRow::Entry(entry) => ListRow {
                        git: entry.is_git,
                        pinned: picker.is_pinned(&entry.path),
                        ..ListRow::entry(entry.label.clone())
                    },
                })
                .collect(),
            selected: picker.selected,
            first_visible: layout.first_visible,
            visible: layout.visible,
            gutters: true,
            on_select: Rc::new(move |i, app| {
                if let Some(entity) = select_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        let is_entry = this.picker.as_mut().is_some_and(|p| {
                            p.select(i);
                            matches!(p.rows.get(i), Some(PickerRow::Entry(_)))
                        });
                        if is_entry {
                            this.confirm_picker();
                        }
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
            on_star: Some(Rc::new(move |i, app| {
                if let Some(entity) = star_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        if let Some(p) = this.picker.as_mut() {
                            p.select(i);
                            if let Some(PickerRow::Entry(entry)) = p.rows.get(i).cloned() {
                                p.toggle_pin(&entry.path);
                            }
                        }
                        this.request_redraw();
                        cx.notify();
                    });
                }
            })),
            on_dismiss: Rc::new(move |app| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.cancel_picker();
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
        }
        .render(self, cx)
    }

    fn render_fork_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(fork) = self.fork.as_ref() else { return div().into_any_element() };
        let scale = self.scale();
        let (surface_w, surface_h) = self.renderer.surface_size();
        let (win_w, win_h) = (surface_w as f32 / scale, surface_h as f32 / scale);
        // The canvas sized this panel off the chrome line height, not the
        // picker's fixed row height.
        let line = self.renderer.chrome_cell_height / scale;
        let row_h = (line + 8.0).round();
        let visible = fork.rows.len().min(FORK_MAX_ROWS);
        let panel_w = (win_w * 0.5).min(560.0).round();
        let panel_h = (row_h * (visible as f32 + 2.0) + FORK_PAD * 2.0).round();
        let top = ((win_h - panel_h) / 3.0).round().max(FORK_PAD);
        let entity = cx.entity().downgrade();
        let select_entity = entity.clone();

        SearchModal {
            id: "fork-picker",
            panel_w,
            top: Some(top),
            header: Some(format!("fork {} from…", fork.name).into()),
            search_h: row_h,
            row_h,
            rows: fork.rows.iter().map(|e| ListRow::entry(e.label.clone())).collect(),
            selected: fork.selected,
            first_visible: 0,
            visible,
            gutters: false,
            on_select: Rc::new(move |i, app| {
                if let Some(entity) = select_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        if let Some(f) = this.fork.as_mut() {
                            f.select(i);
                        }
                        this.confirm_fork();
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
            on_star: None,
            on_dismiss: Rc::new(move |app| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        // Outside click steps back to the dir picker.
                        this.fork = None;
                        this.set_picker(crate::picker::Picker::new());
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
        }
        .render(self, cx)
    }

    fn render_profile_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pp) = self.profile_picker.as_ref() else { return div().into_any_element() };
        let scale = self.scale();
        let (surface_w, surface_h) = self.renderer.surface_size();
        let layout = PickerLayout::compute(surface_w, surface_h, scale, pp.rows.len(), pp.selected);
        let win_w = surface_w as f32 / scale;
        let entity = cx.entity().downgrade();
        let select_entity = entity.clone();

        SearchModal {
            id: "profile-picker",
            panel_w: PANEL_W.min(win_w - 2.0 * PANEL_PAD).max(ROW_H),
            top: None,
            header: None,
            search_h: SEARCH_H,
            row_h: ROW_H,
            rows: pp
                .rows
                .iter()
                .map(|e| ListRow {
                    detail: (!e.detail.is_empty()).then(|| e.detail.clone().into()),
                    ..ListRow::entry(e.label.clone())
                })
                .collect(),
            selected: pp.selected,
            first_visible: layout.first_visible,
            visible: layout.visible,
            gutters: false,
            on_select: Rc::new(move |i, app| {
                if let Some(entity) = select_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        if let Some(p) = this.profile_picker.as_mut() {
                            p.select(i);
                        }
                        this.confirm_profile();
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
            on_star: None,
            on_dismiss: Rc::new(move |app| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.cancel_profile();
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }),
        }
        .render(self, cx)
    }
}
