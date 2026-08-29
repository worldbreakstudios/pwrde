//! Sessions sidebar as a gpui element tree over the canvas.
//!
//! The sidebar used to be painted by `renderer::paint_sidebar_rows`. Only the
//! *pixels* moved here at first: geometry still lives in [`crate::workspace`]
//! (`sidebar_rows` / `sidebar_row_rect`), and row clicks, drags and renames
//! are still resolved on the canvas mouse path in `main.rs` against those
//! same rects. That is deliberate — the rects are the single authority that
//! keeps painting, hit-testing and PTY resize in agreement, so this tree is
//! *absolutely positioned to match them* rather than reimplementing layout or
//! drag-and-drop in gpui's paradigm.
//!
//! Interaction is migrating onto the elements themselves, surface by
//! surface: the page-dot strip ([`App::page_dot_layer`]), the Sessions
//! empty state ([`App::render_empty_state`]), and the Settings page's rows —
//! a real rcn `Input` for the search field plus click-target section rows
//! ([`App::settings_row_layer`]) — are gpui-owned and `occlude()` the
//! canvas, so `main.rs` no longer hit-tests their rects.
//!
//! Colors come from the live chrome theme ([`crate::ui::theme::Theme`]), never
//! from the mock's hardcoded palette, so the panel reads correctly in both
//! light and dark polarity.
//!
//! Pinned groups move out of the rows into an iMessage-style strip above
//! them — one avatar bubble per pin, painted on this same absolute layer
//! straight from [`crate::workspace::pinned_bubble_rect`], so painting and the
//! hit test in `main.rs` never disagree.

use gpui::{
    AnyElement, App as GpuiApp, BoxShadow, ClickEvent, Context, FontWeight, Hsla,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    MouseButton, Styled, Window, div, linear_color_stop, linear_gradient, point,
    prelude::FluentBuilder as _, px,
};

use std::time::SystemTime;

use crate::App;
use crate::git_context::{PrRollup, derive_rollup};
use crate::sidebar_card::{
    CardAvatar, avatar_for, diffstat_line, relative_time, status_line,
};
use crate::ui::theme::Theme;
use crate::workspace::{TITLEBAR_H, TRAFFIC_LIGHT_SAFE_W};

/// Gap between the window edge and the inlaid panel (the spec's window
/// gutter). The panel floats inside it, Apple Messages style.
const GUTTER: f32 = 3.0;

/// Thickness of the sidebar's width-resize grip line, matching the canvas's
/// 2 logical px.
const GRIP_W: f32 = 2.0;

/// Length of the grip's centered pill, matching `renderer.rs`'s 28px.
const GRIP_PILL_H: f32 = 28.0;

/// Opacity of the modal scrim. `renderer.rs` paints the chrome `scrim` token
/// at the same 0.30 over the rest of the window.
const SCRIM_ALPHA: f32 = 0.30;

/// Panel corner radius (`--radius-l` in the spec's token set).
const PANEL_RADIUS: f32 = 18.0;

/// Left padding of a sidebar row's text column. The canvas used the same 12px
/// (`renderer.rs`'s `group_pad`), and the unread dot centers in it.
const ROW_PAD: f32 = 12.0;

/// Diameter of the unread accent dot, matching the canvas painter.
const UNREAD_DOT: f32 = 7.0;

/// Type size of a folder (section) header's name. The mock sets these rows
/// markedly lighter than the cards they open: ~10.5px, bold, uppercase and
/// tracked out, in `muted_foreground` rather than near-black.
const SECTION_TEXT: f32 = 10.5;

/// Corner radius of a one-line row's pill. Mirrors `renderer.rs`'s
/// `ROW_RADIUS`, so the Cleanup, Notes and Settings rows keep exactly the
/// silhouette the canvas gave them.
const ROW_RADIUS: f32 = 14.0;

/// How far a nested one-line row steps its label in — a Notes doc sitting
/// under its vault. The canvas used the same 14px.
const ROW_INDENT: f32 = 14.0;

/// What the Settings search field shows while it is empty.
pub(crate) const SEARCH_SETTINGS_PLACEHOLDER: &str = "Search settings";

/// Diameter of an idle page-dot — the resting state of a page-strip slot,
/// before it crossfades into that page's glyph. Matches the 5px the canvas
/// painter used.
const PAGE_DOT: f32 = 5.0;

/// Type size of a page slot's glyph once the crossfade has resolved. The
/// canvas drew these in the chrome cell, which is this tall.
const PAGE_GLYPH_TEXT: f32 = 12.0;

/// Alpha of an avatar disc's colour wash. The mock sits between .10 and .12
/// depending on state; one value reads consistently and keeps the discs from
/// competing with the text.
const AVATAR_WASH: f32 = 0.12;

/// Size of the commit-graph icon inside the disc. The mock draws an 18px glyph
/// in a 38px circle.
const AVATAR_ICON: f32 = 18.0;

/// Diameter of a preview card's avatar circle. `workspace::CARD_H` is built
/// from it (10px padding + 38px avatar + 10px padding), so the two must agree.
const AVATAR: f32 = 38.0;

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
fn scaled(value: f32) -> f32 {
    value * font_scale()
}

