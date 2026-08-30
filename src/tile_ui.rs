//! Tab strips as a gpui element tree over the canvas: every tile's strip
//! here, and — through [`tab_strip`] — the flyover panel's in `flyover_ui`.
//!
//! The tab pills, titles, × buttons and unread dots used to be canvas quads
//! and labels (`Renderer::build_frame` / `flyover_overlay`). Only the
//! *pixels* have moved here: geometry still comes from [`crate::workspace`]
//! (`layout_tiles` / `tab_strip_rect` / `tile_tab_rect` /
//! `tile_tab_close_rect`), and every click, drag and drop is still resolved
//! on the canvas mouse path in `main.rs` against those same rects — the
//! same first step the sidebar took. What the element tree buys is
//! clipping: each strip is an `overflow_hidden` box, so a title can never
//! bleed past its tab or its card, and the strip sits in the tree's
//! z-order (under the sidebar and the modals) instead of in the canvas's
//! hand-kept paint order.
//!
//! Still canvas-painted, deliberately: the collapse caret (a rotating
//! chevron the canvas draws as line segments), the card divider, the
//! side-strip hover fill, and the drag-and-drop hints — all of which sit
//! *around* the strip rather than in it.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App as GpuiApp, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Styled, Window, div, px, prelude::FluentBuilder as _,
};

use crate::App;
use crate::renderer::color;
use crate::ui::theme::Theme;
use crate::workspace::{self, LayoutRect};

/// Inset of the active/hover pill from its tab's edges.
const PILL_INSET: f32 = 4.0;
/// Inset of the × hover chip from the close rect.
const CHIP_INSET: f32 = 3.0;
/// Left padding of a tab's title.
const TEXT_PAD: f32 = 8.0;
/// Unread dot diameter and the gap after it.
const DOT: f32 = 6.0;
const DOT_GAP: f32 = 5.0;
/// The on-accent ink for the focused pane's active tab.
const ON_ACCENT_INK: (u8, u8, u8) = (255, 255, 255);

/// The colors a strip paints with — resolved from the terminal scheme the
/// way the canvas did, so strips stay legible on light palettes.
#[derive(Clone)]
pub(crate) struct StripStyle {
    pub ink: Hsla,
    pub ink_dim: Hsla,
    pub pill_rgb: (u8, u8, u8),
    pub pill_alpha: f32,
    /// `Some(accent)`: the active tab wears a solid accent pill with
    /// on-accent ink (a focused tile). `None`: glass, as the flyover does.
    pub accent: Option<Hsla>,
    pub unread: Hsla,
    /// `Some(r)` for a fixed pill radius; `None` for a capsule.
    pub pill_radius: Option<f32>,
    /// `Some(r)` for a fixed × chip radius; `None` for a capsule.
    pub chip_radius: Option<f32>,
}

impl StripStyle {
    /// The canvas's scheme mapping: a selected terminal scheme drives the
    /// ink and pill, the adaptive default keeps the chrome theme's colors
    /// (`default_pill_alpha` differs between tiles and the flyover).
    pub(crate) fn from_scheme(th: &crate::theme::Theme, default_pill_alpha: f32) -> Self {
        let scheme = crate::term_theme::selected(crate::theme::dark_active());
        let (ink, ink_dim, pill_rgb, pill_alpha) = match scheme {
            Some(t) => (color(t.fg, 1.0), color(t.fg, 0.55), t.fg, 0.12),
            None => (
                color(th.text_bright, 1.0),
                color(th.text_dim, 1.0),
                (255, 255, 255),
                default_pill_alpha,
            ),
        };
        Self {
            ink,
            ink_dim,
            pill_rgb,
            pill_alpha,
            accent: None,
            unread: color(th.accent, 1.0),
            pill_radius: None,
            chip_radius: None,
        }
    }
}

/// A press on tab `index` of a strip (`close` when it landed on the ×).
/// Handlers stop propagation themselves so the canvas mouse path never
/// re-resolves the press; the canvas still drives any drag that follows.
pub(crate) type PressHandler = Rc<dyn Fn(usize, bool, &MouseDownEvent, &mut GpuiApp)>;

/// One tab of a strip: its title and the physical-px rects the canvas laid
/// it out at (the tab and its × button).
pub(crate) struct StripTab {
    pub title: String,
    pub unread: bool,
    pub tab: LayoutRect,
    pub close: LayoutRect,
}

