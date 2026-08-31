//! The Launch tool panel as a gpui element tree over the canvas.
//!
//! Launch has no real content yet; until it does, this is the "coming soon"
//! placeholder that the canvas used to paint at `workspace::tool_panel`. It
//! now rides the same [`crate::pr_ui::tool_panel_overlay`] shell as the Pull
//! Request and Local-diff tools, so all three panels share one geometry
//! (docked vs floating, width, insets) and one card treatment — and the
//! canvas paints nothing for the panel body, which is what let the old
//! placeholder float over terminal content.

use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};

use crate::App;
use crate::pages::Tool;
use crate::ui::CardTitle;
use crate::ui::theme::Theme;

impl App {
    /// The Launch panel: a card in the tool-panel slot with the tool's title
    /// as its header and the placeholder line as its body.
    pub fn render_launch(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let tool = Tool::Launch;

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .child(CardTitle::new().child(tool.title()))
            .into_any_element();
        let body = div()
            .text_size(px(13.))
            .text_color(theme.muted_foreground)
            .child(format!("{} view coming soon", tool.title()))
            .into_any_element();

        crate::pr_ui::tool_panel_overlay(
            self.tool_panel_floating,
            self.tool_panel_w,
            header,
            body,
            self.glass_backdrop_el(gpui::Corners::all(theme.radius_xl())),
        )
    }
}