impl App {
    /// The sidebar panel: shell, header and the page's own rows.
    ///
    /// There is no Sessions search field — group search is not supported, so
    /// the dead affordance is gone and the rows reclaim its 40px.
    ///
    /// Rendered as a sibling of the terminal canvas, so it paints above it
    /// while the canvas keeps owning input. The shell is *universal* — every
    /// page gets the same inlaid panel and header, and only the row
    /// layer differs (preview cards on Sessions and Pull Requests, one-line
    /// rows everywhere else). Returns an empty element only when the sidebar
    /// is collapsed to zero width.
    pub fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));

        let w = self.sidebar_w();
        if w <= 0.0 {
            return div().into_any_element();
        }

        let theme = Theme::of(cx).clone();
        let panel_w = (w - 2.0 * GUTTER).max(0.0);

        // The flyover ("global terminals") is canvas-painted and spans the full
        // window width, but this panel is a later element sibling — so it was
        // drawn straight over the flyover's left end, hiding whichever pane sat
        // there. Stop the sidebar above the flyover instead: `None` means it is
        // not intruding, `Some(0.0)` that it covers the sidebar outright.
        let ceiling = self.flyover_ceiling();
        if ceiling == Some(0.0) {
            return div().into_any_element();
        }

        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w(px(w))
            // Clipped, not just shortened: the row, dot and grip layers are
            // absolutely positioned against the whole window, so the height has
            // to actually cut them off.
            .overflow_hidden()
            .map(|d| match ceiling {
                Some(limit) => d.h(px(limit)),
                None => d.h_full(),
            })
            .child(
                panel(&theme, panel_w)
                    .child(header())
                    // The rows live in their own absolutely positioned layer,
                    // so the panel only needs to hold the scroll region open
                    // and let the row layer own everything below it.
                    .child(div().flex_1()),
            )
            .child(self.clipped_row_layer(&theme, cx))
            .child(self.drop_feedback_layer(&theme))
            .child(self.header_chips(&theme, cx.entity().downgrade()))
            .child(self.page_dot_layer(&theme, cx.entity().downgrade()))
            .child(self.resize_grip_layer(&theme))
            // Last, so the veil falls over every affordance the panel owns.
            .child(self.overlay_scrim(panel_w))
            .into_any_element()
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
    /// element off the Sessions / Pull Requests pages, or once the workspace
    /// has a tile.
    pub fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        // Sessions and Pull Requests share the card sidebar, so both pages
        // carried the canvas CTA (on Pull Requests it sits under that page's
        // own content card, exactly as before).
        if !matches!(self.page, crate::Page::Sessions | crate::Page::PullRequests)
            || !self.is_empty_state()
        {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();

        // `empty_state_cta` takes device pixels and a scale; this tree is
        // logical, so hand it the logical surface at scale 1.0 the way
        // `page_dot_layer` does.
        let (surface_w, surface_h) = self.renderer.surface_size();
        let scale = self.scale();
        let width = (surface_w as f32 / scale).round() as u32;
        let height = (surface_h as f32 / scale).round() as u32;
        let cta = crate::workspace::empty_state_cta(width, height, 1.0, self.sidebar_w(), self.right_w());
        let hint = crate::workspace::empty_state_hint(width, height, 1.0, self.sidebar_w(), self.right_w());

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
        // A canvas modal (picker, palette, confirm…) scrims the canvas
        // beneath this tree, not this tree: veil the pill and hint the same
        // 30% and drop the click, so the modal keeps the frame.
        let modal = self.modal_overlay_open();
        let veil = |r: &crate::workspace::LayoutRect, radius: f32| {
            div()
                .absolute()
                .left(px(r.x))
                .top(px(r.y))
                .w(px(r.w))
                .h(px(r.h))
                .rounded(px(radius))
                .occlude()
                .bg(gpui::hsla(0.0, 0.0, 0.0, SCRIM_ALPHA))
        };

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
            .when(modal, |d| d.child(veil(&cta, ROW_RADIUS)).child(veil(&hint, 0.0)))
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
            crate::DropTarget::SidebarInsert { .. } | crate::DropTarget::SectionMove { .. }
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

    /// The sidebar's width-resize grip: a slim ink line down the panel's
    /// right edge with a centered pill, shown while the pointer is on the
    /// handle (or mid-drag).
    ///
    /// The canvas drew it centered on `x = sidebar_w`, but the panel's right
    /// edge sits a gutter further left, so the panel swallowed the inner half
    /// and only a sliver survived. Only the pixels moved: it now hugs the
    /// panel edge at `sidebar_w - GUTTER`. The hit-test is untouched — the
    /// press still lands on `workspace::resize_hover_at`'s band around
    /// `sidebar_w` and still starts [`crate::Drag::Sidebar`].
    fn resize_grip_layer(&self, theme: &Theme) -> gpui::Div {
        let layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        // Same suppression the cursor uses: an overlay opened by keyboard
        // while hovering leaves the hover stale, and an unrelated drag owns
        // the pointer.
        if self.modal_overlay_open()
            || !matches!(
                self.drag,
                crate::Drag::None
                    | crate::Drag::Sidebar
                    | crate::Drag::Divider { .. }
                    | crate::Drag::ToolPanelResize
            )
            || self.resize_hover != Some(crate::workspace::ResizeHover::Sidebar)
        {
            return layer;
        }
        let (_, surface_h) = self.renderer.surface_size();
        let height = (surface_h as f32 / self.scale()).round();
        // Top of the tile area down to its bottom inset, exactly the span the
        // canvas grip covered.
        let top = TITLEBAR_H;
        let bottom = (height - crate::workspace::AREA_PAD).max(top);
        let x = (self.sidebar_w() - GUTTER - GRIP_W / 2.0).max(0.0);
        let pill_h = GRIP_PILL_H.min(bottom - top);
        layer
            .child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(top))
                    .w(px(GRIP_W))
                    .h(px(bottom - top))
                    .rounded(px(GRIP_W / 2.0))
                    .bg(theme.foreground.opacity(0.14)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(top + ((bottom - top - pill_h) / 2.0).max(0.0)))
                    .w(px(GRIP_W))
                    .h(px(pill_h))
                    .rounded(px(GRIP_W / 2.0))
                    .bg(theme.foreground.opacity(0.45)),
            )
    }

    /// The modal scrim, over the panel.
    ///
    /// `renderer.rs` dims the whole window behind the picker, palette and the
    /// confirm/message dialogs, but the panel is an opaque element sibling
    /// painted after the canvas, so the sidebar alone stayed bright while
    /// something else owned the clicks. This lays the same 30% black over the
    /// sidebar column whenever [`crate::App::modal_overlay_open`] holds — the
    /// exact overlay set the canvas scrims — so the panel dims with the rest
    /// of the window. The chrome `scrim` token is black in every theme, so
    /// this is black too.
    fn overlay_scrim(&self, panel_w: f32) -> gpui::Div {
        let layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        if !self.modal_overlay_open() {
            return layer;
        }
        // Exactly the panel's rect, corners included: the window gutter around
        // it still shows canvas, which the canvas scrim already dimmed, and
        // laying a second veil there would darken the seam twice over.
        // Occluding, not just tinting: the page dots below it are element
        // click targets now, and a veil that let clicks through would switch
        // pages behind the modal. The canvas path used to swallow those.
        layer.child(
            div()
                .id("sidebar-modal-scrim")
                .absolute()
                .left(px(GUTTER))
                .top(px(GUTTER))
                .w(px(panel_w))
                .bottom(px(GUTTER))
                .rounded(px(PANEL_RADIUS))
                .occlude()
                .bg(gpui::hsla(0.0, 0.0, 0.0, SCRIM_ALPHA)),
        )
    }

    /// The page-dot strip along the bottom of the panel.
    ///
    /// The canvas used to paint this, but the panel is opaque and sits over
    /// it, so the affordance had gone invisible while staying clickable. The
    /// pixels moved here; the rects did not. Each slot is positioned at
    /// exactly [`crate::workspace::page_slot_rect`] — the same rect
    /// `renderer.rs` registers as hot and `main.rs::page_slot_at` hit-tests —
    /// and flag-gated pages (Notes, when the feature is off) drop out of the
    /// strip entirely so the slot indices keep lining up. Each slot is a real
    /// gpui click target that switches the page; the hover crossfade target
    /// (`dot_hover`) is still tracked on the canvas mouse-move path.
    fn page_dot_layer(&self, theme: &Theme, entity: gpui::WeakEntity<Self>) -> gpui::Div {
        let (_, surface_h) = self.renderer.surface_size();
        // `page_slot_rect` takes device pixels and a scale; gpui works in
        // logical pixels, so hand it the logical height at scale 1.0 the way
        // the rest of this file does.
        let height = (surface_h as f32 / self.scale()).round() as u32;
        let w = self.sidebar_w();
        let pages = crate::Page::visible(crate::features::notes_enabled());
        let n = pages.len();

        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        for (i, page) in pages.iter().enumerate() {
            let slot = crate::workspace::page_slot_rect(i, n, height, 1.0, w);
            // The animation array is indexed by the page's *stable global*
            // index, not its slot, so gating a page never shifts the others.
            let p = self
                .dot_anim
                .get(page.index())
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let entity = entity.clone();
            let page = *page;
            // While a canvas modal is up the slot keeps painting (the scrim
            // veil dims and occludes it) but must not act — modality is
            // still the canvas's to own until the modals themselves move.
            let modal = self.modal_overlay_open();
            layer = layer.child(
                page_slot(theme, &slot, page.glyph(), p)
                    .id(("page-slot", i))
                    .occlude()
                    .when(!modal, |slot| {
                        slot.cursor_pointer().on_click(
                            move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                if let Some(entity) = entity.upgrade() {
                                    entity.update(app, |this, cx| {
                                        this.set_page(page);
                                        cx.notify();
                                    });
                                }
                            },
                        )
                    }),
            );
        }
        layer
    }

    /// The two header chips, in their own absolute layer at the rects
    /// `workspace.rs` lays them out at: "⇤" on the left toggles the sidebar
    /// collapse, "＋" at the top right opens the cwd picker (a new group).
    /// They are gpui click targets that occlude the canvas — so the titlebar
    /// window-drag beneath them never sees the press — and go inert while a
    /// canvas modal owns the frame (the modal veil dims them).
    fn header_chips(&self, theme: &Theme, entity: gpui::WeakEntity<Self>) -> gpui::Div {
        let w = self.sidebar_w();
        let cur = self.sidebar_cursor();
        let plus = crate::workspace::new_group_button(1.0, w);
        let collapse = crate::workspace::sidebar_collapse_button(1.0, w);
        let hovered = |r: &crate::workspace::LayoutRect| {
            cur.is_some_and(|(x, y)| r.contains(x, y))
        };
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
        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .child(
                chip(theme, &collapse, hovered(&collapse), "⇤")
                    .id("sidebar-collapse")
                    .occlude()
                    .when(!modal, |c| {
                        c.cursor_pointer()
                            .on_click(handler(entity.clone(), |this| this.toggle_sidebar()))
                    }),
            )
            .child(
                chip(theme, &plus, hovered(&plus), "＋")
                    .id("sidebar-new-group")
                    .occlude()
                    .when(!modal, |c| {
                        c.cursor_pointer()
                            .on_click(handler(entity, |this| this.open_picker()))
                    }),
            )
    }

    /// The row layer, clipped to the band between the header and the page strip.
    ///
    /// Rows are absolutely positioned at the rects `workspace.rs` hands the
    /// mouse path, and nothing stops that list from running past the bottom of
    /// the window — a preview card is 58px, so a dozen groups already overflow.
    /// This wrapper bounds the paint to the region the rows own: it starts
    /// below the header chips and stops above the page strip,
    /// so an overflowing row is cut off rather than painting over the chrome.
    /// The inner layer is offset back up by the same amount and kept
    /// window-tall, so every row still lands at its own absolute rect and the
    /// canvas hit-test stays the authority. (Scrolling to reach the clipped
    /// rows is a separate pass.)
    fn clipped_row_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let (_, surface_h) = self.renderer.surface_size();
        let height = (surface_h as f32 / self.scale()).round();
        // The titlebar's own height, in the same logical pixels the row rects
        // use: `workspace::tab_rect` and `sidebar_row_rect` both start their
        // stack just below it, and the header chips ride inside it.
        let top = crate::workspace::TITLEBAR_H;
        // The page strip owns the bottom of the panel, inside the window
        // gutter; rows stop above it so they can never paint over a page dot.
        let bottom = (height - GUTTER - crate::workspace::PAGE_STRIP_H).max(top);

        div()
            .absolute()
            .left(px(0.0))
            .top(px(top))
            .w_full()
            .h(px(bottom - top))
            .overflow_hidden()
            .child(
                self.sidebar_row_layer(theme, cx)
                    .top(px(-top))
                    .h(px(height)),
            )
    }

    /// The row layer for whichever page is showing.
    ///
    /// Every page draws its rows into the same absolutely positioned layer
    /// over the panel, but they do not share a row *vocabulary*: Sessions and
    /// Pull Requests get card-height preview rows laid out by
    /// [`crate::workspace::sidebar_row_rect`], while Cleanup, Notes and
    /// Settings get one-line rows at [`crate::workspace::tab_rect`] — the very
    /// rects `main.rs` already hit-tests for those pages. The split is
    /// [`App::card_rows`], the same predicate the geometry helpers take.
    fn sidebar_row_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        if self.card_rows() {
            return self.card_row_layer(theme);
        }
        match self.page {
            crate::Page::Cleanup => self.cleanup_row_layer(theme),
            crate::Page::Notes => self.notes_row_layer(theme),
            crate::Page::Settings => self.settings_row_layer(theme, cx),
            // Any page without rows of its own still gets the shell.
            _ => div().absolute().left(px(0.0)).top(px(0.0)).size_full(),
        }
    }

    /// The Settings rows: the live search box in slot 0 and one tab per
    /// [`crate::pages::Section`] at slot `i + 1`, the shift `main.rs`'s
    /// `Page::Settings` mouse branch makes to leave room for the box.
    fn settings_row_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let w = self.sidebar_w();
        let entity = cx.entity().downgrade();
        let mut layer = div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .child(self.settings_search_row(theme, cx));
        for (i, section) in crate::pages::Section::ALL.iter().enumerate() {
            let rect = crate::workspace::tab_rect(i + 1, 1.0, w);
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
        let rect = crate::workspace::settings_search_rect(1.0, self.sidebar_w());
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

    /// The Notes rows: one combined list stacking the registered vaults, then
    /// the active vault's docs (indented), then the trailing add-vault row —
    /// the same three ranges `main.rs`'s `Page::Notes` mouse branch walks.
    /// [`notes_row`] owns the index → row mapping, so paint and hit-test read
    /// one table rather than two hand-kept-in-sync loops.
    fn notes_row_layer(&self, theme: &Theme) -> gpui::Div {
        let w = self.sidebar_w();
        let vaults = crate::notes::vaults();
        let (n_vaults, n_docs) = (vaults.len(), self.notes_docs.len());

        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        for i in 0..(n_vaults + n_docs + 1) {
            let Some(row) = notes_row(i, n_vaults, n_docs) else { break };
            let rect = crate::workspace::tab_rect(i, 1.0, w);
            layer = layer.child(match row {
                // A vault row keeps full-strength ink even when inactive: the
                // vault list is the page's primary navigation. Docs and the
                // add-vault row are secondary, so they dim.
                NotesRow::Vault(v) => self.simple_row(
                    theme,
                    &rect,
                    vault_label(&vaults[v]),
                    v == self.notes_active_vault,
                    false,
                    false,
                ),
                NotesRow::Doc(d) => {
                    let doc = &self.notes_docs[d];
                    let open = self.notes_selected.as_deref() == Some(doc.path.as_path());
                    self.simple_row(theme, &rect, doc.rel.clone(), open, true, true)
                },
                NotesRow::AddVault => {
                    self.simple_row(theme, &rect, ADD_VAULT_LABEL, false, false, true)
                },
            });
        }
        layer
    }

    /// The Cleanup rows: an `All` row at slot 0 and one row per repo that has
    /// worktrees to drop, exactly the tabs `main.rs` hit-tests at
    /// [`crate::workspace::tab_rect`] — slot 0 clears the filter, slot `j + 1`
    /// filters to the `j`th repo. The labels and the active row both come from
    /// [`cleanup_rows`], so the painted order cannot drift from the order the
    /// mouse path walks.
    fn cleanup_row_layer(&self, theme: &Theme) -> gpui::Div {
        let w = self.sidebar_w();
        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        for (i, (label, active)) in cleanup_rows(&self.cleanup).into_iter().enumerate() {
            let rect = crate::workspace::tab_rect(i, 1.0, w);
            layer = layer.child(self.simple_row(theme, &rect, label, active, false, true));
        }
        layer
    }

    /// The Sessions and Pull Requests rows: section headers and preview cards.
    /// Every row is absolutely positioned at exactly the rect
    /// [`crate::workspace::sidebar_row_rect`] hands the mouse path, so a click
    /// that looks like it landed on a row *is* that row as far as hit-testing
    /// is concerned. Scale is 1.0 because gpui already works in logical
    /// pixels; the canvas passes the device scale instead.
    fn card_row_layer(&self, theme: &Theme) -> gpui::Div {
        let rows = crate::workspace::sidebar_rows(&self.workspaces, &self.sections);
        let w = self.sidebar_w();
        let hover = self.sidebar_cursor();
        let active =
            crate::workspace::active_row_index(&rows, &self.workspaces, &self.sections, self.active);

        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        for (i, row) in rows.iter().enumerate() {
            let rect =
                crate::workspace::sidebar_row_rect(&rows, i, &self.workspaces, 1.0, w, true);
            match *row {
                crate::workspace::SidebarRow::SectionHeader { section_idx } => {
                    if let Some(section) = self.sections.get(section_idx) {
                        layer =
                            layer.child(self.section_header_row(theme, section, &rect, hover));
                    }
                }
                crate::workspace::SidebarRow::Group { ws_idx } => {
                    if let Some(ws) = self.workspaces.get(ws_idx) {
                        let selected = active == Some(i);
                        layer = layer.child(self.group_card(
                            theme, ws, ws_idx, &rect, selected, hover,
                        ));
                    }
                }
            }
        }
        // The pinned quick-access strip rides above the rows on this same
        // absolute layer: one bubble per pinned group at exactly the rect
        // `pinned_bubble_rect` hands the mouse path (scale 1.0 — gpui already
        // works in logical px), so a click that lands on a bubble *is* that
        // bubble as far as hit-testing is concerned. `sidebar_rows` has already
        // left these groups out of the ladder, so the strip is their only home.
        let pinned = crate::workspace::pinned_indices(&self.workspaces);
        for (k, &ws_idx) in pinned.iter().enumerate() {
            let Some(ws) = self.workspaces.get(ws_idx) else {
                continue;
            };
            let rect = crate::workspace::pinned_bubble_rect(k, pinned.len(), 1.0, w);
            layer = layer.child(self.pinned_bubble(theme, ws, ws_idx, &rect));
        }
        layer
    }

    /// One iMessage-style pinned bubble: a big round avatar with the group's
    /// name under it, absolutely positioned at the rect
    /// [`crate::workspace::pinned_bubble_rect`] handed the mouse path. The
    /// avatar kind is derived exactly the way [`Self::group_card`] derives it
    /// (`avatar_for` over the cached git context — never a blocking fetch), so
    /// a bubble reads the same state its card would have shown before the pin.
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
                .bg(gantry_accent(theme.dark))
                .shadow(vec![BoxShadow {
                    color: gantry_accent(theme.dark).opacity(0.35),
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
        .when(active, |d| d.border_2().border_color(gantry_accent(theme.dark)))
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
    /// Cleanup's repo filters, Notes' vaults and docs, Settings' section tabs.
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
    fn sidebar_cursor(&self) -> Option<(f32, f32)> {
        hover_cursor(
            self.cursor,
            self.scale(),
            matches!(self.drag, crate::Drag::None),
            self.modal_overlay_open(),
        )
    }

    /// One folder (section) header row: disclosure chevron, emoji, uppercase
    /// name, the `· N` member count while collapsed, a right-aligned count
    /// badge, the delete chip on hover, and the unread dot a collapsed folder
    /// bubbles up from its hidden members. Mirrors what the canvas painted so
    /// nothing regresses; the inline rename editor takes over the name slot
    /// when `editing_section` names this folder.
    fn section_header_row(
        &self,
        theme: &Theme,
        section: &crate::workspace::Section,
        rect: &crate::workspace::LayoutRect,
        hover: Option<(f32, f32)>,
    ) -> gpui::Div {
        let members = self
            .workspaces
            .iter()
            .filter(|w| w.section == Some(section.id))
            .count();
        // A folder speaks for its members: if any of them has unread output the
        // header carries the dot, collapsed or not — that is what the canvas
        // did, and its test pins both states.
        let unread = self
            .workspaces
            .iter()
            .any(|w| w.section == Some(section.id) && w.any_unread());
        let hovered = hover.is_some_and(|(x, y)| rect.contains(x, y));
        let editing = self
            .editing_section
            .as_ref()
            .filter(|(id, _)| *id == section.id)
            .map(|(_, buf)| buf.clone());
        let del = crate::workspace::section_delete_rect(rect, 1.0);

        let name = match &editing {
            // Mid-rename the raw buffer is the truth — no uppercasing, no
            // tracking, and a block caret so the row reads as an editor.
            Some(buf) => div()
                .flex_1()
                .overflow_hidden()
                .text_color(theme.foreground)
                .child(format!("{buf}▏")),
            None => div()
                .flex_none()
                // Reserve for what sits to the right of the name: the scaled
                // chevron and count badge, plus the delete chip's fixed 22px.
                // Only the first two grow with the text, so scaling all 88px
                // would starve the name column. The floor scales too, so it
                // always holds a legible glyph.
                .max_w(px((rect.w - scaled(40.0) - 22.0).max(scaled(40.0))))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(tracked(&if section.collapsed {
                    format!("{} · {members}", section.name.to_uppercase())
                } else {
                    section.name.to_uppercase()
                })),
        };

        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .flex()
            // Bottom-aligned inside the row box: the mock leaves generous air
            // above a folder header and sets it tight to the rows it opens.
            .items_end()
            .pb(px(scaled(4.0)))
            .gap(px(scaled(4.0)))
            .pl(px(scaled(ROW_PAD)))
            .pr(px(scaled(6.0)))
            .text_size(px(scaled(SECTION_TEXT)))
            .font_weight(FontWeight::BOLD)
            .text_color(theme.muted_foreground)
            .when(!section.emoji.is_empty(), |d| {
                d.child(div().flex_none().child(section.emoji.clone()))
            })
            .child(name)
            // Disclosure chevron TRAILS the name in the mock, small and quiet.
            .when(editing.is_none(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(scaled(7.0)))
                        .child(if section.collapsed { "▶" } else { "▼" }),
                )
            })
            // Eats the slack so the badge stays right-aligned.
            .child(div().flex_1())
            // Right-aligned count badge. Hidden while renaming so the editor
            // gets the width, and while the delete chip occupies the corner.
            .when(editing.is_none() && !hovered, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px(px(5.0))
                        .rounded(px(6.0))
                        .bg(theme.muted.opacity(if theme.dark { 0.7 } else { 0.9 }))
                        .child(members.to_string()),
                )
            })
            // Delete chip, drawn at the rect the mouse path resolves clicks
            // against so the affordance cannot drift from its hit box.
            .when(hovered && editing.is_none(), |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(del.x - rect.x))
                        .top(px(del.y - rect.y))
                        .w(px(del.w))
                        .h(px(del.h))
                        .flex()
                        .items_center()
                        .justify_center()
                        // Not scaled: the glyph sits in `section_delete_rect`'s
                        // fixed box, which is also its hit region — growing the
                        // ✕ past it would drift the affordance off its hit box.
                        .text_size(px(11.0))
                        .text_color(theme.muted_foreground)
                        .child("✕"),
                )
            })
            // Unread dot in the left gutter, outside the text column.
            .when(unread, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(scaled((ROW_PAD - UNREAD_DOT) / 2.0)))
                        .top(px((rect.h - scaled(UNREAD_DOT)).max(0.0) / 2.0))
                        .w(px(scaled(UNREAD_DOT)))
                        .h(px(scaled(UNREAD_DOT)))
                        .rounded_full()
                        .bg(theme.primary),
                )
            })
    }

    /// One Sessions preview card: avatar, title + timestamp, status line and
    /// diffstat, per SIDEBAR_SPEC's "Preview card". Everything it says comes
    /// from [`crate::sidebar_card`] — the wording is already decided there, so
    /// this only lays it out. The git context is read through
    /// [`crate::git_context::GitContextCache::get`] and nothing else: the
    /// blocking fetch never runs on the paint path, and a card whose context
    /// has not landed yet degrades to the group's own title.
    fn group_card(
        &self,
        theme: &Theme,
        ws: &crate::workspace::Workspace,
        ws_idx: usize,
        rect: &crate::workspace::LayoutRect,
        selected: bool,
        hover: Option<(f32, f32)>,
    ) -> gpui::Div {
        let hovered = hover.is_some_and(|(x, y)| {
            x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
        });
        let ctx = ws
            .cwd
            .as_deref()
            .and_then(|cwd| self.git_contexts.get(cwd));

        // Always the terminal pane's own name — the primary tile's active tab
        // title, which is what the user renames with `/rename` and what the tab
        // strip shows. It used to be `repo ⎇ branch` with this only as a
        // non-repo fallback, but the pane name is the thing the user chose.
        let title = ws.title();
        let rollup = ctx.map_or(PrRollup::None, |c| derive_rollup(c.pr.as_ref()));
        // Show when this card last asked for attention: the oldest still-unread
        // pane, or the most recent attention moment once everything is read.
        let stamp = ws.attention_at().map(|t| relative_time(t, SystemTime::now()));
        let status = ctx.map(status_line).unwrap_or_default();
        let diff = ctx.and_then(diffstat_line);

        // Text on the accent fill flips to the on-primary treatment; the
        // secondary lines lean on opacity so they stay legible on both.
        let strong = if selected {
            theme.primary_foreground
        } else {
            theme.foreground
        };
        let soft = if selected {
            theme.primary_foreground.opacity(0.78)
        } else {
            theme.muted_foreground
        };

        // The hotkey chip is decorative, but it is laid out from the same rect
        // helper the canvas used, so the title clip can never overlap it.
        let chip = (ws_idx < 9).then(|| crate::workspace::group_hotkey_chip_rect(rect, 1.0));
        let right_pad = chip.map_or(8.0, |c| rect.x + rect.w - c.x + 4.0);

        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(10.0))
            .when(selected, |d| d.bg(gantry_accent(theme.dark)))
            .when(!selected && hovered, |d| {
                d.bg(theme.muted.opacity(if theme.dark { 0.5 } else { 0.7 }))
            })
            .flex()
            .items_center()
            .gap(px(scaled(9.0)))
            .pl(px(scaled(ROW_PAD)))
            .pr(px(right_pad))
            .child(avatar(
                theme,
                avatar_for(
                    rollup,
                    ctx.and_then(|c| c.pr.as_ref()).is_some_and(|p| p.is_draft),
                ),
                selected,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(scaled(1.0)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(scaled(6.0)))
                            .text_size(px(scaled(12.0)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(strong)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .when_some(stamp, |d, s| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(scaled(10.0)))
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(soft)
                                        .child(s),
                                )
                            }),
                    )
                    .when(!status.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(scaled(10.5)))
                                .text_color(soft)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(status),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(scaled(6.0)))
                            .text_size(px(scaled(10.0)))
                            .font_family(crate::renderer::FONT_FAMILY)
                            .text_color(soft)
                            .map(|d| match diff {
                                Some(line) => d
                                    // Still green and red on the accent fill,
                                    // just lightened: a selected card's
                                    // diffstat has to keep reading as a
                                    // diffstat, so the colour survives
                                    // selection rather than washing to white.
                                    .child(
                                        div()
                                            .text_color(if selected {
                                                diff_added_on_accent()
                                            } else {
                                                diff_added(theme.dark)
                                            })
                                            .child(line.added),
                                    )
                                    .child(
                                        div()
                                            .text_color(if selected {
                                                diff_removed_on_accent()
                                            } else {
                                                diff_removed(theme.dark)
                                            })
                                            .child(typographic_minus(&line.removed)),
                                    )
                                    .child(div().child(line.files))
                                    // The uncommitted note rides in the muted
                                    // tint, not the diff greens and reds: it is
                                    // a state ("something is still local"), not
                                    // a count of added or removed lines.
                                    .when_some(line.uncommitted, |d, note| {
                                        d.child(div().child(format!("· {note}")))
                                    }),
                                None => d.child("no code changes"),
                            }),
                    ),
            )
            // ⌘1-9 hint, at the rect the canvas reserved for it.
            .when_some(chip, |d, c| {
                d.child(
                    div()
                        .absolute()
                        .left(px(c.x - rect.x))
                        .top(px(c.y - rect.y))
                        .w(px(c.w))
                        .h(px(c.h))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.0))
                        // Not scaled: `group_hotkey_chip_rect` sizes this pill,
                        // and the row's `right_pad` clears exactly that rect.
                        .text_size(px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(selected, |d| {
                            d.bg(theme.primary_foreground.opacity(0.2))
                                .text_color(theme.primary_foreground)
                        })
                        .when(!selected, |d| {
                            d.bg(theme.muted.opacity(if theme.dark { 0.8 } else { 0.9 }))
                                .text_color(theme.muted_foreground)
                        })
                        .child(format!("⌘{}", ws_idx + 1)),
                )
            })
            // Unread dot, centered in the left gutter like the canvas drew it.
            .when(ws.any_unread(), |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(scaled((ROW_PAD - UNREAD_DOT) / 2.0)))
                        .top(px((rect.h - scaled(UNREAD_DOT)).max(0.0) / 2.0))
                        .w(px(scaled(UNREAD_DOT)))
                        .h(px(scaled(UNREAD_DOT)))
                        .rounded_full()
                        .bg(if selected {
                            theme.primary_foreground
                        } else {
                            theme.primary
                        }),
                )
            })
    }
}

