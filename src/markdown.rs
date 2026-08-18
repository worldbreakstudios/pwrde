//! Markdown rendering for the PR conversation.
//!
//! [`parse`] turns a markdown string into gpui-free [`Block`]s — so the model is
//! unit-testable without a window — and [`prepare`] does the expensive work once
//! (markdown parse + syntect highlight of code fences + kicking off mermaid
//! renders) so the per-frame [`render_prepared`] stays cheap, matching the diff
//! viewer's precompute discipline.
//!
//! Supported: headings, bold/italic/inline-code, bullet + numbered lists, code
//! fences, links, paragraphs, GFM tables, collapsible `<details>`/`<summary>`
//! blocks (honoring `open`), and `mermaid` fences (rendered to an image via
//! [`crate::mermaid`], falling back to a code block). Links are styled but not
//! clickable — the terminal's own OSC-8 path ([`crate::links`]) handles live
//! URLs, this only formats the description.

use std::ops::Range;

use std::collections::HashSet;

use gpui::{
    div, img, px, AnyElement, ClickEvent, FontStyle, FontWeight, HighlightStyle, Hsla,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, StyledText, UnderlineStyle, Window,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::ui::theme::Theme;
use crate::ui::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};

/// One styled run of inline text. Adjacent runs sharing every flag are merged
/// during parsing so the run list stays short.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub href: Option<String>,
}

/// A block-level element. List nesting is flattened onto [`Block::ListItem`]'s
/// `depth` rather than nested structurally, which keeps the model flat and easy
/// to render while still indenting sub-lists.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Inline> },
    Paragraph { spans: Vec<Inline> },
    ListItem { marker: String, depth: u8, spans: Vec<Inline> },
    CodeBlock { lang: Option<String>, code: String },
    /// A ```mermaid fence, kept distinct so it can render as a diagram image.
    Mermaid(String),
    /// A GFM table: header cells then body rows, each cell a run of inline spans.
    Table { headers: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    /// A collapsible `<details>` with its `<summary>` label and nested body.
    Details { open: bool, summary: Vec<Inline>, body: Vec<Block> },
}

/// Parse markdown into block-level elements.
pub fn parse(md: &str) -> Vec<Block> {
    let mut b = Build::default();
    for ev in Parser::new_ext(md, Options::ENABLE_TABLES) {
        b.event(ev);
    }
    b.finish()
}

struct ListCtx {
    ordered: bool,
    next: u64,
}

struct CodeAcc {
    lang: Option<String>,
    text: String,
}

/// An open `<details>` collecting its body blocks until `</details>`.
struct DetailsCtx {
    open: bool,
    summary: Vec<Inline>,
    body: Vec<Block>,
}

/// A GFM table being assembled. Cells reuse the shared inline sink (`spans`).
#[derive(Default)]
struct TableAcc {
    headers: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    in_head: bool,
    cur_row: Vec<Vec<Inline>>,
}

#[derive(Default)]
struct Build {
    blocks: Vec<Block>,
    spans: Vec<Inline>,
    bold: u32,
    italic: u32,
    code: bool,
    href: Option<String>,
    heading: Option<u8>,
    lists: Vec<ListCtx>,
    cur_item: Option<(String, u8)>,
    code_block: Option<CodeAcc>,
    /// Stack of open `<details>` — block output routes into the innermost body.
    details_stack: Vec<DetailsCtx>,
    table: Option<TableAcc>,
}

