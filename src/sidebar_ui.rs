//! The sessions sidebar as a gpui element tree over the canvas: the flat
//! region ground, the sessions list (header block + two-line rows in the
//! GANTRY mock's style), the pinned-bubble strip, the Settings page's rows,
//! and the floating "Show sessions" button while everything is hidden. The
//! folders card beside the list lives in [`crate::folders_ui`].
//!
//! Geometry lives in [`crate::workspace`] (`sessions_list_rect`,
//! `sidebar_rows_filtered` / `sidebar_row_rect`), and row presses and drags
//! are still resolved on the canvas mouse path in `main.rs` against those
//! same rects. That is deliberate — the rects are the single authority that
//! keeps painting, hit-testing and PTY resize in agreement, so this tree is
//! *absolutely positioned to match them* rather than reimplementing layout or
//! drag-and-drop in gpui's paradigm. The header chips, the Settings rows and
//! the collapsed-state button are gpui-owned click targets that `occlude()`
//! the canvas.
//!
//! Colors come from the live chrome theme ([`crate::ui::theme::Theme`]), never
//! from the mock's hardcoded palette, so the region reads correctly in both
//! light and dark polarity.
//!
//! Pinned groups move out of the rows into an iMessage-style strip above
//! them — one avatar bubble per pin, painted on this same absolute layer
//! straight from [`crate::workspace::pinned_bubble_rect`], so painting and the
//! hit test in `main.rs` never disagree.
use gpui::{
    AnyElement, App as GpuiApp, BoxShadow, ClickEvent, Context, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, linear_color_stop, linear_gradient, point,
    prelude::FluentBuilder as _, px,
};

use std::time::SystemTime;

use crate::App;
use crate::git_context::{PrRollup, derive_rollup};
use crate::sidebar_card::{
    CardAvatar, avatar_for, diffstat_line, relative_time,
};
use crate::ui::theme::Theme;

/// Horizontal padding inside a session row (mock: `10px 12px`).
const ROW_PAD: f32 = 12.0;
/// Unread / attention dot under a row's PR-state icon.
const UNREAD_DOT: f32 = 6.0;
/// Corner radius of a session row's selection / hover fill (mock: 9px).
const ROW_RADIUS: f32 = 9.0;
/// Side of the PR-state icon on a row's first line (mock: 11px, tinted
/// with the PR palette below).
const STATUS_ICON: f32 = 12.0;
/// Side of the SVG glyph inside a header chip (mock: 17px chips).
const HEADER_ICON: f32 = 17.0;
/// Extra left inset for an indented simple row (a section member).
const ROW_INDENT: f32 = 14.0;
/// Placeholder text of the Settings page search box; `main.rs` reads it
/// when it builds the rcn `Input`.
pub(crate) const SEARCH_SETTINGS_PLACEHOLDER: &str = "Search settings";

/// Scale sidebar chrome dimensions with the configured application text size.
///
/// Every literal below is written at the default font size; the Accessibility
/// setting "App text size" (`appearance.font_size`) covers the sidebar, so type
/// and the geometry derived from it multiply through here. Row heights scale by
/// the same factor in [`crate::workspace`], which is what keeps painting and
/// hit-testing agreeing.
fn font_scale() -> f32 {
    crate::workspace::row_font_scale()
}

/// `value`, written at the default font size, scaled by [`font_scale`].
pub(crate) fn scaled(value: f32) -> f32 {
    value * font_scale()
}