/// The 38px avatar circle: a tinted fill with a stroked glyph, one treatment
/// per [`CardAvatar`]. SVG icons are not available here, so the glyph is text.
fn avatar(theme: &Theme, kind: CardAvatar, selected: bool) -> gpui::Div {
    let tint = match kind {
        CardAvatar::NoPr => pr_none_ink(theme.dark),
        CardAvatar::Draft => gantry_accent(theme.dark),
        CardAvatar::Open => pr_open(theme.dark),
        CardAvatar::Merged => pr_merged(theme.dark),
    };
    // On the accent fill the disc goes white-on-white-wash; elsewhere it is a
    // low-alpha wash of its own state colour. The mock uses .25 over the accent
    // and .10-.12 otherwise, and — deliberately — no border on any of them: the
    // wash alone separates the disc from the panel.
    let (fill, ink) = if selected {
        (theme.primary_foreground.opacity(0.25), theme.primary_foreground)
    } else {
        (tint.opacity(AVATAR_WASH), tint)
    };
    div()
        .flex_none()
        .w(px(scaled(AVATAR)))
        .h(px(scaled(AVATAR)))
        .rounded_full()
        .bg(fill)
        .flex()
        .items_center()
        .justify_center()
        .child(
            // gpui's `svg()` is a monochrome mask tinted by `text_color`, which
            // is exactly what the mock's single-stroke commit graphs need.
            gpui::svg()
                .path(avatar_icon(kind))
                .w(px(scaled(AVATAR_ICON)))
                .h(px(scaled(AVATAR_ICON)))
                .text_color(ink),
        )
}

