//! The in-window flyover panel's tab strip as a gpui element tree.
//!
//! The strip wears the same mock as every tile's: the shared
//! [`crate::tile_ui::tab_strip`] paints it at the very rects
//! `workspace::flyover_strip_layout` hands both the element tree and the canvas
//! mouse path, which still resolves every click, drag and resize. The panel is
//! focused whenever it is up, so its active chip is always the mock's
//! full-strength one. The minimize / maximize buttons ride the bar's right
//! edge, past the strip's own "+" New tab button. The card, its borders, the
//! divider and the terminal content stay on the canvas.
//!
use std::rc::Rc;

use gpui::{
    AnyElement, App as GpuiApp, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Styled, Window, div, px, prelude::FluentBuilder as _,
};

use crate::App;
use crate::ui::assets::{ICON_MAXIMIZE, ICON_MINIMIZE};
use crate::ui::icon;
use crate::renderer::color;
use crate::tile_ui::{NewTabHandler, PressHandler, StripStyle, StripTab, tab_strip};
use crate::ui::theme::Theme;
use crate::workspace::{self, LayoutRect};

/// Inset of a window button's hover chip.
const CHIP_INSET: f32 = 3.0;
/// The window buttons' hover chip radius, and the faint mock white it is
/// filled with (the same chip the strip's own "+" shows on hover).
const CHIP_RADIUS: f32 = 4.0;
const CHIP_ALPHA: f32 = 0.07;

impl App {
    /// The flyover strip while the panel shows in this window, or an empty
    /// element.
    pub fn render_flyover_chrome(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.flyover_anim <= 0.0 || self.flyover_tabs.is_empty() {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let th = crate::theme::current();
        let scale = self.scale();
        let inv = 1.0 / scale;
        let panel = self.flyover_rect_now();
        let maximized = self.flyover_maximized;

        // The panel owns focus whenever it is up, so its active chip is the
        // mock's full-strength one.
        let style = StripStyle { focused: true, ..StripStyle::from_scheme(th) };
        let modal = self.modal_overlay_open();
        let cur = if matches!(self.drag, crate::Drag::None) && !modal {
            Some((self.cursor.0 as f32, self.cursor.1 as f32))
        } else {
            None
        };
        let hov = |r: &LayoutRect| cur.is_some_and(|(x, y)| r.contains(x, y));
        let font = crate::renderer::chrome_font();
        let entity = cx.entity().downgrade();

        // One layout for this strip: the element tree paints at its rects and
        // the canvas mouse path hit-tests the same ones.
        let titles: Vec<String> = self.flyover_tabs.iter().map(|t| t.title()).collect();
        let layout =
            workspace::flyover_strip_layout(&panel, &titles, self.flyover_active, scale, maximized);
        let bar = layout.bar;
        let tabs: Vec<StripTab> = self
            .flyover_tabs
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| {
                Some(StripTab {
                    title: tab.title(),
                    unread: tab.unread,
                    pinned: tab.pinned,
                    webview: tab.kind() == workspace::TabKind::Webview,
                    favicon: self.webview_favicon(tab.webview_id()),
                    tab: *layout.tabs.get(i)?,
                    close: *layout.closes.get(i)?,
                })
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
        // The "+" takes the flyover's own New-tab path.
        let new_entity = entity.clone();
        let on_new_tab: NewTabHandler = Rc::new(move |app| {
            if let Some(entity) = new_entity.upgrade() {
                entity.update(app, |this, cx| {
                    this.new_flyover_tab();
                    cx.notify();
                });
            }
        });
        let mut strip_el = tab_strip(
            &layout,
            inv,
            &tabs,
            self.flyover_active,
            &hov,
            &style,
            &cx.global::<Theme>().icons.x(),
            on_press,
            on_new_tab,
        );

        // Minimize / maximize buttons at the bar's right edge — not while a
        // modal owns the frame, so a picker never floats over decoy
        // controls (the canvas gated them the same way).
        if !modal {
            for (rect, path, maximize) in [
                (
                    workspace::flyover_minimize_rect(&panel, scale),
                    ICON_MINIMIZE,
                    false,
                ),
                (
                    workspace::flyover_maximize_rect(&panel, scale),
                    ICON_MAXIMIZE,
                    true,
                ),
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
                                    .bg(color(style.pill_rgb, CHIP_ALPHA)),
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
                                    path,
                                    px(12.0),
                                    if h { style.ink } else { style.ink_dim },
                                )),
                        ),
                );
            }
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
