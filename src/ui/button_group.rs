//! ButtonGroup — port of shadcn base-vega `ui/button-group.tsx`.
//!
//! Joins a row of [`Button`]s: outer corners keep their rounding, inner
//! edges go square and share borders. Items are typed (`.item(..)`) so the
//! group can position-style each button. Also exports ButtonGroupText and
//! ButtonGroupSeparator for mixed rows.

use gpui::{AnyElement, App, IntoElement, ParentElement as _, RenderOnce, Styled, Window, div, px};

use crate::ui::button::{Button, GroupPosition};
use crate::ui::separator::Separator;
use crate::ui::theme::Theme;

#[derive(IntoElement)]
pub struct ButtonGroup {
    items: Vec<Button>,
}

impl ButtonGroup {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn item(mut self, button: Button) -> Self {
        self.items.push(button);
        self
    }
}

impl Default for ButtonGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for ButtonGroup {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let count = self.items.len();
        div()
            .flex()
            .flex_row()
            .items_center()
            .children(self.items.into_iter().enumerate().map(|(index, button)| {
                let position = match (index == 0, index + 1 == count) {
                    (true, true) => GroupPosition::Only,
                    (true, false) => GroupPosition::First,
                    (false, true) => GroupPosition::Last,
                    (false, false) => GroupPosition::Middle,
                };
                button.group_position(position)
            }))
    }
}

/// A muted text segment inside a mixed button row (`ButtonGroupText`).
#[derive(IntoElement)]
pub struct ButtonGroupText {
    children: Vec<AnyElement>,
}

impl ButtonGroupText {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }
}

impl Default for ButtonGroupText {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for ButtonGroupText {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(theme.muted_foreground)
            .children(self.children)
    }
}

/// A vertical rule between grouped rows.
#[derive(IntoElement)]
pub struct ButtonGroupSeparator;

impl ButtonGroupSeparator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ButtonGroupSeparator {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for ButtonGroupSeparator {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div().h(px(20.)).child(Separator::vertical())
    }
}