/// gpui has no letter-spacing property, so the mock's `.04em` tracking on a
/// folder header is set by hand: a hair space (U+200A) between characters. At
/// [`SECTION_TEXT`] that reads as the same airy, small-caps register without
/// touching the text system. Purely presentational — the section's real name
/// is never rewritten, and the rename editor skips this entirely.
fn tracked(label: &str) -> String {
    let mut out = String::with_capacity(label.len() * 2);
    for (i, ch) in label.chars().enumerate() {
        if i > 0 {
            out.push('\u{200a}');
        }
        out.push(ch);
    }
    out
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
/// The GANTRY mock's own accent — vitrine's `--accent`, `oklch(0.60 0.19 258)`
/// resolved to sRGB (and its dark-theme sibling).
///
/// The sidebar's selection fill is pinned to this rather than the chrome
/// theme's accent: the panel is a port of a specific design, and the chrome
/// accent (which the user retints freely) made the selected card read as a
/// different, heavier blue than the mock's.
fn gantry_accent(dark: bool) -> Hsla {
    let (r, g, b) = crate::theme::gantry_accent(dark);
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

/// The two diffstat tints a card uses *on the accent fill*: the normal green
/// and red go muddy there, so the mock lightens both (`#c6f3cd` / `#ffd4cf`)
/// rather than dropping the colour, which is what keeps a selected card's
/// diffstat readable as a diffstat.
fn diff_added_on_accent() -> Hsla {
    gpui::rgb(0xc6f3cd).into()
}

fn diff_removed_on_accent() -> Hsla {
    gpui::rgb(0xffd4cf).into()
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

/// Diff green. The diff viewer keeps its own private copies of these
/// (`pr_ui::green`/`pr_ui::red`), so the values are matched here rather than
/// shared — they are the one colour pair the theme has no token for.
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

/// The inlaid panel shell: gutter inset, 18px radius, hairline border, a soft
/// drop shadow with the spec's inset top rim highlight, and the vertical
/// material gradient.
fn panel(theme: &Theme, w: f32) -> gpui::Div {
    // The mock's panel is not a flat card — it is cool, blue-tinted glass that
    // fades over its top 30% and then holds: `#eaf4fb -> #f4f8fb 30%`. Deriving
    // the fill from `theme.card` alone (near-white in light chrome) is what made
    // it read as flat white, so the chrome material is blended *toward* the
    // mock's glass instead. The blend keeps a retinted chrome visible while the
    // cast stays GANTRY's.
    let (top, bottom) = if theme.dark {
        // Dark glass is the same idea inverted: vitrine's dark material is a
        // cool near-black, so lift the top slightly rather than tinting it.
        (shade(theme.card, 0.04), shade(theme.card, -0.015))
    } else {
        (
            mix_hsla(theme.card, gpui::rgb(0xeaf4fb).into(), GLASS_BLEND),
            mix_hsla(theme.card, gpui::rgb(0xf4f8fb).into(), GLASS_BLEND),
        )
    };

    div()
        .absolute()
        .left(px(GUTTER))
        .top(px(GUTTER))
        .w(px(w))
        .bottom(px(GUTTER))
        .flex()
        .flex_col()
        .rounded(px(PANEL_RADIUS))
        .border_1()
        // A hairline, not a drawn edge: the mock's is rgba(0,0,0,.08), which is
        // lighter than the theme's general-purpose border token.
        .border_color(theme.border.opacity(0.55))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(top, 0.0),
            // Stops at .30, so — as in the mock — the fade happens in the top
            // third and everything below it holds one tone.
            linear_color_stop(bottom, 0.30),
        ))
        .shadow(vec![
            // Drop shadow (`--shadow-2`).
            BoxShadow {
                color: gpui::black().opacity(if theme.dark { 0.45 } else { 0.14 }),
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(10.0),
                spread_radius: px(0.0),
                inset: false,
            },
            // Top rim highlight (`--highlight-top`), the one-pixel lit edge
            // that sells the inlaid material.
            BoxShadow {
                color: gpui::white().opacity(if theme.dark { 0.10 } else { 0.70 }),
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: true,
            },
        ])
}

/// How far the panel's fill is pulled toward the mock's glass tint. High enough
/// that the cast reads as GANTRY's, low enough that a retinted chrome theme
/// still tells.
const GLASS_BLEND: f32 = 0.72;

/// Blend two colours in sRGB, `t` of the way from `a` to `b`.
///
/// Via `Rgba` rather than interpolating `Hsla` directly: hue interpolation
/// between a near-grey and a tinted colour swings through whatever hue the grey
/// nominally has, which is not a blend anyone asked for.
fn mix_hsla(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (gpui::Rgba::from(a), gpui::Rgba::from(b));
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

/// Header block: the titlebar-height strip plus the toggle / new-session
/// glyph row.
///
/// The three traffic lights are *not* drawn here. They are the real native
/// macOS window buttons floating over the transparent titlebar (see
/// `workspace::titlebar`), so painting decorative dots would double-draw them.
/// Instead the strip reserves `TRAFFIC_LIGHT_SAFE_W` of left inset and the
/// glyph row sits below it, which is exactly the space
/// `workspace::sidebar_row_rect` skips before the first row.
fn header() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .child(
            // Native traffic-light strip: empty on purpose, sized so the rows
            // below line up with the geometry the canvas hit-tests against.
            div()
                .h(px(TITLEBAR_H - GUTTER))
                .flex_none()
                .pl(px(TRAFFIC_LIGHT_SAFE_W - GUTTER)),
        )
}

/// One header chip, painted at `rect` — the very rect the canvas mouse path
/// hit-tests for the action the glyph depicts. Hover lifts a rounded well
/// behind the glyph; the click itself is still resolved on the canvas, so no
/// gpui handler is attached here.
fn chip(
    theme: &Theme,
    rect: &crate::workspace::LayoutRect,
    hovered: bool,
    ch: &'static str,
) -> gpui::Div {
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
        .when(hovered, |c| {
            c.bg(theme.muted.opacity(if theme.dark { 0.5 } else { 0.7 }))
        })
        // Not scaled: a header chip is a fixed square icon well from
        // `header_chip_row`, sized by the titlebar rather than by the text size.
        .text_size(px(13.0))
        .text_color(if hovered { theme.foreground } else { theme.muted_foreground })
        .child(ch)
}

/// One slot of the page strip, painted at exactly the rect
/// [`crate::workspace::page_slot_rect`] hands back — the same rect
/// `main.rs::page_slot_at` turns a click into a page with.
///
/// `progress` is the slot's animation position: at 0 the slot is a small
/// muted dot, at 1 it is the page's glyph, and in between the two crossfade,
/// exactly the way the canvas painter drew it. The caller attaches the click
/// handler (`page_dot_layer`).
fn page_slot(
    theme: &Theme,
    rect: &crate::workspace::LayoutRect,
    glyph: &'static str,
    progress: f32,
) -> gpui::Div {
    let p = progress.clamp(0.0, 1.0);
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .flex()
        .items_center()
        .justify_center()
        .when(p < 1.0, |s| {
            s.child(
                div()
                    .absolute()
                    .w(px(PAGE_DOT))
                    .h(px(PAGE_DOT))
                    .rounded(px(PAGE_DOT / 2.0))
                    .bg(theme.muted_foreground.opacity(0.45 * (1.0 - p))),
            )
        })
        .when(p > 0.0, |s| {
            s.child(
                div()
                    .absolute()
                    // Not scaled: the glyph is centred in `page_slot_rect`'s
                    // fixed slot inside the page strip, whose height is chrome
                    // geometry rather than body text.
                    .text_size(px(PAGE_GLYPH_TEXT))
                    .text_color(theme.foreground.opacity(p))
                    .child(glyph),
            )
        })
}

/// The Cleanup sidebar's rows, in slot order: `("All", no filter)` first, then
/// one `("{repo} · {count}", is the filter)` row per repo the scan found.
///
/// Pure so the row *order* — which is what `main.rs` turns a click's slot index
/// back into a repo filter with — can be tested without a renderer. An
/// unscanned or failed scan yields just the `All` row, because
/// [`crate::cleanup::Cleanup::repos`] is empty until the scan is `Ready`.
fn cleanup_rows(cleanup: &crate::cleanup::Cleanup) -> Vec<(String, bool)> {
    let filter = cleanup.repo_filter.as_deref();
    let mut rows = vec![("All".to_string(), filter.is_none())];
    for repo in &cleanup.repos() {
        rows.push((
            format!("{} · {}", repo.display, repo.count),
            filter == Some(repo.root.as_str()),
        ));
    }
    rows
}

/// The label of the Notes sidebar's trailing row. ASCII on purpose — it is what
/// the canvas painted, and the row is decorative until `main.rs` toggles the
/// add-vault prompt from the same slot.
const ADD_VAULT_LABEL: &str = "+ Add vault";

/// What a Notes sidebar slot addresses.
///
/// The rows are three stacked ranges — `[0, V)` vaults, `[V, V + D)` the active
/// vault's docs, and `V + D` the add-vault row — so a bare index means nothing
/// without the two counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotesRow {
    /// The `i`th registered vault.
    Vault(usize),
    /// The `i`th doc of the *active* vault.
    Doc(usize),
    /// The trailing row that opens the add-vault prompt.
    AddVault,
}