impl Build {
    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => match self.code_block.as_mut() {
                Some(acc) => acc.text.push_str(&t),
                None => self.push_inline(&t),
            },
            Event::Code(t) => {
                let prev = self.code;
                self.code = true;
                self.push_inline(&t);
                self.code = prev;
            }
            Event::SoftBreak | Event::HardBreak => match self.code_block.as_mut() {
                Some(acc) => acc.text.push('\n'),
                None => self.push_inline(" "),
            },
            // Raw HTML: we only act on <details>/<summary>/</details>; other tags
            // (inline <b>, comments, …) are ignored.
            Event::Html(s) | Event::InlineHtml(s) => self.html(&s),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(level_to_u8(level)),
            Tag::List(start) => self.lists.push(ListCtx { ordered: start.is_some(), next: start.unwrap_or(1) }),
            Tag::Item => {
                // Flush the previous item (or the parent's text preceding a
                // nested list) before starting this one.
                self.flush_item();
                let depth = self.lists.len().saturating_sub(1) as u8;
                let marker = match self.lists.last_mut() {
                    Some(ctx) if ctx.ordered => {
                        let n = ctx.next;
                        ctx.next += 1;
                        format!("{n}.")
                    }
                    _ => "•".to_string(),
                };
                self.cur_item = Some((marker, depth));
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().filter(|s| !s.is_empty()).map(str::to_string)
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code_block = Some(CodeAcc { lang, text: String::new() });
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Link { dest_url, .. } => self.href = Some(dest_url.to_string()),
            Tag::Table(_) => self.table = Some(TableAcc::default()),
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.cur_row = Vec::new();
                }
            }
            // A cell's inline content accumulates in the shared `spans` sink;
            // `TableCell` end takes it. Clear any stray spans first.
            Tag::TableCell => self.spans.clear(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                let spans = std::mem::take(&mut self.spans);
                if !spans.is_empty() {
                    self.push_block(Block::Heading { level, spans });
                }
            }
            TagEnd::Paragraph => {
                if self.lists.is_empty() {
                    let spans = std::mem::take(&mut self.spans);
                    if !spans.is_empty() {
                        self.push_block(Block::Paragraph { spans });
                    }
                } else {
                    // Loose list item: keep paragraphs on one line with a space.
                    self.push_inline(" ");
                }
            }
            TagEnd::List(_) => {
                self.flush_item();
                self.lists.pop();
            }
            TagEnd::Item => self.flush_item(),
            TagEnd::CodeBlock => {
                if let Some(acc) = self.code_block.take() {
                    let mut code = acc.text;
                    while code.ends_with('\n') {
                        code.pop();
                    }
                    if acc.lang.as_deref() == Some("mermaid") {
                        self.push_block(Block::Mermaid(code));
                    } else {
                        self.push_block(Block::CodeBlock { lang: acc.lang, code });
                    }
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.push_block(Block::Table { headers: t.headers, rows: t.rows });
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.cur_row);
                    t.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                let cell = trim_spans(std::mem::take(&mut self.spans));
                if let Some(t) = self.table.as_mut() {
                    if t.in_head {
                        t.headers.push(cell);
                    } else {
                        t.cur_row.push(cell);
                    }
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Link => self.href = None,
            _ => {}
        }
    }

    /// Route a finished block to the innermost open `<details>` body, or to the
    /// top-level block list.
    fn push_block(&mut self, block: Block) {
        match self.details_stack.last_mut() {
            Some(d) => d.body.push(block),
            None => self.blocks.push(block),
        }
    }

    /// Handle a raw-HTML fragment, acting only on `<details>` / `<summary>` /
    /// `</details>`. A fragment can carry several of these (GitHub emits
    /// `<details>` and `<summary>…</summary>` in one HTML block), so scan
    /// left-to-right. Unrecognized HTML is ignored.
    fn html(&mut self, frag: &str) {
        let mut rest = frag;
        loop {
            let details = find_ci(rest, "<details");
            let summary = find_ci(rest, "<summary>");
            let close = find_ci(rest, "</details>");
            // Act on whichever tag comes first in the fragment.
            let next = [details, summary, close].into_iter().flatten().min();
            let Some(pos) = next else { break };
            if Some(pos) == details {
                let after = &rest[pos..];
                let end = after.find('>').map(|e| pos + e + 1).unwrap_or(rest.len());
                let open = rest[pos..end].contains("open");
                self.details_stack.push(DetailsCtx { open, summary: Vec::new(), body: Vec::new() });
                rest = &rest[end..];
            } else if Some(pos) == summary {
                let inner_start = pos + "<summary>".len();
                if let Some(close_rel) = find_ci(&rest[inner_start..], "</summary>") {
                    let label = strip_tags(&rest[inner_start..inner_start + close_rel]).trim().to_string();
                    if let Some(d) = self.details_stack.last_mut() {
                        if !label.is_empty() {
                            d.summary = vec![Inline { text: label, bold: true, ..Default::default() }];
                        }
                    }
                    rest = &rest[inner_start + close_rel + "</summary>".len()..];
                } else {
                    break; // unterminated summary in this fragment; leave it
                }
            } else {
                // </details>
                if let Some(d) = self.details_stack.pop() {
                    self.push_block(Block::Details { open: d.open, summary: d.summary, body: d.body });
                }
                rest = &rest[pos + "</details>".len()..];
            }
        }
    }

    fn push_inline(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut() {
            if last.bold == (self.bold > 0)
                && last.italic == (self.italic > 0)
                && last.code == self.code
                && last.href.as_deref() == self.href.as_deref()
            {
                last.text.push_str(text);
                return;
            }
        }
        self.spans.push(Inline {
            text: text.to_string(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            code: self.code,
            href: self.href.clone(),
        });
    }

    fn flush_item(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let (marker, depth) = self.cur_item.clone().unwrap_or_else(|| ("•".to_string(), 0));
        let spans = trim_spans(std::mem::take(&mut self.spans));
        if !spans.is_empty() {
            self.push_block(Block::ListItem { marker, depth, spans });
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush_item();
        if !self.spans.is_empty() {
            let spans = std::mem::take(&mut self.spans);
            self.push_block(Block::Paragraph { spans });
        }
        // Close any `<details>` left unterminated (malformed input), innermost
        // first so nesting is preserved as each is routed to its parent.
        while let Some(d) = self.details_stack.pop() {
            self.push_block(Block::Details { open: d.open, summary: d.summary, body: d.body });
        }
        self.blocks
    }
}

/// Case-insensitive byte-offset search for `needle` in `haystack`.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.to_ascii_lowercase(), needle.to_ascii_lowercase());
    h.find(&n)
}