/// The strip box for `bar` (physical px, converted with `inv`) holding the
/// tabs: active/hover pills, titles clipped short of ×, unread dots, and the
/// × with its hover chip. Returns the absolutely positioned, clipped box so
/// the caller can append controls of its own (the flyover's window buttons)
/// before mounting it.
pub(crate) fn tab_strip(
    bar: &LayoutRect,
    inv: f32,
    tabs: &[StripTab],
    active: usize,
    hov: &dyn Fn(&LayoutRect) -> bool,
    style: &StripStyle,
    on_press: PressHandler,
) -> gpui::Div {
    let mut strip_el = div()
        .absolute()
        .left(px(bar.x * inv))
        .top(px(bar.y * inv))
        .w(px(bar.w * inv))
        .h(px(bar.h * inv))
        .overflow_hidden();

    for (ti, tab) in tabs.iter().enumerate() {
        let is_active = ti == active;
        let close_hov = hov(&tab.close);
        let tab_hov = hov(&tab.tab) && !close_hov;
        // Rects relative to the strip box, in logical px.
        let rel = |r: &LayoutRect| ((r.x - bar.x) * inv, (r.y - bar.y) * inv, r.w * inv, r.h * inv);
        let (tx, ty, tw, tth) = rel(&tab.tab);
        let (cx_, cy_, cw, ch) = rel(&tab.close);

        let press = on_press.clone();
        let mut tab_el = div()
            .absolute()
            .left(px(tx))
            .top(px(ty))
            .w(px(tw))
            .h(px(tth))
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, move |ev, _win: &mut Window, app: &mut GpuiApp| {
                press(ti, false, ev, app)
            });

        // Pill: the active tab's (accent when the pane is focused, glass
        // otherwise), or a half-strength preview on hover.
        if is_active || tab_hov {
            let pill_h = (tth - 2.0 * PILL_INSET).max(0.0);
            let mut pill = div()
                .absolute()
                .left(px(PILL_INSET))
                .top(px(PILL_INSET))
                .w(px((tw - 2.0 * PILL_INSET).max(0.0)))
                .h(px(pill_h))
                .rounded(px(style.pill_radius.unwrap_or(pill_h / 2.0)));
            pill = match (is_active, style.accent) {
                (true, Some(accent)) => pill.bg(accent),
                (true, None) => glass(
                    pill,
                    color(style.pill_rgb, style.pill_alpha),
                    color_alpha(style.ink, 0.18),
                ),
                _ => glass(
                    pill,
                    color(style.pill_rgb, style.pill_alpha * 0.55),
                    color_alpha(style.ink, 0.10),
                ),
            };
            tab_el = tab_el.child(pill);
        }

        // Title (with the unread dot before it), clipped short of ×.
        let text = if tab.title.is_empty() { "shell".to_string() } else { tab.title.clone() };
        let text_color = match (is_active, style.accent) {
            (true, Some(_)) => color(ON_ACCENT_INK, 1.0),
            (true, None) => style.ink,
            _ => style.ink_dim,
        };
        let mut text_left = TEXT_PAD;
        if tab.unread {
            tab_el = tab_el.child(
                div()
                    .absolute()
                    .left(px(text_left))
                    .top(px(((tth - DOT) / 2.0).round()))
                    .w(px(DOT))
                    .h(px(DOT))
                    .rounded(px(DOT / 2.0))
                    .bg(style.unread),
            );
            text_left += DOT + DOT_GAP;
        }
        tab_el = tab_el.child(
            div()
                .absolute()
                .left(px(text_left))
                .top(px(0.0))
                .h(px(tth))
                .w(px((cx_ - tx - text_left).max(0.0)))
                .overflow_hidden()
                .whitespace_nowrap()
                .flex()
                .items_center()
                .text_color(text_color)
                .child(text),
        );

        // × and its hover chip. Its press wins over the tab's: the inner
        // listener runs first and stops propagation.
        let chip_h = (ch - 2.0 * CHIP_INSET).max(0.0);
        let press = on_press.clone();
        tab_el = tab_el.child(
            div()
                .absolute()
                .left(px(cx_ - tx))
                .top(px(cy_ - ty))
                .w(px(cw))
                .h(px(ch))
                .on_mouse_down(MouseButton::Left, move |ev, _win: &mut Window, app: &mut GpuiApp| {
                    press(ti, true, ev, app)
                })
                .when(close_hov, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(CHIP_INSET))
                            .top(px(CHIP_INSET))
                            .w(px((cw - 2.0 * CHIP_INSET).max(0.0)))
                            .h(px(chip_h))
                            .rounded(px(style.chip_radius.unwrap_or(chip_h / 2.0)))
                            .bg(color(style.pill_rgb, (style.pill_alpha * 2.0).min(1.0))),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(0.0))
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(if close_hov { style.ink } else { style.ink_dim })
                        .child("×"),
                ),
        );

        strip_el = strip_el.child(tab_el);
    }
    strip_el
}