/// Map a Notes sidebar slot onto the thing it addresses, or `None` when the
/// slot is past the end of the list.
///
/// This is the whole of the Notes row vocabulary, kept pure so it can be tested
/// against the ranges `main.rs`'s `Page::Notes` mouse branch walks: paint and
/// hit-test agree exactly when they agree here.
fn notes_row(index: usize, n_vaults: usize, n_docs: usize) -> Option<NotesRow> {
    if index < n_vaults {
        Some(NotesRow::Vault(index))
    } else if index < n_vaults + n_docs {
        Some(NotesRow::Doc(index - n_vaults))
    } else if index == n_vaults + n_docs {
        Some(NotesRow::AddVault)
    } else {
        None
    }
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

/// A vault's row label: its directory name, falling back to the whole path when
/// the vault sits at a filesystem root and has no final component.
fn vault_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}


/// Nudge a token's lightness by `delta`, clamped. Used for the panel's
/// vertical gradient so the material derives from the live theme instead of
/// the mock's fixed grays.
fn shade(color: Hsla, delta: f32) -> Hsla {
    Hsla {
        l: (color.l + delta).clamp(0.0, 1.0),
        ..color
    }
}

#[cfg(test)]
mod tests {

    /// The tile cards and this panel must round off identically — the mock
    /// draws both on vitrine's `--radius-l`, and a mismatch reads as two
    /// different materials sitting next to each other.
    #[test]
    fn panel_and_tile_cards_share_a_radius() {
        assert_eq!(PANEL_RADIUS, crate::renderer::CARD_RADIUS);
    }
    use gpui::AssetSource as _;
    use super::*;
    use crate::cleanup::{Cleanup, ScanState, WorktreeInfo};
    use std::path::Path;

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