/// Strip any `<...>` tags from an HTML fragment, leaving the text content.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn level_to_u8(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Drop leading/trailing whitespace introduced by inter-paragraph spacing in a
/// list item, then any runs left empty.
fn trim_spans(mut spans: Vec<Inline>) -> Vec<Inline> {
    if let Some(first) = spans.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = spans.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    spans.retain(|s| !s.text.is_empty());
    spans
}

// ── Preparation + rendering ─────────────────────────────────────────────────

/// A block whose expensive work is already done: markdown is parsed, and code
/// fences are syntax-highlighted into plain colour spans. Inline text keeps its
/// raw [`Inline`] spans — their colours come from the live theme at render time
/// — while code carries pre-highlighted spans so rendering never re-runs syntect.
#[derive(Clone, Debug)]
pub enum PreparedBlock {
    Heading { level: u8, spans: Vec<Inline> },
    Paragraph { spans: Vec<Inline> },
    ListItem { marker: String, depth: u8, spans: Vec<Inline> },
    /// One entry per code line, each a run of pre-highlighted spans.
    Code(Vec<Vec<crate::highlight::Span>>),
    /// A GFM table: header cells then body rows of inline spans.
    Table { headers: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    /// A collapsible section. `key` is a stable content hash used to track the
    /// user's expand/collapse toggles; `open` is the `<details open>` default.
    Details { open: bool, key: u64, summary: Vec<Inline>, body: Vec<PreparedBlock> },
    /// A mermaid diagram: `key` looks up the rendered image in [`crate::mermaid`];
    /// `source_lines` is the pre-highlighted source shown as a fallback.
    Mermaid { key: u64, source_lines: Vec<Vec<crate::highlight::Span>> },
}

/// A fully-prepared markdown body: parse + syntax-highlight done once (off the
/// per-frame render path), then rendered cheaply every frame — the same
/// precompute discipline the diff viewer uses (`DiffRender`).
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    blocks: Vec<PreparedBlock>,
}

/// A click handler for a details toggle. Boxed so markdown stays App-agnostic —
/// the caller supplies the handler that flips its own expand-state.
pub type OnToggle = Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>;

/// Everything [`render_prepared`] needs beyond the prepared blocks: the live
/// theme, which collapsibles the user has flipped from their default state, and
/// a factory building the toggle handler for a details `key`.
pub struct MdCtx<'a> {
    pub theme: &'a Theme,
    pub flipped: &'a HashSet<u64>,
    pub toggle: &'a dyn Fn(u64) -> OnToggle,
    /// Monotonic per-render counter, reset each frame, used to give every
    /// rendered `<details>` a unique key even when two collapsibles have
    /// byte-identical content (which would otherwise share a `details_key` —
    /// duplicate gpui element id + linked toggle). Render order is deterministic
    /// across frames, so the derived key is stable and toggles still persist.
    pub seq: &'a std::cell::Cell<u64>,
}

