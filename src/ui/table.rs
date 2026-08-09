//! Table — port of shadcn base-vega `ui/table.tsx`.
//!
//! gpui has no HTML table layout, so rows are flex rows and column sizing
//! comes from the cells: `TableHead`/`TableCell` default to `flex: 1` and
//! take an optional fixed `.w(px)` — give the same widths to matching
//! header/body cells to keep columns aligned. Selected/hover row states
//! mirror the source's `data-[state=selected]` and `hover:` styles.
//!
//! Local additions over the rcn source: `TableRow::on_click`, and flex cells
//! carry `min-w: 0` + hidden overflow so columns shrink uniformly (and stay
//! aligned across rows) when the table runs out of width.

use gpui::{
    AnyElement, App, ClickEvent, FontWeight, InteractiveElement as _, IntoElement, ParentElement,
    Pixels, RenderOnce, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::ui::theme::{Theme, alpha};

/// w-full text-sm — the outer table container.
#[derive(IntoElement)]
pub struct Table {
    children: Vec<AnyElement>,
}

impl Table {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Table {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Table {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            .text_size(px(14.))
            .line_height(px(20.))
            .children(self.children)
    }
}

/// thead: rows keep their bottom border.
#[derive(IntoElement)]
pub struct TableHeader {
    children: Vec<AnyElement>,
}

impl TableHeader {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for TableHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableHeader {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div().flex().flex_col().children(self.children)
    }
}

/// tbody.
#[derive(IntoElement)]
pub struct TableBody {
    children: Vec<AnyElement>,
}

impl TableBody {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for TableBody {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableBody {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableBody {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div().flex().flex_col().children(self.children)
    }
}

/// tfoot: border-t bg-muted/50 font-medium.
#[derive(IntoElement)]
pub struct TableFooter {
    children: Vec<AnyElement>,
}

impl TableFooter {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for TableFooter {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableFooter {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableFooter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .bg(alpha(theme.muted, 0.5))
            .font_weight(FontWeight::MEDIUM)
            .children(self.children)
    }
}

type RowClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// tr: border-b hover:bg-muted/50 data-[state=selected]:bg-muted.
#[derive(IntoElement)]
pub struct TableRow {
    id: Option<gpui::ElementId>,
    selected: bool,
    last: bool,
    /// Local addition (not in rcn): click handler, active only when the row has an id.
    on_click: Option<RowClickHandler>,
    children: Vec<AnyElement>,
}

impl TableRow {
    pub fn new() -> Self {
        Self {
            id: None,
            selected: false,
            last: false,
            on_click: None,
            children: Vec::new(),
        }
    }

    /// Give the row an id to enable the hover background.
    pub fn id(mut self, id: impl Into<gpui::ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The source drops the border on `tbody tr:last-child`.
    pub fn last(mut self, last: bool) -> Self {
        self.last = last;
        self
    }

    /// Local addition (not in rcn): row click-to-select, same style as `Button::on_click`.
    /// Only wired when the row has an id.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Default for TableRow {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let base = div()
            .flex()
            .flex_row()
            .w_full()
            .items_center()
            .when(!self.last, |el| el.border_b_1().border_color(theme.border))
            .when(self.selected, |el| el.bg(theme.muted));
        match self.id {
            Some(id) => {
                let hover_bg = alpha(theme.muted, 0.5);
                let el = base.id(id).hover(move |s| s.bg(hover_bg));
                let el = match self.on_click {
                    Some(handler) => el.cursor_pointer().on_click(handler),
                    None => el,
                };
                el.children(self.children).into_any_element()
            }
            None => base.children(self.children).into_any_element(),
        }
    }
}

/// th: h-10 px-2 text-left font-medium text-foreground.
#[derive(IntoElement)]
pub struct TableHead {
    width: Option<Pixels>,
    children: Vec<AnyElement>,
}

impl TableHead {
    pub fn new() -> Self {
        Self {
            width: None,
            children: Vec::new(),
        }
    }

    /// Fixed column width; unset columns share remaining space equally.
    pub fn w(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }
}

impl Default for TableHead {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableHead {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableHead {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(40.))
            .px(px(8.))
            .whitespace_nowrap()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.foreground)
            .map(|el| match self.width {
                Some(width) => el.w(width).flex_shrink_0(),
                // min-w 0 + hidden overflow (local addition): without them a
                // flex cell refuses to shrink below its nowrap content, so
                // narrow windows resolve different column widths per row and
                // the table stops lining up.
                None => el.flex_1().min_w(px(0.)).overflow_hidden(),
            })
            .children(self.children)
    }
}

/// td: p-2 align-middle.
#[derive(IntoElement)]
pub struct TableCell {
    width: Option<Pixels>,
    children: Vec<AnyElement>,
}

impl TableCell {
    pub fn new() -> Self {
        Self {
            width: None,
            children: Vec::new(),
        }
    }

    /// Fixed column width matching the corresponding [`TableHead`].
    pub fn w(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }
}

impl Default for TableCell {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableCell {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableCell {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .p(px(8.))
            .whitespace_nowrap()
            .map(|el| match self.width {
                Some(width) => el.w(width).flex_shrink_0(),
                // Same local addition as TableHead: keep flex columns
                // shrinkable so rows stay aligned when space runs out.
                None => el.flex_1().min_w(px(0.)).overflow_hidden(),
            })
            .children(self.children)
    }
}

/// caption: mt-4 text-sm text-muted-foreground (rendered below the table).
#[derive(IntoElement)]
pub struct TableCaption {
    children: Vec<AnyElement>,
}

impl TableCaption {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for TableCaption {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TableCaption {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableCaption {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .mt(px(16.))
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(theme.muted_foreground)
            .children(self.children)
    }
}