    /// A worktree in `repo_root` — only the fields the row list reads matter.
    fn wt(repo_root: &str, id: &str) -> WorktreeInfo {
        WorktreeInfo {
            repo_root: repo_root.to_string(),
            path: format!("{repo_root}/.worktrees/{id}"),
            id: id.to_string(),
            branch: Some(format!("feat/{id}")),
            head: "abc1234".to_string(),
            dirty_count: 0,
            merged: false,
            ahead: 0,
            behind: 0,
            last_activity_ms: 0.0,
            is_current: false,
            pr: None,
            dirty_files: Vec::new(),
        }
    }

    fn ready(worktrees: Vec<WorktreeInfo>, filter: Option<&str>) -> Cleanup {
        let mut state = Cleanup::default();
        state.scan = Some(ScanState::Ready(worktrees));
        state.repo_filter = filter.map(str::to_string);
        state
    }

    #[test]
    fn cleanup_rows_lead_with_all_then_one_row_per_repo() {
        let state = ready(
            vec![
                wt("/src/alpha", "wt-1"),
                wt("/src/alpha", "wt-2"),
                wt("/src/beta", "wt-3"),
            ],
            None,
        );
        let rows = cleanup_rows(&state);
        assert_eq!(
            rows,
            vec![
                ("All".to_string(), true),
                ("alpha · 2".to_string(), false),
                ("beta · 1".to_string(), false),
            ]
        );
    }