/// Parse `md` and pre-highlight its code fences at the given polarity, kicking
/// off any mermaid renders. Do this once when data loads, not on the render path.
pub fn prepare(md: &str, dark: bool) -> Prepared {
    Prepared { blocks: parse(md).into_iter().map(|b| prepare_block(b, dark)).collect() }
}

fn prepare_block(b: Block, dark: bool) -> PreparedBlock {
    match b {
        Block::Heading { level, spans } => PreparedBlock::Heading { level, spans },
        Block::Paragraph { spans } => PreparedBlock::Paragraph { spans },
        Block::ListItem { marker, depth, spans } => {
            PreparedBlock::ListItem { marker, depth, spans }
        }
        Block::CodeBlock { lang, code } => {
            let mut hl =
                crate::highlight::LineHighlighter::new(lang.as_deref().unwrap_or("txt"), dark);
            PreparedBlock::Code(code.split('\n').map(|line| hl.highlight(line)).collect())
        }
        Block::Table { headers, rows } => PreparedBlock::Table { headers, rows },
        Block::Details { open, summary, body } => {
            let key = details_key(&summary, &body);
            let body = body.into_iter().map(|b| prepare_block(b, dark)).collect();
            PreparedBlock::Details { open, key, summary, body }
        }
        Block::Mermaid(source) => {
            let key = crate::mermaid::ensure(&source, dark);
            let mut hl = crate::highlight::LineHighlighter::new("txt", dark);
            let source_lines = source.split('\n').map(|line| hl.highlight(line)).collect();
            PreparedBlock::Mermaid { key, source_lines }
        }
    }
}

/// Build the gpui element tree from an already-[`prepare`]d body. Cheap: no
/// markdown parsing and no syntect, so it's safe to call every frame.
pub fn render_prepared(prepared: &Prepared, ctx: &MdCtx) -> AnyElement {
    let mut col = div().flex().flex_col().gap_2();
    for block in &prepared.blocks {
        col = col.child(render_prepared_block(block, ctx));
    }
    col.into_any_element()
}

fn render_prepared_block(block: &PreparedBlock, ctx: &MdCtx) -> AnyElement {
    let theme = ctx.theme;
    match block {
        PreparedBlock::Heading { level, spans } => {
            let size = match level {
                1 => 18.,
                2 => 16.,
                3 => 14.,
                _ => 13.,
            };
            div()
                .text_size(px(size))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(inline_text(spans, theme))
                .into_any_element()
        }
        PreparedBlock::Paragraph { spans } => div()
            .text_size(px(12.))
            .text_color(theme.foreground)
            .child(inline_text(spans, theme))
            .into_any_element(),
        PreparedBlock::ListItem { marker, depth, spans } => div()
            .flex()
            .flex_row()
            .gap_2()
            .pl(px(4. + *depth as f32 * 16.))
            .text_size(px(12.))
            .text_color(theme.foreground)
            .child(div().flex_none().text_color(theme.muted_foreground).child(SharedString::from(marker.clone())))
            .child(div().flex_1().min_w(px(0.)).child(inline_text(spans, theme)))
            .into_any_element(),
        PreparedBlock::Code(lines) => render_code_lines(lines, theme),
        PreparedBlock::Table { headers, rows } => render_table(headers, rows, ctx),
        PreparedBlock::Details { open, key, summary, body } => {
            render_details(*open, *key, summary, body, ctx)
        }
        PreparedBlock::Mermaid { key, source_lines } => match crate::mermaid::state(*key) {
            crate::mermaid::Render::Ready(path) => div()
                .child(img(path).max_w_full())
                .into_any_element(),
            crate::mermaid::Render::Pending => div()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .child("Rendering diagram…")
                .into_any_element(),
            // Fall back to the source, and surface *why* the diagram didn't
            // render (mmdc missing, a mermaid syntax error, …) so it's not a
            // silent blank.
            crate::mermaid::Render::Unavailable(reason) => {
                let code = render_code_lines(source_lines, theme);
                if reason.is_empty() {
                    code
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(code)
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme.muted_foreground)
                                .child(format!("mermaid unavailable: {reason}")),
                        )
                        .into_any_element()
                }
            }
        },
    }
}