/// A left-press handler for a sidebar row: stops the press at the element
/// (so the canvas mouse path never re-resolves it), records the pointer the
/// way the canvas does, commits an open section rename first — a click
/// anywhere always did, so the editor never lingers and swallows keystrokes
/// — then runs `act` on the app. Any drag the press arms is still driven by
/// the canvas `on_mouse_move` / `on_mouse_up`.
pub(crate) fn press(
    entity: gpui::WeakEntity<App>,
    act: impl Fn(&mut App, &MouseDownEvent, &mut Context<App>) + 'static,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut GpuiApp) + 'static {
    move |ev, _win, app| {
        app.stop_propagation();
        if let Some(entity) = entity.upgrade() {
            entity.update(app, |this, cx| {
                this.note_pointer(ev);
                if this.editing_section.is_some() {
                    this.commit_section_rename();
                }
                act(this, ev, cx);
                cx.notify();
            });
        }
    }
}

impl App {
    pub fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));

        let w = self.sidebar_w();
        if w <= 0.0 {
            return div().into_any_element();
        }

        let theme = Theme::of(cx).clone();

        // The flyover ("global terminals") is canvas-painted and spans the full
        // window width, but this region is a later element sibling — so it was
        // drawn straight over the flyover's left end, hiding whichever pane sat
        // there. Stop the sidebar above the flyover instead: `None` means it is
        // not intruding, `Some(0.0)` that it covers the sidebar outright.
        let ceiling = self.flyover_ceiling();
        if ceiling == Some(0.0) {
            return div().into_any_element();
        }

        let clip_height = |d: gpui::Div| match ceiling {
            Some(limit) => d.h(px(limit)),
            None => d.h_full(),
        };

        // One layer, exactly the region wide, holding the flat ground, the
        // folders card, the sessions list (header + clipped rows) and the
        // drop feedback. Clipped, not just shortened: the layers are
        // absolutely positioned against the whole window, so the height has
        // to actually cut them off.
        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w(px(w))
            .overflow_hidden()
            .map(clip_height)
            .child(region_ground(&theme, w))
            .when(self.folders_visible(), |d| d.child(self.render_folders_card(&theme, cx)))
            .child(self.clipped_row_layer(&theme, cx))
            .child(self.drop_feedback_layer(&theme))
            .child(self.sessions_header(&theme, cx.entity().downgrade()))
            .into_any_element()
    }

    /// The floating "Show sessions" chip beside the relocated traffic lights
    /// while the whole region is hidden (⌘S): the inward-bracket twin of the
    /// header's "Focus terminals" glyph, bare over the top-left tile's tab
    /// strip (which `workspace::COLLAPSED_STRIP_INSET` keeps clear of it),
    /// reopening the sidebar. Empty while the sidebar is open.
    pub fn render_collapsed_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.sidebar_collapsed || self.flyover_ceiling() == Some(0.0) {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let rect = crate::workspace::show_sessions_button(1.0);
        let hovered = self.sidebar_cursor().is_some_and(|(x, y)| rect.contains(x, y));
        let modal = self.modal_overlay_open();
        let entity = cx.entity().downgrade();
        icon_chip(&theme, &rect, hovered, false, crate::ui::assets::ICON_MINIMIZE)
            .id("show-sessions")
            .occlude()
            .when(!modal, |d| {
                d.cursor_pointer().on_click(
                    move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(app, |this, cx| {
                                this.toggle_sidebar();
                                cx.notify();
                            });
                        }
                    },
                )
            })
            .into_any_element()
    }

    /// The sessions list rect in logical px (the element tree's unit), so
    /// every row helper here and the physical-px hit tests in `main.rs`
    /// derive from the same `workspace::sessions_list_rect`.
    pub(crate) fn list_rect(&self) -> crate::workspace::LayoutRect {
        crate::workspace::sessions_list_rect(
            self.sidebar_expanded_w,
            self.folders_visible(),
            self.logical_height(),
            1.0,
        )
    }

    /// The window's logical height, rounded like the row rects are.
    pub(crate) fn logical_height(&self) -> u32 {
        let (_, surface_h) = self.renderer.surface_size();
        (surface_h as f32 / self.scale()).round() as u32
    }


    /// The region that drags the window: the titlebar strip above the
    /// sidebar, or — with the sidebar folded away — the traffic-light corner
    /// the top-left tile's strip cedes. It is an element now, so the
    /// hit-test is gpui's; the move itself still goes through
    /// `Window::start_window_move` from the press (the window is opened with
    /// `app_owns_titlebar_drag`, and gpui's `WindowControlArea::Drag`
    /// hitboxes are a no-op on macOS in the pinned rev). The header chips
    /// paint above it and occlude, so a chip press never drags.
    pub fn render_window_drag_zones(&self) -> AnyElement {
        let zone = if self.sidebar_collapsed {
            crate::workspace::collapsed_drag_zone(1.0)
        } else {
            crate::workspace::titlebar(1.0, self.sidebar_w())
        };
        div()
            .absolute()
            .left(px(zone.x))
            .top(px(zone.y))
            .w(px(zone.w))
            .h(px(zone.h))
            .on_mouse_down(MouseButton::Left, |_ev, window: &mut Window, app: &mut GpuiApp| {
                // Native traffic-light buttons handle their own clicks; a
                // press anywhere else in the strip drags the window.
                app.stop_propagation();
                window.start_window_move();
            })
            .into_any_element()
    }

    /// The Sessions empty state: a centered "New group" pill with its ⇧⌘T
    /// hint, in the terminal area.
    ///
    /// Element-owned: the pill is a real gpui button (hover + click resolve
    /// here, not on the canvas mouse path), positioned at
    /// [`crate::workspace::empty_state_cta`] / [`empty_state_hint`] so it
    /// lands exactly where the canvas used to paint it. Returns an empty
    /// element off the Sessions page, or once the workspace has a tile.
    pub fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.page != crate::Page::Sessions
            || !self.is_empty_state()
        {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();

        // `empty_state_cta` takes device pixels and a scale; this tree is
        // logical, so hand it the logical surface at scale 1.0 the way
        // `list_rect` does.
        let (surface_w, surface_h) = self.renderer.surface_size();
        let scale = self.scale();
        let width = (surface_w as f32 / scale).round() as u32;
        let height = (surface_h as f32 / scale).round() as u32;
        let cta = crate::workspace::empty_state_cta(width, height, 1.0, self.sidebar_w());
        let hint = crate::workspace::empty_state_hint(width, height, 1.0, self.sidebar_w());

        // Same glass as the canvas `Renderer::pill`: dark chrome lifts the
        // card fill 30% toward white so it reads as light glass on the
        // vibrancy ground; the rim is the ink at 0.28.
        let fill = if theme.dark {
            let lift = |v: f32| v + (1.0 - v) * 0.30;
            let rgb = gpui::Rgba::from(theme.card);
            Hsla::from(gpui::Rgba { r: lift(rgb.r), g: lift(rgb.g), b: lift(rgb.b), a: 1.0 })
        } else {
            theme.card
        };
        let rim = theme.foreground.opacity(0.28);
        let font = crate::renderer::chrome_font();
        let entity = cx.entity().downgrade();
        // A modal's own scrim sits above this tree; the pill only goes
        // inert underneath it.
        let modal = self.modal_overlay_open();

        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .child(
                div()
                    .id("empty-state-cta")
                    .absolute()
                    .left(px(cta.x))
                    .top(px(cta.y))
                    .w(px(cta.w))
                    .h(px(cta.h))
                    .occlude()
                    .when(!modal, |d| d.cursor_pointer())
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ROW_RADIUS))
                    .border(px(0.5))
                    .border_color(rim)
                    .bg(fill.opacity(0.62))
                    .hover(move |s| s.bg(fill.opacity(0.85)))
                    .shadow(vec![BoxShadow {
                        color: gpui::black().opacity(0.10),
                        offset: point(px(0.0), px(1.0)),
                        blur_radius: px(3.0),
                        spread_radius: px(0.0),
                        inset: false,
                    }])
                    .font_family(crate::renderer::FONT_FAMILY)
                    .text_size(px(font))
                    .text_color(theme.foreground)
                    .child("New group")
                    .when(!modal, |d| {
                        d.on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, cx| {
                                    this.open_picker();
                                    cx.notify();
                                });
                            }
                        })
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(hint.x))
                    .top(px(hint.y))
                    .w(px(hint.w))
                    .h(px(hint.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(crate::renderer::FONT_FAMILY)
                    .text_size(px(font))
                    .text_color(theme.muted_foreground.opacity(0.9))
                    .child("press ⇧⌘T"),
            )
            .into_any_element()
    }

    /// How far down the sidebar may paint before it would cover the flyover
    /// panel, in logical pixels.
    ///
    /// `None` when the flyover is not showing in this window — closed, or
    /// popped out into its own — so the sidebar runs full height. `Some(0.0)`
    /// when the flyover is maximized and owns the window, in which case the
    /// sidebar should not paint at all.
    ///
    /// Read every frame rather than cached: `flyover_rect_now` follows the
    /// slide animation, so the sidebar's floor tracks the panel on the way in
    /// and out instead of snapping once it lands.
    pub(crate) fn flyover_ceiling(&self) -> Option<f32> {
        if !self.flyover_open || self.flyover_windowed || self.flyover_tabs.is_empty() {
            return None;
        }
        let panel = self.flyover_rect_now();
        // Device pixels from the renderer's geometry; this tree is logical.
        let top = (panel.y / self.scale()).max(0.0);
        let (_, surface_h) = self.renderer.surface_size();
        let height = surface_h as f32 / self.scale();
        // Fully off-screen (anim at rest, or a degenerate rect) is not an
        // intrusion at all.
        if top >= height {
            return None;
        }
        Some(top)
    }

    /// Drag-and-drop feedback for the sidebar: the landing highlight and the
    /// insertion line.
    ///
    /// `main.rs` used to push both as canvas `fg_quads`, but the panel is a
    /// later element sibling and occluded them, so reordering a group or
    /// dropping it into a folder showed nothing at all. Only the pixels moved:
    /// this reads the very same [`crate::DropTarget`] `main.rs` resolves from
    /// the live [`crate::Drag`] and paints at exactly the rect
    /// `App::drop_hint` returns (asked for logical px, since gpui lays out at
    /// scale 1.0). Drops are still resolved on the canvas mouse path — no gpui
    /// handlers here — and canvas-side targets stay on the canvas.
    fn drop_feedback_layer(&self, theme: &Theme) -> gpui::Div {
        let layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        let Some(target) = self.current_drop_target().filter(|t| t.in_sidebar()) else {
            return layer;
        };
        let Some(rect) = self.drop_hint(target, 1.0) else {
            return layer;
        };
        // An insertion line is a 2px sliver; a landing highlight is a whole
        // row. The line reads as a solid accent rule, the highlight as the
        // canvas's translucent accent wash plus a hairline so its edges show
        // against an already-tinted row.
        let line = matches!(
            target,
            crate::DropTarget::SidebarInsert { .. }
        );
        let mark = div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h));
        layer.child(if line {
            mark.rounded(px(rect.h / 2.0)).bg(theme.primary)
        } else {
            mark.rounded(px(ROW_RADIUS))
                .bg(theme.primary.opacity(0.28))
                .border_1()
                .border_color(theme.primary.opacity(0.75))
        })
    }

    /// The sessions list's header: the title block ("All sessions", the
    /// folder's name, or "Settings", over the row count) and the chip
    /// cluster — "Show folders" at the left while the card is hidden, then
    /// Focus terminals, ＋ and the Settings gear at the right. Every chip is a
    /// real gpui click target that `occlude()`s the canvas; hover reads off
    /// the canvas cursor so the chips light up under the same pointer the
    /// rows use.
    fn sessions_header(&self, theme: &Theme, entity: gpui::WeakEntity<Self>) -> gpui::Div {
        let list = self.list_rect();
        let folders = self.folders_visible();
        let chips = crate::workspace::sessions_header_chips(&list, folders, 1.0);
        let cur = self.sidebar_cursor();
        let hovered = |r: &crate::workspace::LayoutRect| cur.is_some_and(|(x, y)| r.contains(x, y));
        let modal = self.modal_overlay_open();
        let handler = |entity: gpui::WeakEntity<Self>, act: fn(&mut Self)| {
            move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        act(this);
                        cx.notify();
                    });
                }
            }
        };

        let settings = self.page == crate::Page::Settings;
        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();

        if let Some(show) = chips.show_folders {
            layer = layer.child(
                icon_chip(theme, &show, hovered(&show), false, crate::ui::assets::ICON_PANEL_LEFT)
                    .id("sidebar-show-folders")
                    .occlude()
                    .when(!modal, |c| {
                        c.cursor_pointer()
                            .on_click(handler(entity.clone(), |this| this.toggle_folders()))
                    }),
            );
        }
        layer
            .child(
                icon_chip(
                    theme,
                    &chips.focus,
                    hovered(&chips.focus),
                    false,
                    crate::ui::assets::ICON_MAXIMIZE,
                )
                .id("sidebar-collapse")
                .occlude()
                .when(!modal, |c| {
                    c.cursor_pointer()
                        .on_click(handler(entity.clone(), |this| this.toggle_sidebar()))
                }),
            )
            .child(
                icon_chip(theme, &chips.plus, hovered(&chips.plus), false, crate::ui::assets::ICON_PLUS)
                    .id("sidebar-new-group")
                    .occlude()
                    .when(!modal, |c| {
                        c.cursor_pointer()
                            .on_click(handler(entity.clone(), |this| this.open_picker()))
                    }),
            )
            .child(
                icon_chip(theme, &chips.gear, hovered(&chips.gear), settings, crate::ui::assets::ICON_SETTINGS)
                    .id("sidebar-settings")
                    .occlude()
                    .when(!modal, |c| {
                        c.cursor_pointer().on_click(handler(entity, |this| {
                            let page = if this.page == crate::Page::Settings {
                                crate::Page::Sessions
                            } else {
                                crate::Page::Settings
                            };
                            this.set_page(page);
                        }))
                    }),
            )
    }


    fn clipped_row_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let list = self.list_rect();
        // Rows start below the list header — exactly where
        // `workspace::sidebar_row_rect` and `tab_rect` start their stacks —
        // and stop at the list's bottom edge, the region's padding.
        let top = list.y + crate::workspace::SESSIONS_HEADER_H;
        let bottom = (list.y + list.h).max(top);
        let height = self.logical_height() as f32;
        let w = self.sidebar_w();

        div()
            .absolute()
            .left(px(list.x))
            .top(px(top))
            .w(px(list.w.max(0.0)))
            .h(px(bottom - top))
            .overflow_hidden()
            .child(
                self.sidebar_row_layer(theme, cx)
                    .left(px(-list.x))
                    .top(px(-top))
                    .w(px(w))
                    .h(px(height)),
            )
    }


    /// The row layer for whichever page is showing.
    ///
    /// Every page draws its rows into the same absolutely positioned layer
    /// over the panel, but they do not share a row *vocabulary*: Sessions
    /// gets card-height preview rows laid out by
    /// [`crate::workspace::sidebar_row_rect`], while Settings
    /// gets one-line rows at [`crate::workspace::tab_rect`] — the very
    /// rects `main.rs` already hit-tests for those pages. The split is
    /// [`App::card_rows`], the same predicate the geometry helpers take.
    fn sidebar_row_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let entity = cx.entity().downgrade();
        if self.card_rows() {
            return self.card_row_layer(theme, entity);
        }
        match self.page {
            crate::Page::Settings => self.settings_row_layer(theme, cx),
            // Any page without rows of its own still gets the shell.
            _ => div().absolute().left(px(0.0)).top(px(0.0)).size_full(),
        }
    }

    /// The Settings rows: the live search box in slot 0 and one tab per
    /// [`crate::pages::Section`] at slot `i + 1`, the shift `main.rs`'s
    /// `Page::Settings` mouse branch makes to leave room for the box.
    fn settings_row_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let list = self.list_rect();
        let entity = cx.entity().downgrade();
        let mut layer = div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .child(self.settings_search_row(theme, cx));
        for (i, section) in crate::pages::Section::ALL.iter().enumerate() {
            let rect = crate::workspace::tab_rect(i + 1, 1.0, &list);
            let active = *section == self.section;
            let section = *section;
            let entity = entity.clone();
            layer = layer.child(
                self.simple_row(theme, &rect, section.label(), active, false, true)
                    .id(("settings-section", i))
                    .occlude()
                    .cursor_pointer()
                    .on_click(move |_ev: &ClickEvent, win: &mut Window, app: &mut GpuiApp| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(app, |this, cx| {
                                this.section = section;
                                this.recording = None;
                                this.clear_settings_search(cx);
                                this.blur_settings_search(win, cx);
                                cx.notify();
                            });
                        }
                    }),
            );
        }
        layer
    }

    /// The Settings search box, sitting in slot 0 where
    /// [`crate::workspace::settings_search_rect`] expects it. The field
    /// itself is the rcn [`crate::ui::Input`] entity in `App::settings_search`
    /// (bare, at the row's type size), so typing, selection and the caret are
    /// the framework's; this row supplies the sidebar shell around it: the
    /// active-row treatment while focused, and a painted fill even when
    /// unfocused (unlike a row, which is transparent until hovered) because
    /// it is an affordance, not a selection. It occludes the canvas, so the
    /// click that focuses it never reaches the canvas mouse path.
    fn settings_search_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let rect = crate::workspace::settings_search_rect(1.0, &self.list_rect());
        let focused = self.settings_search_focus;
        // Same lift as `simple_row`: dark chrome's card token needs it to read
        // as light glass over the panel material.
        let fill = if theme.dark {
            shade(theme.card, 0.12)
        } else {
            theme.card
        };
        // The row's type size tracks the Accessibility text setting; keep the
        // field's in step (no-op when unchanged).
        let text_size = px(scaled(12.0));
        self.settings_search.update(cx, |input, _| input.set_text_size(Some(text_size)));

        div()
            .id("settings-search")
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .occlude()
            .rounded(px(ROW_RADIUS))
            .when(focused, |d| {
                d.bg(fill.opacity(0.78))
                    .border_1()
                    .border_color(theme.foreground.opacity(0.28))
                    .shadow(vec![BoxShadow {
                        color: gpui::black().opacity(if theme.dark { 0.35 } else { 0.12 }),
                        offset: point(px(0.0), px(1.0)),
                        blur_radius: px(4.0),
                        spread_radius: px(0.0),
                        inset: false,
                    }])
            })
            .when(!focused, |d| {
                d.bg(theme.muted.opacity(if theme.dark { 0.5 } else { 0.7 }))
            })
            .flex()
            .items_center()
            .pl(px(scaled(ROW_PAD)))
            .pr(px(scaled(ROW_PAD)))
            .child(self.settings_search.clone())
    }


    fn card_row_layer(&self, theme: &Theme, entity: gpui::WeakEntity<Self>) -> gpui::Div {
        let rows = self.sidebar_rows();
        let list = self.list_rect();
        let hover = self.sidebar_cursor();
        let active =
            crate::workspace::active_row_index(&rows, self.active);

        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        for (i, row) in rows.iter().enumerate() {
            let crate::workspace::SidebarRow { ws_idx } = *row;
            let Some(ws) = self.workspaces.get(ws_idx) else {
                continue;
            };
            let rect = crate::workspace::sidebar_row_rect(&rows, i, &self.workspaces, self.folder_filter, 1.0, &list);
            let selected = active == Some(i);
            let last = i + 1 == rows.len();
            layer = layer.child(
                self.session_row(theme, ws, &rect, selected, last, hover).on_mouse_down(
                    MouseButton::Left,
                    press(entity.clone(), move |this, _ev, _cx| this.press_group_row(ws_idx)),
                ),
            );
        }
        if rows.is_empty() && self.folder_filter.is_some() {
            // A folder with no (unpinned) members: say so where its first
            // row would sit, so the list never reads as broken.
            let rect = crate::workspace::sidebar_row_rect(&rows, 0, &self.workspaces, self.folder_filter, 1.0, &list);
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(rect.x + scaled(ROW_PAD)))
                    .top(px(rect.y + scaled(10.0)))
                    .text_size(px(scaled(11.0)))
                    .text_color(theme.muted_foreground)
                    .child("No sessions in this folder"),
            );
        }
        let pinned = crate::workspace::pinned_indices(&self.workspaces, self.folder_filter);
        for (k, &ws_idx) in pinned.iter().enumerate() {
            let Some(ws) = self.workspaces.get(ws_idx) else {
                continue;
            };
            let rect = crate::workspace::pinned_bubble_rect(k, pinned.len(), 1.0, &list);
            layer = layer.child(self.pinned_bubble(theme, ws, ws_idx, &rect).on_mouse_down(
                MouseButton::Left,
                press(entity.clone(), move |this, _ev, _cx| this.press_pinned(ws_idx)),
            ));
        }
        layer
    }


    /// One iMessage-style pinned bubble: a big round avatar with the group's
    /// name under it, absolutely positioned at the rect
    /// [`crate::workspace::pinned_bubble_rect`] handed the mouse path. The
    /// avatar kind is derived exactly the way [`Self::session_row`] derives
    /// its status icon (`avatar_for` over the cached git context — never a
    /// blocking fetch), so a bubble reads the same state its row showed
    /// before the pin.
    /// The active group wears a gantry ring, an unread group gets a
    /// dot beside its name, and hover is deliberately inert: the bubbles have
    /// no hover well, they are just a big target.
    fn pinned_bubble(
        &self,
        theme: &Theme,
        ws: &crate::workspace::Workspace,
        ws_idx: usize,
        rect: &crate::workspace::LayoutRect,
    ) -> gpui::Div {
        let ctx = ws
            .cwd
            .as_deref()
            .and_then(|cwd| self.git_contexts.get(cwd));
        let rollup = ctx.map_or(PrRollup::None, |c| derive_rollup(c.pr.as_ref()));
        let kind = avatar_for(
            rollup,
            ctx.and_then(|c| c.pr.as_ref()).is_some_and(|p| p.is_draft),
        );
        let active = ws_idx == self.active;

        // The 64px disc. A draft glows in the gantry accent like its card
        // does, with a soft shadow to lift it off the panel; the other three
        // states wear GitHub's status gradients, inked for each fill.
        let disc = div()
            .flex_none()
            .w(px(64.0))
            .h(px(64.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center();
        let disc = match kind {
            CardAvatar::Draft => disc
                .bg(accent())
                .shadow(vec![BoxShadow {
                    color: accent().opacity(0.35),
                    offset: point(px(0.0), px(3.0)),
                    blur_radius: px(8.0),
                    spread_radius: px(0.0),
                    inset: false,
                }]),
            CardAvatar::Open => disc.bg(linear_gradient(
                180.,
                linear_color_stop(gpui::rgb(0xb7e3c0), 0.),
                linear_color_stop(gpui::rgb(0x7cc98c), 1.),
            )),
            CardAvatar::NoPr => disc.bg(linear_gradient(
                180.,
                linear_color_stop(gpui::rgb(0xc9ccd4), 0.),
                linear_color_stop(gpui::rgb(0x9aa0ab), 1.),
            )),
            CardAvatar::Merged => disc.bg(linear_gradient(
                180.,
                linear_color_stop(gpui::rgb(0xd9c8f7), 0.),
                linear_color_stop(gpui::rgb(0xb08ff0), 1.),
            )),
        }
        // The active group's ring is the only chrome a bubble carries.
        .when(active, |d| d.border_2().border_color(accent()))
        .child(
            gpui::svg()
                .path(avatar_icon(kind))
                .w(px(28.0))
                .h(px(28.0))
                .text_color(match kind {
                    CardAvatar::Draft | CardAvatar::NoPr => gpui::white(),
                    CardAvatar::Open => gpui::rgb(0x155e2b).into(),
                    CardAvatar::Merged => pr_merged(theme.dark),
                }),
        );

        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(5.0)) // the geometry's disc→label gap; the rect owns the rest.
            .child(disc)
            .child(
                div()
                    .max_w(px(rect.w))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(4.0))
                    .text_size(px(scaled(10.5)))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .when(ws.any_unread(), |d| {
                        d.child(
                            div()
                                .flex_none()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(theme.primary),
                        )
                    })
                    .child(
                        div()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(ws.title()),
                    ),
            )
    }

    /// One one-line row, for the pages with no git context worth previewing:
    /// Settings' section tabs.
    ///
    /// `rect` is whatever [`crate::workspace::tab_rect`] handed the mouse path
    /// for this row's index, so the pixels and the hit box cannot drift apart.
    /// `indent` steps a nested row's label in, and `dim` marks a row whose
    /// label is secondary — the canvas painted those in `ink_dim` unless they
    /// were active. The fills mirror the canvas painter exactly: the active row
    /// gets the card pill plus a soft shadow, a merely hovered row gets a
    /// weaker muted fill, and every other row stays transparent. Hover comes
    /// from [`App::sidebar_cursor`], so it is suppressed mid-drag here too.
    fn simple_row(
        &self,
        theme: &Theme,
        rect: &crate::workspace::LayoutRect,
        label: impl Into<SharedString>,
        active: bool,
        indent: bool,
        dim: bool,
    ) -> gpui::Div {
        let hovered = self
            .sidebar_cursor()
            .is_some_and(|(x, y)| rect.contains(x, y));
        // Dark chrome's card token sits *below* the panel material, so the
        // canvas lifted it toward white to read as light glass; light chrome's
        // card already does.
        let fill = if theme.dark {
            shade(theme.card, 0.12)
        } else {
            theme.card
        };
        let ink = if active || !dim {
            theme.foreground
        } else {
            theme.muted_foreground
        };

        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(ROW_RADIUS))
            .when(active, |d| {
                d.bg(fill.opacity(0.78))
                    .border_1()
                    .border_color(theme.foreground.opacity(0.28))
                    .shadow(vec![BoxShadow {
                        color: gpui::black().opacity(if theme.dark { 0.35 } else { 0.12 }),
                        offset: point(px(0.0), px(1.0)),
                        blur_radius: px(4.0),
                        spread_radius: px(0.0),
                        inset: false,
                    }])
            })
            .when(!active && hovered, |d| {
                d.bg(theme.muted.opacity(if theme.dark { 0.5 } else { 0.7 }))
            })
            .flex()
            .items_center()
            .pl(px(scaled(ROW_PAD + if indent { ROW_INDENT } else { 0.0 })))
            .pr(px(scaled(ROW_PAD)))
            .text_size(px(scaled(12.0)))
            .text_color(ink)
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.into()),
            )
    }

    /// Cursor position in logical pixels for hover tests, or `None` while a
    /// drag is in flight or a modal overlay owns the frame — the canvas
    /// suppresses hover in both cases too (`renderer.rs` nulls its whole
    /// cursor while an overlay is up), and the two surfaces must agree about
    /// what looks hot.
    pub(crate) fn sidebar_cursor(&self) -> Option<(f32, f32)> {
        hover_cursor(
            self.cursor,
            self.scale(),
            matches!(self.drag, crate::Drag::None),
            self.modal_overlay_open(),
        )
    }

    /// One flat two-line session row at `rect` (its
    /// [`crate::workspace::sidebar_row_rect`]), in the GANTRY mock's style:
    /// line one is the title, the relative time and the PR-state icon; line
    /// two the diffstat as `+A −R · N uncommitted` (or "no code changes").
    /// The unread dot sits under the PR-state icon; a hairline separator
    /// closes every row but the last.
    fn session_row(
        &self,
        theme: &Theme,
        ws: &crate::workspace::Workspace,
        rect: &crate::workspace::LayoutRect,
        selected: bool,
        last: bool,
        hover: Option<(f32, f32)>,
    ) -> gpui::Div {
        let hovered = hover.is_some_and(|(x, y)| rect.contains(x, y));
        let ctx = ws
            .cwd
            .as_deref()
            .and_then(|cwd| self.git_contexts.get(cwd));

        let title = ws.title();
        let rollup = ctx.map_or(PrRollup::None, |c| derive_rollup(c.pr.as_ref()));
        let kind = avatar_for(
            rollup,
            ctx.and_then(|c| c.pr.as_ref()).is_some_and(|p| p.is_draft),
        );
        let stamp = ws.attention_at().map(|t| relative_time(t, SystemTime::now()));
        let diff = ctx.and_then(diffstat_line);

        let strong = theme.foreground;
        let soft = theme.muted_foreground;
        let tint = match kind {
            CardAvatar::NoPr => pr_none_ink(theme.dark),
            CardAvatar::Draft => accent(),
            CardAvatar::Open => pr_open(theme.dark),
            CardAvatar::Merged => pr_merged(theme.dark),
        };
        // Mock: the selected row is a white wash (`rgba(255,255,255,.10)`) on
        // the dark ground, an ink wash on the light one; hover is half that.
        let wash = |a: f32| {
            if theme.dark {
                gpui::white().opacity(a)
            } else {
                theme.foreground.opacity(a * 0.8)
            }
        };

        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(ROW_RADIUS))
            .when(selected, |d| d.bg(wash(0.10)))
            .when(!selected && hovered, |d| d.bg(wash(0.05)))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(scaled(2.0)))
            .pl(px(scaled(ROW_PAD)))
            .pr(px(scaled(ROW_PAD)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(scaled(6.0)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(scaled(12.5)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(strong)
                            .child(title),
                    )
                    .when_some(stamp, |d, s| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(px(scaled(11.0)))
                                .text_color(soft)
                                .child(s),
                        )
                    })
                    .child(
                        gpui::svg()
                            .flex_none()
                            .path(avatar_icon(kind))
                            .w(px(scaled(STATUS_ICON)))
                            .h(px(scaled(STATUS_ICON)))
                            .text_color(tint),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(scaled(6.0)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .items_center()
                            .gap(px(scaled(4.0)))
                            .text_size(px(scaled(11.0)))
                            .font_family(crate::renderer::FONT_FAMILY)
                            .text_color(soft)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .map(|d| match diff {
                                Some(line) => d
                                    .child(
                                        div()
                                            .text_color(diff_added(theme.dark))
                                            .child(line.added),
                                    )
                                    .child(
                                        div()
                                            .text_color(diff_removed(theme.dark))
                                            .child(typographic_minus(&line.removed)),
                                    )
                                    .when_some(line.uncommitted, |d, note| {
                                        d.child(div().child(format!("· {note}")))
                                    }),
                                None => d.child("no code changes"),
                            }),
                    )
                    // The unread dot sits under the PR-state icon: a column
                    // as wide as the icon, so the text edge never shifts.
                    .child(
                        div()
                            .flex_none()
                            .w(px(scaled(STATUS_ICON)))
                            .h(px(scaled(STATUS_ICON)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(ws.any_unread(), |d| {
                                d.child(
                                    div()
                                        .w(px(scaled(UNREAD_DOT)))
                                        .h(px(scaled(UNREAD_DOT)))
                                        .rounded_full()
                                        .bg(theme.primary),
                                )
                            }),
                    ),
            )
            .when(!last, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(scaled(ROW_PAD)))
                        .right(px(scaled(ROW_PAD)))
                        .bottom(px(0.0))
                        .h(px(1.0))
                        .bg(separator(theme)),
                )
            })
    }
}

