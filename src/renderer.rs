//! Stateless layout / metrics / color helper for the gpui renderer.
//!
//! In the old wgpu+glyphon design this module owned a GPU surface and drew
//! frames immediate-mode. Under gpui the paint model is inverted: painting
//! happens inside a gpui `Element`'s `paint()` (see `main.rs`), which calls
//! `window.paint_quad(...)` for fills and shapes text via
//! `window.text_system().shape_line(...)`. So this module no longer touches
//! the GPU at all — it is pure geometry, color, and terminal-snapshot logic.
//!
//! `build_frame` walks the workspace tree + terminal grids and produces a
//! [`Frame`] of plain data (background/foreground quads, per-pane colored text
//! runs, and chrome/picker labels). `main.rs`'s terminal `Element` consumes
//! that data and does the actual painting.
//!
//! Frame structure (paint order): window gradient (painted by `main.rs`) →
//! chrome quads (sidebar rows, tile cards) → per-pane text (terminal grids) →
//! foreground quads (block glyph geometry, cursor, links) → labels (tab
//! titles, picker) → picker overlay.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Hsla, RenderImage};
use image::{Delay, Frame as ImageFrame, RgbaImage};
use termwiz::surface::CursorVisibility;
use wezterm_term::color::ColorPalette;
use wezterm_term::image::{ImageCell, ImageData, ImageDataType};

use crate::pages::Page;
use crate::rect::char_rects;
use crate::term::Session;
use crate::theme::Theme;
use crate::workspace::{self, LayoutRect, Workspace};

pub const FONT_SIZE: f32 = 15.0;
const LINE_HEIGHT_FACTOR: f32 = 1.25;
/// Concrete monospace family. Naming a real installed font (not the generic
/// `Family::Monospace`) skips per-word font-fallback resolution, and the Nerd
/// Font glyph coverage keeps fallback from firing on powerline/icon glyphs.
pub const FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";
/// Content inset inside each tile's terminal region, logical px. Generous
/// enough that the card's rounded corners never clip glyphs.
const PANE_PAD: f32 = 8.0;
// All chrome colors live in `theme::Theme` presets (Arc-style dark cards on a
// gradient by default); the renderer reads the active theme each frame.

/// Corner radius of the floating tile cards and chrome panels, logical px.
///
/// Vitrine's `--radius-l`: the GANTRY mock draws its terminal tiles on a
/// radius close to the folders card's (`folders_ui::CARD_RADIUS`), and a tile
/// that rounded off much tighter than the card beside it read as two
/// different materials.
pub(crate) const CARD_RADIUS: f32 = 18.0;
/// Corner radius of the sidebar's rounded rows, logical px: half the 28px
/// row height, so rows paint as fully-rounded iTerm2-style capsules. The
/// `pill` helper clamps it per-rect, so shorter pills stay capsules too.
const ROW_RADIUS: f32 = 14.0;

fn brighten(c: (u8, u8, u8)) -> (u8, u8, u8) {
    let blend = |v: u8| v.saturating_add(((255 - v) as f32 * 0.4) as u8);
    (blend(c.0), blend(c.1), blend(c.2))
}

/// True when the frame's cursor (if any) sits inside `r` — the single gate
/// every hover treatment shares, so a rect only highlights when `main.rs`
/// would route a click to it (both sides use the same layout math).
fn hover(cursor: Option<(f32, f32)>, r: &LayoutRect) -> bool {
    cursor.is_some_and(|(x, y)| r.contains(x, y))
}

/// An sRGB u8 color mapped to a gpui [`Hsla`] with an explicit alpha.
pub fn color(rgb: (u8, u8, u8), alpha: f32) -> Hsla {
    gpui::Rgba {
        r: rgb.0 as f32 / 255.0,
        g: rgb.1 as f32 / 255.0,
        b: rgb.2 as f32 / 255.0,
        a: alpha,
    }
    .into()
}

/// Drop-shadow styles a quad can carry (painted under it by `main.rs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shadow {
    None,
    /// The big soft shadow under a floating tile card.
    Card,
}

/// A solid (optionally rounded / bordered / shadowed) fill quad, in physical
/// px. `main.rs` converts these to gpui `Bounds<Pixels>` + `paint_quad` at
/// paint time.
#[derive(Clone, Copy)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: Hsla,
    /// Corner radius in physical px (0 = square).
    pub radius: f32,
    /// Border width in physical px (0 = none).
    pub border: f32,
    pub border_color: Hsla,
    pub shadow: Shadow,
}

impl Quad {
    fn shadow(mut self, shadow: Shadow) -> Self {
        self.shadow = shadow;
        self
    }

}

/// One colored run of text within a grid row (or a label line).
#[derive(Clone)]
pub struct TextSpan {
    pub text: String,
    pub color: Hsla,
}

/// A pane's text laid out for painting: origin (physical px) plus one span-list
/// per grid row (already coalesced into same-colored runs).
pub struct PaneText {
    pub origin: (f32, f32),
    pub rows: Vec<Vec<TextSpan>>,
    /// Inline image placements (kitty graphics / iTerm2 / sixel) attached to
    /// cells in this pane, in physical px relative to `origin`.
    pub images: Vec<PaneImage>,
}

/// One placement of an inline image inside a pane, resolved to physical px
/// relative to the pane's content origin. An image spanning several cells is
/// attached to *each* of them by the emulator, each attachment carrying the
/// slice of the decoded image that lands on that cell — so a placement is one
/// of these per covered cell, and they tile.
#[derive(Clone)]
pub struct PaneImage {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Attachment z-index: negative paints under the glyphs (kitty `z=`).
    pub z: i32,
    /// The decoded image in the order gpui samples it (see [`render_image`]).
    pub image: Arc<RenderImage>,
    /// Sub-rect of the decoded image this attachment shows, in source px
    /// (`x`, `y`, `w`, `h`).
    pub src: (f32, f32, f32, f32),
    /// The emulator payload this placement was decoded from. Kept so the mouse
    /// path can turn a click into a file macOS Preview can open.
    pub source: Arc<ImageData>,
}

/// Decode an emulator image payload to `(width, height, RGBA8 pixels)`.
/// Encoded blobs (kitty `f=100`, iTerm2 `File=`) go through the `image` crate
/// — the same decoder wezterm itself uses; raw (`f=32`) and animated RGBA use
/// their first frame. Blob-leased payloads (`EncodedLease`) are not reachable
/// here: pwrde's terminal never swaps images out to the blob store.
pub(crate) fn decode_pixels(data: &ImageDataType) -> Option<(u32, u32, Vec<u8>)> {
    match data {
        ImageDataType::Rgba8 {
            data,
            width,
            height,
            ..
        } => Some((*width, *height, data.clone())),
        ImageDataType::AnimRgba8 {
            frames,
            width,
            height,
            ..
        } => Some((*width, *height, frames.first()?.clone())),
        ImageDataType::EncodedFile(bytes) => {
            let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
            Some((decoded.width(), decoded.height(), decoded.into_raw()))
        }
        _ => None,
    }
}

/// Upload-ready image for gpui: gpui's `RenderImage` frames are BGRA, so the
/// straight-alpha RGBA this decodes to is written red/blue-swapped (exactly as
/// `backdrop::Impression::to_render_image` does for the glass backdrop).
fn render_image(w: u32, h: u32, rgba: &[u8]) -> RenderImage {
    let w = w.max(1);
    let h = h.max(1);
    let mut buf = RgbaImage::new(w, h);
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let (x, y) = ((i as u32) % w, (i as u32) / w);
        if x < w && y < h {
            buf.put_pixel(x, y, image::Rgba([px[2], px[1], px[0], px[3]]));
        }
    }
    RenderImage::new(vec![ImageFrame::from_parts(
        buf,
        0,
        0,
        Delay::from_numer_denom_ms(0, 1),
    )])
}

