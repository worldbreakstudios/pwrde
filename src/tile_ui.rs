//! Tab strips as a gpui element tree over the canvas: every tile's strip
//! here, and — through [`tab_strip`] — the flyover panel's in `flyover_ui`.
//!
//! The mock: a 40px bar (6px of padding, a 30px tab row, 4px below) with no
//! fill of its own, sitting on the terminal ground. The active tab is a chip —
//! an 8px-rounded card of the mock's white at .07 with a .10 border (dimmed to
//! .04 with no border while its tile is unfocused) — holding a 14px terminal /
//! globe glyph, a 12.5px title clipped short of the ×, and the × itself.
//! Inactive tabs are text only; a 1×14 hairline sits 10px off each tab edge,
//! and a "+" New tab button follows the last one.
//!
//! The geometry — chips, closes, hairlines, the "+", the caret — is one pure
//! computation per strip in [`crate::workspace`] (`tile_strip_layout` /
//! `flyover_strip_layout`), and every click, drag and drop is still resolved
//! on the canvas mouse path in `main.rs` against that same layout, so painted
//! and hit-tested rects cannot disagree.
//!
//! What the element tree buys is clipping: each strip is an `overflow_hidden`
//! box, so a title can never bleed past its tab or its card, and the strip sits
//! in the tree's z-order (under the sidebar and the modals) instead of in the
//! canvas's hand-kept paint order.
//!
//! Still canvas-painted, deliberately: the card divider, the side-strip hover
//! fill, and the drag-and-drop hints — all of which sit *around* the strip
//! rather than in it. The collapse caret is the same standard `svg` chevron it
//! became when the canvas glyphs went away.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App as GpuiApp, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Styled, Window, div, px, prelude::FluentBuilder as _,
};

use crate::App;
use crate::renderer::color;
use crate::ui::assets::{ICON_GLOBE, ICON_PLUS, ICON_TERMINAL};
use crate::ui::icon;
use crate::ui::theme::Theme;
use crate::workspace::{self, LayoutRect};

/// The chip: radius 8, 10px of padding on the left, 8px on the right, and 8px
/// between its children.
const CHIP_RADIUS: f32 = 8.0;
const CHIP_PAD_L: f32 = 10.0;
const CHIP_PAD_R: f32 = 8.0;
const CHIP_GAP: f32 = 8.0;
/// Horizontal padding inside an inactive tab.
const TAB_PAD_H: f32 = 6.0;
/// The tab glyph (terminal / globe) and the title's type size.
const GLYPH: f32 = 14.0;
const TITLE_SIZE: f32 = 12.5;
/// The "+" New tab glyph.
const NEW_TAB_GLYPH: f32 = 15.0;
/// Room an active chip keeps clear at its right for the ×: the 8px gap plus
/// the 14px glyph.
const CLOSE_AREA: f32 = 22.0;
/// Unread dot diameter and the gap after it.
const DOT: f32 = 6.0;
const DOT_GAP: f32 = 5.0;
/// The mock's whites as alphas over the scheme ink (`pill_rgb`): the chip's
/// fill and border, the dimmed unfocused chip, the faint hover preview, the
/// × chip and the separator hairlines.
const CHIP_FILL: f32 = 0.07;
const CHIP_BORDER: f32 = 0.10;
const CHIP_FILL_DIM: f32 = 0.04;
const CHIP_FILL_HOVER: f32 = 0.035;
const CLOSE_CHIP: f32 = 0.12;
const SEP_ALPHA: f32 = 0.12;

/// The colors a strip paints with — resolved from the terminal scheme the way
/// the canvas did, so strips stay legible on light palettes.
///
/// The mock's whites are alphas over the scheme ink (`pill_rgb`) and its two
/// greys are `ink` / `ink_dim`, so a light palette stays legible: on the dark
/// default the result is the mock's .07 chip, .10 border, bright title ink and
/// dim icon / × / inactive ink.
#[derive(Clone)]
pub(crate) struct StripStyle {
    /// The title ink (a focused chip's title) and the dim ink for everything
    /// else — icons, inactive titles, the ×, and an unfocused tile's title.
    pub ink: Hsla,
    pub ink_dim: Hsla,
    /// The white the chip, its border and the hairlines are painted with.
    pub pill_rgb: (u8, u8, u8),
    /// The tile owns focus: its active chip is the mock's full-strength .07
    /// fill with a .10 border. An unfocused tile dims the chip to .04 and
    /// drops the border. The flyover is always focused while it is up.
    pub focused: bool,
    pub unread: Hsla,
}