    #[test]
    fn cleanup_rows_move_the_active_flag_onto_the_filtered_repo() {
        let state = ready(
            vec![wt("/src/alpha", "wt-1"), wt("/src/beta", "wt-2")],
            Some("/src/beta"),
        );
        let rows = cleanup_rows(&state);
        // Slot 0 is "All" and stops being active; slot 2 is the second repo,
        // the same slot main.rs maps back onto `repos()[1]`.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ("All".to_string(), false));
        assert_eq!(rows[1].1, false);
        assert_eq!(rows[2], ("beta · 1".to_string(), true));
    }

    #[test]
    fn cleanup_rows_are_just_all_before_a_scan_lands() {
        assert_eq!(
            cleanup_rows(&Cleanup::default()),
            vec![("All".to_string(), true)]
        );
        let failed = {
            let mut state = Cleanup::default();
            state.set_failed("boom".to_string());
            state
        };
        assert_eq!(cleanup_rows(&failed), vec![("All".to_string(), true)]);
    }

    #[test]
    fn notes_rows_stack_vaults_then_docs_then_add_vault() {
        // Two vaults, three docs: [0,2) vaults, [2,5) docs, 5 add-vault — the
        // ranges main.rs's Page::Notes mouse branch walks.
        let rows: Vec<Option<NotesRow>> = (0..7).map(|i| notes_row(i, 2, 3)).collect();
        assert_eq!(
            rows,
            vec![
                Some(NotesRow::Vault(0)),
                Some(NotesRow::Vault(1)),
                Some(NotesRow::Doc(0)),
                Some(NotesRow::Doc(1)),
                Some(NotesRow::Doc(2)),
                Some(NotesRow::AddVault),
                None,
            ]
        );
    }

    #[test]
    fn notes_rows_degenerate_to_just_add_vault() {
        // No vaults registered yet: slot 0 is the add-vault row, nothing after.
        assert_eq!(notes_row(0, 0, 0), Some(NotesRow::AddVault));
        assert_eq!(notes_row(1, 0, 0), None);
        // A vault with no docs keeps add-vault directly under the vault row.
        assert_eq!(notes_row(0, 1, 0), Some(NotesRow::Vault(0)));
        assert_eq!(notes_row(1, 1, 0), Some(NotesRow::AddVault));
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

    #[test]
    fn vault_labels_prefer_the_directory_name() {
        assert_eq!(vault_label(Path::new("/src/notes/work")), "work");
        // A path with no final component falls back to the whole path.
        assert_eq!(vault_label(Path::new("/")), "/");
    }
}
