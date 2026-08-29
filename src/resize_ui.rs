//! Resize handles as gpui elements: the sidebar edge, the split dividers
//! between tiles, the tool panel's left edge and the flyover's top edge.
//!
//! The canvas mouse path used to hit-test all four by hand in
//! `on_mouse_down` (a `GRAB`-inflated band around each edge) and painted the
//! hover grip — an ink line with a centered pill — from `resize_hover`.
//! Each handle is now a thin element at that same band: it owns the cursor
//! (`CursorStyle::Resize…`), starts the drag on its own mouse-down (setting
//! the same `Drag` / `resize_hover` state the canvas did, then stopping
//! propagation so the canvas path never also sees the press), and paints
//! its grip when hovered or dragged. The drag itself — pointer moves and
//! the release — is still driven by the canvas `on_mouse_move` /
//! `on_mouse_up`, which is why the handles do not occlude: those listeners
//! must keep hearing the pointer while it crosses the handle.
//!
//! Hover detection (`resize_hover`) stays on the canvas `on_mouse_move` via
//! `workspace::resize_hover_at`, which the sticky drag cursor also needs.

use gpui::{
    AnyElement, App as GpuiApp, Context, CursorStyle, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Styled, Window, div, px,
    prelude::FluentBuilder as _,
};

use crate::App;
use crate::ui::theme::Theme;
use crate::workspace::{self, LayoutRect, ResizeHover};

/// Grip line thickness (the canvas drew 2 physical px per scale unit).
const GRIP_W: f32 = 2.0;
/// Length of the grip's centered pill.
const GRIP_PILL: f32 = 28.0;
/// Gap between the window edge and the sidebar's inlaid panel
/// (`sidebar_ui::GUTTER`); the sidebar grip hugs the panel, not the edge.
const SIDEBAR_GUTTER: f32 = 3.0;

/// The grip's pill: `GRIP_PILL` long (clamped to the line), centered along
/// the line — the geometry `Renderer::push_resize_grip` used.
fn grip_pill(rect: &LayoutRect, vertical: bool) -> LayoutRect {
    if vertical {
        let h = GRIP_PILL.min(rect.h);
        LayoutRect { x: rect.x, y: rect.y + ((rect.h - h) / 2.0).max(0.0), w: rect.w, h }
    } else {
        let w = GRIP_PILL.min(rect.w);
        LayoutRect { x: rect.x + ((rect.w - w) / 2.0).max(0.0), y: rect.y, w, h: rect.h }
    }
}

/// The grip line for an edge at `x` (logical px): `GRIP_W` wide, centered on
/// the edge, spanning `y..y + h` — the tool panel's left edge and the tile
/// dividers' lines are laid out this way; the sidebar's hugs the panel one
/// gutter further left.
fn edge_line(x: f32, y: f32, h: f32) -> LayoutRect {
    LayoutRect { x: x - GRIP_W / 2.0, y, w: GRIP_W, h }
}

/// The grip: a faint line the length of the handle with a stronger pill in
/// its middle, oriented along the handle.
fn grip(theme: &Theme, rect: &LayoutRect, vertical: bool) -> gpui::Div {
    let thickness = if vertical { rect.w } else { rect.h };
    let radius = thickness / 2.0;
    let pill = grip_pill(rect, vertical);
    div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .size_full()
        .child(
            div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.w))
                .h(px(rect.h))
                .rounded(px(radius))
                .bg(theme.foreground.opacity(0.14)),
        )
        .child(
            div()
                .absolute()
                .left(px(pill.x))
                .top(px(pill.y))
                .w(px(pill.w))
                .h(px(pill.h))
                .rounded(px(radius))
                .bg(theme.foreground.opacity(0.45)),
        )
}

