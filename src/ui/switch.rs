//! Switch — port of shadcn base-vega `ui/switch.tsx`.
//!
//! Controlled: the caller owns `checked` and receives the next value in
//! `on_change`. Sizes: Sm (24×14, 12px thumb) and Default (32×18, 16px
//! thumb). Aria-invalid styles are omitted.

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use crate::ui::motion;
use crate::ui::theme::{Theme, alpha};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SwitchSize {
    Sm,
    #[default]
    Default,
}

impl SwitchSize {
    /// Track (width, height) and thumb diameter. The source's default track
    /// is 32×18.4; rounded to whole pixels here.
    fn track(self) -> (f32, f32) {
        match self {
            Self::Sm => (24., 14.),
            Self::Default => (32., 18.),
        }
    }

    fn thumb(self) -> f32 {
        match self {
            Self::Sm => 12.,
            Self::Default => 16.,
        }
    }
}

type ChangeHandler = Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    size: SwitchSize,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            size: SwitchSize::default(),
            disabled: false,
            on_change: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn size(mut self, size: SwitchSize) -> Self {
        self.size = size;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let (track_w, track_h) = self.size.track();
        let thumb = self.size.thumb();

        // data-checked:bg-primary data-unchecked:bg-input
        // (dark unchecked: bg-input/80)
        let track_bg = if self.checked {
            theme.primary
        } else if theme.dark {
            alpha(theme.input, 0.8)
        } else {
            theme.input
        };

        // Thumb: bg-background; dark checked → primary-foreground,
        // dark unchecked → foreground.
        let thumb_bg = if theme.dark {
            if self.checked {
                theme.primary_foreground
            } else {
                theme.foreground
            }
        } else {
            theme.background
        };

        // translate-x-[calc(100%-2px)] against the 1px transparent track
        // border: thumb rests 1px from either end.
        let thumb_x = if self.checked {
            track_w - thumb - 2.
        } else {
            1.
        };

        let checked = self.checked;
        div()
            .id(self.id)
            .relative()
            .flex_shrink_0()
            .w(px(track_w))
            .h(px(track_h))
            .rounded_full()
            .bg(track_bg)
            .shadow_xs()
            .when(self.disabled, |s| s.opacity(0.5))
            .when(!self.disabled, |s| {
                let ring = motion::focus_ring(&theme);
                s.tab_index(0)
                    .focus_visible(move |s| s.border_color(theme.ring).shadow(ring.clone()))
                    .when_some(self.on_change, |s, on_change| {
                        s.on_click(move |_, window, cx| on_change(&!checked, window, cx))
                    })
            })
            .child(
                div()
                    .absolute()
                    .top(px((track_h - thumb) / 2.))
                    .left(px(thumb_x))
                    .size(px(thumb))
                    .rounded_full()
                    .bg(thumb_bg),
            )
    }
}