fn render_table(headers: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>], ctx: &MdCtx) -> AnyElement {
    let theme = ctx.theme;
    let ncols = headers.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));

    // Content-derived column widths. rcn cells with an explicit `.w()` become
    // `flex_shrink_0`, so the table sizes to content and — with `Table::w_auto`
    // (no `w_full`) — can exceed its container and scroll horizontally instead
    // of dividing the width and clipping each nowrap cell.
    let cell_chars = |cell: &[Inline]| cell.iter().map(|s| s.text.chars().count()).sum::<usize>();
    let col_w: Vec<gpui::Pixels> = (0..ncols)
        .map(|c| {
            let mut max = headers.get(c).map(|h| cell_chars(h)).unwrap_or(0);
            for row in rows {
                if let Some(cell) = row.get(c) {
                    max = max.max(cell_chars(cell));
                }
            }
            px((max as f32 * 7.0 + 24.0).clamp(64.0, 460.0))
        })
        .collect();
    let width_of = |c: usize| col_w.get(c).copied().unwrap_or(px(120.));

    let head = TableRow::new().children(
        headers
            .iter()
            .enumerate()
            .map(|(c, cell)| TableHead::new().w(width_of(c)).child(inline_text(cell, theme)).into_any_element()),
    );
    let body = rows.iter().map(|row| {
        TableRow::new()
            .children(row.iter().enumerate().map(|(c, cell)| {
                TableCell::new().w(width_of(c)).child(inline_text(cell, theme)).into_any_element()
            }))
            .into_any_element()
    });
    let table = Table::new()
        .w_auto()
        .child(TableHeader::new().child(head))
        .child(TableBody::new().children(body));
    // A content-width table can exceed the card; wrap it so it scrolls
    // horizontally. The id (stable render-order seq) preserves scroll offset.
    let seq = ctx.seq.get();
    ctx.seq.set(seq + 1);
    div()
        .id(("md-table", seq as usize))
        .max_w_full()
        .overflow_x_scroll()
        .child(table)
        .into_any_element()
}

fn render_details(
    open: bool,
    key: u64,
    summary: &[Inline],
    body: &[PreparedBlock],
    ctx: &MdCtx,
) -> AnyElement {
    let theme = ctx.theme;
    // Salt the content hash with this details' render-order position so two
    // byte-identical collapsibles get distinct ids/toggle keys (no duplicate
    // gpui id, no linked toggle). Deterministic order → stable across frames.
    let seq = ctx.seq.get();
    ctx.seq.set(seq + 1);
    let key = key ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    // Effective state = default (`open`) flipped by any user toggle.
    let expanded = open ^ ctx.flipped.contains(&key);
    let summary_spans = if summary.is_empty() {
        vec![Inline { text: "Details".into(), bold: true, ..Default::default() }]
    } else {
        summary.to_vec()
    };
    let header = div()
        .id(("md-details", key as usize))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(theme.foreground)
        .child(
            div()
                .flex_none()
                .text_color(theme.muted_foreground)
                .child(if expanded { "▾" } else { "▸" }),
        )
        .child(inline_text(&summary_spans, theme))
        .on_click((ctx.toggle)(key));
    let mut col = div().flex().flex_col().gap_1().child(header);
    if expanded {
        let mut inner = div().flex().flex_col().gap_2().pl(px(14.));
        for b in body {
            inner = inner.child(render_prepared_block(b, ctx));
        }
        col = col.child(inner);
    }
    col.into_any_element()
}

/// Stable content hash for a `<details>` — used to track expand toggles across
/// re-renders. Based on the summary + a text digest of the body, so it's stable
/// even as surrounding content shifts.
fn details_key(summary: &[Inline], body: &[Block]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in summary {
        s.text.hash(&mut h);
    }
    let mut text = String::new();
    for b in body {
        block_text(b, &mut text);
    }
    text.hash(&mut h);
    h.finish()
}