impl StripStyle {
    /// The canvas's scheme mapping: a selected terminal scheme drives the ink
    /// and the chip white, the adaptive default keeps the chrome theme's.
    pub(crate) fn from_scheme(th: &crate::theme::Theme) -> Self {
        let scheme = crate::term_theme::selected(crate::theme::dark_active());
        // The ink follows the resolved foreground, so a per-colour override
        // (or a custom theme that renames the base) reads on the strip too.
        let resolved = crate::term_theme::resolved(crate::theme::dark_active());
        let (ink, ink_dim, pill_rgb) = match scheme {
            Some(_) => {
                let fg = resolved.colors.fg;
                (color(fg, 1.0), color(fg, 0.55), fg)
            },
            None => (color(th.text_bright, 1.0), color(th.text_dim, 1.0), (255, 255, 255)),
        };
        Self { ink, ink_dim, pill_rgb, focused: false, unread: color(th.accent, 1.0) }
    }
}

/// A press on tab `index` of a strip (`close` when it landed on the ×).
/// Handlers stop propagation themselves so the canvas mouse path never
/// re-resolves the press; the canvas still drives any drag that follows.
pub(crate) type PressHandler = Rc<dyn Fn(usize, bool, &MouseDownEvent, &mut GpuiApp)>;

/// A press on the strip's "+" New tab button.
pub(crate) type NewTabHandler = Rc<dyn Fn(&mut GpuiApp)>;

/// One tab of a strip: its title, its glyph kind, and the physical-px rects
/// the strip layout handed it (the tab and its × button).
pub(crate) struct StripTab {
    pub title: String,
    pub unread: bool,
    /// Pinned tabs keep their full title but draw a pin ring before it and
    /// no × — they can't be closed until unpinned.
    pub pinned: bool,
    /// A webview tab wears a globe glyph; a terminal tab a terminal glyph.
    pub webview: bool,
    pub tab: LayoutRect,
    pub close: LayoutRect,
}

