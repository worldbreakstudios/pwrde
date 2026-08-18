//! Card — port of shadcn base-vega `ui/card.tsx`.
//!
//! Compound card layout: Card shell plus Header / Title / Description / Action /
//! Content / Footer. Size (`default` | `sm`) drives the shared spacing token
//! (24px / 16px).
//!
//! Omissions vs source (no gpui equivalent yet):
//! - CSS variable cascade (`--card-spacing`) — each part takes `.size(CardSize)`
//!   explicitly and applies its own horizontal padding (gpui has no CSS variable
//!   cascade). Callers should pass the same size used on the parent Card.
//! - `@container` / grid template header (`has-data-[slot=card-action]:grid-cols-…`)
//! - first/last child image rounding (`*: [img:first-child]:rounded-t-xl`, etc.)
//! - `group-data-[size=sm]/card:text-sm` title size shrink
//! - `[.border-b]:pb-(--card-spacing)` / `[.border-t]:pt-(--card-spacing)` utilities
//!
//! Local additions over the rcn source: [`Card::h_full`] and
//! [`CardContent::flex_1`], so a card can fill a page-sized region with the
//! content band absorbing the leftover height; [`Card::floating`] swaps the
//! resting shadow for `shadow_lg` when the card floats over other content.

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, RenderOnce, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::ui::theme::{Theme, alpha};

/// Card spacing size. Maps to shadcn `data-size` / `--card-spacing`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum CardSize {
    /// 24px spacing (`--spacing(6)`).
    #[default]
    Default,
    /// 16px spacing (`--spacing(4)`).
    Sm,
}

impl CardSize {
    fn spacing(self) -> f32 {
        match self {
            CardSize::Default => 24.,
            CardSize::Sm => 16.,
        }
    }
}

/// flex flex-col gap-(--card-spacing) overflow-hidden rounded-xl bg-card
/// py-(--card-spacing) text-sm text-card-foreground shadow-xs ring-1
/// ring-foreground/10
#[derive(IntoElement)]
pub struct Card {
    size: CardSize,
    full: bool,
    floating: bool,
    children: Vec<AnyElement>,
}

impl Card {
    pub fn new() -> Self {
        Self {
            size: CardSize::Default,
            full: false,
            floating: false,
            children: Vec::new(),
        }
    }

    pub fn size(mut self, size: CardSize) -> Self {
        self.size = size;
        self
    }

    /// Local addition: fill the parent's height instead of sizing to
    /// content, for page-filling cards. Pair with [`CardContent::flex_1`].
    pub fn h_full(mut self) -> Self {
        self.full = true;
        self
    }

    /// Local addition: raise the resting shadow to `shadow_lg` so the card
    /// reads as floating over other content instead of sitting flush.
    pub fn floating(mut self) -> Self {
        self.floating = true;
        self
    }
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let spacing = px(self.size.spacing());

        div()
            .flex()
            .flex_col()
            .when(self.full, |el| el.h_full())
            .gap(spacing)
            .overflow_hidden()
            .rounded(theme.radius_xl())
            .bg(theme.card)
            .py(spacing)
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(theme.card_foreground)
            .when(self.floating, |el| el.shadow_lg())
            .when(!self.floating, |el| el.shadow_xs())
            .border_1()
            .border_color(alpha(theme.foreground, 0.1))
            .children(self.children)
    }
}

/// flex flex-col gap-1 px-(--card-spacing)
///
/// Size must be set to match the parent Card — gpui has no CSS variable cascade.
#[derive(IntoElement)]
pub struct CardHeader {
    size: CardSize,
    children: Vec<AnyElement>,
}

impl CardHeader {
    pub fn new() -> Self {
        Self {
            size: CardSize::Default,
            children: Vec::new(),
        }
    }

    pub fn size(mut self, size: CardSize) -> Self {
        self.size = size;
        self
    }
}

impl Default for CardHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for CardHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CardHeader {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(self.size.spacing()))
            .children(self.children)
    }
}

/// cn-font-heading text-base leading-normal font-medium
#[derive(IntoElement)]
pub struct CardTitle {
    children: Vec<AnyElement>,
}

impl CardTitle {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for CardTitle {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for CardTitle {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CardTitle {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        div()
            .text_size(px(16.))
            .line_height(px(24.))
            .font_weight(FontWeight::MEDIUM)
            .when_some(theme.heading_font(), |el, f| el.font_family(f))
            .children(self.children)
    }
}

/// text-sm text-muted-foreground
#[derive(IntoElement)]
pub struct CardDescription {
    children: Vec<AnyElement>,
}

impl CardDescription {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for CardDescription {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for CardDescription {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CardDescription {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        div()
            .text_size(px(14.))
            .text_color(theme.muted_foreground)
            .children(self.children)
    }
}

/// Aligned to the end of a flex row (source places action in a grid end cell).
/// Use inside a flex-row parent, or nest with title/description in a row wrapper.
#[derive(IntoElement)]
pub struct CardAction {
    children: Vec<AnyElement>,
}

impl CardAction {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for CardAction {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for CardAction {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CardAction {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div().ml_auto().children(self.children)
    }
}

/// flex flex-col gap-3 px-(--card-spacing)
///
/// Size must be set to match the parent Card — gpui has no CSS variable cascade.
#[derive(IntoElement)]
pub struct CardContent {
    size: CardSize,
    grow: bool,
    children: Vec<AnyElement>,
}

impl CardContent {
    pub fn new() -> Self {
        Self {
            size: CardSize::Default,
            grow: false,
            children: Vec::new(),
        }
    }

    pub fn size(mut self, size: CardSize) -> Self {
        self.size = size;
        self
    }

    /// Local addition: grow to take the card's remaining height (shrinkable,
    /// so a scrollable child inside stays bounded). See [`Card::h_full`].
    pub fn flex_1(mut self) -> Self {
        self.grow = true;
        self
    }
}

impl Default for CardContent {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for CardContent {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CardContent {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .when(self.grow, |el| el.flex_1().min_h(px(0.)))
            .gap(px(12.))
            .px(px(self.size.spacing()))
            .children(self.children)
    }
}

/// flex items-center px-(--card-spacing)
///
/// Size must be set to match the parent Card — gpui has no CSS variable cascade.
#[derive(IntoElement)]
pub struct CardFooter {
    size: CardSize,
    children: Vec<AnyElement>,
}

impl CardFooter {
    pub fn new() -> Self {
        Self {
            size: CardSize::Default,
            children: Vec::new(),
        }
    }

    pub fn size(mut self, size: CardSize) -> Self {
        self.size = size;
        self
    }
}

impl Default for CardFooter {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for CardFooter {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CardFooter {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(self.size.spacing()))
            .children(self.children)
    }
}