/// Resolve one cell attachment to a [`PaneImage`]. wezterm attaches the image
/// to the cell it covers: the quad is that cell's rect expanded by the
/// attachment's padding, and `top_left`/`bottom_right` select the slice of the
/// decoded image that belongs to the cell (normalised image coords). Same
/// maths as wezterm-gui's `populate_image_quad`, in physical px.
///
/// The returned `x`/`y` are pane-relative: the cell grid is `col/row * cell`
/// and the pane origin is deliberately not a parameter — the caller adds
/// `PaneText::origin` exactly once, at paint time, the same way it places
/// glyph rows.
fn pane_image(
    attachment: &ImageCell,
    col: usize,
    row: usize,
    cell_w: f32,
    cell_h: f32,
    image: Arc<RenderImage>,
    source: Arc<ImageData>,
) -> PaneImage {
    let (pad_left, pad_top, pad_right, pad_bottom) = attachment.padding();
    let (left, top) = (pad_left as f32, pad_top as f32);
    // Pane-relative (the caller adds `PaneText::origin` back, exactly once,
    // when it converts to gpui logical px) — matching how glyph rows are
    // placed. Subtracting a pane origin here while `paint_pane_images` added
    // only the window origin put every quad a whole pane-origin to the left,
    // i.e. over the sidebar.
    let x = col as f32 * cell_w + left;
    let y = row as f32 * cell_h + top;
    let size = image.size(0);
    let (img_w, img_h) = (size.width.0 as f32, size.height.0 as f32);
    let tl = attachment.top_left();
    let br = attachment.bottom_right();
    PaneImage {
        x,
        y,
        w: cell_w + left - pad_right as f32,
        h: cell_h + top - pad_bottom as f32,
        z: attachment.z_index(),
        image,
        src: (
            *tl.x * img_w,
            *tl.y * img_h,
            (*br.x - *tl.x) * img_w,
            (*br.y - *tl.y) * img_h,
        ),
        source,
    }
}

/// An inline-image placement in *window* coordinates: one of a pane's
/// [`PaneImage`] quads with the pane's content origin folded in. The mouse path
/// hit-tests clicks against these instead of re-walking the terminal grid, and
/// the payload rides along so a click can hand the picture to Preview.
#[derive(Clone)]
pub struct ImageHit {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Attachment z-index (kitty `z=`), so overlapping placements resolve the
    /// same way the painter stacked them.
    pub z: i32,
    pub source: Arc<ImageData>,
}