/// Append a block's plain text to `out` (for hashing / identity only).
fn block_text(b: &Block, out: &mut String) {
    let push = |spans: &[Inline], out: &mut String| {
        for s in spans {
            out.push_str(&s.text);
        }
    };
    match b {
        Block::Heading { spans, .. } | Block::Paragraph { spans, .. } => push(spans, out),
        Block::ListItem { spans, .. } => push(spans, out),
        Block::CodeBlock { code, .. } | Block::Mermaid(code) => out.push_str(code),
        Block::Table { headers, rows } => {
            for c in headers {
                push(c, out);
            }
            for r in rows {
                for c in r {
                    push(c, out);
                }
            }
        }
        Block::Details { summary, body, .. } => {
            push(summary, out);
            for b in body {
                block_text(b, out);
            }
        }
    }
}

/// Assemble a single [`StyledText`] from inline runs, applying bold / italic via
/// per-run [`HighlightStyle`] and distinguishing inline code and links by colour
/// (and an underline for links).
fn inline_text(spans: &[Inline], theme: &Theme) -> StyledText {
    let mut text = String::new();
    let mut runs: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(spans.len());
    for s in spans {
        let start = text.len();
        text.push_str(&s.text);
        let mut style = HighlightStyle::default();
        if s.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if s.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if s.href.is_some() {
            style.color = Some(theme.primary);
            style.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(theme.primary), wavy: false });
        } else if s.code {
            style.color = Some(theme.muted_foreground);
            style.background_color = Some(theme.muted);
        }
        runs.push((start..text.len(), style));
    }
    StyledText::new(SharedString::from(text)).with_highlights(runs)
}

fn render_code_lines(lines: &[Vec<crate::highlight::Span>], theme: &Theme) -> AnyElement {
    let mut col = div().flex().flex_col();
    for spans in lines {
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        let runs = hl_runs(spans);
        // A blank line renders zero-height, collapsing the gap; keep a space so
        // vertical spacing in the block is preserved.
        let styled =
            StyledText::new(SharedString::from(if text.is_empty() { " ".to_string() } else { text }))
                .with_highlights(runs);
        col = col.child(div().child(styled));
    }
    div()
        .rounded_md()
        .p_2()
        .bg(theme.muted)
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(12.))
        .text_color(theme.foreground)
        .child(col)
        .into_any_element()
}