/// The hairline that separates list rows and the folders card's sections:
/// `rgba(255,255,255,.07)` on the dark ground, an ink hairline on the light.
pub(crate) fn separator(theme: &Theme) -> Hsla {
    if theme.dark {
        gpui::white().opacity(0.07)
    } else {
        theme.foreground.opacity(0.08)
    }
}

/// The glyph each avatar state wears, kept pure so a test can pin that the
/// four states read differently.
///
/// Every glyph here is plain Latin punctuation or an arrow/check that the
/// macOS UI font carries itself — nothing that falls back to a missing-glyph
/// box the way a branch or fork symbol would.
fn avatar_icon(kind: CardAvatar) -> &'static str {
    match kind {
        CardAvatar::NoPr => crate::ui::assets::ICON_PR_NONE,
        CardAvatar::Draft => crate::ui::assets::ICON_PR_DRAFT,
        CardAvatar::Open => crate::ui::assets::ICON_PR_OPEN,
        CardAvatar::Merged => crate::ui::assets::ICON_PR_MERGED,
    }
}

/// Git's PR green (`#1a7f37` in the mock), lightened for dark chrome the way
/// GitHub's own dark palette does, so the stroke keeps its contrast.
/// The GANTRY mock's `--accent` as a gpui color — the user's accent setting
/// (System follows macOS), resolved by `theme::accent_color`. Pinned to that
/// rather than the chrome theme's own `accent`, which the user retints
/// freely: the selected card has to agree with the focused pane's tab pill.
fn accent() -> Hsla {
    let (r, g, b) = crate::theme::accent_color();
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Ink for the "no pull request yet" commit graph — the mock's `#57606a`.
fn pr_none_ink(dark: bool) -> Hsla {
    if dark {
        gpui::rgb(0x8b949e).into()
    } else {
        gpui::rgb(0x57606a).into()
    }
}

fn pr_open(dark: bool) -> Hsla {
    if dark {
        gpui::rgb(0x3fb950).into()
    } else {
        gpui::rgb(0x1a7f37).into()
    }
}

/// Git's merged purple (`#8250df` in the mock), with the same dark-chrome
/// lift as [`pr_open`].
fn pr_merged(dark: bool) -> Hsla {
    if dark {
        gpui::rgb(0xa371f7).into()
    } else {
        gpui::rgb(0x8250df).into()
    }
}

/// Diff green. These are the one colour pair the theme has no token for, so
/// they live here beside the card rollups that paint them.
fn diff_added(dark: bool) -> Hsla {
    if dark {
        gpui::rgb(0x78be8c).into()
    } else {
        gpui::rgb(0x228b54).into()
    }
}

/// The removed count with its ASCII `-` swapped for the typographic minus
/// `−` (U+2212), which is what the mock sets and what lines up with the digits.
///
/// [`crate::sidebar_card::diffstat_line`] already formats the sign into the
/// string, so this only re-inks the one it made — it never adds a second.
fn typographic_minus(removed: &str) -> String {
    match removed.strip_prefix('-') {
        Some(rest) => format!("−{rest}"),
        None => removed.to_string(),
    }
}

/// Diff red, the counterpart to [`diff_added`].
fn diff_removed(dark: bool) -> Hsla {
    if dark {
        gpui::rgb(0xe06c75).into()
    } else {
        gpui::rgb(0xc0392b).into()
    }
}

/// The region's flat ground (mock: a plain, slightly darker fill — no
/// card). The chrome's `gradient_to` is the accent-tinted base tone the
/// old panel settled to, so the ground keeps the theme's cast without the
/// panel's glow or rim.
fn region_ground(theme: &Theme, w: f32) -> gpui::Div {
    let chrome = crate::theme::current();
    let ground = crate::theme::mix(chrome.gradient_to, chrome.accent, if theme.dark { 0.06 } else { 0.03 });
    div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w(px(w))
        .h_full()
        .bg(crate::renderer::color(ground, 1.0))
}

/// One square icon chip, painted at `rect` — the very rect
/// `workspace::sessions_header_chips` / `folders_header_chips` lay out.
/// Hover lifts a rounded well behind the glyph; `active` fills it with the
/// accent (the Settings gear while Settings is showing).
pub(crate) fn icon_chip(
    theme: &Theme,
    rect: &crate::workspace::LayoutRect,
    hovered: bool,
    active: bool,
    icon: &'static str,
) -> gpui::Div {
    let ink = if active {
        theme.primary_foreground
    } else if hovered {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .when(active, |c| c.bg(theme.primary))
        .when(!active && hovered, |c| {
            c.bg(theme.muted.opacity(if theme.dark { 0.5 } else { 0.7 }))
        })
        .child(
            gpui::svg()
                .path(icon)
                .w(px(HEADER_ICON))
                .h(px(HEADER_ICON))
                .text_color(ink),
        )
}

/// The logical-pixel cursor the panel hovers against, or `None` when nothing
/// in the sidebar may look hot.
///
/// Two gates, both copied from the canvas painter so the surfaces agree: a
/// drag in flight owns the pointer, and a modal overlay owns the whole frame
/// (`renderer.rs` nulls its cursor wholesale while one is up, so rows and the
/// section delete glyph must not light up under a palette or confirm dialog
/// that is swallowing the clicks).
fn hover_cursor(
    cursor: (f64, f64),
    scale: f32,
    drag_idle: bool,
    overlay_open: bool,
) -> Option<(f32, f32)> {
    if !drag_idle || overlay_open {
        return None;
    }
    let s = scale.max(0.01);
    Some((cursor.0 as f32 / s, cursor.1 as f32 / s))
}


/// Nudge a token's lightness by `delta`, clamped, so a fill derives from the
/// live theme instead of the mock's fixed grays.
fn shade(color: Hsla, delta: f32) -> Hsla {
    Hsla {
        l: (color.l + delta).clamp(0.0, 1.0),
        ..color
    }
}

#[cfg(test)]
mod tests {

    use gpui::AssetSource as _;
    use super::*;

    #[test]
    fn a_cards_diffstat_carries_exactly_one_leading_sign() {
        // `GitContext::empty` is private to its module, so build one by hand —
        // every field is public, which is what makes that cheap.
        let c = crate::git_context::GitContext {
            is_git: true,
            cwd: std::path::PathBuf::from("/src/pwrde"),
            repo: Some("pwrde".to_string()),
            branch: Some("main".to_string()),
            default_branch: Some("main".to_string()),
            branch_diff: None,
            dirty: Some(crate::git::DirtyStats {
                files: 7,
                insertions: 558,
                deletions: 16,
            }),
            pr: None,
            pr_error: None,
            fetched_at: std::time::UNIX_EPOCH,
        };
        let line = crate::sidebar_card::diffstat_line(&c).expect("a dirty tree has a diffstat");

        // What the element tree paints, verbatim.
        let added = line.added.clone();
        let removed = typographic_minus(&line.removed);

        assert_eq!(added, "+558");
        assert_eq!(removed, "−16");
        assert_eq!(added.matches('+').count(), 1);
        assert_eq!(removed.matches('−').count(), 1);
        assert!(!removed.contains('-'), "the ASCII minus must be replaced");
        assert!(added[1..].chars().all(|c| c.is_ascii_digit() || c == ','));
        assert!(removed[3..].chars().all(|c| c.is_ascii_digit() || c == ','));
    }

    #[test]
    fn avatar_states_wear_distinct_icons() {
        use CardAvatar::{Draft, Merged, NoPr, Open};
        let all = [NoPr, Draft, Open, Merged].map(avatar_icon);
        for (i, path) in all.iter().enumerate() {
            // Every state must resolve to a real embedded asset, or gpui's
            // `svg()` silently paints nothing and the disc reads as empty.
            assert!(
                crate::ui::assets::Assets
                    .load(path)
                    .ok()
                    .flatten()
                    .is_some_and(|bytes| !bytes.is_empty()),
                "state {i} points at {path}, which is not an embedded asset"
            );
            for other in &all[i + 1..] {
                assert_ne!(path, other, "two avatar states share an icon");
            }
        }
    }

    #[test]
    fn the_pr_palette_matches_the_mock_and_lifts_in_the_dark() {
        // The mock's light values, exactly.
        assert_eq!(pr_open(false), gpui::rgb(0x1a7f37).into());
        assert_eq!(pr_merged(false), gpui::rgb(0x8250df).into());
        // Dark chrome gets its own, brighter pair — never the light one.
        assert_ne!(pr_open(true), pr_open(false));
        assert_ne!(pr_merged(true), pr_merged(false));
        assert!(pr_open(true).l > pr_open(false).l);
        assert!(pr_merged(true).l > pr_merged(false).l);
        // Green and purple must never collapse onto one another.
        assert_ne!(pr_open(true), pr_merged(true));
        assert_ne!(pr_open(false), pr_merged(false));
    }

    #[test]
    fn hover_dies_under_a_drag_or_an_overlay() {
        // Idle and unobstructed: physical pixels come back as logical ones.
        assert_eq!(hover_cursor((40.0, 100.0), 2.0, true, false), Some((20.0, 50.0)));
        // A drag owns the pointer.
        assert_eq!(hover_cursor((40.0, 100.0), 2.0, false, false), None);
        // A modal overlay owns the whole frame, exactly as the canvas painter
        // decided — no row highlight, no section delete glyph.
        assert_eq!(hover_cursor((40.0, 100.0), 2.0, true, true), None);
        assert_eq!(hover_cursor((40.0, 100.0), 2.0, false, true), None);
        // A degenerate scale must not divide by zero.
        assert!(hover_cursor((40.0, 100.0), 0.0, true, false).is_some());
    }

    #[test]
    fn scaling_is_a_no_op_at_the_default_text_size() {
        // `font_scale` reads the live setting, so this asserts the identity the
        // literals below it are written against rather than a configured value:
        // at the default chrome font, every dimension passes through unchanged.
        let scale = crate::workspace::row_font_scale();
        assert_eq!(font_scale(), scale);
        // Capped, because the sidebar does not scroll yet.
        assert!(font_scale() <= 1.5, "the sidebar factor must stay capped");
        assert_eq!(scaled(0.0), 0.0);
        assert_eq!(scaled(12.0), 12.0 * scale);
        // Proportional, so a row and the type inside it grow together.
        assert_eq!(scaled(24.0), 2.0 * scaled(12.0));
        // The sidebar and the layout math must agree on the factor, or rows
        // and their contents scale apart — `font_scale` delegates for exactly
        // that reason, so this pins the delegation rather than a coincidence.
        assert_eq!(font_scale(), crate::workspace::row_font_scale());
    }
}