/// The strip box for `layout` (physical px, converted with `inv`): the mock's
/// chips, glyphs, titles, × buttons, hairlines and "+" button, all at the rects
/// [`crate::workspace::tile_strip_layout`] — or its flyover mirror — hands the
/// canvas mouse path as well, so painted and hit-tested geometry cannot
/// disagree. Returns the absolutely positioned, clipped box so the caller can
/// append controls of its own (the flyover's window buttons) before mounting
/// it.
pub(crate) fn tab_strip(
    layout: &workspace::StripLayout,
    inv: f32,
    tabs: &[StripTab],
    active: usize,
    hov: &dyn Fn(&LayoutRect) -> bool,
    style: &StripStyle,
    close_icon: &str,
    on_press: PressHandler,
    on_new_tab: NewTabHandler,
) -> gpui::Div {
    let bar = &layout.bar;
    let mut strip_el = div()
        .absolute()
        .left(px(bar.x * inv))
        .top(px(bar.y * inv))
        .w(px(bar.w * inv))
        .h(px(bar.h * inv))
        .overflow_hidden();
    // Rects relative to the strip box, in logical px.
    let rel = |r: &LayoutRect| ((r.x - bar.x) * inv, (r.y - bar.y) * inv, r.w * inv, r.h * inv);

    // A hairline between adjacent tabs — and between the last tab and the "+".
    for sep in &layout.separators {
        let (sx, sy, sw, sh) = rel(sep);
        strip_el = strip_el.child(
            div()
                .absolute()
                .left(px(sx))
                .top(px(sy))
                .w(px(sw))
                .h(px(sh))
                .bg(color(style.pill_rgb, SEP_ALPHA)),
        );
    }

    for (ti, tab) in tabs.iter().enumerate() {
        let is_active = ti == active;
        // Pinned tabs render no ×, so their close rect must not eat the chip
        // hover either — a press there selects the tab.
        let close_hov = !tab.pinned && hov(&tab.close);
        let tab_hov = hov(&tab.tab) && !close_hov;
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

        // The chip: the active tab's — the mock's .07 fill with a .10 border
        // while its tile is focused, .04 and no border while it is not — or
        // the faint preview an inactive tab shows on hover (no border, and it
        // reserves no width).
        if is_active || tab_hov {
            let alpha = if is_active {
                if style.focused { CHIP_FILL } else { CHIP_FILL_DIM }
            } else {
                CHIP_FILL_HOVER
            };
            let mut chip = div()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .w(px(tw))
                .h(px(tth))
                .rounded(px(CHIP_RADIUS))
                .bg(color(style.pill_rgb, alpha));
            if is_active && style.focused {
                chip = chip.border_1().border_color(color(style.pill_rgb, CHIP_BORDER));
            }
            tab_el = tab_el.child(chip);
        }

        // The title row: glyph, pin ring, unread dot, then the title, clipped
        // short of the × on an active chip.
        let text = if tab.title.is_empty() { "shell".to_string() } else { tab.title.clone() };
        let text_color = if is_active && style.focused { style.ink } else { style.ink_dim };
        let icon_path = if tab.webview { ICON_GLOBE } else { ICON_TERMINAL };
        let mut x = if is_active { CHIP_PAD_L } else { TAB_PAD_H };
        tab_el = tab_el.child(
            div()
                .absolute()
                .left(px(x))
                .top(px((tth - GLYPH) / 2.0))
                .w(px(GLYPH))
                .h(px(GLYPH))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_path, px(GLYPH * inv), style.ink_dim)),
        );
        x += GLYPH + CHIP_GAP;
        // Pin ring: a hollow dot in the tab's own ink so it reads apart from
        // the filled accent unread dot that may follow it.
        if tab.pinned {
            tab_el = tab_el.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(((tth - DOT) / 2.0).round()))
                    .w(px(DOT))
                    .h(px(DOT))
                    .rounded(px(DOT / 2.0))
                    .border_1()
                    .border_color(text_color),
            );
            x += DOT + DOT_GAP;
        }
        if tab.unread {
            tab_el = tab_el.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(((tth - DOT) / 2.0).round()))
                    .w(px(DOT))
                    .h(px(DOT))
                    .rounded(px(DOT / 2.0))
                    .bg(style.unread),
            );
            x += DOT + DOT_GAP;
        }
        // An active chip keeps its × area clear; an inactive tab's title runs
        // to its padding (its × appears only on hover, over the title).
        let text_right = if is_active && !tab.pinned {
            (tw - CHIP_PAD_R - CLOSE_AREA).max(0.0)
        } else {
            (tw - TAB_PAD_H).max(0.0)
        };
        tab_el = tab_el.child(
            div()
                .absolute()
                .left(px(x))
                .top(px(0.0))
                .h(px(tth))
                .w(px((text_right - x).max(0.0)))
                .overflow_hidden()
                .whitespace_nowrap()
                .flex()
                .items_center()
                .text_size(px(TITLE_SIZE))
                .text_color(text_color)
                .child(text),
        );

        // A pinned tab has no ×: its close rect stays in the canvas hit list
        // but presses there fall through to the tab (`close_active_tab`
        // refuses pinned tabs regardless). Any other tab shows its × while it
        // is active or hovered — so an inactive tab can still be closed.
        if tab.pinned || !(is_active || close_hov || tab_hov) {
            strip_el = strip_el.child(tab_el);
            continue;
        }

        // × and its hover chip. Its press wins over the tab's: the inner
        // listener runs first and stops propagation.
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
                            .left(px(0.0))
                            .top(px(0.0))
                            .w(px(cw))
                            .h(px(ch))
                            .rounded(px(CHIP_RADIUS))
                            .bg(color(style.pill_rgb, CLOSE_CHIP)),
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
                        .child(icon(
                            close_icon,
                            px(GLYPH * inv),
                            if close_hov { style.ink } else { style.ink_dim },
                        )),
                ),
        );

        strip_el = strip_el.child(tab_el);
    }

    // The "+" New tab button, after the final separator. Its press stops
    // propagation like the tabs' so the canvas path never re-resolves it.
    if let Some(plus) = layout.new_tab {
        let (plus_x, plus_y, plus_w, plus_h) = rel(&plus);
        let plus_hov = hov(&plus);
        let go = on_new_tab.clone();
        strip_el = strip_el.child(
            div()
                .absolute()
                .left(px(plus_x))
                .top(px(plus_y))
                .w(px(plus_w))
                .h(px(plus_h))
                .rounded(px(CHIP_RADIUS))
                .when(plus_hov, |d| d.bg(color(style.pill_rgb, CHIP_FILL_HOVER)))
                .on_mouse_down(MouseButton::Left, move |_ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                    app.stop_propagation();
                    go(app)
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
                        .child(icon(
                            ICON_PLUS,
                            px(NEW_TAB_GLYPH * inv),
                            if plus_hov { style.ink } else { style.ink_dim },
                        )),
                ),
        );
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

        let base = StripStyle::from_scheme(th);
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
                    let cr = workspace::tile_caret_rect(rect, scale);
                    let entity = entity.clone();
                    let badge = tile
                        .tabs
                        .iter()
                        .any(|t| t.unread)
                        .then(|| {
                            div()
                                .absolute()
                                .top(px(3.0))
                                .right(px(3.0))
                                .size(px(6.0))
                                .rounded_full()
                                .bg(accent)
                                .into_any_element()
                        });
                    layer = layer.child(
                        div()
                            .absolute()
                            .left(px(rect.x * inv))
                            .top(px(rect.y * inv))
                            .w(px(rect.w * inv))
                            .h(px(rect.h * inv))
                            .child(
                                div()
                                    .absolute()
                                    .left(px((cr.x - rect.x) * inv))
                                    .top(px((cr.y - rect.y) * inv))
                                    .w(px(cr.w * inv))
                                    .h(px(cr.h * inv))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon(
                                        cx.global::<Theme>().icons.chevron_right(),
                                        px(14.0 * inv),
                                        base.ink_dim,
                                    ))
                                    .children(badge),
                            )
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
            // One layout for this strip: the painters below read its rects, and
            // so does every hit-test on the canvas mouse path.
            let titles: Vec<String> = tile.tabs.iter().map(|t| t.title()).collect();
            let layout =
                workspace::tile_strip_layout(&strip, &titles, tile.active, scale, has_caret);
            let bar = layout.bar;
            let tabs: Vec<StripTab> = tile
                .tabs
                .iter()
                .enumerate()
                .filter_map(|(ti, tab)| {
                    Some(StripTab {
                        title: tab.title(),
                        unread: tab.unread,
                        pinned: tab.pinned,
                        webview: tab.kind() == workspace::TabKind::Webview,
                        tab: *layout.tabs.get(ti)?,
                        close: *layout.closes.get(ti)?,
                    })
                })
                .collect();
            let style = StripStyle { focused, ..base.clone() };
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
            // The "+": focus this tile first, then take the standard New-tab
            // path, so the button and the ⌘T binding land in the same place.
            let new_entity = entity.clone();
            let on_new_tab: NewTabHandler = Rc::new(move |app| {
                if let Some(entity) = new_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.new_tab_in_tile(tile_id);
                        cx.notify();
                    });
                }
            });
            let mut strip_el = tab_strip(
                &layout,
                inv,
                &tabs,
                tile.active,
                &hov,
                &style,
                &cx.global::<Theme>().icons.x(),
                on_press,
                on_new_tab,
            );
            // The collapse caret: a standard svg chevron now (was canvas-
            // painted). The press target stays the same square, hover keeps
            // brightening the glyph, expanded/collapsed swap icons the way
            // `select` does, and an unread tab still badges it while
            // collapsed.
            if has_caret {
                let cr = workspace::tile_caret_rect(rect, scale);
                let entity = entity.clone();
                let badge = (tile.collapsed && tile.tabs.iter().any(|t| t.unread)).then(
                    || {
                        div()
                            .absolute()
                            .top(px(3.0))
                            .right(px(3.0))
                            .size(px(6.0))
                            .rounded_full()
                            .bg(accent)
                            .into_any_element()
                    },
                );
                strip_el = strip_el.child(
                    div()
                        .absolute()
                        .flex()
                        .items_center()
                        .justify_center()
                        .left(px((cr.x - bar.x) * inv))
                        .top(px((cr.y - bar.y) * inv))
                        .w(px(cr.w * inv))
                        .h(px(cr.h * inv))
                        .child(icon(
                            if tile.collapsed {
                                cx.global::<Theme>().icons.chevron_right()
                            } else {
                                cx.global::<Theme>().icons.chevron_down()
                            },
                            px(14.0 * inv),
                            if hov(&cr) { style.ink } else { style.ink_dim },
                        ))
                        .children(badge)
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


