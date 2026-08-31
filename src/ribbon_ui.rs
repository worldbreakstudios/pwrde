//! The tool ribbon's press targets as gpui elements.
//!
//! The ribbon's pixels — the slot pills and the vector glyphs
//! (`Renderer::ribbon_icon`) — stay on the canvas: the glyphs are built from
//! geometry because fonts cannot be trusted to carry them. What moves here
//! is the *press*: one invisible element per registered tool slot at
//! `workspace::ribbon_slot_rect`, which toggles the tool and stops the press
//! so the canvas mouse path never re-resolves it. The canvas keeps its
//! fallthrough for a press beside the ribbon (a floating panel dismisses on
//! blur there).

use gpui::{
    AnyElement, App as GpuiApp, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Styled, Window, div, px,
};

use crate::App;
use crate::workspace;

impl App {
    /// One press target per ribbon slot, or an empty element while the page
    /// or group registers no tools (or a modal owns the frame).
    pub fn render_ribbon_presses(&self, cx: &mut Context<Self>) -> AnyElement {
        let tools = self.tools_for(self.page);
        if tools.is_empty() || self.modal_overlay_open() {
            return div().into_any_element();
        }
        let scale = self.scale();
        let inv = 1.0 / scale;
        let (surface_w, _) = self.renderer.surface_size();
        let entity = cx.entity().downgrade();
        // The flyover panel is canvas-painted over the ribbon, and this
        // layer sits above the canvas: a slot the open panel covers must not
        // get a press target, or its click would toggle the tool instead of
        // reaching the panel's window buttons underneath the cursor.
        // Same gate as the flyover paint path (`flyover_ui`): the panel is on
        // screen while it animates, and never in windowed mode.
        let cover = (self.flyover_anim > 0.0
            && !self.flyover_windowed
            && !self.flyover_tabs.is_empty())
        .then(|| self.flyover_rect_now());
        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();
        for (i, tool) in tools.into_iter().enumerate() {
            let slot = workspace::ribbon_slot_rect(i, surface_w, scale);
            if cover.as_ref().is_some_and(|c| c.intersects(&slot)) {
                continue;
            }
            let entity = entity.clone();
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(slot.x * inv))
                    .top(px(slot.y * inv))
                    .w(px(slot.w * inv))
                    .h(px(slot.h * inv))
                    .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                        app.stop_propagation();
                        if let Some(entity) = entity.upgrade() {
                            entity.update(app, |this, cx| {
                                this.note_pointer(ev);
                                this.toggle_tool(tool);
                                this.request_redraw();
                                cx.notify();
                            });
                        }
                    }),
            );
        }
        layer.into_any_element()
    }
}
