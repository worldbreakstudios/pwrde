//! AlertDialog — port of shadcn base-vega `ui/alert-dialog.tsx`.
//!
//! A modal that interrupts the user and expects a response: unlike
//! [`Dialog`](crate::ui::dialog::Dialog), there is no close button. Upstream
//! the backdrop does not dismiss; in pwrde a backdrop click handler can be
//! attached via [`AlertDialog::on_backdrop_click`].
//! Header/Title/Description/Footer share the Dialog shapes.
//!
//! Sizing and shape overrides come from the caller via [`Styled`] and apply
//! to the floating panel (the element carrying background, border, and shadow),
//! not the full-viewport backdrop.
//!
//! Local additions (pwrde-specific, not in vendored rcn):
//! - [`AlertDialog::on_backdrop_click`] — click handler on the full-viewport
//!   scrim behind the panel (panel clicks are occluded and never reach it).
//! - [`AlertDialog::scrim`] — the backdrop color, for callers whose chrome
//!   has its own scrim token (upstream hardcodes `hsla(0, 0, 0, 0.5)`).
//! - The panel's fixed 512px width yields to the caller's [`Styled`]
//!   refinements (`w_auto()` gives a content-sized panel).

use gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Refineable as _, RenderOnce, StatefulInteractiveElement as _, StyleRefinement,
    Styled, Window, anchored, deferred, div, point, px,
};

pub use crate::ui::dialog::{
    DialogDescription as AlertDialogDescription, DialogFooter as AlertDialogFooter,
    DialogHeader as AlertDialogHeader, DialogTitle as AlertDialogTitle,
};
use crate::ui::theme::Theme;

/// Modal alert surface. Sizing and shape overrides via [`Styled`] target the
/// floating panel root (bg/border/shadow), not the backdrop.
#[derive(IntoElement)]
pub struct AlertDialog {
    id: ElementId,
    open: bool,
    scrim: Option<Hsla>,
    on_backdrop_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl AlertDialog {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            open: false,
            scrim: None,
            on_backdrop_click: None,
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Local addition: the backdrop color (default `hsla(0, 0, 0, 0.5)`).
    pub fn scrim(mut self, color: Hsla) -> Self {
        self.scrim = Some(color);
        self
    }

    /// Local addition: handler for clicks on the scrim outside the panel.
    /// The panel itself occludes its area, so card clicks never reach this.
    pub fn on_backdrop_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_backdrop_click = Some(Box::new(handler));
        self
    }
}

impl ParentElement for AlertDialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for AlertDialog {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for AlertDialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let viewport = window.viewport_size();

        let width = px(512.).min(viewport.width - px(32.));
        let mut panel = div()
            .occlude()
            .flex()
            .flex_col()
            .gap(px(16.))
            .w(width)
            .rounded(theme.radius_lg())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .p(px(24.))
            .shadow_lg()
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(theme.foreground)
            .children(self.children);
        panel.style().refine(&self.style);

        let scrim = self.scrim.unwrap_or(gpui::hsla(0., 0., 0., 0.5));
        let mut backdrop = div()
            .id(self.id)
            .occlude()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(scrim);
        if let Some(handler) = self.on_backdrop_click {
            backdrop = backdrop.on_click(handler);
        }
        backdrop = backdrop.child(crate::ui::motion::dialog_in("alert-dialog-in", panel));

        deferred(anchored().position(point(px(0.), px(0.))).child(backdrop)).into_any_element()
    }
}