impl App {
    /// Every tile's tab strip, or an empty element off the Sessions page.
    pub fn render_tile_chrome(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.page != crate::Page::Sessions || self.is_empty_state() {
            return div().into_any_element();
        }
        // The flyover is canvas-painted and slides over the bottom of the
        // tiles; stop this layer above it exactly as the sidebar does.
        let ceiling = self.flyover_ceiling();
        if ceiling == Some(0.0) {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let th = crate::theme::current();
        let scale = self.scale();
        let inv = 1.0 / scale;
        let ws = &self.workspaces[self.active];
        let area = self.area();
        let sidebar_w = self.sidebar_w();
        let (tiles, _) = workspace::layout_tiles(&ws.root, area, scale);
        let axis_map: HashMap<u64, Option<workspace::Dir>> =
            workspace::tile_collapse_axis(&ws.root).into_iter().collect();

        let base = StripStyle::from_scheme(th, 0.13);
        let accent = color(crate::theme::accent_color(), 1.0);

        // Hover in physical px, like the canvas: none while dragging or
        // under a modal.
        let modal = self.modal_overlay_open();
        let cur = if matches!(self.drag, crate::Drag::None) && !modal {
            Some((self.cursor.0 as f32, self.cursor.1 as f32))
        } else {
            None
        };
        let hov = |r: &LayoutRect| cur.is_some_and(|(x, y)| r.contains(x, y));
        let font = crate::renderer::chrome_font();
        let entity = cx.entity().downgrade();

        let mut layer = div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w_full()
            .overflow_hidden()
            .map(|d| match ceiling {
                Some(limit) => d.h(px(limit)),
                None => d.h_full(),
            })
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(font));

        for (id, rect) in &tiles {
            let Some(tile) = ws.root.find_tile(*id) else { continue };
            let axis = axis_map.get(id).copied().flatten();
            let has_caret = axis.is_some();
            let collapsing = has_caret && (tile.collapsed || tile.collapse_anim > 0.0);
            let tile_id = *id;
            // A sideways strip shows only its caret (canvas-painted); the
            // whole bare card is one press target that expands it.
            if axis == Some(workspace::Dir::Row) && collapsing {
                if tile.collapsed {
                    let entity = entity.clone();
                    layer = layer.child(
                        div()
                            .absolute()
                            .left(px(rect.x * inv))
                            .top(px(rect.y * inv))
                            .w(px(rect.w * inv))
                            .h(px(rect.h * inv))
                            .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                                app.stop_propagation();
                                if let Some(entity) = entity.upgrade() {
                                    entity.update(app, |this, cx| {
                                        this.note_pointer(ev);
                                        this.press_tile_expand(tile_id);
                                        cx.notify();
                                    });
                                }
                            }),
                    );
                }
                continue;
            }
            let focused = ws.focused_tile == *id;
            let strip = workspace::tab_strip_rect(area, rect, scale, sidebar_w);
            let bar = workspace::tile_tab_bar(&strip, scale);
            let n = tile.tabs.len();
            let tabs: Vec<StripTab> = tile
                .tabs
                .iter()
                .enumerate()
                .map(|(ti, tab)| StripTab {
                    title: tab.session.title(),
                    unread: tab.unread,
                    tab: workspace::tile_tab_rect(&strip, ti, n, scale, has_caret),
                    close: workspace::tile_tab_close_rect(&strip, ti, n, scale, has_caret),
                })
                .collect();
            let style = StripStyle { accent: focused.then_some(accent), ..base.clone() };
            let press_entity = entity.clone();
            let on_press: PressHandler = Rc::new(move |ti, close, ev, app| {
                app.stop_propagation();
                if let Some(entity) = press_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.note_pointer(ev);
                        this.press_tile_tab(tile_id, ti, close, ev.click_count);
                        cx.notify();
                    });
                }
            });
            let mut strip_el = tab_strip(&bar, inv, &tabs, tile.active, &hov, &style, on_press);
            // The collapse caret: canvas-painted (a rotating chevron), but its
            // press is an element target at the same square.
            if has_caret {
                let cr = workspace::tile_caret_rect(rect, scale);
                let entity = entity.clone();
                strip_el = strip_el.child(
                    div()
                        .absolute()
                        .left(px((cr.x - bar.x) * inv))
                        .top(px((cr.y - bar.y) * inv))
                        .w(px(cr.w * inv))
                        .h(px(cr.h * inv))
                        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                            app.stop_propagation();
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, cx| {
                                    this.note_pointer(ev);
                                    this.press_tile_caret(tile_id, ev.click_count);
                                    cx.notify();
                                });
                            }
                        }),
                );
            }
            layer = layer.child(strip_el);
        }
        layer.into_any_element()
    }
}

/// The canvas `glass` treatment: a fill with a half-pixel rim.
fn glass(d: gpui::Div, fill: Hsla, rim: Hsla) -> gpui::Div {
    d.bg(fill).border(px(0.5)).border_color(rim)
}

fn color_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