impl ImageHit {
    /// Fold a pane's content origin into one of the placements it painted.
    pub fn from_pane(origin: (f32, f32), img: &PaneImage) -> Self {
        Self {
            x: origin.0 + img.x,
            y: origin.1 + img.y,
            w: img.w,
            h: img.h,
            z: img.z,
            source: Arc::clone(&img.source),
        }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// The image payload under a point (physical px, window coordinates), or
/// `None` for an ordinary terminal cell. Where placements overlap, the topmost
/// wins: highest z-index first, then the most recently attached payload. Every
/// covered cell of one placement carries the same payload, so any hit inside it
/// resolves to the same bytes.
pub fn image_at(hits: &[ImageHit], px: f32, py: f32) -> Option<Arc<ImageData>> {
    hits.iter()
        .filter(|h| h.contains(px, py))
        .max_by_key(|h| (h.z, h.source.hash()))
        .map(|h| Arc::clone(&h.source))
}

/// A single line of chrome/picker text, positioned in physical px. `clip` is
/// the rect the text must not overflow (right/bottom edges), used by `main.rs`
/// to clip long titles.
#[derive(Clone)]
pub struct LabelSpec {
    pub text: String,
    pub color: Hsla,
    pub left: f32,
    pub top: f32,
    pub clip: LayoutRect,
    /// Font size override in physical px; `None` uses the standard size.
    pub size: Option<f32>,
}


/// Per-frame page/navigation state the renderer needs beyond the workspaces:
/// which page is up plus the overlay facts the canvas paints around.
/// No sidebar rows live here any more — every page's rows are an element tree
/// (`sidebar_ui`), so neither their contents nor the inline editors ever
/// reach the canvas.
pub struct ChromeState {
    pub page: Page,
    /// Bottom safe area (logical px) reserved for the Flow pill bar; 0 while
    /// the `features.flow` flag is off. See `flow_ui::safe_area_h`.
    pub flow_inset: f32,
    /// Physical-pixel cursor position for hover painting. `None` while any
    /// drag is active so hover highlights are suppressed mid-drag.
    pub cursor: Option<(f32, f32)>,
    /// An element-tree modal (confirm dialog / message panel, `modal_ui`)
    /// owns the window: the canvas chrome goes inert and the terminal cursor
    /// hides, exactly as for the canvas-painted overlays.
    pub element_modal: bool,
}

/// Everything `main.rs`'s terminal `Element` needs to paint one frame — all
/// plain data, no GPU or shaping state.
pub struct Frame {
    /// Chrome fills painted under the text (sidebar rows, tile cards).
    pub bg_quads: Vec<Quad>,
    /// Per-pane colored text runs.
    pub panes: Vec<PaneText>,
    /// Foreground fills painted over the text (block/box geometry, cursor,
    /// link underlines).
    pub fg_quads: Vec<Quad>,
    /// Chrome labels (tab titles).
    pub labels: Vec<LabelSpec>,
    /// Flyover panel fills (card background, tab strip, selection rects).
    /// Painted after labels, above the tiles.
    pub flyover_quads: Vec<Quad>,
    /// Flyover terminal text runs.
    pub flyover_panes: Vec<PaneText>,
    /// Flyover foreground fills (cursor, selection).
    pub flyover_fg_quads: Vec<Quad>,
    /// Every interactive rect drawn this frame, in draw order (topmost last).
    /// Used by main.rs to compute ui_hover by reverse-iterating. Resize handles
    /// are excluded — they have their own hover/cursor logic. When a modal
    /// overlay is open, only overlay rects appear here.
    pub hot: Vec<LayoutRect>,
}

/// Stateless renderer: owns only cell metrics and scale. All measurements
/// come from gpui's text system (see `main.rs`), so `new` takes them as
/// arguments instead of creating a GPU surface. The terminal color palette is
/// resolved from settings once per frame in [`Renderer::build_frame`].
pub struct Renderer {
    width: u32,
    height: u32,

    /// Decoded inline images, keyed by wezterm's content hash of the image
    /// data, so a placement that is redrawn every frame decodes once.
    image_cache: RefCell<HashMap<[u8; 32], Arc<RenderImage>>>,

    pub scale: f32,
    /// Terminal grid cell metrics (from the `terminal.font_size` setting).
    pub cell_width: f32,
    pub cell_height: f32,
    /// Chrome/UI text cell metrics (from the `appearance.font_size` setting),
    /// used to lay out and vertically-center chrome labels. Independent of the
    /// terminal cell so the app text can zoom without touching the grid.
    pub chrome_cell_width: f32,
    pub chrome_cell_height: f32,
    /// Logical (pre-scale) font sizes, retained so `main.rs`'s paint can detect
    /// a settings change and re-measure/reflow without re-reading every frame.
    term_font: f32,
    chrome_font: f32,
}

/// Smallest / largest / step for the user-adjustable font sizes (logical px).
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 40.0;
pub const FONT_SIZE_STEP: f32 = 1.0;

/// The configured terminal-grid font size (logical px), clamped to the allowed
/// range. Defaults to [`FONT_SIZE`] when unset.
pub fn terminal_font() -> f32 {
    crate::settings::get_f32("terminal.font_size", FONT_SIZE).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// The configured chrome/UI font size (logical px), clamped to the allowed
/// range. Defaults to [`FONT_SIZE`] when unset.
pub fn chrome_font() -> f32 {
    crate::settings::get_f32("appearance.font_size", FONT_SIZE).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// The chrome font size as a multiple of the default [`FONT_SIZE`].
///
/// Element-tree surfaces (the sidebar) hardcode their type and geometry at the
/// default size and multiply through this, which is how they track the
/// `appearance.font_size` setting without re-deriving every literal.
pub fn chrome_font_scale() -> f32 {
    chrome_font() / FONT_SIZE
}

/// Nudge a font-size setting by `delta`, clamping to the allowed range, and
/// persist it. Used by the ⌘= / ⌘- zoom actions and the Accessibility
/// settings steppers.
pub fn bump_font(key: &str, delta: f32) {
    let cur = crate::settings::get_f32(key, FONT_SIZE).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    let next = (cur + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    crate::settings::set(key, (f64::from(next)).into());
}

/// Measure the advance width of a monospace cell (physical px) by shaping a
/// representative glyph at `font_size` (logical px) × `scale` in gpui's text
/// system.
pub fn measure_cell_width(window: &mut gpui::Window, scale: f32, font_size: f32) -> f32 {
    let font_size = gpui::px(font_size * scale);
    let run = gpui::TextRun {
        len: 1,
        font: gpui::font(FONT_FAMILY),
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line("M".into(), font_size, &[run], None);
    f32::from(line.width).max(1.0)
}

impl Renderer {
    pub fn new(scale: f32, cell_width: f32, width: u32, height: u32) -> Self {
        let mut renderer = Self {
            width: width.max(1),
            height: height.max(1),
            image_cache: RefCell::new(HashMap::new()),
            scale,
            cell_width: 0.0,
            cell_height: 0.0,
            chrome_cell_width: 0.0,
            chrome_cell_height: 0.0,
            term_font: FONT_SIZE,
            chrome_font: FONT_SIZE,
        };
        // Seed both metrics from the measured width at the default font; the
        // first paint re-measures against the live settings if they differ.
        renderer.update_metrics(scale, FONT_SIZE, cell_width, FONT_SIZE, cell_width);
        renderer
    }

    /// Recompute terminal and chrome cell metrics for a new display scale or a
    /// changed font-size setting. The caller re-measures each cell width at the
    /// matching logical font via [`measure_cell_width`].
    ///
    /// Widths retain the text system's fractional advance: rounding here makes
    /// glyph positions drift away from cell geometry across a row. Heights stay
    /// integer-rounded so adjacent rows share physical-pixel boundaries.
    pub fn update_metrics(
        &mut self,
        scale: f32,
        term_font: f32,
        term_cell_width: f32,
        chrome_font: f32,
        chrome_cell_width: f32,
    ) {
        self.scale = scale;
        self.term_font = term_font;
        self.chrome_font = chrome_font;
        self.cell_width = term_cell_width;
        self.cell_height = (term_font * scale * LINE_HEIGHT_FACTOR).round();
        self.chrome_cell_width = chrome_cell_width;
        self.chrome_cell_height = (chrome_font * scale * LINE_HEIGHT_FACTOR).round();
    }

    /// Integer physical-pixel metrics for the PTY protocol boundary. Rendering
    /// continues to use the measured fractional width.
    pub fn pty_cell_size(&self) -> (u16, u16) {
        (self.cell_width.round() as u16, self.cell_height.round() as u16)
    }

    /// The logical (pre-scale) terminal / chrome font sizes currently in effect.
    pub fn term_font(&self) -> f32 {
        self.term_font
    }
    pub fn chrome_font_logical(&self) -> f32 {
        self.chrome_font
    }

    /// Physical-px font size for shaping the terminal grid (logical × scale).
    pub fn font_size(&self) -> f32 {
        self.term_font * self.scale
    }

    /// Physical-px font size for shaping chrome/UI labels (logical × scale).
    pub fn chrome_font_size(&self) -> f32 {
        self.chrome_font * self.scale
    }

    /// The active theme, re-read from the settings store each call so a
    /// Themes-page click restyles the very next frame.
    pub fn theme(&self) -> &'static Theme {
        crate::theme::current()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Grid dimensions that fit inside a tile's *content* rect (minus pad).
    pub fn grid_size_for(&self, rect: &LayoutRect) -> (usize, usize) {
        let pad = PANE_PAD * self.scale;
        let cols = ((rect.w - 2.0 * pad) / self.cell_width).floor() as usize;
        let rows = ((rect.h - 2.0 * pad) / self.cell_height).floor() as usize;
        (cols.max(2), rows.max(1))
    }

    /// Where a content rect's terminal cells start, in physical px.
    fn content_origin(&self, rect: &LayoutRect) -> (f32, f32) {
        let pad = (PANE_PAD * self.scale).round();
        (rect.x + pad, rect.y + pad)
    }

    /// The (col, row) cell under a point within a tile's content rect.
    pub fn cell_at(&self, content: &LayoutRect, px: f32, py: f32) -> Option<(usize, usize)> {
        let (ox, oy) = self.content_origin(content);
        if px < ox || py < oy {
            return None;
        }
        Some((
            ((px - ox) / self.cell_width) as usize,
            ((py - oy) / self.cell_height) as usize,
        ))
    }

    /// Translucent highlight quads for the session's active selection, one
    /// per visible row of the span. No-op for an empty (zero-width) selection.
    fn selection_rects(&self, session: &Session, origin: (f32, f32), rects: &mut Vec<Quad>) {
        let Some((start, end)) = session.selection_span() else { return };
        // A zero-width selection (a bare click, no drag) paints nothing —
        // mirrors `selected_text`, which returns no text for the same state.
        if start == end {
            return;
        }
        let offset = session.scroll_offset() as i32;
        let term = session.term.lock().unwrap();
        let screen = term.screen();
        let rows = screen.physical_rows;
        let cols = screen.physical_cols;
        let phys = screen.scrollback_or_visible_range(&(-offset..rows as i32 - offset));
        let s_top = screen.phys_to_stable_row_index(phys.start);
        drop(term);

        for vrow in 0..rows {
            let r = s_top + vrow as isize;
            if r < start.1 || r > end.1 {
                continue;
            }
            // Column span for this row: the first row starts at the anchor
            // col, the last row ends after the head cell, rows between are
            // full-width.
            let (c0, c1) = if start.1 == end.1 {
                (start.0, end.0 + 1)
            } else if r == start.1 {
                (start.0, cols)
            } else if r == end.1 {
                (0, end.0 + 1)
            } else {
                (0, cols)
            };
            let c1 = c1.min(cols);
            if c1 <= c0 {
                continue;
            }
            rects.push(self.cell_rect(
                origin,
                c0,
                vrow,
                0.0,
                0.0,
                (c1 - c0) as f32,
                1.0,
                self.theme().accent,
                0.30,
            ));
        }
    }

    /// Walk the active workspace + terminal grids and produce a [`Frame`] of
    /// plain data for `main.rs`'s `Element` to paint. Replaces the old wgpu
    /// `draw()`; the per-frame painting now lives in the gpui element.
    pub fn build_frame(
        &self,
        workspaces: &[Workspace],
        active: usize,
        sidebar_w: f32,
        drop_hint: Option<LayoutRect>,
        link_hover: Option<(u64, usize, usize)>,
        chrome: &ChromeState,
    ) -> Frame {
        let th = self.theme();
        // Terminal scheme, resolved once per frame like the chrome theme so
        // an Appearance-page click restyles the very next paint. Pane chrome
        // (card fill, tab text, divider, active-tab pill) follows the scheme
        // so tab strips stay legible on light palettes; the adaptive default
        // keeps the chrome theme's exact colors.
        let scheme = crate::term_theme::selected(crate::theme::dark_active());
        let term_palette = crate::term_theme::build(scheme, th.term_bg);
        // `pane_bg` is deliberately dropped: the ground is painted in that
        // colour already (`term_scheme_bg`), so no pane fills itself.
        let (_, _pane_ink, _pane_ink_dim, pane_divider, pane_pill) = match scheme {
            Some(t) => (t.bg, (t.fg, 1.0), (t.fg, 0.55), (t.fg, 0.15), (t.fg, 0.12)),
            None => (
                th.term_bg,
                (th.text_bright, 1.0),
                (th.text_dim, 1.0),
                (th.card_divider, 1.0),
                ((255, 255, 255), 0.13),
            ),
        };
        let ws = &workspaces[active];
        let (width, height) = (self.width, self.height);
        // The terminal area runs to the window's right edge minus the normal
        // AREA_PAD — there is no right-edge tool panel or ribbon any more.
        let area =
            workspace::terminal_area(width, height, self.scale, sidebar_w, chrome.flow_inset);
        let empty = workspaces.len() == 1 && workspaces[0].is_empty();
        // Dividers aren't painted (the gap between cards shows the gradient);
        // they remain drag handles for hit-testing in `main.rs`. The empty
        // state draws no tile cards at all — just the centered CTA.
        let (tiles, _dividers) = if empty {
            (Vec::new(), Vec::new())
        } else {
            workspace::layout_tiles(&ws.root, area, self.scale)
        };

        let mut bg_quads: Vec<Quad> = Vec::new();
        let mut fg_quads: Vec<Quad> = Vec::new();
        let mut labels: Vec<LabelSpec> = Vec::new();
        let mut panes: Vec<PaneText> = Vec::new();
        let mut hot: Vec<LayoutRect> = Vec::new();
        // Overlays are modal: while one is up only its elements hover or
        // register as hot; the chrome underneath goes inert (mirroring the
        // click routing in `main.rs`, which sends every click to the overlay).
        let overlay_open = chrome.element_modal;
        let cur = if overlay_open { None } else { chrome.cursor };
        // Which axis each tile would collapse along (its parent split's dir);
        // `None` = root leaf, which shows no caret and cannot collapse.
        let collapse_axis_map: std::collections::HashMap<u64, Option<workspace::Dir>> =
            workspace::tile_collapse_axis(&ws.root).into_iter().collect();

        // ── Sidebar chrome (identical geometry on every page) ──────────
        // The window gradient is painted by `main.rs` before these quads;
        // the sidebar itself is transparent — its rounded rows float on it.
        let row_r = (ROW_RADIUS * self.scale).round();
        // Traffic lights are the native macOS buttons now (transparent titlebar),
        // so we no longer draw our own here.
        // The whole left region — folders card, sessions list, header chips,
        // Settings rows and the empty state — is an element tree now
        // (`sidebar_ui::render_sidebar`, `folders_ui`), so the canvas paints
        // nothing for it on any page.

        let card_r = (CARD_RADIUS * self.scale).round();

        if matches!(
            chrome.page,
            Page::Settings | Page::Tool(_)
        ) {
            // Content is a gpui overlay (settings_ui) or,
            // for a tool page, painted by `tool_page` — the canvas paints the
            // sidebar only here.
        } else {
            let hair = (1.0 * self.scale).round().max(1.0);
            // Messages-style blending: only the focused pane is a *card*. The
            // rest share the window's ground (which is `term_bg` too), so they
            // carry no shadow and no divider and are delimited only by their
            // capsule tab — the same way an unselected Messages thread has no
            // chrome of its own.
            let focus = ws.focused_tile;
            // Focused tile LAST. `layout_tiles` returns tree order, so an
            // unfocused pane's fill could land after the focused pane's shadow
            // and clip it — a soft gradient stopping at a hard edge, which read
            // as a smudge rather than a shadow. Painting the raised card after
            // everything that sits at ground level makes the shadow composite
            // uniformly on all four sides.
            let mut order: Vec<&(u64, workspace::LayoutRect)> = tiles.iter().collect();
            order.sort_by_key(|(id, _)| *id == focus);
            for (id, rect) in order {
                let is_focused = *id == focus;
                // Unfocused panes skip the fill entirely: the ground is already
                // painted in this exact colour (`term_scheme_bg`), so the quad
                // is a no-op that can only overdraw a neighbour's shadow. That
                // is what makes them read as the app background rather than as
                // panes that happen to match it.
                // No fill and no shadow for any pane, focused included. The
                // ground is already this exact colour, and a shadow made the
                // focused pane pop out of the window instead of sitting in it.
                // Focus is shown by tinting its active tab accent — the same
                // signal the sidebar uses for the selected group, so the two
                // read as one idea.
                let axis = collapse_axis_map.get(id).copied().flatten();
                let Some(tile) = ws.root.find_tile(*id) else { continue };
                // Collapsed (or mid-animation) panes hide their content; a
                // sideways-collapsed pane is a bare strip showing only the caret.
                let collapsing = axis.is_some() && (tile.collapsed || tile.collapse_anim > 0.0);
                let side_strip = axis == Some(workspace::Dir::Row) && collapsing;
                let strip = workspace::tab_strip_rect(area, rect, self.scale, sidebar_w);
                let bar = workspace::tile_tab_bar(&strip, self.scale);
                // The rule under the tab strip is part of the card, so it
                // goes with the card: an unfocused pane would otherwise be a
                // stray hairline floating on the ground.
                if !collapsing && is_focused {
                    let divider =
                        LayoutRect { x: rect.x, y: bar.y + bar.h - hair, w: rect.w, h: hair };
                    bg_quads.push(self.px_rect(&divider, pane_divider.0, pane_divider.1, 0.0));
                }
                if side_strip {
                    // A sideways-collapsed strip is one big "expand" target:
                    // any click reopens it, so the whole bare card hovers.
                    if hover(cur, rect) {
                        bg_quads.push(self.px_rect(rect, pane_pill.0, pane_pill.1 * 0.6, card_r));
                    }
                    hot.push(*rect);
                }
            }

            // ── Terminal snapshots + per-tile chrome (tab strips) ──────────
            let focused_tile = Some(ws.focused_tile);
            for (id, rect) in &tiles {
                let Some(tile) = ws.root.find_tile(*id) else { continue };
                let axis = collapse_axis_map.get(id).copied().flatten();
                let has_caret = axis.is_some();
                let collapsing = has_caret && (tile.collapsed || tile.collapse_anim > 0.0);
                let side_strip = axis == Some(workspace::Dir::Row) && collapsing;
                // Collapsed (or mid-animation) panes paint no terminal
                // content — the card is just its tab strip.
                let content = workspace::tile_content(rect, self.scale);
                let origin = self.content_origin(&content);
                if !collapsing
                    && let Some(session) = tile.tabs.get(tile.active).and_then(|t| t.session())
                {
                    let draw_cursor = Some(*id) == focused_tile && !chrome.element_modal;
                    let tile_hover = link_hover
                        .filter(|(hid, _, _)| *hid == *id)
                        .map(|(_, col, row)| (col, row));
                    let (rows, images) = self.snapshot_pane(
                        session,
                        &term_palette,
                        origin,
                        draw_cursor,
                        tile_hover,
                        &mut bg_quads,
                        &mut fg_quads,
                    );
                    panes.push(PaneText { origin, rows, images });
                    self.selection_rects(session, origin, &mut fg_quads);
                }
                // A sideways strip has no room for the strip's labels: only
                // the caret shows. A stacked collapse keeps its tab labels
                // (clicking one focuses + expands).
                if side_strip {
                    continue;
                }

                // The strip's pixels — pills, titles, × buttons, unread dots
                // — are an element tree now (`tile_ui`). The canvas keeps
                // only the hit rects: the close rect after its tab so
                // reverse iteration (topmost wins) resolves × over the tab.
                let strip = workspace::tab_strip_rect(area, rect, self.scale, sidebar_w);
                for ti in 0..tile.tabs.len() {
                    let tr = workspace::tile_tab_rect(&strip, ti, tile.tabs.len(), self.scale, has_caret);
                    let close =
                        workspace::tile_tab_close_rect(&strip, ti, tile.tabs.len(), self.scale, has_caret);
                    hot.push(tr);
                    hot.push(close);
                }
            }


            // Drag-drop target hint (a translucent accent overlay).
            if let Some(hint) = drop_hint {
                fg_quads.push(self.px_rect(&hint, th.accent, 0.3, row_r));
            }
        }

        // Resize grips (sidebar edge, dividers, flyover edge) are element
        // handles now (`resize_ui`), which paint their own hover grip.

        // Frame-stats overlay (Settings → Debug toggle): one line near the
        // content area's top-right, painted on every page.
        if crate::settings::get_bool("debug.overlay", false) {
            let (cols, rows) = ws
                .focused()
                .and_then(|t| t.active_tab())
                .map_or((0, 0), |tab| (tab.cols, tab.rows));
            let text =
                format!("{width}×{height} px · {:.2}x · grid {cols}×{rows}", self.scale);
            let pad = (14.0 * self.scale).round();
            let w = text.chars().count() as f32 * self.chrome_cell_width;
            labels.push(LabelSpec {
                text,
                color: color(th.text_dim, 0.9),
                left: (area.x + area.w - pad - w).round(),
                top: (area.y + (6.0 * self.scale)).round(),
                clip: area,
                size: None,
            });
        }

        // ── Modal scoping ────────────────────────────────────────────
        // A modal overlay owns the frame's interactivity: the chrome hot
        // rects collected above go inert, and only overlay elements register.
        if overlay_open {
            hot.clear();
        }

        Frame {
            bg_quads,
            panes,
            fg_quads,
            labels,
            flyover_quads: Vec::new(),
            flyover_panes: Vec::new(),
            flyover_fg_quads: Vec::new(),
            hot,
        }
    }


    /// The terminal background the active colors want: the selected scheme's
    /// bg, or the chrome theme's terminal background under the adaptive
    /// default. The popout window fills with this behind the flyover card.
    pub fn term_scheme_bg(&self) -> (u8, u8, u8) {
        crate::term_theme::selected(crate::theme::dark_active())
            .map_or(self.theme().term_bg, |t| t.bg)
    }

    /// Build all geometry for the flyover terminal panel (card, tab strip,
    /// terminal text). Returns the frame fields to be set on the caller's Frame.
    /// `tabs` is the flyover tab list, `active` is the active tab index,
    /// `panel_rect` is where the panel sits — the slide-animated bottom strip
    /// in the main window (`workspace::flyover_rect`), or the full window in
    /// the popout — and `focused` indicates whether it holds keyboard focus.
    /// An open-but-empty panel (first-open picker flow) still paints its card
    /// so the slide-in reads; only the tab/content parts need tabs.
    /// `show_window_buttons` draws the minimize/maximize squares at the
    /// bar's right — the in-window panel wants them, the popout window has
    /// real window controls instead.
    pub fn flyover_overlay(
        &self,
        tabs: &[crate::workspace::Tab],
        active: usize,
        panel_rect: &crate::workspace::LayoutRect,
        focused: bool,
        draw_cursor: bool,
        show_window_buttons: bool,
        maximized: bool,
        _cursor: Option<(f32, f32)>,
        hot: &mut Vec<LayoutRect>,
    ) -> (Vec<Quad>, Vec<PaneText>, Vec<Quad>) {
        let th = self.theme();
        let scale = self.scale;
        // Resolve the terminal scheme exactly as `build_frame` does for tile
        // cards, so the flyover follows the Appearance-page terminal colors:
        // scheme bg/fg drive the card and tab chrome when one is selected.
        let scheme = crate::term_theme::selected(crate::theme::dark_active());
        let term_palette = crate::term_theme::build(scheme, th.term_bg);
        let (pane_bg, _pane_ink, _pane_ink_dim, pane_divider) = match scheme {
            Some(t) => (t.bg, (t.fg, 1.0), (t.fg, 0.55), (t.fg, 0.15)),
            None => (
                th.term_bg,
                (th.text_bright, 1.0),
                (th.text_dim, 1.0),
                (th.card_divider, 1.0),
            ),
        };

        let tab_bar = crate::workspace::flyover_tab_bar(panel_rect, scale);
        let content = crate::workspace::flyover_content(panel_rect, scale);
        let n = tabs.len();

        let mut quads: Vec<Quad> = Vec::new();
        let mut fg_quads: Vec<Quad> = Vec::new();
        let mut panes: Vec<PaneText> = Vec::new();

        // Card background + border.
        let card_r = (8.0 * scale).round();
        quads.push(self.px_rect(panel_rect, pane_bg, 1.0, card_r).shadow(Shadow::Card));
        // Subtle top border line.
        let border = crate::workspace::LayoutRect {
            x: panel_rect.x,
            y: panel_rect.y,
            w: panel_rect.w,
            h: (1.0_f32 * scale).round().max(1.0),
        };
        quads.push(self.px_rect(&border, pane_divider.0, pane_divider.1 * 0.5, 0.0));

        // Tab hit rects: the strip's pixels (pills, titles, × buttons, unread
        // dots) are element-tree tabs from `flyover_ui` now, so the canvas
        // emits only the rects the mouse path still resolves clicks on —
        // same `workspace::flyover_tab_rect` / `flyover_tab_close_rect`
        // geometry the element tree renders at.
        for (i, _tab) in tabs.iter().enumerate() {
            let tr = crate::workspace::flyover_tab_rect(panel_rect, i, n, scale, maximized);
            let close = crate::workspace::flyover_tab_close_rect(panel_rect, i, n, scale, maximized);
            // Close after its tab so reverse iteration (topmost wins)
            // resolves × over the tab it sits in.
            hot.push(tr);
            hot.push(close);
        }

        // Minimize / maximize hit rects at the bar's right edge. Their
        // buttons are element-tree svgs in `flyover_ui`; the canvas keeps
        // only the rects. `draw_cursor` is the interactivity gate (false
        // while a modal overlay owns the frame), so inert controls must not
        // register as clickable above the overlay.
        if show_window_buttons && draw_cursor {
            hot.push(crate::workspace::flyover_minimize_rect(panel_rect, scale));
            hot.push(crate::workspace::flyover_maximize_rect(panel_rect, scale));
        }

        // Tab-bar bottom divider line.
        let divider = crate::workspace::LayoutRect {
            x: tab_bar.x,
            y: tab_bar.y + tab_bar.h - (1.0_f32 * scale).round().max(1.0),
            w: tab_bar.w,
            h: (1.0_f32 * scale).round().max(1.0),
        };
        quads.push(self.px_rect(&divider, pane_divider.0, pane_divider.1, 0.0));

        // Terminal content for the active tab.
        if let Some(session) = tabs.get(active).and_then(|tab| tab.session()) {
            let origin = self.content_origin(&content);
            let (rows, images) = self.snapshot_pane(
                session,
                &term_palette,
                origin,
                draw_cursor && focused,
                None,
                &mut quads,
                &mut fg_quads,
            );
            panes.push(PaneText { origin, rows, images });
            self.selection_rects(session, origin, &mut fg_quads);
        }

        (quads, panes, fg_quads)
    }

    /// Build a CLI tool page: one card filling `area`, styled like the
    /// focused tile, with the tool's name in its strip and — once the command
    /// has ended — a relaunch hint. Slots into the frame's ordinary layers.
    pub fn tool_page(
        &self,
        tab: &crate::workspace::Tab,
        area: &LayoutRect,
        title: &str,
        exited: bool,
        draw_cursor: bool,
    ) -> (Vec<Quad>, PaneText, Vec<Quad>, Vec<LabelSpec>) {
        let th = self.theme();
        let scale = self.scale;
        let scheme = crate::term_theme::selected(crate::theme::dark_active());
        let term_palette = crate::term_theme::build(scheme, th.term_bg);
        let (pane_bg, pane_ink, pane_ink_dim) = match scheme {
            Some(t) => (t.bg, (t.fg, 1.0), (t.fg, 0.55)),
            None => (th.term_bg, (th.text_bright, 1.0), (th.text_dim, 1.0)),
        };
        let mut quads = Vec::new();
        let mut fg_quads = Vec::new();
        let mut labels = Vec::new();

        let card_r = (CARD_RADIUS * scale).round();
        quads.push(self.px_rect(area, pane_bg, 1.0, card_r).shadow(Shadow::Card));

        // Strip: the tool's name, then the exit hint when there is one.
        let bar = crate::workspace::tile_tab_bar(area, scale);
        let text_pad = (12.0 * scale).round();
        let top = (bar.y + (bar.h - self.chrome_cell_height) / 2.0).round();
        labels.push(LabelSpec {
            text: title.to_string(),
            color: color(pane_ink.0, pane_ink.1),
            left: bar.x + text_pad,
            top,
            clip: bar,
            size: None,
        });
        if exited {
            let hint = "exited — ⏎ to relaunch";
            let w = hint.chars().count() as f32 * self.chrome_cell_width;
            labels.push(LabelSpec {
                text: hint.to_string(),
                color: color(pane_ink_dim.0, pane_ink_dim.1),
                left: (bar.x + bar.w - text_pad - w).max(bar.x),
                top,
                clip: bar,
                size: None,
            });
        }

        let content = crate::workspace::tile_content(area, scale);
        let origin = self.content_origin(&content);
        let Some(session) = tab.session() else {
            return (
                quads,
                PaneText { origin, rows: Vec::new(), images: Vec::new() },
                fg_quads,
                labels,
            );
        };
        let (rows, images) = self.snapshot_pane(
            session,
            &term_palette,
            origin,
            draw_cursor && !exited,
            None,
            &mut quads,
            &mut fg_quads,
        );
        self.selection_rects(session, origin, &mut fg_quads);
        (quads, PaneText { origin, rows, images }, fg_quads, labels)
    }

    /// Snapshot one pane's grid into per-row text spans + geometry quads,
    /// offset to `origin`. One `Vec<TextSpan>` per grid row (so the caller can
    /// shape each row independently). Holds the terminal lock only for the walk.
    /// Decode (and cache) an emulator image payload for painting.
    fn decoded_image(&self, data: &ImageData) -> Option<Arc<RenderImage>> {
        let key = data.hash();
        if let Some(cached) = self.image_cache.borrow().get(&key) {
            return Some(Arc::clone(cached));
        }
        let (w, h, rgba) = {
            let guard = data.data();
            decode_pixels(&guard)
        }?;
        let image = Arc::new(render_image(w, h, &rgba));
        self.image_cache.borrow_mut().insert(key, Arc::clone(&image));
        Some(image)
    }

    fn snapshot_pane(
        &self,
        session: &Session,
        palette: &ColorPalette,
        origin: (f32, f32),
        draw_cursor: bool,
        hover: Option<(usize, usize)>,
        bg_rects: &mut Vec<Quad>,
        rects: &mut Vec<Quad>,
    ) -> (Vec<Vec<TextSpan>>, Vec<PaneImage>) {
        let th = self.theme();
        // Box-drawing line thickness in px, and in cell-relative units.
        let thickness = (self.cell_width / 8.0).round().max(1.0);
        let (tx, ty) = (thickness / self.cell_width, thickness / self.cell_height);

        // Honor scrollback: render the viewport shifted up by `scroll_offset`
        // lines into history instead of always the live bottom.
        let offset = session.scroll_offset() as i32;
        let term = session.term.lock().unwrap();
        let screen = term.screen();
        let rows = screen.physical_rows;
        let lines = screen.lines_in_phys_range(
            screen.scrollback_or_visible_range(&(-offset..rows as i32 - offset)),
        );

        // Coalesce per-cell colors into runs: one span per same-colored
        // stretch keeps the shaping input small.
        // Links get the accent color + an underline quad; ⌘-click opens.
        // Detection is wrap-aware: a URL broken across rows is one link.
        let links = crate::links::links_in_lines(&lines, screen.physical_cols);
        // Resolve which URL (if any) the mouse is hovering over — covers all
        // rows of a wrapped link so the entire anchor brightens together.
        let hovered_url: Option<String> = hover.and_then(|(col, row)| {
            crate::links::hovered_url(&links, row, col).map(|s| s.to_owned())
        });
        for l in &links {
            let is_hovered = hovered_url.as_deref() == Some(l.url.as_str());
            let span = (l.end_col - l.start_col + 1) as f32;
            // Hovered links get a slightly thicker underline, still hugging
            // the cell bottom.
            let (y_off, height) = if is_hovered { (0.91, 0.09) } else { (0.92, 0.06) };
            rects.push(self.cell_rect(
                origin, l.start_col, l.row, 0.0, y_off, span, height, th.accent, 1.0,
            ));
        }

        let mut rows_spans: Vec<Vec<TextSpan>> = Vec::with_capacity(lines.len());
        let mut images: Vec<PaneImage> = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            let mut spans: Vec<TextSpan> = Vec::new();
            for cell in line.visible_cells() {
                let col = cell.cell_index();
                let attrs = cell.attrs();
                // Inline images hang off the cell's attributes; each covered
                // cell carries its own slice of the decoded image.
                if let Some(attachments) = attrs.images() {
                    for attachment in &attachments {
                        let data = attachment.image_data();
                        if let Some(image) = self.decoded_image(data) {
                            images.push(pane_image(
                                attachment,
                                col,
                                row,
                                self.cell_width,
                                self.cell_height,
                                image,
                                data.clone(),
                            ));
                        }
                    }
                }
                // Reverse video swaps fg/bg: the cell fills with the resolved
                // foreground and the glyph is shaped in the resolved background,
                // so reversed cells stay legible instead of vanishing.
                let (fg, bg) = if attrs.reverse() {
                    (
                        palette.resolve_bg(attrs.background()),
                        palette.resolve_fg(attrs.foreground()),
                    )
                } else {
                    (
                        palette.resolve_fg(attrs.foreground()),
                        palette.resolve_bg(attrs.background()),
                    )
                };
                // Cell background fill goes in the BG layer (painted before the
                // glyphs), so reverse/standout cells (zsh's bracketed-paste
                // highlight, Claude's selected rows) don't cover their text with
                // a solid box. Skip the terminal default bg (window clear covers
                // it) so we only emit quads for cells that actually differ.
                if bg != palette.background {
                    let (br, bg8, bb, _) = bg.to_srgb_u8();
                    bg_rects.push(self.cell_rect(
                        origin, col, row, 0.0, 0.0, 1.0, 1.0, (br, bg8, bb), 1.0,
                    ));
                }
                let (r, g, b, _) = fg.to_srgb_u8();
                let rgb = match links.iter().find(|l| l.contains(row, col)) {
                    Some(l) if hovered_url.as_deref() == Some(l.url.as_str()) => {
                        brighten(th.accent)
                    },
                    Some(_) => th.accent,
                    None => (r, g, b),
                };

                // Block elements and box-drawing lines are drawn as exact
                // cell-filling geometry, never as glyphs: fonts don't
                // guarantee they tile, which leaves gaps between rows.
                let mut ch_iter = cell.str().chars();
                let single = (ch_iter.next(), ch_iter.next());
                if let (Some(ch), None) = single
                    && let Some(units) = char_rects(ch, tx, ty)
                {
                    rects.extend(units.iter().map(|u| {
                        self.cell_rect(origin, col, row, u.x, u.y, u.w, u.h, rgb, u.alpha)
                    }));
                    // Keep column alignment in the text run.
                    match spans.last_mut() {
                        Some(span) => span.text.push(' '),
                        _ => spans.push(TextSpan { text: " ".into(), color: color(rgb, 1.0) }),
                    }
                    continue;
                }

                let hsla = color(rgb, 1.0);
                match spans.last_mut() {
                    Some(span) if span.color == hsla => span.text.push_str(cell.str()),
                    _ => spans.push(TextSpan { text: cell.str().to_string(), color: hsla }),
                }
            }
            rows_spans.push(spans);
        }

        // Cursor: a solid quad, drawn on top of the text (focused tile only).
        let cur = term.cursor_pos();
        if draw_cursor && cur.visibility == CursorVisibility::Visible && cur.y >= 0 {
            let (r, g, b, _) = palette.foreground.to_srgb_u8();
            rects.push(self.cell_rect(
                origin,
                cur.x,
                cur.y as usize,
                0.0,
                0.0,
                1.0,
                1.0,
                (r, g, b),
                1.0,
            ));
        }

        (rows_spans, images)
    }

    /// A quad straight from layout coordinates (already physical px).
    fn px_rect(&self, r: &LayoutRect, rgb: (u8, u8, u8), alpha: f32, radius: f32) -> Quad {
        Quad {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
            color: color(rgb, alpha),
            radius,
            border: 0.0,
            border_color: Hsla::default(),
            shadow: Shadow::None,
        }
    }

    /// Build a pixel-space quad for a sub-region of a cell, with edges snapped
    /// to physical pixels so adjacent cells tile without seams.
    #[allow(clippy::too_many_arguments)]
    fn cell_rect(
        &self,
        origin: (f32, f32),
        col: usize,
        row: usize,
        ux: f32,
        uy: f32,
        uw: f32,
        uh: f32,
        rgb: (u8, u8, u8),
        alpha: f32,
    ) -> Quad {
        let base_x = origin.0 + col as f32 * self.cell_width;
        let base_y = origin.1 + row as f32 * self.cell_height;
        // Round each edge (not pos+size) so neighbors share exact edges.
        let x0 = (base_x + ux * self.cell_width).round();
        let y0 = (base_y + uy * self.cell_height).round();
        let x1 = (base_x + (ux + uw) * self.cell_width).round();
        let y1 = (base_y + (uy + uh) * self.cell_height).round();
        Quad {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
            color: color(rgb, alpha),
            radius: 0.0,
            border: 0.0,
            border_color: Hsla::default(),
            shadow: Shadow::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn flyover_without_strip_pixels_keeps_hot_rects() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let panel = crate::workspace::flyover_rect(1600, 1000, scale, 1.0, 0.35, false);
        let tabs = [crate::workspace::Tab::new(crate::term::Session::placeholder())];
        let close = crate::workspace::flyover_tab_close_rect(&panel, 0, 1, scale, false);
        let cursor = Some((close.x + close.w / 2.0, close.y + close.h / 2.0));

        let mut hot = Vec::new();
        let (quads, _, _) = renderer
            .flyover_overlay(&tabs, 0, &panel, true, true, true, false, cursor, &mut hot);

        assert_eq!(hot.len(), 4, "tab + close + minimize + maximize stay hot");
        assert!(hot.iter().any(|r| r.x == close.x && r.y == close.y));

        // The strip's pixels ride the element tree now: the canvas paints
        // only the card, top border, tab-bar divider and terminal content,
        // so nothing may sit inside the tab bar above its divider.
        assert!(quads.len() >= 3, "card, border and divider still paint");
        let tab_bar = crate::workspace::flyover_tab_bar(&panel, scale);
        let divider_y = tab_bar.y + tab_bar.h - (1.0_f32 * scale).round().max(1.0);
        // The card background and top border span the full panel width and
        // are retained chrome; per-tab pills, chips and unread dots were
        // always narrower than the bar. So a strip pixel is a quad fully
        // inside the bar above its divider that is NOT one of those
        // full-width layers.
        let strip_pixels = quads.iter().any(|q| {
            q.w < tab_bar.w
                && q.x >= tab_bar.x
                && q.x + q.w <= tab_bar.x + tab_bar.w
                && q.y >= tab_bar.y
                && q.y + q.h <= divider_y
        });
        assert!(!strip_pixels, "no strip pixels inside the tab bar");
    }

    /// Flyover hit-testing is interactivity-only now: the × hover, chip and
    /// label pixels live in the element tree, so canvas output is identical
    /// with or without a cursor and only the hot rects matter. While a modal
    /// overlay owns the frame (`draw_cursor == false`) the inert window
    /// buttons must drop out of the hot list entirely.
    #[test]
    fn flyover_hot_rects_track_interactivity() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let panel = crate::workspace::flyover_rect(1600, 1000, scale, 1.0, 0.35, false);
        let tabs = [crate::workspace::Tab::new(crate::term::Session::placeholder())];
        let close = crate::workspace::flyover_tab_close_rect(&panel, 0, 1, scale, false);

        let mut hot = Vec::new();
        renderer
            .flyover_overlay(&tabs, 0, &panel, true, true, true, false, None, &mut hot);
        // Tab, its ×, and the two window buttons are all interactive.
        assert_eq!(hot.len(), 4, "tab + close + minimize + maximize are hot");
        assert!(hot.iter().any(|r| r.x == close.x && r.y == close.y));

        // A cursor changes pixels only (element-tree hover); the hot list
        // — and therefore the canvas output — is identical.
        let cursor = Some((close.x + close.w / 2.0, close.y + close.h / 2.0));
        let mut hot2 = Vec::new();
        renderer
            .flyover_overlay(&tabs, 0, &panel, true, true, true, false, cursor, &mut hot2);
        assert_eq!(hot2, hot, "hot rects do not depend on the cursor");

        // A modal overlay owns the frame: minimize/maximize become inert and
        // must not register as clickable above the overlay.
        let mut hot3 = Vec::new();
        renderer
            .flyover_overlay(&tabs, 0, &panel, true, false, true, false, None, &mut hot3);
        assert_eq!(
            hot3.len(),
            2,
            "overlay-open frame keeps only tab + close hot"
        );
        let minr = crate::workspace::flyover_minimize_rect(&panel, scale);
        assert!(!hot3.iter().any(|r| r.x == minr.x && r.y == minr.y));
    }

    #[test]
    fn fractional_cell_width_survives_until_pty_boundary() {
        let mut renderer = Renderer::new(1.5, 10.8, 1600, 1000);
        assert_eq!(renderer.cell_width, 10.8);
        assert_eq!(renderer.chrome_cell_width, 10.8);
        assert_eq!(renderer.pty_cell_size(), (11, 28));

        renderer.update_metrics(1.5, 13.0, 9.4, 12.0, 8.6);
        assert_eq!(renderer.cell_width, 9.4);
        assert_eq!(renderer.chrome_cell_width, 8.6);
        assert_eq!(renderer.pty_cell_size(), (9, 24));
    }

    #[test]
    fn cumulative_column_positions_track_measured_advance() {
        let renderer = Renderer::new(1.5, 10.8, 1600, 1000);
        let origin = (100.0, 50.0);
        let mut right = origin.0;

        for col in 0..12 {
            let q = renderer.cell_rect(origin, col, 0, 0.0, 0.0, 1.0, 1.0, (255, 0, 0), 1.0);
            assert_eq!(q.x, right, "column {col} must share the previous edge");
            right = q.x + q.w;
        }

        // Twelve fractional cells span 129.6 px, not the 132 px produced by
        // rounding each cell to 11 px before laying out the grid.
        assert!((right - origin.0 - 129.6).abs() <= 0.5);
    }

    #[test]
    fn raw_rgba_image_data_reaches_gpui_as_bgra() {
        // 2x1 opaque red + green, the kitty `f=32` payload shape.
        let data = ImageData::with_data(ImageDataType::new_single_frame(
            2,
            1,
            vec![255, 0, 0, 255, 0, 255, 0, 255],
        ));
        let renderer = Renderer::new(2.0, 8.0, 100, 100);
        let decoded = renderer.decoded_image(&data).expect("raw RGBA must decode");
        assert_eq!(decoded.size(0).width.0, 2);
        assert_eq!(decoded.size(0).height.0, 1);
        let bytes = decoded.as_bytes(0).expect("frame 0 exists");
        assert_eq!(&bytes[..4], &[0, 0, 255, 255], "red lands in the BGRA red slot");
        assert_eq!(&bytes[4..], &[0, 255, 0, 255], "green stays green");
    }

    #[test]
    fn encoded_image_data_decodes_through_the_image_crate() {
        // A 1x1 PNG, as a kitty `f=100` or iTerm2 `File=` payload delivers it.
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0xE0, 0x12, 0x91, 0xFB, 0x0F, 0x00, 0x01, 0xA4, 0x01, 0x3C, 0x93, 0x8B,
            0x0E, 0xB7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let data = ImageData::with_raw_data(PNG.to_vec());
        let renderer = Renderer::new(2.0, 8.0, 100, 100);
        let decoded = renderer
            .decoded_image(&data)
            .expect("a PNG payload must decode");
        assert_eq!(decoded.size(0).width.0, 1);
        assert_eq!(decoded.size(0).height.0, 1);
        // Same image, second call: served from the cache, not re-decoded.
        assert!(renderer.decoded_image(&data).is_some());
    }

    #[test]
    fn image_attachment_maps_cell_padding_and_texture_coords() {
        let data = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            4,
            2,
            vec![0u8; 4 * 2 * 4],
        )));
        let attachment = ImageCell::with_z_index(
            wezterm_term::image::TextureCoordinate::new_f32(0.0, 0.0),
            wezterm_term::image::TextureCoordinate::new_f32(1.0, 0.5),
            Arc::clone(&data),
            1,
            2,
            3,
            4,
            5,
            Some(7),
            None,
        );
        let renderer = Renderer::new(2.0, 8.0, 100, 100);
        let image = renderer.decoded_image(&data).expect("raw RGBA must decode");
        let pi = pane_image(&attachment, 3, 1, 8.0, 16.0, image, Arc::clone(&data));
        assert_eq!(pi.z, 1, "attachment z-index is carried through");
        // Cell 3 of an 8px grid starts at 24px *within the pane*; padding
        // shifts the quad. The pane origin is deliberately NOT folded in —
        // `paint_pane_images` adds it (see
        // `pane_image_stays_pane_relative_for_the_painter_to_offset`).
        assert_eq!(pi.x, 24.0 + 2.0);
        assert_eq!(pi.y, 16.0 + 3.0);
        assert_eq!(pi.w, 8.0 + 2.0 - 4.0);
        assert_eq!(pi.h, 16.0 + 3.0 - 5.0);
        // top_left/bottom_right select the top half of a 4x2 image.
        assert_eq!(pi.src, (0.0, 0.0, 4.0, 1.0));
    }

    #[test]
    fn pane_image_stays_pane_relative_for_the_painter_to_offset() {
        // Regression: the pane's content origin must be added exactly once, at
        // paint time. Baking a pane origin into the quad while
        // `paint_pane_images` added only the window origin (not the pane
        // origin) threw every quad a whole pane origin to the left, painting
        // the image over the sidebar. pane_image now takes no origin at all,
        // so its coordinates are the cell grid inside the pane — the same
        // frame `PaneText::rows` glyphs are shaped in.
        let data = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            4,
            4,
            vec![0u8; 4 * 4 * 4],
        )));
        let attachment = ImageCell::with_z_index(
            wezterm_term::image::TextureCoordinate::new_f32(0.0, 0.0),
            wezterm_term::image::TextureCoordinate::new_f32(1.0, 1.0),
            Arc::clone(&data),
            0,
            0,
            0,
            0,
            0,
            None,
            None,
        );
        let renderer = Renderer::new(2.0, 8.0, 100, 100);
        let image = renderer.decoded_image(&data).expect("raw RGBA must decode");
        let pi = pane_image(&attachment, 2, 3, 8.0, 16.0, image, Arc::clone(&data));
        // Column 2 row 3 of an 8x16 grid, no window and no pane offset in it.
        assert_eq!((pi.x, pi.y), (16.0, 48.0));
        assert_eq!((pi.w, pi.h), (8.0, 16.0));
        // The painter's placement is then pane origin + this, once:
        //   gpui x = window origin + (pane_origin.0 + pi.x) / scale
        // which is what `paint_pane_images` computes from `PaneText::origin`.
        let pane_origin = (494.0_f32, 39.0_f32);
        assert_eq!(pane_origin.0 + pi.x, 510.0);
        assert_eq!(pane_origin.1 + pi.y, 87.0);
    }

    #[test]
    fn image_quad_width_tracks_the_renderer_cell_geometry() {
        // A placement spanning one cell must come out `cell_w x cell_h` at the
        // renderer's own metrics — the same metrics `grid_size_for` divides the
        // pane rect by, and the ones `Session::resize` is handed (see
        // `App::cell_px`), so emulator-side placement and paint agree.
        let data = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            8,
            8,
            vec![0u8; 8 * 8 * 4],
        )));
        let attachment = ImageCell::with_z_index(
            wezterm_term::image::TextureCoordinate::new_f32(0.0, 0.0),
            wezterm_term::image::TextureCoordinate::new_f32(1.0, 1.0),
            Arc::clone(&data),
            0,
            0,
            0,
            0,
            0,
            None,
            None,
        );
        // Odd, fractional cell width, as the text system actually measures it.
        let renderer = Renderer::new(2.0, 9.25, 100, 100);
        let image = renderer.decoded_image(&data).expect("raw RGBA must decode");
        let pi = pane_image(&attachment, 0, 0, 9.25, 18.5, image, Arc::clone(&data));
        assert_eq!(pi.w, 9.25, "one cell wide at the renderer's cell width");
        assert_eq!(pi.h, 18.5, "one cell tall at the renderer's cell height");
    }

    #[test]
    fn image_hits_resolve_a_click_and_prefer_the_topmost_placement() {
        // Two placements over the same cell, stacked like kitty's `z=`.
        let bottom = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            4,
            4,
            vec![1u8; 4 * 4 * 4],
        )));
        let top = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            4,
            4,
            vec![2u8; 4 * 4 * 4],
        )));
        let cell = |data: &Arc<ImageData>, z: i32| {
            ImageCell::with_z_index(
                wezterm_term::image::TextureCoordinate::new_f32(0.0, 0.0),
                wezterm_term::image::TextureCoordinate::new_f32(1.0, 1.0),
                Arc::clone(data),
                z,
                0,
                0,
                0,
                0,
                None,
                None,
            )
        };
        let renderer = Renderer::new(2.0, 8.0, 100, 100);
        let below = pane_image(
            &cell(&bottom, 1),
            2,
            3,
            8.0,
            16.0,
            renderer.decoded_image(&bottom).expect("raw RGBA must decode"),
            Arc::clone(&bottom),
        );
        let above = pane_image(
            &cell(&top, 5),
            2,
            3,
            8.0,
            16.0,
            renderer.decoded_image(&top).expect("raw RGBA must decode"),
            Arc::clone(&top),
        );
        // The pane's content origin (physical px) is folded in exactly once.
        let hits = [
            ImageHit::from_pane((100.0, 40.0), &below),
            ImageHit::from_pane((100.0, 40.0), &above),
        ];
        assert_eq!((hits[0].x, hits[0].y), (116.0, 88.0));
        assert_eq!((hits[0].w, hits[0].h), (8.0, 16.0));
        // A click on a plain cell — left of column 2, past its right edge, or
        // a row above — is no image at all.
        assert!(image_at(&hits, 115.9, 90.0).is_none());
        assert!(image_at(&hits, 124.1, 90.0).is_none());
        assert!(image_at(&hits, 116.5, 87.9).is_none());
        // Inside the cell the higher z-index placement wins, and it is the
        // payload we hand to Preview that decides.
        let hit = image_at(&hits, 116.5, 90.0).expect("a click inside the cell hits an image");
        assert_eq!(hit.hash(), top.hash(), "the topmost placement wins");
        let only_below = [ImageHit::from_pane((100.0, 40.0), &below)];
        assert_eq!(
            image_at(&only_below, 116.5, 90.0).unwrap().hash(),
            bottom.hash(),
            "dropping the top placement reveals the one under it"
        );
    }
}