/// Convert consecutive highlight spans into byte-range → colour runs (mirrors
/// `pr_ui::spans_to_runs`, kept local so this module owns its rendering).
fn hl_runs(spans: &[crate::highlight::Span]) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut runs = Vec::with_capacity(spans.len());
    let mut ix = 0usize;
    for s in spans {
        let len = s.text.len();
        if len > 0 {
            let color: Hsla = s.color.into();
            runs.push((ix..ix + len, HighlightStyle { color: Some(color), ..Default::default() }));
        }
        ix += len;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_paragraph() {
        let blocks = parse("# Title\n\nHello world.");
        assert_eq!(
            blocks,
            vec![
                Block::Heading { level: 1, spans: vec![Inline { text: "Title".into(), ..Default::default() }] },
                Block::Paragraph { spans: vec![Inline { text: "Hello world.".into(), ..Default::default() }] },
            ]
        );
    }

    #[test]
    fn heading_levels() {
        let blocks = parse("### Deep");
        assert_eq!(blocks, vec![Block::Heading { level: 3, spans: vec![Inline { text: "Deep".into(), ..Default::default() }] }]);
    }

    #[test]
    fn parses_gfm_table() {
        let blocks = parse("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |");
        match &blocks[0] {
            Block::Table { headers, rows } => {
                assert_eq!(headers.len(), 2);
                assert_eq!(headers[0][0].text, "A");
                assert_eq!(headers[1][0].text, "B");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0][0][0].text, "1");
                assert_eq!(rows[1][1][0].text, "4");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parses_mermaid_fence_distinctly() {
        let blocks = parse("```mermaid\nsequenceDiagram\nA->>B: hi\n```");
        assert_eq!(blocks, vec![Block::Mermaid("sequenceDiagram\nA->>B: hi".into())]);
        // A non-mermaid fence stays a CodeBlock.
        let blocks = parse("```rust\nfn x() {}\n```");
        assert!(matches!(blocks[0], Block::CodeBlock { .. }));
    }

    #[test]
    fn parses_collapsible_details() {
        let md = "<details open>\n<summary><b>Screenshots</b></summary>\n\nhello world\n</details>";
        let blocks = parse(md);
        match &blocks[0] {
            Block::Details { open, summary, body } => {
                assert!(open, "open attribute honored");
                assert_eq!(summary[0].text, "Screenshots");
                assert!(summary[0].bold);
                // Body markdown is parsed, not shown raw.
                assert!(
                    body.iter().any(|b| matches!(b, Block::Paragraph { spans } if spans[0].text.contains("hello"))),
                    "body: {body:?}"
                );
            }
            other => panic!("expected details, got {other:?}"),
        }
    }

    #[test]
    fn collapsed_details_defaults() {
        // No `open` attribute → open == false.
        let blocks = parse("<details>\n<summary>More</summary>\n\nbody\n</details>");
        assert!(matches!(&blocks[0], Block::Details { open: false, .. }));
    }

    #[test]
    fn strip_tags_removes_markup() {
        assert_eq!(strip_tags("<b>Hi</b> there"), "Hi there");
        assert_eq!(strip_tags("plain"), "plain");
    }

    #[test]
    fn prepare_highlights_code_and_keeps_inline_spans() {
        let p = prepare("# Title\n\n```rust\nfn main() {}\n```", true);
        assert!(matches!(p.blocks[0], PreparedBlock::Heading { level: 1, .. }));
        match &p.blocks[1] {
            PreparedBlock::Code(lines) => {
                // One code line, highlighted into at least one span whose text
                // reassembles the source.
                assert_eq!(lines.len(), 1);
                let joined: String = lines[0].iter().map(|s| s.text.as_str()).collect();
                assert_eq!(joined, "fn main() {}");
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn bold_italic_and_inline_code() {
        let blocks = parse("**bold** *em* `code`");
        let Block::Paragraph { spans } = &blocks[0] else { panic!("expected paragraph, got {blocks:?}") };
        assert!(spans.iter().any(|s| s.text == "bold" && s.bold));
        assert!(spans.iter().any(|s| s.text == "em" && s.italic));
        assert!(spans.iter().any(|s| s.text == "code" && s.code));
    }

    #[test]
    fn bullet_list() {
        let blocks = parse("- one\n- two");
        assert_eq!(
            blocks,
            vec![
                Block::ListItem { marker: "•".into(), depth: 0, spans: vec![Inline { text: "one".into(), ..Default::default() }] },
                Block::ListItem { marker: "•".into(), depth: 0, spans: vec![Inline { text: "two".into(), ..Default::default() }] },
            ]
        );
    }

    #[test]
    fn ordered_list_numbers() {
        let blocks = parse("1. first\n2. second");
        let markers: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["1.", "2."]);
    }

    #[test]
    fn nested_list_gets_depth() {
        let blocks = parse("- outer\n    - inner");
        let depths: Vec<u8> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1]);
    }

    #[test]
    fn fenced_code_block_keeps_lang_and_body() {
        let blocks = parse("```rust\nfn main() {}\n```");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock { lang: Some("rust".into()), code: "fn main() {}".into() }]
        );
    }

    #[test]
    fn multiline_code_block_preserves_lines() {
        let blocks = parse("```\na\nb\n```");
        assert_eq!(blocks, vec![Block::CodeBlock { lang: None, code: "a\nb".into() }]);
    }

    #[test]
    fn link_captures_href() {
        let blocks = parse("see [docs](https://example.com/x)");
        let Block::Paragraph { spans } = &blocks[0] else { panic!("expected paragraph, got {blocks:?}") };
        assert!(spans.iter().any(|s| s.text == "docs" && s.href.as_deref() == Some("https://example.com/x")));
    }

    #[test]
    fn adjacent_plain_text_merges() {
        // A soft break joins into one run rather than fragmenting.
        let blocks = parse("one two three");
        let Block::Paragraph { spans } = &blocks[0] else { panic!("expected paragraph") };
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "one two three");
    }

    #[test]
    fn empty_input_yields_no_blocks() {
        assert!(parse("").is_empty());
        assert!(parse("   \n  ").is_empty());
    }
}
