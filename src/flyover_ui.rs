//! The in-window flyover panel's tab strip as a gpui element tree.
//!
//! The strip's pixels (pills, titles, × buttons, unread dots, the minimize /
//! maximize buttons) used to be quads and labels from
//! `Renderer::flyover_overlay`. They ride [`crate::tile_ui::tab_strip`] now,
//! at the very rects `workspace::flyover_tab_rect` / `flyover_tab_close_rect`
//! / `flyover_minimize_rect` / `flyover_maximize_rect` hand the canvas mouse
//! path, which still resolves every click, drag and resize. The card, its
//! borders, the divider and the terminal content stay on the canvas.
//!
//! The popout window (`FlyoverPopout`) keeps painting its strip on the
//! canvas (`flyover_overlay(.., paint_strip = true)`): it has no window
//! buttons, its own cursor, and its own element root.

use std::rc::Rc;

use gpui::{
    AnyElement, App as GpuiApp, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Styled, Window, div, px, prelude::FluentBuilder as _,
};

use crate::App;
use crate::renderer::color;
use crate::tile_ui::{PressHandler, StripStyle, StripTab, modal_veil, tab_strip};
use crate::ui::theme::Theme;
use crate::workspace::{self, LayoutRect};

/// Inset of a window button's hover chip.
const CHIP_INSET: f32 = 3.0;
/// The flyover's pill and chip radii (the canvas used 7 and 4).
const PILL_RADIUS: f32 = 7.0;
const CHIP_RADIUS: f32 = 4.0;

impl App {
    /// The flyover strip while the panel shows in this window, or an empty
    /// element.
    pub fn render_flyover_chrome(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.flyover_anim <= 0.0 || self.flyover_windowed || self.flyover_tabs.is_empty() {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let th = crate::theme::current();
        let scale = self.scale();
        let inv = 1.0 / scale;
        let panel = self.flyover_rect_now();
        let bar = workspace::flyover_tab_bar(&panel, scale);
        let n = self.flyover_tabs.len();
        let maximized = self.flyover_maximized;

        let style = StripStyle {
            pill_radius: Some(PILL_RADIUS),
            chip_radius: Some(CHIP_RADIUS),
            ..StripStyle::from_scheme(th, 0.09)
        };
        let modal = self.modal_overlay_open();
        let cur = if matches!(self.drag, crate::Drag::None) && !modal {
            Some((self.cursor.0 as f32, self.cursor.1 as f32))
        } else {
            None
        };
        let hov = |r: &LayoutRect| cur.is_some_and(|(x, y)| r.contains(x, y));
        let font = crate::renderer::chrome_font();
        let entity = cx.entity().downgrade();

        let tabs: Vec<StripTab> = self
            .flyover_tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| StripTab {
                title: tab.session.title(),
                unread: tab.unread,
                tab: workspace::flyover_tab_rect(&panel, i, n, scale, maximized),
                close: workspace::flyover_tab_close_rect(&panel, i, n, scale, maximized),
            })
            .collect();
        let press_entity = entity.clone();
        let on_press: PressHandler = Rc::new(move |ti, close, ev, app| {
            app.stop_propagation();
            if let Some(entity) = press_entity.upgrade() {
                entity.update(app, |this, cx| {
                    this.note_pointer(ev);
                    this.press_flyover_tab(ti, close);
                    cx.notify();
                });
            }
        });
        let mut strip_el = tab_strip(&bar, inv, &tabs, self.flyover_active, &hov, &style, on_press);

        // Minimize / maximize buttons at the bar's right edge — not while a
        // modal owns the frame, so a picker never floats over decoy
        // controls (the canvas gated them the same way).
        if !modal {
            for (rect, glyph, maximize) in [
                (workspace::flyover_minimize_rect(&panel, scale), "–", false),
                (workspace::flyover_maximize_rect(&panel, scale), "□", true),
            ] {
                let h = hov(&rect);
                let entity = entity.clone();
                strip_el = strip_el.child(
                    div()
                        .absolute()
                        .left(px((rect.x - bar.x) * inv))
                        .top(px((rect.y - bar.y) * inv))
                        .w(px(rect.w * inv))
                        .h(px(rect.h * inv))
                        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                            app.stop_propagation();
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, cx| {
                                    this.note_pointer(ev);
                                    if maximize {
                                        this.flyover_toggle_maximized();
                                    } else {
                                        this.toggle_flyover();
                                    }
                                    this.request_redraw();
                                    cx.notify();
                                });
                            }
                        })
                        .when(h, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .left(px(CHIP_INSET))
                                    .top(px(CHIP_INSET))
                                    .w(px((rect.w * inv - 2.0 * CHIP_INSET).max(0.0)))
                                    .h(px((rect.h * inv - 2.0 * CHIP_INSET).max(0.0)))
                                    .rounded(px(CHIP_RADIUS))
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
                                .text_color(if h { style.ink } else { style.ink_dim })
                                .child(glyph),
                        ),
                );
            }
        } else {
            strip_el = strip_el.child(modal_veil(th));
        }

        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(font))
            .child(strip_el)
            .into_any_element()
    }
}