impl App {
    /// Every resize handle on screen, or an empty element while a modal owns
    /// the frame.
    pub fn render_resize_handles(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.modal_overlay_open() {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let scale = self.scale();
        let inv = 1.0 / scale;
        let (surface_w, surface_h) = self.renderer.surface_size();
        let win_h = surface_h as f32 * inv;
        let grab = crate::GRAB;
        let entity = cx.entity().downgrade();

        // A press on a handle arms the drag exactly as the canvas did, then
        // stops there so the canvas `on_mouse_down` never re-resolves it.
        let arm = |entity: gpui::WeakEntity<Self>,
                   drag: crate::Drag,
                   hover: Option<ResizeHover>| {
            move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                app.stop_propagation();
                if let Some(entity) = entity.upgrade() {
                    let (drag, hover) = (drag.clone(), hover.clone());
                    entity.update(app, |this, cx| {
                        let s = this.scale() as f64;
                        this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                        this.modifiers = ev.modifiers;
                        this.drag = drag;
                        if hover.is_some() {
                            this.resize_hover = hover;
                        }
                        this.request_redraw();
                        cx.notify();
                    });
                }
            }
        };
        let handle = |rect: LayoutRect, cursor: CursorStyle| {
            div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.w))
                .h(px(rect.h))
                .cursor(cursor)
        };

        // The flyover overlays the sidebar edge and the dividers, so those
        // handles stop above it — the same ceiling the sidebar uses.
        let ceiling = self.flyover_ceiling();
        let mut under = div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w_full()
            .overflow_hidden()
            .map(|d| match ceiling {
                Some(limit) => d.h(px(limit)),
                None => d.h_full(),
            });

        // Sidebar edge (every page shares the width).
        let sidebar_w = self.sidebar_w();
        if sidebar_w > 0.0 {
            let top = workspace::TITLEBAR_H;
            let bottom = (win_h - workspace::AREA_PAD).max(top);
            let band = LayoutRect { x: sidebar_w - grab, y: top, w: 2.0 * grab, h: bottom - top };
            let lit = self.resize_hover == Some(ResizeHover::Sidebar)
                || matches!(self.drag, crate::Drag::Sidebar);
            let line = edge_line((sidebar_w - SIDEBAR_GUTTER).max(GRIP_W / 2.0), top, bottom - top);
            under = under
                .when(lit, |d| d.child(grip(&theme, &line, true)))
                .child(
                    handle(band, CursorStyle::ResizeLeftRight).on_mouse_down(
                        MouseButton::Left,
                        arm(entity.clone(), crate::Drag::Sidebar, Some(ResizeHover::Sidebar)),
                    ),
                );
        }

        // Split dividers between tiles (Sessions only, never in the empty state).
        if self.page == crate::Page::Sessions && !self.is_empty_state() {
            let ws = &self.workspaces[self.active];
            let (_, dividers) = workspace::layout_tiles(&ws.root, self.area(), scale);
            for d in dividers {
                let vertical = d.dir == workspace::Dir::Row;
                let lit = matches!(&self.resize_hover, Some(ResizeHover::Divider { path, .. }) if *path == d.path)
                    || matches!(&self.drag, crate::Drag::Divider { path } if *path == d.path);
                let rect = LayoutRect {
                    x: d.rect.x * inv,
                    y: d.rect.y * inv,
                    w: d.rect.w * inv,
                    h: d.rect.h * inv,
                };
                let band = rect.inflate(grab);
                let cursor = if vertical {
                    CursorStyle::ResizeLeftRight
                } else {
                    CursorStyle::ResizeUpDown
                };
                under = under
                    .when(lit, |el| el.child(grip(&theme, &rect, vertical)))
                    .child(handle(band, cursor).on_mouse_down(
                        MouseButton::Left,
                        arm(
                            entity.clone(),
                            crate::Drag::Divider { path: d.path.clone() },
                            Some(ResizeHover::Divider { path: d.path.clone(), dir: d.dir }),
                        ),
                    ));
            }
        }

        // Tool panel's left edge (it sits over the tile area, so it is
        // appended after the dividers and wins against them).
        if self.visible_tool().is_some() {
            let panel = workspace::tool_panel(
                surface_w,
                surface_h,
                scale,
                self.tool_panel_w,
                self.tool_panel_floating,
            );
            let pgrab = workspace::TOOL_PANEL_RESIZE_GRAB;
            let (x, y, h) = (panel.x * inv, panel.y * inv, panel.h * inv);
            let band = LayoutRect { x: x - pgrab, y, w: 2.0 * pgrab, h };
            let lit = self.resize_hover == Some(ResizeHover::ToolPanel)
                || matches!(self.drag, crate::Drag::ToolPanelResize);
            let line = edge_line(x, y, h);
            under = under
                .when(lit, |d| d.child(grip(&theme, &line, true)))
                .child(handle(band, CursorStyle::ResizeLeftRight).on_mouse_down(
                    MouseButton::Left,
                    arm(entity.clone(), crate::Drag::ToolPanelResize, Some(ResizeHover::ToolPanel)),
                ));
        }

        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full().child(under);

        // Flyover top edge: a height-resize drag (not when maximized — there
        // is no meaningful height to drag).
        if self.flyover_anim > 0.0
            && self.flyover_open
            && !self.flyover_windowed
            && !self.flyover_maximized
            && !self.flyover_tabs.is_empty()
        {
            let panel = self.flyover_rect_now();
            let fgrab = workspace::FLYOVER_RESIZE_GRAB;
            let band = LayoutRect {
                x: panel.x * inv,
                y: panel.y * inv - fgrab,
                w: panel.w * inv,
                h: 2.0 * fgrab,
            };
            layer = layer.child(handle(band, CursorStyle::ResizeUpDown).on_mouse_down(
                MouseButton::Left,
                arm(entity, crate::Drag::FlyoverResize, None),
            ));
        }

        layer.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grip line is `GRIP_W` wide and centered on its edge, the way the
    /// canvas drew the tool panel and divider grips.
    #[test]
    fn edge_line_is_centered_on_the_edge() {
        let line = edge_line(400.0, 10.0, 500.0);
        assert_eq!((line.x, line.y, line.w, line.h), (400.0 - GRIP_W / 2.0, 10.0, GRIP_W, 500.0));
    }

    /// The pill is `GRIP_PILL` long along the line, centered, and never
    /// longer than the line itself.
    #[test]
    fn grip_pill_centers_and_clamps() {
        let vertical = LayoutRect { x: 10.0, y: 100.0, w: GRIP_W, h: 300.0 };
        let pill = grip_pill(&vertical, true);
        assert_eq!((pill.x, pill.w, pill.h), (10.0, GRIP_W, GRIP_PILL));
        assert_eq!(pill.y, 100.0 + (300.0 - GRIP_PILL) / 2.0);

        let short = LayoutRect { x: 0.0, y: 0.0, w: 12.0, h: GRIP_W };
        let pill = grip_pill(&short, false);
        assert_eq!((pill.x, pill.w), (0.0, 12.0), "a short line keeps the pill inside it");
    }
}
