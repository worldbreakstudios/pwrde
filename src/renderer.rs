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

use gpui::Hsla;
use termwiz::surface::CursorVisibility;
use wezterm_term::color::ColorPalette;

use crate::pages::{self, Page, Section};
use crate::palette::Palette;
use crate::picker::{ForkPicker, Picker, PickerLayout, PickerRow, ProfilePicker};
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
/// Corner radius of the floating tile cards, logical px.
const CARD_RADIUS: f32 = 12.0;
/// Corner radius of the sidebar's rounded rows, logical px: half the 28px
/// row height, so rows paint as fully-rounded iTerm2-style capsules. The
/// `pill` helper clamps it per-rect, so shorter pills stay capsules too.
const ROW_RADIUS: f32 = 14.0;

/// Blend `c` 40% toward white — brightens the hovered link color.
/// Truncate `text` to at most `max_chars` characters, ending in `…` when
/// anything was cut. Zero (or one) available column yields an empty string
/// rather than a lone ellipsis.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return String::new();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

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
    /// The subtle shadow under the sidebar's active group row.
    Soft,
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

    fn border(mut self, width: f32, color: Hsla) -> Self {
        self.border = width;
        self.border_color = color;
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

/// A pane's collapse caret: a chevron that rotates from pointing down
/// (expanded) to pointing right (collapsed). Quads can't rotate, so `main.rs`
/// paints it as a small filled gpui path. Position in physical px, angle in
/// radians.
#[derive(Clone, Copy)]
pub struct CaretSpec {
    pub cx: f32,
    pub cy: f32,
    /// Half-width of the chevron.
    pub size: f32,
    /// 0.0 points down; `-FRAC_PI_2` points right.
    pub angle: f32,
    pub color: Hsla,
}

/// Button labels of the confirm dialog (shared by layout and drawing).
const CONFIRM_CANCEL: &str = "Cancel";

/// The confirm dialog's rects, in physical px.
pub struct ConfirmLayout {
    pub panel: LayoutRect,
    pub cancel: LayoutRect,
    pub close: LayoutRect,
}

/// Geometry of the save-workspace modal (panel, both text fields, destination
/// rows), shared by drawing and `main.rs` hit-testing.
pub struct SaveLayout {
    pub panel: LayoutRect,
    pub name: LayoutRect,
    pub desc: LayoutRect,
    /// One rect per destination choice; empty while the fields are edited.
    pub rows: Vec<LayoutRect>,
}

/// Snapshot of the save-workspace modal for painting: field buffers, which
/// field holds the caret, and — once the modal reaches the destination stage —
/// the destination row labels with the selected index.
pub struct SaveModalView<'a> {
    pub name: &'a str,
    pub description: &'a str,
    /// 0 = Name focused, 1 = Description.
    pub field: usize,
    /// `Some((row_labels, selected))` in the destination stage, else `None`.
    pub dest: Option<(&'a [String], usize)>,
}

/// Per-frame page/navigation state the renderer needs beyond the workspaces:
/// which page is up, which settings section, the dot-strip animation
/// progresses (0..1 per page), the keyboard row being rebound, sidebar
/// sections, and any in-progress inline editors.
pub struct ChromeState<'a> {
    pub page: Page,
    pub section: Section,
    pub dot_anim: &'a [f32],
    /// Sidebar section definitions (Sessions page). Display order is derived
    /// via [`workspace::sidebar_rows`]; empty sections append at the end.
    pub sections: &'a [workspace::Section],
    /// In-progress section rename: `(section_id, buffer)`. When set, that
    /// section header paints the buffer + caret instead of emoji/name.
    pub editing_section: Option<(u64, &'a str)>,
    /// Cleanup page state (sidebar repo list; table is gpui-overlaid).
    pub cleanup: &'a crate::cleanup::Cleanup,
    /// Whether the experimental `features.notes` flag is on. Gates the Notes
    /// page out of the dot strip entirely when off.
    pub notes_enabled: bool,
    /// Registered notes vault names, in registration order (sidebar tabs).
    pub notes_vaults: &'a [String],
    /// Index into `notes_vaults` of the vault the Notes page is showing.
    pub notes_active_vault: usize,
    /// The active vault's markdown docs, as sidebar-row labels (relative paths).
    pub notes_doc_rels: &'a [String],
    /// Index into `notes_doc_rels` of the open doc, if any.
    pub notes_selected_doc: Option<usize>,
    /// Tools registered for this page/group, in ribbon slot order (resolved
    /// by `App::tools_for`). Empty hides the ribbon and its inset entirely.
    pub ribbon_tools: &'a [pages::Tool],
    /// Which right-side tool panel is open, if any (ribbon slot highlighted).
    /// Only painted while it appears in `ribbon_tools`.
    pub open_tool: Option<pages::Tool>,
    /// Width of the open tool panel in logical px.
    pub tool_panel_w: f32,
    /// Whether the tool panel floats over the tiles (vs. docking the edge).
    pub tool_panel_floating: bool,
    /// Physical-pixel cursor position for hover painting. `None` while any
    /// drag is active so hover highlights are suppressed mid-drag.
    pub cursor: Option<(f32, f32)>,
    /// In-progress search query for the settings sidebar search box.
    pub settings_query: &'a str,
    /// Whether the settings search box has keyboard focus.
    pub settings_search_focus: bool,
    /// Which polarity the Appearance page previews (pure UI state, decoupled
    /// from the applied appearance mode).
    pub preview_dark: bool,
    /// The Appearance dropdown whose options menu is open, if any.
    pub appearance_menu: Option<crate::pages::AppearanceDropdown>,
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
    /// Painted after labels, before picker_quads so modal overlays sit above.
    pub flyover_quads: Vec<Quad>,
    /// Flyover terminal text runs.
    pub flyover_panes: Vec<PaneText>,
    /// Flyover foreground fills (cursor, selection).
    pub flyover_fg_quads: Vec<Quad>,
    /// Flyover tab-strip labels.
    pub flyover_labels: Vec<LabelSpec>,
    /// Picker overlay fills painted over everything else (scrim, panel, rows).
    pub picker_quads: Vec<Quad>,
    /// Picker overlay labels, painted last.
    pub picker_labels: Vec<LabelSpec>,
    /// Collapse carets, painted as rotated chevron paths over the chrome.
    pub carets: Vec<CaretSpec>,
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
        self.cell_width = term_cell_width.round();
        self.cell_height = (term_font * scale * LINE_HEIGHT_FACTOR).round();
        self.chrome_cell_width = chrome_cell_width.round();
        self.chrome_cell_height = (chrome_font * scale * LINE_HEIGHT_FACTOR).round();
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
        resize_hover: Option<&workspace::ResizeHover>,
        link_hover: Option<(u64, usize, usize)>,
        picker: Option<&Picker>,
        fork: Option<&ForkPicker>,
        profile: Option<&ProfilePicker>,
        save: Option<&SaveModalView>,
        palette: Option<&Palette>,
        message: Option<&(String, bool)>,
        confirm: Option<(&str, &str)>,
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
        let (pane_bg, pane_ink, pane_ink_dim, pane_divider, pane_pill) = match scheme {
            Some(t) => (t.bg, (t.fg, 1.0), (t.fg, 0.55), (t.fg, 0.15), (t.fg, 0.12)),
            None => (
                th.term_bg,
                (th.text_bright, 1.0),
                (th.text_dim, 1.0),
                (th.card_divider, 1.0),
                ((255, 255, 255), 0.09),
            ),
        };
        let ws = &workspaces[active];
        let (width, height) = (self.width, self.height);
        // Right inset: the ribbon (when this page/group registers tools) plus
        // the open tool panel — but only when docked. A floating panel reserves
        // no tile width (it overlays the tiles), so the terminal fills the full
        // width behind it. Must match `App::right_w_for` so painting, PTY
        // sizing, and hit-testing agree on the tile area.
        let open_tool = chrome.open_tool.filter(|t| chrome.ribbon_tools.contains(t));
        let panel_w = if open_tool.is_some() && !chrome.tool_panel_floating {
            chrome.tool_panel_w
        } else {
            0.0
        };
        let right_w = if chrome.ribbon_tools.is_empty() {
            0.0
        } else {
            workspace::RIBBON_W + panel_w
        };
        let area = workspace::terminal_area(width, height, self.scale, sidebar_w, right_w);
        let empty = workspaces.len() == 1 && workspaces[0].is_empty();
        // Dividers aren't painted (the gap between cards shows the gradient);
        // they remain drag handles for hit-testing in `main.rs`. The empty
        // state draws no tile cards at all — just the centered CTA.
        let (tiles, dividers) = if empty {
            (Vec::new(), Vec::new())
        } else {
            workspace::layout_tiles(&ws.root, area, self.scale)
        };

        let mut bg_quads: Vec<Quad> = Vec::new();
        let mut fg_quads: Vec<Quad> = Vec::new();
        let mut labels: Vec<LabelSpec> = Vec::new();
        let mut panes: Vec<PaneText> = Vec::new();
        let mut carets: Vec<CaretSpec> = Vec::new();
        let mut hot: Vec<LayoutRect> = Vec::new();
        // Overlays are modal: while one is up only its elements hover or
        // register as hot; the chrome underneath goes inert (mirroring the
        // click routing in `main.rs`, which sends every click to the overlay).
        let overlay_open = confirm.is_some()
            || message.is_some()
            || save.is_some()
            || profile.is_some()
            || fork.is_some()
            || picker.is_some()
            || palette.is_some();
        let cur = if overlay_open { None } else { chrome.cursor };
        // Which axis each tile would collapse along (its parent split's dir);
        // `None` = root leaf, which shows no caret and cannot collapse.
        let collapse_axis_map: std::collections::HashMap<u64, Option<workspace::Dir>> =
            workspace::tile_collapse_axis(&ws.root).into_iter().collect();

        // ── Sidebar chrome (identical geometry on every page) ──────────
        // The window gradient is painted by `main.rs` before these quads;
        // the sidebar itself is transparent — its rounded rows float on it.
        // `sidebar_w == 0.0` means collapsed: skip all of it (the empty-state
        // CTA still paints — it is positioned off `terminal_area`).
        let collapsed = sidebar_w == 0.0;
        let row_r = (ROW_RADIUS * self.scale).round();
        let group_pad = (12.0 * self.scale).round();
        // Traffic lights are the native macOS buttons now (transparent titlebar),
        // so we no longer draw our own here.
        match chrome.page {
            Page::Notes if collapsed => {},
            Page::Notes => {
                // One combined sidebar, stacked top-to-bottom in the same rows
                // groups occupy on Sessions:
                //   [0, V)         vault rows (active highlighted)
                //   [V, V+D)       the active vault's doc rows (indented)
                //   V+D            an "＋ Add vault…" row
                // The hit-test in main.rs (`Page::Notes` mouse branch) walks the
                // same index ranges, so paint and clicks stay in lock-step.
                let n_vaults = chrome.notes_vaults.len();
                let n_docs = chrome.notes_doc_rels.len();

                // (row index, label, active?, indent?, dim-when-inactive?)
                let mut rows: Vec<(usize, String, bool, bool, bool)> = Vec::new();
                for (i, name) in chrome.notes_vaults.iter().enumerate() {
                    rows.push((i, name.clone(), i == chrome.notes_active_vault, false, false));
                }
                for (k, rel) in chrome.notes_doc_rels.iter().enumerate() {
                    let selected = chrome.notes_selected_doc == Some(k);
                    rows.push((n_vaults + k, rel.clone(), selected, true, true));
                }
                rows.push((n_vaults + n_docs, "+ Add vault".to_string(), false, false, true));

                for (i, text, active, indent, dim) in rows {
                    let tab = workspace::tab_rect(i, self.scale, sidebar_w);
                    if active {
                        bg_quads.push(self.pill(&tab, th, 0.78, row_r).shadow(Shadow::Soft));
                    } else if hover(cur, &tab) {
                        bg_quads.push(self.pill(&tab, th, 0.40, row_r));
                    }
                    hot.push(tab);
                    let indent_px = if indent { (14.0 * self.scale).round() } else { 0.0 };
                    let ink = if active || !dim { th.ink } else { th.ink_dim };
                    labels.push(LabelSpec {
                        text,
                        color: color(ink, 1.0),
                        left: tab.x + group_pad + indent_px,
                        top: (tab.y + (tab.h - self.chrome_cell_height) / 2.0).round(),
                        clip: LayoutRect { w: tab.w - group_pad - indent_px, ..tab },
                        size: None,
                    });
                }
            },
            // The Pull Requests page shares the Sessions sidebar (its list is
            // scoped to the active group's repo, so group switching applies).
            Page::Sessions | Page::PullRequests => {
                if !collapsed {
                    // Side-by-side "+ group" / "+ section" buttons below the titlebar.
                    let new_group = workspace::new_group_button(self.scale, sidebar_w);
                    let hov = hover(cur, &new_group);
                    bg_quads.push(
                        self.pill(&new_group, th, if hov { 0.85 } else { 0.55 }, row_r)
                            .shadow(if hov { Shadow::Soft } else { Shadow::None }),
                    );
                    hot.push(new_group);
                    labels.push(LabelSpec {
                        text: "+ group".into(),
                        color: color(th.ink_dim, 1.0),
                        left: (new_group.x + group_pad).round(),
                        top: (new_group.y + (new_group.h - self.chrome_cell_height) / 2.0).round(),
                        clip: new_group,
                        size: None,
                    });
                    let new_section = workspace::new_section_button(self.scale, sidebar_w);
                    let hov = hover(cur, &new_section);
                    bg_quads.push(
                        self.pill(&new_section, th, if hov { 0.85 } else { 0.55 }, row_r)
                            .shadow(if hov { Shadow::Soft } else { Shadow::None }),
                    );
                    hot.push(new_section);
                    labels.push(LabelSpec {
                        text: "+ section".into(),
                        color: color(th.ink_dim, 1.0),
                        left: (new_section.x + group_pad).round(),
                        top: (new_section.y + (new_section.h - self.chrome_cell_height) / 2.0).round(),
                        clip: new_section,
                        size: None,
                    });
                }
                if empty {
                    // Empty state: a centered CTA instead of group rows.
                    // Empty sections (if any) still render below the buttons.
                    let cta = workspace::empty_state_cta(width, height, self.scale, sidebar_w, right_w);
                    let hint = workspace::empty_state_hint(width, height, self.scale, sidebar_w, right_w);
                    let hov = hover(cur, &cta);
                    bg_quads.push(
                        self.pill(&cta, th, if hov { 0.85 } else { 0.62 }, row_r)
                            .shadow(Shadow::Soft),
                    );
                    hot.push(cta);
                    let cta_text = "New group";
                    let cta_w = cta_text.chars().count() as f32 * self.chrome_cell_width;
                    labels.push(LabelSpec {
                        text: cta_text.into(),
                        color: color(th.ink, 1.0),
                        left: (cta.x + ((cta.w - cta_w) / 2.0).max(0.0)).round(),
                        top: (cta.y + (cta.h - self.chrome_cell_height) / 2.0).round(),
                        clip: cta,
                        size: None,
                    });
                    let hint_text = "press ⇧⌘T";
                    let hint_w = hint_text.chars().count() as f32 * self.chrome_cell_width;
                    labels.push(LabelSpec {
                        text: hint_text.into(),
                        color: color(th.ink_dim, 0.9),
                        left: (hint.x + ((hint.w - hint_w) / 2.0).max(0.0)).round(),
                        top: (hint.y + (hint.h - self.chrome_cell_height) / 2.0).round(),
                        clip: hint,
                        size: None,
                    });
                }
                // Painted even in the empty state (under the CTA) so a just-
                // created section is visible before it gains members.
                if !collapsed {
                    self.paint_sidebar_rows(
                        workspaces,
                        active,
                        sidebar_w,
                        chrome,
                        th,
                        row_r,
                        group_pad,
                        cur,
                        &mut bg_quads,
                        &mut labels,
                        &mut hot,
                    );
                }
            },
            Page::Settings if collapsed => {},
            Page::Settings => {
                // Search box in the top slot: typing filters every section's
                // settings (the content card lists the matches). Focused it
                // takes the active-tab treatment; a quad caret trails the
                // query like the primary-command editor's.
                let search = workspace::settings_search_rect(self.scale, sidebar_w);
                let focused = chrome.settings_search_focus;
                if focused {
                    bg_quads.push(self.pill(&search, th, 0.78, row_r).shadow(Shadow::Soft));
                } else {
                    bg_quads.push(self.pill(&search, th, 0.40, row_r));
                }
                hot.push(search);
                let empty = chrome.settings_query.is_empty();
                let text_top = (search.y + (search.h - self.chrome_cell_height) / 2.0).round();
                if !empty || !focused {
                    labels.push(LabelSpec {
                        text: if empty { "Search settings".into() } else { chrome.settings_query.to_string() },
                        color: color(if empty { th.ink_dim } else { th.ink }, 1.0),
                        left: search.x + group_pad,
                        top: text_top,
                        clip: LayoutRect { w: search.w - group_pad, ..search },
                        size: None,
                    });
                }
                if focused {
                    let w = chrome.settings_query.chars().count() as f32 * self.chrome_cell_width;
                    let caret = LayoutRect {
                        x: (search.x + group_pad + w + if empty { 0.0 } else { 2.0 }).round(),
                        y: text_top,
                        w: (2.0 * self.scale).round().max(1.0),
                        h: self.chrome_cell_height,
                    };
                    bg_quads.push(self.px_rect(&caret, th.accent, 1.0, 0.0));
                }

                // Settings sections as sidebar tabs, shifted to slot i+1 to
                // make room for the search box at slot 0.
                for (i, section) in Section::ALL.iter().enumerate() {
                    let tab = workspace::tab_rect(i + 1, self.scale, sidebar_w);
                    let active_row = *section == chrome.section;
                    if active_row {
                        bg_quads.push(self.pill(&tab, th, 0.78, row_r).shadow(Shadow::Soft));
                    } else if hover(cur, &tab) {
                        // Hovered inactive tab: the active pill at a fraction
                        // of its strength, shadowless so it reads as "would
                        // select", not "selected".
                        bg_quads.push(self.pill(&tab, th, 0.40, row_r));
                    }
                    hot.push(tab);
                    labels.push(LabelSpec {
                        text: section.label().into(),
                        color: color(if active_row { th.ink } else { th.ink_dim }, 1.0),
                        left: tab.x + group_pad,
                        top: (tab.y + (tab.h - self.chrome_cell_height) / 2.0).round(),
                        clip: LayoutRect { w: tab.w - group_pad, ..tab },
                        size: None,
                    });
                }
            },
            Page::Cleanup if collapsed => {},
            Page::Cleanup => {
                // "All" plus one tab per repo with drop worktrees, in the same
                // rows the groups occupy on Sessions so the chrome reads as one.
                let repos = chrome.cleanup.repos();
                let filter = chrome.cleanup.repo_filter.as_deref();
                let mut tabs: Vec<(String, bool)> =
                    vec![("All".to_string(), filter.is_none())];
                for repo in &repos {
                    tabs.push((
                        format!("{} · {}", repo.display, repo.count),
                        filter == Some(repo.root.as_str()),
                    ));
                }
                for (i, (label, active_row)) in tabs.iter().enumerate() {
                    let tab = workspace::tab_rect(i, self.scale, sidebar_w);
                    if *active_row {
                        bg_quads.push(self.pill(&tab, th, 0.78, row_r).shadow(Shadow::Soft));
                    } else if hover(cur, &tab) {
                        bg_quads.push(self.pill(&tab, th, 0.40, row_r));
                    }
                    hot.push(tab);
                    labels.push(LabelSpec {
                        text: label.clone(),
                        color: color(if *active_row { th.ink } else { th.ink_dim }, 1.0),
                        left: tab.x + group_pad,
                        top: (tab.y + (tab.h - self.chrome_cell_height) / 2.0).round(),
                        clip: LayoutRect { w: tab.w - group_pad, ..tab },
                        size: None,
                    });
                }
            },
        }

        // ── Page-dot strip (bottom of the sidebar, every page) ─────────
        // Each slot crossfades between a subtle dot and the page's glyph as
        // its animation progress moves 0 → 1 (hovered or active page).
        if !collapsed {
            // Flag-gated pages (Notes) drop out of the strip entirely, so the
            // slots stay contiguous; the animation array is still indexed by
            // the page's stable global index.
            let pages = Page::visible(chrome.notes_enabled);
            let n_pages = pages.len();
            for (i, page) in pages.iter().enumerate() {
                let slot = workspace::page_slot_rect(i, n_pages, height, self.scale, sidebar_w);
                // The dot→glyph crossfade is the hover treatment here; hot
                // registration just adds the pointing hand.
                hot.push(slot);
                let p = chrome
                    .dot_anim
                    .get(page.index())
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                if p < 1.0 {
                    let d = (5.0 * self.scale).round().max(2.0);
                    let dot = LayoutRect {
                        x: (slot.x + (slot.w - d) / 2.0).round(),
                        y: (slot.y + (slot.h - d) / 2.0).round(),
                        w: d,
                        h: d,
                    };
                    bg_quads.push(self.px_rect(&dot, th.ink_dim, 0.45 * (1.0 - p), d / 2.0));
                }
                if p > 0.0 {
                    let glyph = page.glyph();
                    let gw = glyph.chars().count() as f32 * self.chrome_cell_width;
                    labels.push(LabelSpec {
                        text: glyph.into(),
                        color: color(th.ink, p),
                        left: (slot.x + (slot.w - gw) / 2.0).round(),
                        // Glyphs may be a hair wider than the slot ("<>"): allow
                        // a small clip overhang so they aren't shaved.
                        top: (slot.y + (slot.h - self.chrome_cell_height) / 2.0).round(),
                        clip: slot.inflate((4.0 * self.scale).round()),
                        size: None,
                    });
                }
            }
        }

        let card_r = (CARD_RADIUS * self.scale).round();

        // ── Tool ribbon + panel (right edge, when tools are registered) ──
        // Like the sidebar, the ribbon strip is transparent on the window
        // gradient: only the slot pills and the panel card paint.
        for (i, tool) in chrome.ribbon_tools.iter().enumerate() {
            let slot = workspace::ribbon_slot_rect(i, width, self.scale);
            let active = open_tool == Some(*tool);
            let hov = hover(cur, &slot);
            if active || hov {
                let m = (4.0 * self.scale).round();
                let pill = LayoutRect {
                    x: slot.x + m,
                    y: slot.y + m,
                    w: (slot.w - 2.0 * m).max(0.0),
                    h: (slot.h - 2.0 * m).max(0.0),
                };
                bg_quads.push(
                    self.pill(&pill, th, if active { 0.85 } else { 0.40 }, row_r)
                        .shadow(if active { Shadow::Soft } else { Shadow::None }),
                );
            }
            let ink = if active || hov { th.ink } else { th.ink_dim };
            self.ribbon_icon(*tool, &slot, ink, &mut bg_quads, &mut carets);
            hot.push(slot);
        }
        // The PR and Local-diff panels are real gpui element trees drawn over
        // the canvas (see `pr_ui` / `local_diff_ui`), so nothing is painted for
        // them here. Launch has no element overlay yet, so it keeps the
        // canvas-painted "coming soon" placeholder card.
        if let Some(tool @ pages::Tool::Launch) = open_tool {
            let panel = workspace::tool_panel(width, height, self.scale, chrome.tool_panel_w, chrome.tool_panel_floating);
            let pad = (14.0 * self.scale).round();
            // Chrome-polarity card like Settings/Cleanup, not a dark tile.
            bg_quads.push(self.px_rect(&panel, th.card, 1.0, card_r).shadow(Shadow::Card));
            labels.push(LabelSpec {
                text: tool.title().into(),
                color: color(th.ink, 1.0),
                left: panel.x + pad,
                top: panel.y + pad,
                clip: panel,
                size: None,
            });
            labels.push(LabelSpec {
                text: format!("{} view coming soon", tool.title()),
                color: color(th.ink_dim, 1.0),
                left: panel.x + pad,
                top: (panel.y + pad + 2.0 * self.chrome_cell_height).round(),
                clip: panel,
                size: None,
            });
        }

        if chrome.page == Page::Settings
            && chrome.section == Section::Appearance
            && chrome.settings_query.is_empty()
        {
            // ── Settings → Appearance: canvas-painted for the WYSIWYG
            // previews; every other Settings section is the gpui overlay in
            // settings_ui (which also owns search-results mode).
            self.appearance_page(&area, chrome, cur, &mut bg_quads, &mut labels, &mut hot);
        } else if chrome.page == Page::Cleanup
            || chrome.page == Page::Settings
            || chrome.page == Page::PullRequests
            || chrome.page == Page::Notes
        {
            // Content is a gpui overlay (cleanup_ui / settings_ui / pr_ui) —
            // the canvas paints the sidebar only.
        } else {
            let hair = (1.0 * self.scale).round().max(1.0);
            for (id, rect) in &tiles {
                // Each tile is a floating dark card: rounded, shadowed, with its
                // tab strip inside the card above a hairline divider.
                bg_quads.push(self.px_rect(rect, pane_bg, 1.0, card_r).shadow(Shadow::Card));
                let axis = collapse_axis_map.get(id).copied().flatten();
                let Some(tile) = ws.root.find_tile(*id) else { continue };
                // Collapsed (or mid-animation) panes hide their content; a
                // sideways-collapsed pane is a bare strip showing only the caret.
                let collapsing = axis.is_some() && (tile.collapsed || tile.collapse_anim > 0.0);
                let side_strip = axis == Some(workspace::Dir::Row) && collapsing;
                let strip = workspace::tab_strip_rect(area, rect, self.scale, sidebar_w);
                let bar = workspace::tile_tab_bar(&strip, self.scale);
                if !collapsing {
                    let divider =
                        LayoutRect { x: rect.x, y: bar.y + bar.h - hair, w: rect.w, h: hair };
                    bg_quads.push(self.px_rect(&divider, pane_divider.0, pane_divider.1, 0.0));
                }
                if !side_strip {
                    // Active tab: a subtle rounded pill inside the strip (white
                    // works on every theme's dark card).
                    let tr = workspace::tile_tab_rect(
                        &strip,
                        tile.active,
                        tile.tabs.len(),
                        self.scale,
                        axis.is_some(),
                    );
                    let m = (4.0 * self.scale).round();
                    let pill = LayoutRect {
                        x: tr.x + m,
                        y: tr.y + m,
                        w: (tr.w - 2.0 * m).max(0.0),
                        h: (tr.h - 2.0 * m).max(0.0),
                    };
                    bg_quads.push(self.px_rect(&pill, pane_pill.0, pane_pill.1, (7.0 * self.scale).round()));
                } else {
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
                // The caret chevron, rotating down (expanded) → right
                // (collapsed) with the pane's animation progress.
                if has_caret {
                    let cr = workspace::tile_caret_rect(rect, self.scale);
                    // Hovered caret brightens to full ink; a side strip is one
                    // whole-card target, so the caret isn't hot on its own.
                    let caret_hov = !side_strip && hover(cur, &cr);
                    carets.push(CaretSpec {
                        cx: cr.x + cr.w / 2.0,
                        cy: cr.y + cr.h / 2.0,
                        size: (4.5 * self.scale).round(),
                        angle: -std::f32::consts::FRAC_PI_2
                            * tile.collapse_anim.clamp(0.0, 1.0),
                        color: if caret_hov {
                            color(pane_ink.0, pane_ink.1)
                        } else {
                            color(pane_ink_dim.0, pane_ink_dim.1)
                        },
                    });
                    if !side_strip {
                        hot.push(cr);
                    }
                    // While collapsed, any tab's unread dot bubbles up to a
                    // badge on the chevron so hidden panes can still call
                    // for attention.
                    if tile.collapsed && tile.tabs.iter().any(|t| t.unread) {
                        let ds = (6.0 * self.scale).round();
                        let pad = (3.0 * self.scale).round();
                        let dot = LayoutRect {
                            x: cr.x + cr.w - ds - pad,
                            y: cr.y + pad,
                            w: ds,
                            h: ds,
                        };
                        fg_quads.push(self.px_rect(&dot, th.accent, 1.0, ds / 2.0));
                    }
                }
                // Collapsed (or mid-animation) panes paint no terminal
                // content — the card is just its tab strip.
                let content = workspace::tile_content(rect, self.scale);
                let origin = self.content_origin(&content);
                if !collapsing
                    && let Some(session) = tile.tabs.get(tile.active).map(|t| &t.session)
                {
                    let draw_cursor = Some(*id) == focused_tile
                        && picker.is_none()
                        && fork.is_none()
                        && palette.is_none()
                        && message.is_none()
                        && confirm.is_none();
                    let tile_hover = link_hover
                        .filter(|(hid, _, _)| *hid == *id)
                        .map(|(_, col, row)| (col, row));
                    let rows = self.snapshot_pane(
                        session,
                        &term_palette,
                        origin,
                        draw_cursor,
                        tile_hover,
                        &mut bg_quads,
                        &mut fg_quads,
                    );
                    panes.push(PaneText { origin, rows });
                    self.selection_rects(session, origin, &mut fg_quads);
                }
                // A sideways strip has no room for the strip's labels: only
                // the caret shows. A stacked collapse keeps its tab labels
                // (clicking one focuses + expands).
                if side_strip {
                    continue;
                }

                // Tab labels for this tile's tab strip.
                let strip = workspace::tab_strip_rect(area, rect, self.scale, sidebar_w);
                let tab_text_pad = (8.0 * self.scale).round();
                for (ti, tab) in tile.tabs.iter().enumerate() {
                    let tr = workspace::tile_tab_rect(&strip, ti, tile.tabs.len(), self.scale, has_caret);
                    let close =
                        workspace::tile_tab_close_rect(&strip, ti, tile.tabs.len(), self.scale, has_caret);
                    let close_hov = hover(cur, &close);
                    if ti != tile.active && hover(cur, &tr) && !close_hov {
                        // Hovered inactive tab: the active pill's geometry at
                        // about half strength, so it previews without claiming
                        // to be selected.
                        let m = (4.0 * self.scale).round();
                        let pill = LayoutRect {
                            x: tr.x + m,
                            y: tr.y + m,
                            w: (tr.w - 2.0 * m).max(0.0),
                            h: (tr.h - 2.0 * m).max(0.0),
                        };
                        bg_quads.push(self.px_rect(
                            &pill,
                            pane_pill.0,
                            pane_pill.1 * 0.55,
                            (7.0 * self.scale).round(),
                        ));
                    }
                    if close_hov {
                        // Browser-tab style: a small rounded chip behind the ×.
                        let inset = (3.0 * self.scale).round();
                        let chip = LayoutRect {
                            x: close.x + inset,
                            y: close.y + inset,
                            w: (close.w - 2.0 * inset).max(0.0),
                            h: (close.h - 2.0 * inset).max(0.0),
                        };
                        bg_quads.push(self.px_rect(
                            &chip,
                            pane_pill.0,
                            (pane_pill.1 * 2.0).min(1.0),
                            (4.0 * self.scale).round(),
                        ));
                    }
                    // The close rect is hot after its tab so reverse iteration
                    // (topmost wins) resolves × over the tab it sits in.
                    hot.push(tr);
                    hot.push(close);
                    let title = tab.session.title();
                    let text = if title.is_empty() { "shell".to_string() } else { title };
                    // Unread: an accent dot before the title, which shifts
                    // right to make room (the clip's right edge is unchanged).
                    let mut text_left = tr.x + tab_text_pad;
                    if tab.unread {
                        let ds = (6.0 * self.scale).round();
                        let dot = LayoutRect {
                            x: text_left,
                            y: (tr.y + (tr.h - ds) / 2.0).round(),
                            w: ds,
                            h: ds,
                        };
                        fg_quads.push(self.px_rect(&dot, th.accent, 1.0, ds / 2.0));
                        text_left += ds + (5.0 * self.scale).round();
                    }
                    labels.push(LabelSpec {
                        text,
                        color: if ti == tile.active {
                            color(pane_ink.0, pane_ink.1)
                        } else {
                            color(pane_ink_dim.0, pane_ink_dim.1)
                        },
                        left: text_left,
                        top: (tr.y + (tr.h - self.chrome_cell_height) / 2.0).round(),
                        clip: LayoutRect {
                            w: (close.x - tr.x - tab_text_pad).max(0.0),
                            ..tr
                        },
                        size: None,
                    });
                    labels.push(LabelSpec {
                        text: "×".to_string(),
                        color: if close_hov {
                            color(pane_ink.0, pane_ink.1)
                        } else {
                            color(pane_ink_dim.0, pane_ink_dim.1)
                        },
                        left: close.x + ((close.w - self.chrome_cell_width) / 2.0).round(),
                        top: (tr.y + (tr.h - self.chrome_cell_height) / 2.0).round(),
                        clip: tr,
                        size: None,
                    });
                }
            }

            // Focused-tile border (only interesting with multiple tiles): a
            // rounded accent outline hugging the card's corner radius.
            if tiles.len() > 1
                && let Some((_, rect)) = tiles.iter().find(|(id, _)| Some(*id) == focused_tile)
            {
                let t = (1.5 * self.scale).round();
                fg_quads.push(
                    self.px_rect(rect, th.term_bg, 0.0, card_r).border(t, color(th.accent, 0.9)),
                );
            }

            // Drag-drop target hint (a translucent accent overlay).
            if let Some(hint) = drop_hint {
                fg_quads.push(self.px_rect(&hint, th.accent, 0.3, row_r));
            }
        }

        // Resize-handle hover: slim ink line + centered grip pill so the drag
        // target reads before the press. Geometry lives here; main only paints.
        if let Some(hover) = resize_hover {
            match hover {
                workspace::ResizeHover::Sidebar => {
                    let sb = (sidebar_w * self.scale).round();
                    let line_w = (2.0 * self.scale).round().max(1.0);
                    let top = workspace::titlebar(self.scale, sidebar_w).h;
                    // Same bottom margin the tile area uses (AREA_PAD via terminal_area).
                    let bottom = area.y + area.h;
                    let line = LayoutRect {
                        x: sb - line_w / 2.0,
                        y: top,
                        w: line_w,
                        h: (bottom - top).max(0.0),
                    };
                    self.push_resize_grip(&mut bg_quads, &line, true, th.ink);
                }
                workspace::ResizeHover::Divider { path, .. } => {
                    // Empty state has no dividers; find is a no-op then.
                    if let Some(d) = dividers.iter().find(|d| d.path == *path) {
                        let vertical = d.dir == workspace::Dir::Row;
                        self.push_resize_grip(&mut bg_quads, &d.rect, vertical, th.ink);
                    }
                }
                workspace::ResizeHover::ToolPanel => {
                    if open_tool.is_some() {
                        let panel = workspace::tool_panel(
                            width,
                            height,
                            self.scale,
                            chrome.tool_panel_w,
                            chrome.tool_panel_floating,
                        );
                        let line_w = (2.0 * self.scale).round().max(1.0);
                        let line = LayoutRect {
                            x: panel.x - line_w / 2.0,
                            y: panel.y,
                            w: line_w,
                            h: panel.h,
                        };
                        self.push_resize_grip(&mut bg_quads, &line, true, th.ink);
                    }
                }
            }
        }

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

        // ── Picker overlay (over everything) ───────────────────────────
        // A modal overlay owns the frame's interactivity: the chrome hot
        // rects collected above go inert, and only overlay elements register.
        if overlay_open {
            hot.clear();
        }
        let mut picker_quads: Vec<Quad> = Vec::new();
        let mut picker_labels: Vec<LabelSpec> = Vec::new();
        if let Some((text, accept)) = confirm {
            picker_labels =
                self.confirm_overlay(text, accept, chrome.cursor, &mut picker_quads, &mut hot);
        } else if let Some(s) = save {
            let layout = self.save_layout(s.dest.map_or(0, |(rows, _)| rows.len()));
            picker_labels =
                self.save_overlay(s, &layout, chrome.cursor, &mut picker_quads, &mut hot);
        } else if let Some(pp) = profile {
            let layout =
                PickerLayout::compute(width, height, self.scale, pp.rows.len(), pp.selected);
            picker_labels =
                self.profile_overlay(pp, &layout, chrome.cursor, &mut picker_quads, &mut hot);
        } else if let Some(p) = picker {
            let layout = PickerLayout::compute(width, height, self.scale, p.rows.len(), p.selected);
            picker_labels =
                self.picker_overlay(p, &layout, chrome.cursor, &mut picker_quads, &mut hot);
        } else if let Some(f) = fork {
            picker_labels = self.fork_overlay(f, chrome.cursor, &mut picker_quads, &mut hot);
        } else if let Some(pal) = palette {
            let layout =
                PickerLayout::compute(width, height, self.scale, pal.rows.len(), pal.selected);
            picker_labels =
                self.palette_overlay(pal, &layout, chrome.cursor, &mut picker_quads, &mut hot);
        } else if let Some((text, _)) = message {
            picker_labels = self.message_overlay(text, &mut picker_quads);
        } else if chrome.page == Page::Settings && chrome.section == Section::Appearance {
            // The Appearance dropdown menu floats over the settings card in
            // the picker layers, but is not modal — the page stays live.
            if let Some(menu) = chrome.appearance_menu {
                picker_labels = self.appearance_menu_overlay(
                    &area,
                    menu,
                    chrome.cursor,
                    &mut picker_quads,
                    &mut hot,
                );
            }
        }

        Frame {
            bg_quads,
            panes,
            fg_quads,
            labels,
            flyover_quads: Vec::new(),
            flyover_panes: Vec::new(),
            flyover_fg_quads: Vec::new(),
            flyover_labels: Vec::new(),
            picker_quads,
            picker_labels,
            carets,
            hot,
        }
    }

    /// Paint Sessions sidebar rows (section headers + group cards) using the
    /// pure `sidebar_rows` / `sidebar_row_rect` geometry so hit-testing and
    /// drop resolution can share the same layout.
    fn paint_sidebar_rows(
        &self,
        workspaces: &[Workspace],
        active: usize,
        sidebar_w: f32,
        chrome: &ChromeState,
        th: &Theme,
        row_r: f32,
        group_pad: f32,
        cur: Option<(f32, f32)>,
        bg_quads: &mut Vec<Quad>,
        labels: &mut Vec<LabelSpec>,
        hot: &mut Vec<LayoutRect>,
    ) {
        let rows = workspace::sidebar_rows(workspaces, chrome.sections);
        let active_row = workspace::active_row_index(&rows, workspaces, chrome.sections, active);
        let header_size = self.chrome_font_size() * 0.9;

        for (i, row) in rows.iter().enumerate() {
            let rect = workspace::sidebar_row_rect(&rows, i, workspaces, self.scale, sidebar_w);
            let is_active_row = active_row == Some(i);
            match *row {
                workspace::SidebarRow::SectionHeader { section_idx } => {
                    let Some(sec) = chrome.sections.get(section_idx) else {
                        continue;
                    };
                    let editing = chrome
                        .editing_section
                        .filter(|(id, _)| *id == sec.id)
                        .map(|(_, buf)| buf);
                    if is_active_row {
                        bg_quads
                            .push(self.pill(&rect, th, 0.78, row_r).shadow(Shadow::Soft));
                    } else if editing.is_none() && hover(cur, &rect) {
                        bg_quads.push(self.pill(&rect, th, 0.40, row_r));
                    }
                    hot.push(rect);
                    if let Some(buf) = editing {
                        bg_quads.push(self.px_rect(&rect, th.accent, 0.18, row_r));
                        let caret_w = (2.0 * self.scale).round().max(1.0);
                        let text_left = (rect.x + group_pad).round();
                        let top =
                            (rect.y + (rect.h - self.chrome_cell_height) / 2.0).round();
                        labels.push(LabelSpec {
                            text: buf.to_string(),
                            color: color(th.ink, 1.0),
                            left: text_left,
                            top,
                            clip: LayoutRect {
                                w: (rect.w - group_pad - caret_w - 2.0).max(0.0),
                                ..rect
                            },
                            size: Some(header_size),
                        });
                        let w = buf.chars().count() as f32 * self.chrome_cell_width * 0.9;
                        let caret = LayoutRect {
                            x: (text_left + w).round(),
                            y: top,
                            w: caret_w,
                            h: self.chrome_cell_height,
                        };
                        bg_quads.push(self.px_rect(&caret, th.accent, 1.0, 0.0));
                    } else {
                        let chevron = if sec.collapsed { "▸" } else { "▾" };
                        let member_count = workspaces
                            .iter()
                            .filter(|w| w.section == Some(sec.id))
                            .count();
                        let mut text = String::new();
                        text.push_str(chevron);
                        text.push(' ');
                        if !sec.emoji.is_empty() {
                            text.push_str(&sec.emoji);
                            text.push(' ');
                        }
                        text.push_str(&sec.name);
                        if sec.collapsed && member_count > 0 {
                            text.push_str(&format!(" · {member_count}"));
                        }
                        // Reserve the right gutter for the delete-section
                        // button so the label never reflows when it appears.
                        let del = workspace::section_delete_rect(&rect, self.scale);
                        let gap = (4.0 * self.scale).round();
                        labels.push(LabelSpec {
                            text,
                            color: color(
                                if is_active_row { th.ink } else { th.ink_dim },
                                1.0,
                            ),
                            left: (rect.x + group_pad).round(),
                            top: (rect.y + (rect.h - self.chrome_cell_height) / 2.0).round(),
                            clip: LayoutRect {
                                w: (del.x - rect.x - gap).max(0.0),
                                ..rect
                            },
                            size: Some(header_size),
                        });
                        // Delete-section button, revealed on row hover: a
                        // rounded chip behind an × (mirrors the tab close).
                        if hover(cur, &rect) {
                            let del_hov = hover(cur, &del);
                            let inset = (3.0 * self.scale).round();
                            let chip = LayoutRect {
                                x: del.x + inset,
                                y: del.y + inset,
                                w: (del.w - 2.0 * inset).max(0.0),
                                h: (del.h - 2.0 * inset).max(0.0),
                            };
                            bg_quads.push(self.px_rect(
                                &chip,
                                th.card,
                                if del_hov { 1.0 } else { 0.5 },
                                (4.0 * self.scale).round(),
                            ));
                            labels.push(LabelSpec {
                                text: "×".to_string(),
                                color: color(
                                    if del_hov { th.ink } else { th.ink_dim },
                                    1.0,
                                ),
                                left: del.x
                                    + ((del.w - self.chrome_cell_width) / 2.0).round(),
                                top: (rect.y + (rect.h - self.chrome_cell_height) / 2.0)
                                    .round(),
                                clip: rect,
                                size: Some(header_size),
                            });
                        }
                        // Group unread bubbles up: if any member workspace has
                        // an unread tab, light an accent dot on the section
                        // header (regardless of collapsed/expanded state).
                        let section_has_unread = workspaces
                            .iter()
                            .filter(|w| w.section == Some(sec.id))
                            .any(|w| w.any_unread());
                        if section_has_unread {
                            let ds = (7.0 * self.scale).round();
                            let dot = LayoutRect {
                                x: (rect.x + (group_pad - ds) / 2.0).round(),
                                y: (rect.y + (rect.h - ds) / 2.0).round(),
                                w: ds,
                                h: ds,
                            };
                            bg_quads.push(self.px_rect(&dot, th.accent, 1.0, ds / 2.0));
                        }
                    }
                }
                workspace::SidebarRow::Group { ws_idx } => {
                    let Some(ws_item) = workspaces.get(ws_idx) else {
                        continue;
                    };
                    if is_active_row {
                        bg_quads
                            .push(self.pill(&rect, th, 0.78, row_r).shadow(Shadow::Soft));
                    } else if hover(cur, &rect) {
                        bg_quads.push(self.pill(&rect, th, 0.40, row_r));
                    }
                    hot.push(rect);
                    let clip_w = rect.w - group_pad;
                    // Unread (any unread tab in the group lights the dot):
                    // accent dot in the left padding gutter, centered on the
                    // row; text stays put so rows keep alignment.
                    if ws_item.any_unread() {
                        let ds = (7.0 * self.scale).round();
                        let dot = LayoutRect {
                            x: (rect.x + (group_pad - ds) / 2.0).round(),
                            y: (rect.y + (rect.h - ds) / 2.0).round(),
                            w: ds,
                            h: ds,
                        };
                        bg_quads.push(self.px_rect(&dot, th.accent, 1.0, ds / 2.0));
                    }
                    labels.push(LabelSpec {
                        text: ws_item.title(),
                        color: color(
                            if is_active_row { th.ink } else { th.ink_dim },
                            1.0,
                        ),
                        left: rect.x + group_pad,
                        top: (rect.y + (rect.h - self.chrome_cell_height) / 2.0).round(),
                        clip: LayoutRect { w: clip_w, ..rect },
                        size: None,
                    });
                }
            }
        }
    }

    /// The Settings → Appearance page, still canvas-painted: the other
    /// Settings sections render as the rcn element tree in `settings_ui`, but
    /// this page's WYSIWYG theme/terminal preview cards live here so they keep
    /// their original pixel-exact look. Geometry comes from the
    /// `workspace::appearance_*` helpers so `main.rs` hit-tests the same
    /// pixels.
    fn appearance_page(
        &self,
        area: &LayoutRect,
        chrome: &ChromeState,
        cur: Option<(f32, f32)>,
        bg_quads: &mut Vec<Quad>,
        labels: &mut Vec<LabelSpec>,
        hot: &mut Vec<LayoutRect>,
    ) {
        let th = self.theme();
        let scale = self.scale;
        let pad = (14.0 * scale).round();
        // Like the Cleanup page, the card follows the chrome polarity (white
        // in light themes, raised dark in dark ones) rather than the
        // always-dark terminal fill, so it reads with ink like the sidebar.
        bg_quads.push(self.px_rect(area, th.card, 1.0, (CARD_RADIUS * scale).round()).shadow(Shadow::Card));

        let header_h = (workspace::SETTINGS_HEADER_H * scale).round();
        labels.push(LabelSpec {
            text: chrome.section.label().into(),
            color: color(th.ink, 1.0),
            left: area.x + pad,
            top: (area.y + (header_h - self.chrome_cell_height) / 2.0).round(),
            clip: *area,
            size: None,
        });

        // Rows that would spill past the card bottom are dropped, not clipped
        // mid-glyph.
        let fits = |row: &LayoutRect| row.y + row.h <= area.y + area.h - pad;
        let mid = |row: &LayoutRect| (row.y + (row.h - self.chrome_cell_height) / 2.0).round();

        let mode = crate::theme::mode();
        let preview_dark = chrome.preview_dark;
        let small = self.chrome_font_size() * 0.85;
        let small_cw = self.chrome_cell_width * 0.85;
        let chip = (9.0 * scale).round();
        let chip_gap = (3.0 * scale).round();
        let ipad = (10.0 * scale).round();

        // ── Header: mode segments + preview polarity toggle ──
        let header = workspace::appearance_header_row(area, scale);
        if fits(&header) {
            labels.push(LabelSpec {
                text: "Mode".into(),
                color: color(th.ink, 1.0),
                left: header.x,
                top: mid(&header),
                clip: header,
                size: None,
            });
            let mode_row = workspace::appearance_mode_row(area, self.chrome_cell_width, scale);
            for (i, m) in crate::theme::Mode::ALL.iter().enumerate() {
                let seg = workspace::mode_segment_rect(&mode_row, i, self.chrome_cell_width, scale);
                let on = *m == mode;
                let hov = !on && hover(cur, &seg);
                bg_quads.push(self.px_rect(
                    &seg,
                    if on { th.accent } else { th.ink },
                    if on {
                        0.9
                    } else if hov {
                        0.12
                    } else {
                        0.06
                    },
                    seg.h / 2.0,
                ));
                hot.push(seg);
                let lw = m.label().chars().count() as f32 * self.chrome_cell_width;
                labels.push(LabelSpec {
                    text: m.label().into(),
                    color: color(if on { (255, 255, 255) } else { th.ink_dim }, 1.0),
                    left: (seg.x + (seg.w - lw) / 2.0).round(),
                    top: mid(&header),
                    clip: seg,
                    size: None,
                });
            }
            // "Preview" caption + the Light/Dark toggle it names,
            // controlling which polarity the cards below show.
            let seg0 = workspace::preview_segment_rect(&header, 0, self.chrome_cell_width, scale);
            let cap_w = "Preview".chars().count() as f32 * self.chrome_cell_width;
            labels.push(LabelSpec {
                text: "Preview".into(),
                color: color(th.ink_dim, 1.0),
                left: (seg0.x - cap_w - (14.0 * scale).round()).round(),
                top: mid(&header),
                clip: header,
                size: None,
            });
            for (i, name) in ["Light", "Dark"].iter().enumerate() {
                let seg = workspace::preview_segment_rect(&header, i, self.chrome_cell_width, scale);
                let on = (i == 1) == preview_dark;
                let hov = !on && hover(cur, &seg);
                bg_quads.push(self.px_rect(
                    &seg,
                    if on { th.accent } else { th.ink },
                    if on {
                        0.9
                    } else if hov {
                        0.12
                    } else {
                        0.06
                    },
                    seg.h / 2.0,
                ));
                hot.push(seg);
                let lw = name.chars().count() as f32 * self.chrome_cell_width;
                labels.push(LabelSpec {
                    text: (*name).into(),
                    color: color(if on { (255, 255, 255) } else { th.ink_dim }, 1.0),
                    left: (seg.x + (seg.w - lw) / 2.0).round(),
                    top: mid(&header),
                    clip: seg,
                    size: None,
                });
            }
        }

        // ── Column captions ──
        for (col, caption) in [(0usize, "App Theme"), (1, "Terminal Colors")] {
            let column = workspace::appearance_column(area, col, scale);
            let cap = LayoutRect {
                h: (workspace::APPEARANCE_CAPTION_H * scale).round(),
                ..column
            };
            if fits(&cap) {
                labels.push(LabelSpec {
                    text: caption.into(),
                    color: color(th.accent, 1.0),
                    left: column.x,
                    top: (cap.y + (cap.h - self.chrome_cell_height) / 2.0).round(),
                    clip: cap,
                    size: Some(small),
                });
            }
        }

        // ── The four dropdown fields ──
        for d in pages::AppearanceDropdown::ALL {
            let (col, field) = d.grid();
            let frect = workspace::appearance_dropdown_rect(area, col, field, scale);
            let pill = workspace::appearance_dropdown_pill(area, col, field, scale);
            if !fits(&pill) {
                continue;
            }
            labels.push(LabelSpec {
                text: d.label().into(),
                color: color(th.ink_dim, 1.0),
                left: frect.x + (2.0 * scale).round(),
                top: frect.y,
                clip: frect,
                size: Some(small),
            });
            // The slot the preview currently reflects carries the
            // accent tint, mirroring the picked style elsewhere.
            let live = d.dark() == preview_dark;
            let hov = hover(cur, &pill);
            bg_quads.push(self.px_rect(
                &pill,
                if live { th.accent } else { th.ink },
                if live { 0.14 } else { 0.06 } + if hov { 0.06 } else { 0.0 },
                (7.0 * scale).round(),
            ));
            hot.push(pill);
            let (swatches, name): ([(u8, u8, u8); 3], &str) = match d {
                pages::AppearanceDropdown::ThemeLight
                | pages::AppearanceDropdown::ThemeDark => {
                    let t = crate::theme::selected(d.dark());
                    ([t.gradient_from, t.card, t.accent], t.label)
                },
                pages::AppearanceDropdown::TermLight
                | pages::AppearanceDropdown::TermDark => {
                    let sel = crate::term_theme::selected(d.dark());
                    let (_, _, ansi) = crate::term_theme::preview_colors(sel, th.term_bg);
                    ([ansi[1], ansi[2], ansi[4]], sel.map_or("Default", |t| t.label))
                },
            };
            let cy = (pill.y + (pill.h - chip) / 2.0).round();
            for (i, c) in swatches.iter().enumerate() {
                let r = LayoutRect {
                    x: (pill.x + ipad + i as f32 * (chip + chip_gap)).round(),
                    y: cy,
                    w: chip,
                    h: chip,
                };
                bg_quads.push(self.px_rect(&r, *c, 1.0, (3.0 * scale).round()));
            }
            let text_left =
                (pill.x + ipad + 3.0 * (chip + chip_gap) + (6.0 * scale)).round();
            let pmid = (pill.y + (pill.h - self.chrome_cell_height) / 2.0).round();
            labels.push(LabelSpec {
                text: name.into(),
                color: color(th.ink, 1.0),
                left: text_left,
                top: pmid,
                clip: LayoutRect {
                    x: text_left,
                    w: (pill.x + pill.w - ipad - self.chrome_cell_width - text_left).max(0.0),
                    ..pill
                },
                size: None,
            });
            labels.push(LabelSpec {
                text: "▾".into(),
                color: color(th.ink_dim, 1.0),
                left: (pill.x + pill.w - ipad - self.chrome_cell_width).round(),
                top: pmid,
                clip: pill,
                size: None,
            });
        }

        // ── App preview: a miniature of the chrome under the
        // previewed polarity's selected theme ──
        let pt = crate::theme::selected(preview_dark);
        let pv = workspace::appearance_preview_card(area, 0, scale);
        if fits(&pv) && pv.h > 80.0 * scale {
            let r = (10.0 * scale).round();
            bg_quads.push(self.px_rect(&pv, pt.gradient_from, 1.0, r).shadow(Shadow::Soft));
            // Title row: traffic dots + theme name.
            let dot = (8.0 * scale).round();
            let dgap = (5.0 * scale).round();
            let title_h = (24.0 * scale).round();
            for i in 0..3 {
                let drect = LayoutRect {
                    x: (pv.x + ipad + i as f32 * (dot + dgap)).round(),
                    y: (pv.y + (title_h - dot) / 2.0).round(),
                    w: dot,
                    h: dot,
                };
                bg_quads.push(self.px_rect(&drect, pt.ink, 0.25, dot / 2.0));
            }
            labels.push(LabelSpec {
                text: format!("{} · Preview", pt.label),
                color: color(pt.ink_dim, 1.0),
                left: (pv.x + ipad + 3.0 * (dot + dgap) + (6.0 * scale)).round(),
                top: (pv.y + (title_h - self.chrome_cell_height) / 2.0).round(),
                clip: pv,
                size: Some(small),
            });
            // Mini sidebar: three group rows, the first raised.
            let body = LayoutRect {
                x: pv.x,
                y: pv.y + title_h,
                w: pv.w,
                h: (pv.h - title_h).max(0.0),
            };
            let sb_w = (110.0 * scale).round().min((body.w * 0.35).round());
            let row_h = (22.0 * scale).round();
            for (i, name) in ["flaky tests", "stripe v4", "docs pass"].iter().enumerate() {
                let rrect = LayoutRect {
                    x: (body.x + ipad).round(),
                    y: (body.y + (6.0 * scale) + i as f32 * (row_h + (4.0 * scale)))
                        .round(),
                    w: (sb_w - 1.5 * ipad).max(0.0),
                    h: row_h,
                };
                if rrect.y + rrect.h > pv.y + pv.h - ipad {
                    break;
                }
                if i == 0 {
                    bg_quads.push(self.px_rect(&rrect, pt.card, 0.9, (6.0 * scale).round()));
                }
                labels.push(LabelSpec {
                    text: (*name).into(),
                    color: color(if i == 0 { pt.ink } else { pt.ink_dim }, 1.0),
                    left: rrect.x + (7.0 * scale).round(),
                    top: (rrect.y + (rrect.h - self.chrome_cell_height) / 2.0).round(),
                    clip: rrect,
                    size: Some(small),
                });
            }
            // Mini tile: the floating terminal card, deliberately a
            // small element like in the real layout — the theme's
            // polarity reads from the gradient + surface ground. The
            // pane itself mirrors `build_frame`'s chrome: the
            // selected profile's ground/text when one is set, the
            // theme's terminal tokens for the adaptive Default.
            let (pane_bg, pane_ink, pane_dim, pane_divider) =
                match crate::term_theme::selected(preview_dark) {
                    Some(t) => (t.bg, t.fg, (t.fg, 0.55), (t.fg, 0.15)),
                    None => (
                        pt.term_bg,
                        pt.text_bright,
                        (pt.text_dim, 1.0),
                        (pt.card_divider, 1.0),
                    ),
                };
            let tile = LayoutRect {
                x: (body.x + sb_w).round(),
                y: (body.y + (6.0 * scale)).round(),
                w: (body.w - sb_w - ipad).max(0.0),
                h: (body.h * 0.42).round().max(0.0),
            };
            bg_quads.push(
                self.px_rect(&tile, pane_bg, 1.0, (8.0 * scale).round())
                    .shadow(Shadow::Soft),
            );
            let strip_h = (20.0 * scale).round();
            for (i, (tab, on)) in [("zsh", true), ("cargo", false)].iter().enumerate() {
                labels.push(LabelSpec {
                    text: (*tab).into(),
                    color: if *on {
                        color(pane_ink, 1.0)
                    } else {
                        color(pane_dim.0, pane_dim.1)
                    },
                    left: (tile.x + ipad + i as f32 * (46.0 * scale)).round(),
                    top: (tile.y + (strip_h - self.chrome_cell_height) / 2.0).round(),
                    clip: tile,
                    size: Some(small),
                });
            }
            let hairline = LayoutRect {
                x: tile.x,
                y: (tile.y + strip_h).round(),
                w: tile.w,
                h: (1.0 * scale).round().max(1.0),
            };
            bg_quads.push(self.px_rect(&hairline, pane_divider.0, pane_divider.1, 0.0));
            // A couple of pane lines in the pane's text colors.
            for (i, (line, (c, a))) in [
                ("$ cargo run", (pane_ink, 1.0)),
                ("   Compiling pwrde", pane_dim),
            ]
            .iter()
            .enumerate()
            {
                let top = (tile.y
                    + strip_h
                    + (8.0 * scale)
                    + i as f32 * (self.chrome_cell_height + (4.0 * scale)))
                    .round();
                if top + self.chrome_cell_height > tile.y + tile.h - (6.0 * scale) {
                    break;
                }
                labels.push(LabelSpec {
                    text: (*line).into(),
                    color: color(*c, *a),
                    left: (tile.x + ipad).round(),
                    top,
                    clip: tile,
                    size: Some(small),
                });
            }
            let body_bottom = body.y + body.h - ipad;
            // Surface card on the gradient (popover / panel chrome).
            let sc = LayoutRect {
                x: tile.x,
                y: (tile.y + tile.h + (10.0 * scale)).round(),
                w: tile.w,
                h: (46.0 * scale).round(),
            };
            if sc.y + sc.h < body_bottom {
                bg_quads.push(
                    self.px_rect(&sc, pt.card, 1.0, (7.0 * scale).round())
                        .shadow(Shadow::Soft),
                );
                labels.push(LabelSpec {
                    text: "Surface card".into(),
                    color: color(pt.ink, 1.0),
                    left: (sc.x + (8.0 * scale)).round(),
                    top: (sc.y + (6.0 * scale)).round(),
                    clip: sc,
                    size: Some(small),
                });
                let sub = "Secondary text on surface · ";
                labels.push(LabelSpec {
                    text: sub.into(),
                    color: color(pt.ink_dim, 1.0),
                    left: (sc.x + (8.0 * scale)).round(),
                    top: (sc.y + (24.0 * scale)).round(),
                    clip: sc,
                    size: Some(small),
                });
                labels.push(LabelSpec {
                    text: "a link".into(),
                    color: color(pt.accent, 1.0),
                    left: (sc.x + (8.0 * scale) + sub.chars().count() as f32 * small_cw)
                        .round(),
                    top: (sc.y + (24.0 * scale)).round(),
                    clip: sc,
                    size: Some(small),
                });
            }
            // Primary / secondary buttons on the gradient.
            let btn_h = (20.0 * scale).round();
            let by = (sc.y + sc.h + (10.0 * scale)).round();
            if by + btn_h < body_bottom {
                let bw1 = ("Primary".len() as f32 * small_cw + 2.0 * ipad).round();
                let b1 = LayoutRect { x: tile.x, y: by, w: bw1, h: btn_h };
                bg_quads.push(self.px_rect(&b1, pt.accent, 1.0, (6.0 * scale).round()));
                labels.push(LabelSpec {
                    text: "Primary".into(),
                    color: color((255, 255, 255), 1.0),
                    left: (b1.x + ipad).round(),
                    top: (b1.y + (b1.h - self.chrome_cell_height) / 2.0).round(),
                    clip: b1,
                    size: Some(small),
                });
                let bw2 = ("Secondary".len() as f32 * small_cw + 2.0 * ipad).round();
                let b2 = LayoutRect {
                    x: (b1.x + b1.w + (8.0 * scale)).round(),
                    y: by,
                    w: bw2,
                    h: btn_h,
                };
                bg_quads.push(self.px_rect(&b2, pt.ink, 0.08, (6.0 * scale).round()));
                labels.push(LabelSpec {
                    text: "Secondary".into(),
                    color: color(pt.ink, 1.0),
                    left: (b2.x + ipad).round(),
                    top: (b2.y + (b2.h - self.chrome_cell_height) / 2.0).round(),
                    clip: b2,
                    size: Some(small),
                });
            }
            // Token swatch strip, ink-ringed on the gradient.
            let chy = (by + btn_h + (12.0 * scale)).round();
            if chy + chip < body_bottom {
                let tokens = [pt.gradient_from, pt.card, pt.term_bg, pt.accent, pt.ink];
                for (i, c) in tokens.iter().enumerate() {
                    let r = LayoutRect {
                        x: (tile.x + i as f32 * (chip + chip_gap)).round(),
                        y: chy,
                        w: chip,
                        h: chip,
                    };
                    // A faint ink ring keeps chips visible when a
                    // token matches the gradient ground.
                    let ring = (1.0 * scale).round().max(1.0);
                    bg_quads.push(self.px_rect(
                        &r.inflate(ring),
                        pt.ink,
                        0.25,
                        (3.0 * scale).round() + ring,
                    ));
                    bg_quads.push(self.px_rect(&r, *c, 1.0, (3.0 * scale).round()));
                }
                labels.push(LabelSpec {
                    text: "bg · surface · pane · accent · ink".into(),
                    color: color(pt.ink_dim, 1.0),
                    left: (tile.x + 5.0 * (chip + chip_gap) + (6.0 * scale)).round(),
                    top: (chy + (chip - self.chrome_cell_height) / 2.0).round(),
                    clip: *area,
                    size: Some(small),
                });
            }
        }

        // ── Terminal preview: a fake shell session in the previewed
        // polarity's selected scheme ──
        let sel = crate::term_theme::selected(preview_dark);
        let (tfg, tbg, ansi) = crate::term_theme::preview_colors(sel, pt.term_bg);
        let pv = workspace::appearance_preview_card(area, 1, scale);
        if fits(&pv) && pv.h > 80.0 * scale {
            let r = (10.0 * scale).round();
            bg_quads.push(self.px_rect(&pv, tbg, 1.0, r).shadow(Shadow::Soft));
            let strip_h = (24.0 * scale).round();
            let strip = LayoutRect { h: strip_h, ..pv };
            bg_quads.push(self.px_rect(&strip, (0, 0, 0), 0.18, r));
            labels.push(LabelSpec {
                text: format!("{} · Preview", sel.map_or("Default", |t| t.label)),
                color: color(tfg, 0.7),
                left: (pv.x + ipad).round(),
                top: (pv.y + (strip_h - self.chrome_cell_height) / 2.0).round(),
                clip: strip,
                size: Some(small),
            });
            // The full ANSI table, right-aligned in the strip.
            let c8 = (8.0 * scale).round();
            let cgap = (3.0 * scale).round();
            let x0 = pv.x + pv.w - ipad - (8.0 * c8 + 7.0 * cgap);
            for (i, c) in ansi.iter().enumerate() {
                let chip_r = LayoutRect {
                    x: (x0 + i as f32 * (c8 + cgap)).round(),
                    y: (pv.y + (strip_h - c8) / 2.0).round(),
                    w: c8,
                    h: c8,
                };
                bg_quads.push(self.px_rect(&chip_r, *c, 1.0, (2.0 * scale).round()));
            }
            // Fake session exercising fg, dim fg, and the accents.
            let fgc = color(tfg, 1.0);
            let dimc = color(tfg, 0.55);
            let red = color(ansi[1], 1.0);
            let green = color(ansi[2], 1.0);
            let yellow = color(ansi[3], 1.0);
            let magenta = color(ansi[5], 1.0);
            let cyan = color(ansi[6], 1.0);
            let lines: Vec<Vec<(&str, gpui::Hsla)>> = vec![
                vec![("you@dev", green), (":~/checkout$", dimc), (" git status", fgc)],
                vec![("On branch ", dimc), ("fix/flaky-capture", cyan)],
                vec![("  modified:  ", red), ("tests/conftest.py", fgc)],
                vec![("  new file:  ", green), ("tests/test_clock.py", fgc)],
                vec![("you@dev", green), (":~$", dimc), (" pytest -q", fgc)],
                vec![("warning: ", yellow), ("2 deprecation warnings", fgc)],
                vec![
                    ("400 passed ", green),
                    ("0 failed", red),
                    (" in ", fgc),
                    ("41.2s", magenta),
                ],
                vec![("❯ ", magenta), ("agent watching e2e ", fgc)],
            ];
            let lh = (self.chrome_cell_height + (4.0 * scale)).round();
            let mut y = (pv.y + strip_h + (8.0 * scale)).round();
            let mut cursor_pos = None;
            for spans in lines {
                if y + self.chrome_cell_height > pv.y + pv.h - ipad {
                    break;
                }
                let mut x = (pv.x + ipad).round();
                for (text, c) in spans {
                    let w = text.chars().count() as f32 * self.chrome_cell_width;
                    labels.push(LabelSpec {
                        text: text.into(),
                        color: c,
                        left: x,
                        top: y,
                        clip: pv,
                        size: None,
                    });
                    x = (x + w).round();
                }
                cursor_pos = Some((x, y));
                y += lh;
            }
            // Block cursor trailing the last line.
            if let Some((cx, cy)) = cursor_pos {
                let cur_r = LayoutRect {
                    x: cx,
                    y: cy,
                    w: (self.chrome_cell_width * 0.6).round().max(1.0),
                    h: self.chrome_cell_height,
                };
                bg_quads.push(self.px_rect(&cur_r, tfg, 0.9, 0.0));
            }
        }

        // ── Footer: theme sharing actions + live-apply note ──
        // (`fits` excludes the card's own bottom band, so gate on the
        // footer clearing the header instead.)
        let footer = workspace::appearance_footer_row(area, scale);
        if footer.y > header.y + header.h {
            for (i, name) in
                ["Import from Clipboard", "Copy Theme String"].iter().enumerate()
            {
                let b = workspace::appearance_footer_action(area, i, self.chrome_cell_width, scale);
                let hov = hover(cur, &b);
                bg_quads.push(self.px_rect(
                    &b,
                    th.ink,
                    if hov { 0.12 } else { 0.06 },
                    b.h / 2.0,
                ));
                hot.push(b);
                let lw = name.chars().count() as f32 * self.chrome_cell_width;
                labels.push(LabelSpec {
                    text: (*name).into(),
                    color: color(th.ink, 1.0),
                    left: (b.x + (b.w - lw) / 2.0).round(),
                    top: (b.y + (b.h - self.chrome_cell_height) / 2.0).round(),
                    clip: b,
                    size: None,
                });
            }
            let note = "changes apply live";
            let w = note.chars().count() as f32 * self.chrome_cell_width;
            labels.push(LabelSpec {
                text: note.into(),
                color: color(th.ink_dim, 1.0),
                left: (footer.x + footer.w - w).round(),
                top: mid(&footer),
                clip: footer,
                size: None,
            });
        }
    }

    /// One half-width Appearance slot: a pill with the entry's label, ANSI
    /// preview chips for terminal schemes, and an accent dot on the entry the
    /// resolved mode is actually applying. `picked` marks the entry its own
    /// polarity slot points at (both slots stay visible at once).
    #[allow(clippy::too_many_arguments)]
    /// The Appearance page's open dropdown menu: a floating card listing the
    /// slot's options, drawn in the picker layers so it paints above the page.
    /// Not a modal overlay — the page stays interactive; menu rects are simply
    /// pushed after the page's so reverse hit-order favors them.
    fn appearance_menu_overlay(
        &self,
        area: &LayoutRect,
        menu: crate::pages::AppearanceDropdown,
        cursor: Option<(f32, f32)>,
        quads: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let mut labels = Vec::new();
        let (col, field) = menu.grid();
        let dark = menu.dark();
        // Each option as (swatches, label, selected), resolved once.
        let rows: Vec<([(u8, u8, u8); 3], &'static str, bool)> = match menu {
            crate::pages::AppearanceDropdown::ThemeLight
            | crate::pages::AppearanceDropdown::ThemeDark => {
                let sel = crate::theme::selected(dark).name;
                crate::pages::theme_options(dark)
                    .iter()
                    .map(|t| ([t.gradient_from, t.card, t.accent], t.label, t.name == sel))
                    .collect()
            },
            crate::pages::AppearanceDropdown::TermLight
            | crate::pages::AppearanceDropdown::TermDark => {
                let sel = crate::term_theme::selected(dark).map(|t| t.name);
                crate::pages::term_options(dark)
                    .iter()
                    .map(|o| match o {
                        Some(t) => {
                            ([t.ansi[1], t.ansi[2], t.ansi[4]], t.label, sel == Some(t.name))
                        },
                        None => {
                            let (_, _, ansi) =
                                crate::term_theme::preview_colors(None, th.term_bg);
                            ([ansi[1], ansi[2], ansi[4]], "Default", sel.is_none())
                        },
                    })
                    .collect()
            },
        };
        let panel = workspace::appearance_menu_panel(area, col, field, rows.len(), scale);
        quads.push(self.px_rect(&panel, th.card, 0.98, (9.0 * scale).round()).shadow(Shadow::Card));
        hot.push(panel);
        let ipad = (10.0 * scale).round();
        let chip = (9.0 * scale).round();
        let cgap = (3.0 * scale).round();
        for (i, (chips, name, selected)) in rows.iter().enumerate() {
            let row = workspace::appearance_menu_item(area, col, field, rows.len(), i, scale);
            let inner = LayoutRect {
                x: row.x + (4.0 * scale).round(),
                w: (row.w - (8.0 * scale).round()).max(0.0),
                ..row
            };
            if *selected {
                quads.push(self.px_rect(&inner, th.accent, 0.14, (6.0 * scale).round()));
            } else if hover(cursor, &row) {
                quads.push(self.px_rect(&inner, th.ink, 0.08, (6.0 * scale).round()));
            }
            hot.push(row);
            let cy = (row.y + (row.h - chip) / 2.0).round();
            for (j, c) in chips.iter().enumerate() {
                let r = LayoutRect {
                    x: (inner.x + ipad + j as f32 * (chip + cgap)).round(),
                    y: cy,
                    w: chip,
                    h: chip,
                };
                quads.push(self.px_rect(&r, *c, 1.0, (3.0 * scale).round()));
            }
            let text_left = (inner.x + ipad + 3.0 * (chip + cgap) + (6.0 * scale)).round();
            let top = (row.y + (row.h - self.chrome_cell_height) / 2.0).round();
            labels.push(LabelSpec {
                text: (*name).into(),
                color: color(if *selected { th.ink } else { th.ink_dim }, 1.0),
                left: text_left,
                top,
                clip: inner,
                size: None,
            });
            if *selected {
                labels.push(LabelSpec {
                    text: "\u{25cf}".into(),
                    color: color(th.accent, 1.0),
                    left: (inner.x + inner.w - ipad - self.chrome_cell_width).round(),
                    top,
                    clip: inner,
                    size: None,
                });
            }
        }
        labels
    }


    fn picker_overlay(
        &self,
        picker: &Picker,
        layout: &PickerLayout,
        cursor: Option<(f32, f32)>,
        rects: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let mut labels = Vec::new();

        // Full-window dimming scrim behind the popover (light, so the warm
        // gradient still reads through it).
        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));

        // The popover: a floating white card matching the Arc chrome, with an
        // ink-tinted search field inside it.
        rects.push(
            self.px_rect(&layout.panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );
        rects.push(self.px_rect(&layout.search, th.ink, 0.06, (7.0 * scale).round()));

        // Search text (or placeholder) with a caret trailing the query.
        let search_top = (layout.search.y + (layout.search.h - self.chrome_cell_height) / 2.0).round();
        let (text, c) = if picker.query.is_empty() {
            ("Search repos…".to_string(), th.ink_dim)
        } else {
            (picker.query.clone(), th.ink)
        };
        labels.push(LabelSpec {
            text,
            color: color(c, 1.0),
            left: layout.search.x + pad,
            top: search_top,
            clip: layout.search,
            size: None,
        });
        let caret_x = layout.search.x + pad + picker.query.chars().count() as f32 * self.chrome_cell_width;
        let caret = LayoutRect {
            x: caret_x,
            y: search_top,
            w: (2.0 * scale).round().max(1.0),
            h: self.chrome_cell_height,
        };
        rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));

        // Visible rows: headers, selected-row highlight, labels, glyphs.
        for i in layout.first_visible..(layout.first_visible + layout.visible) {
            let (Some(row), Some(prow)) = (layout.row_rect(i), picker.rows.get(i)) else {
                continue;
            };
            let top = (row.y + (row.h - self.chrome_cell_height) / 2.0).round();
            match prow {
                PickerRow::Header(title) => labels.push(LabelSpec {
                    text: title.to_string(),
                    color: color(th.ink_dim, 1.0),
                    left: row.x + pad,
                    top,
                    clip: row,
                    size: None,
                }),
                PickerRow::Entry(entry) => {
                    let m = (6.0 * scale).round();
                    let pill = LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..row };
                    if i == picker.selected {
                        // Accent-tinted rounded pill, inset from the panel edges.
                        rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
                    } else if hover(cursor, &row) {
                        // Hovered row: the selection pill's shape in plain ink,
                        // dimmer, so it never reads as the keyboard selection.
                        rects.push(self.px_rect(&pill, th.ink, 0.06, (7.0 * scale).round()));
                    }
                    hot.push(row);
                    // Label, clipped short of the glyph gutter on the right.
                    let label_bounds =
                        LayoutRect { w: (row.w - 2.0 * layout.row_h).max(0.0), ..row };
                    labels.push(LabelSpec {
                        text: entry.label.clone(),
                        color: color(th.ink, 1.0),
                        left: row.x + pad,
                        top,
                        clip: label_bounds,
                        size: None,
                    });
                    if entry.is_git {
                        let gx = row.x + row.w - 2.0 * layout.row_h;
                        let cell = LayoutRect { x: gx, y: row.y, w: layout.row_h, h: row.h };
                        labels.push(LabelSpec {
                            text: "\u{e0a0}".to_string(),
                            color: color(th.accent, 1.0),
                            left: gx + (layout.row_h - self.chrome_cell_width) / 2.0,
                            top,
                            clip: cell,
                            size: None,
                        });
                    }
                    if picker.is_pinned(&entry.path) {
                        let star = layout.star_rect(&row);
                        labels.push(LabelSpec {
                            text: "★".to_string(),
                            color: color((205, 150, 35), 1.0),
                            left: star.x + (layout.row_h - self.chrome_cell_width) / 2.0,
                            top,
                            clip: star,
                            size: None,
                        });
                    }
                },
            }
        }
        labels
    }

    /// Command-palette overlay: a filterable list of every rebindable action,
    /// with its current ⌘ binding right-aligned in the row. Painted from the
    /// same [`PickerLayout`] `main.rs` hit-tests so clicks agree with pixels.
    fn palette_overlay(
        &self,
        palette: &Palette,
        layout: &PickerLayout,
        cursor: Option<(f32, f32)>,
        rects: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let mut labels = Vec::new();

        // Scrim + card + search field, matching the dir picker.
        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));
        rects.push(
            self.px_rect(&layout.panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );
        rects.push(self.px_rect(&layout.search, th.ink, 0.06, (7.0 * scale).round()));

        // Search text (or placeholder) with a caret trailing the query.
        let search_top = (layout.search.y + (layout.search.h - self.chrome_cell_height) / 2.0).round();
        let (text, c) = if palette.query.is_empty() {
            ("Run a command…".to_string(), th.ink_dim)
        } else {
            (palette.query.clone(), th.ink)
        };
        labels.push(LabelSpec {
            text,
            color: color(c, 1.0),
            left: layout.search.x + pad,
            top: search_top,
            clip: layout.search,
            size: None,
        });
        let caret_x = layout.search.x + pad + palette.query.chars().count() as f32 * self.chrome_cell_width;
        let caret = LayoutRect {
            x: caret_x,
            y: search_top,
            w: (2.0 * scale).round().max(1.0),
            h: self.chrome_cell_height,
        };
        rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));

        // Visible rows: selected-row highlight, action label, binding hint.
        for i in layout.first_visible..(layout.first_visible + layout.visible) {
            let (Some(row), Some(action)) = (layout.row_rect(i), palette.rows.get(i)) else {
                continue;
            };
            let top = (row.y + (row.h - self.chrome_cell_height) / 2.0).round();
            let m = (6.0 * scale).round();
            let pill = LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..row };
            if i == palette.selected {
                // Accent-tinted rounded pill, inset from the panel edges.
                rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
            } else if hover(cursor, &row) {
                rects.push(self.px_rect(&pill, th.ink, 0.06, (7.0 * scale).round()));
            }
            hot.push(row);
            // Current binding, right-aligned; the label clips short of it.
            let binding = action.binding().display();
            let binding_w = binding.chars().count() as f32 * self.chrome_cell_width;
            let binding_x = row.x + row.w - pad - binding_w;
            labels.push(LabelSpec {
                text: binding,
                color: color(th.ink_dim, 1.0),
                left: binding_x,
                top,
                clip: row,
                size: None,
            });
            labels.push(LabelSpec {
                text: action.label().to_string(),
                color: color(th.ink, 1.0),
                left: row.x + pad,
                top,
                clip: LayoutRect { w: (binding_x - pad - row.x).max(0.0), ..row },
                size: None,
            });
        }
        labels
    }

    /// Step-2 fork-source overlay: a centered, filterable list of the branch /
    /// worktree choices for the group being forked. Styled like the dir picker.
    /// TODO(gpui-port): per-scope tag colors and scroll-to-selection.
    fn fork_overlay(
        &self,
        fork: &ForkPicker,
        cursor: Option<(f32, f32)>,
        rects: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let mut labels = Vec::new();

        // Dimming scrim behind the popover (light, matching the dir picker).
        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));

        // Centered panel sized to the (capped) row count: a floating white
        // card matching the Arc chrome.
        let row_h = (self.chrome_cell_height + 8.0 * scale).round();
        let visible = fork.rows.len().min(12);
        let panel_w = (self.width as f32 * 0.5).min(560.0 * scale).round();
        let panel_h = (row_h * (visible as f32 + 2.0) + pad * 2.0).round();
        let panel_x = ((self.width as f32 - panel_w) / 2.0).round();
        let panel_y = ((self.height as f32 - panel_h) / 3.0).round().max(pad);
        let panel = LayoutRect { x: panel_x, y: panel_y, w: panel_w, h: panel_h };
        rects.push(
            self.px_rect(&panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );

        // Header: "fork <name> from…".
        let header = LayoutRect { x: panel_x, y: panel_y + pad, w: panel_w, h: row_h };
        labels.push(LabelSpec {
            text: format!("fork {} from…", fork.name),
            color: color(th.ink_dim, 1.0),
            left: panel_x + pad,
            top: (header.y + (row_h - self.chrome_cell_height) / 2.0).round(),
            clip: header,
            size: None,
        });

        // Filter box + caret.
        let search =
            LayoutRect { x: panel_x + pad, y: panel_y + pad + row_h, w: panel_w - 2.0 * pad, h: row_h };
        rects.push(self.px_rect(&search, th.ink, 0.06, (7.0 * scale).round()));
        let search_top = (search.y + (search.h - self.chrome_cell_height) / 2.0).round();
        let (text, c) = if fork.query.is_empty() {
            ("Filter branches…".to_string(), th.ink_dim)
        } else {
            (fork.query.clone(), th.ink)
        };
        labels.push(LabelSpec {
            text,
            color: color(c, 1.0),
            left: search.x + pad,
            top: search_top,
            clip: search,
            size: None,
        });
        let caret_x = search.x + pad + fork.query.chars().count() as f32 * self.chrome_cell_width;
        let caret = LayoutRect {
            x: caret_x,
            y: search_top,
            w: (2.0 * scale).round().max(1.0),
            h: self.chrome_cell_height,
        };
        rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));

        // Rows.
        let rows_top = panel_y + pad + row_h * 2.0;
        for (i, entry) in fork.rows.iter().take(visible).enumerate() {
            let row = LayoutRect { x: panel_x, y: rows_top + row_h * i as f32, w: panel_w, h: row_h };
            let top = (row.y + (row.h - self.chrome_cell_height) / 2.0).round();
            let m = (6.0 * scale).round();
            let pill = LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..row };
            if i == fork.selected {
                // Accent-tinted rounded pill, inset from the panel edges.
                rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
            } else if hover(cursor, &row) {
                rects.push(self.px_rect(&pill, th.ink, 0.06, (7.0 * scale).round()));
            }
            hot.push(row);
            labels.push(LabelSpec {
                text: entry.label.clone(),
                color: color(th.ink, 1.0),
                left: row.x + pad,
                top,
                clip: LayoutRect { w: panel_w - 2.0 * pad, ..row },
                size: None,
            });
        }
        labels
    }

    /// Step-3 workspace-profile overlay: the discovered `.pwrspace.json`
    /// profiles for the group being created, default row first. Drawn from the
    /// same [`PickerLayout`] `main.rs` hit-tests (like the command palette) so
    /// clicks agree with pixels. Each row shows the profile name with its
    /// description and source dimmed, right-aligned.
    fn profile_overlay(
        &self,
        profile: &ProfilePicker,
        layout: &PickerLayout,
        cursor: Option<(f32, f32)>,
        rects: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let mut labels = Vec::new();

        // Scrim + card + search field, matching the dir picker.
        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));
        rects.push(
            self.px_rect(&layout.panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );
        rects.push(self.px_rect(&layout.search, th.ink, 0.06, (7.0 * scale).round()));

        // Search text (or a placeholder naming the group) with a caret.
        let search_top = (layout.search.y + (layout.search.h - self.chrome_cell_height) / 2.0).round();
        let (text, c) = if profile.query.is_empty() {
            (format!("Launch {} with…", profile.name), th.ink_dim)
        } else {
            (profile.query.clone(), th.ink)
        };
        labels.push(LabelSpec {
            text,
            color: color(c, 1.0),
            left: layout.search.x + pad,
            top: search_top,
            clip: layout.search,
            size: None,
        });
        let caret_x =
            layout.search.x + pad + profile.query.chars().count() as f32 * self.chrome_cell_width;
        let caret = LayoutRect {
            x: caret_x,
            y: search_top,
            w: (2.0 * scale).round().max(1.0),
            h: self.chrome_cell_height,
        };
        rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));

        // Visible rows: selected pill, profile name, dim detail right-aligned.
        for i in layout.first_visible..(layout.first_visible + layout.visible) {
            let (Some(row), Some(entry)) = (layout.row_rect(i), profile.rows.get(i)) else {
                continue;
            };
            let top = (row.y + (row.h - self.chrome_cell_height) / 2.0).round();
            let m = (6.0 * scale).round();
            let pill = LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..row };
            if i == profile.selected {
                rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
            } else if hover(cursor, &row) {
                rects.push(self.px_rect(&pill, th.ink, 0.06, (7.0 * scale).round()));
            }
            hot.push(row);
            // The name has priority: the detail only gets the width left over
            // after it (a long description truncates with an ellipsis rather
            // than pushing the name out of the row).
            let label_w = entry.label.chars().count() as f32 * self.chrome_cell_width;
            let room = row.w - 2.0 * pad - label_w - 2.0 * self.chrome_cell_width;
            let detail = truncate_chars(&entry.detail, (room / self.chrome_cell_width) as usize);
            let detail_w = detail.chars().count() as f32 * self.chrome_cell_width;
            let detail_x = row.x + row.w - pad - detail_w;
            if !detail.is_empty() {
                labels.push(LabelSpec {
                    text: detail,
                    color: color(th.ink_dim, 1.0),
                    left: detail_x,
                    top,
                    clip: row,
                    size: None,
                });
            }
            labels.push(LabelSpec {
                text: entry.label.clone(),
                color: color(th.ink, 1.0),
                left: row.x + pad,
                top,
                clip: LayoutRect { w: (detail_x - pad - row.x).max(0.0), ..row },
                size: None,
            });
        }
        labels
    }

    /// Geometry of the save-workspace modal, shared by drawing and `main.rs`
    /// hit-testing. `dest_rows` is the number of destination choices shown
    /// (0 while the name/description fields are being edited).
    pub fn save_layout(&self, dest_rows: usize) -> SaveLayout {
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let row_h = (self.chrome_cell_height + 8.0 * scale).round();
        let caption_h = (self.chrome_cell_height + 4.0 * scale).round();

        let panel_w = (self.width as f32 * 0.5).min(560.0 * scale).round();
        let dest_h = if dest_rows > 0 { caption_h + dest_rows as f32 * row_h } else { 0.0 };
        let panel_h = (pad * 2.0 + row_h + 2.0 * (caption_h + row_h) + dest_h).round();
        let panel_x = ((self.width as f32 - panel_w) / 2.0).round();
        let panel_y = ((self.height as f32 - panel_h) / 3.0).round().max(pad);
        let panel = LayoutRect { x: panel_x, y: panel_y, w: panel_w, h: panel_h };

        let field_w = panel_w - 2.0 * pad;
        let name_y = panel_y + pad + row_h + caption_h;
        let name = LayoutRect { x: panel_x + pad, y: name_y, w: field_w, h: row_h };
        let desc_y = name_y + row_h + caption_h;
        let desc = LayoutRect { x: panel_x + pad, y: desc_y, w: field_w, h: row_h };

        let rows_top = desc_y + row_h + caption_h;
        let rows = (0..dest_rows)
            .map(|i| LayoutRect {
                x: panel_x,
                y: rows_top + i as f32 * row_h,
                w: panel_w,
                h: row_h,
            })
            .collect();
        SaveLayout { panel, name, desc, rows }
    }

    /// Save-workspace modal: Name and Description fields, then (once both are
    /// entered) the destination rows. The focused field carries the caret; the
    /// selected destination row carries the accent pill.
    fn save_overlay(
        &self,
        view: &SaveModalView,
        layout: &SaveLayout,
        cursor: Option<(f32, f32)>,
        rects: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let mut labels = Vec::new();

        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));
        rects.push(
            self.px_rect(&layout.panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );

        // Title.
        labels.push(LabelSpec {
            text: "Save as workspace".into(),
            color: color(th.ink_dim, 1.0),
            left: layout.panel.x + pad,
            top: (layout.panel.y + pad).round(),
            clip: layout.panel,
            size: None,
        });

        // Fields: caption above each box; the focused one is brighter and
        // carries the caret (only while still in the field-editing stage).
        let editing_fields = view.dest.is_none();
        for (i, (caption, value, rect)) in [
            ("Name", view.name, &layout.name),
            ("Description", view.description, &layout.desc),
        ]
        .into_iter()
        .enumerate()
        {
            labels.push(LabelSpec {
                text: caption.into(),
                color: color(th.ink_dim, 1.0),
                left: rect.x,
                top: (rect.y - self.chrome_cell_height - 2.0 * scale).round(),
                clip: layout.panel,
                size: None,
            });
            let focused = editing_fields && view.field == i;
            // Hover matches the focused tint — clicking would focus the field.
            let hov = hover(cursor, rect);
            rects.push(self.px_rect(
                rect,
                th.ink,
                if focused || hov { 0.10 } else { 0.06 },
                (7.0 * scale).round(),
            ));
            hot.push(*rect);
            let top = (rect.y + (rect.h - self.chrome_cell_height) / 2.0).round();
            labels.push(LabelSpec {
                text: value.to_string(),
                color: color(th.ink, 1.0),
                left: rect.x + pad,
                top,
                clip: *rect,
                size: None,
            });
            if focused {
                let caret_x = rect.x + pad + value.chars().count() as f32 * self.chrome_cell_width;
                let caret = LayoutRect {
                    x: caret_x,
                    y: top,
                    w: (2.0 * scale).round().max(1.0),
                    h: self.chrome_cell_height,
                };
                rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));
            }
        }

        // Destination rows.
        if let Some((dest_labels, selected)) = view.dest {
            if let Some(first) = layout.rows.first() {
                labels.push(LabelSpec {
                    text: "Save to".into(),
                    color: color(th.ink_dim, 1.0),
                    left: layout.panel.x + pad,
                    top: (first.y - self.chrome_cell_height - 2.0 * scale).round(),
                    clip: layout.panel,
                    size: None,
                });
            }
            for (i, (text, row)) in dest_labels.iter().zip(layout.rows.iter()).enumerate() {
                let top = (row.y + (row.h - self.chrome_cell_height) / 2.0).round();
                let m = (6.0 * scale).round();
                let pill = LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..*row };
                if i == selected {
                    rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
                } else if hover(cursor, row) {
                    rects.push(self.px_rect(&pill, th.ink, 0.06, (7.0 * scale).round()));
                }
                hot.push(*row);
                labels.push(LabelSpec {
                    text: text.clone(),
                    color: color(th.ink, 1.0),
                    left: row.x + pad,
                    top,
                    clip: LayoutRect { w: row.w - 2.0 * pad, ..*row },
                    size: None,
                });
            }
        }
        labels
    }

    /// Geometry of the confirm dialog (panel + buttons), shared by drawing and
    /// `main.rs` hit-testing so clicks always agree with pixels.
    pub fn confirm_layout(&self, text: &str, accept: &str) -> ConfirmLayout {
        let scale = self.scale;
        let pad = (16.0 * scale).round();
        let gap = (10.0 * scale).round();
        let btn_pad = (14.0 * scale).round();
        let btn_h = (self.chrome_cell_height + 10.0 * scale).round();
        let text_w = text.chars().count() as f32 * self.chrome_cell_width;
        let cancel_w =
            (CONFIRM_CANCEL.chars().count() as f32 * self.chrome_cell_width + 2.0 * btn_pad).round();
        let close_w = (accept.chars().count() as f32 * self.chrome_cell_width + 2.0 * btn_pad).round();
        let w = (text_w.max(cancel_w + gap + close_w) + 2.0 * pad)
            .min(self.width as f32 - 2.0 * pad);
        let h = (self.chrome_cell_height + gap + btn_h + 2.0 * pad).round();
        let x = ((self.width as f32 - w) / 2.0).round();
        let y = ((self.height as f32 - h) / 2.0).round();
        let by = (y + pad + self.chrome_cell_height + gap).round();
        let close_x = (x + w - pad - close_w).round();
        let cancel_x = (close_x - gap - cancel_w).round();
        ConfirmLayout {
            panel: LayoutRect { x, y, w, h },
            cancel: LayoutRect { x: cancel_x, y: by, w: cancel_w, h: btn_h },
            close: LayoutRect { x: close_x, y: by, w: close_w, h: btn_h },
        }
    }

    /// Centered confirm dialog: a message line over Cancel / accept buttons.
    /// Styled like the message panel; the destructive accept button carries
    /// the accent fill.
    fn confirm_overlay(
        &self,
        text: &str,
        accept: &str,
        cursor: Option<(f32, f32)>,
        rects: &mut Vec<Quad>,
        hot: &mut Vec<LayoutRect>,
    ) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (16.0 * scale).round();
        let btn_r = (7.0 * scale).round();
        let layout = self.confirm_layout(text, accept);

        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));
        rects.push(
            self.px_rect(&layout.panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );

        let mut labels = vec![LabelSpec {
            text: text.to_string(),
            color: color(th.ink, 1.0),
            left: layout.panel.x + pad,
            top: (layout.panel.y + pad).round(),
            clip: layout.panel,
            size: None,
        }];
        for (rect, label, danger) in
            [(&layout.cancel, CONFIRM_CANCEL, false), (&layout.close, accept, true)]
        {
            let hov = hover(cursor, rect);
            rects.push(
                self.px_rect(
                    rect,
                    if danger { th.accent } else { th.ink },
                    if danger {
                        if hov { 1.0 } else { 0.9 }
                    } else if hov {
                        0.14
                    } else {
                        0.08
                    },
                    btn_r,
                )
                .shadow(if hov { Shadow::Soft } else { Shadow::None }),
            );
            hot.push(*rect);
            let w = label.chars().count() as f32 * self.chrome_cell_width;
            labels.push(LabelSpec {
                text: label.into(),
                color: color(if danger { (255, 255, 255) } else { th.ink }, 1.0),
                left: (rect.x + (rect.w - w) / 2.0).round(),
                top: (rect.y + (rect.h - self.chrome_cell_height) / 2.0).round(),
                clip: *rect,
                size: None,
            });
        }
        labels
    }

    /// Centered one-line message panel (worktree provisioning / failure note).
    fn message_overlay(&self, text: &str, rects: &mut Vec<Quad>) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (16.0 * scale).round();
        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));

        let w = (text.chars().count() as f32 * self.chrome_cell_width + pad * 2.0)
            .min(self.width as f32 - pad * 2.0);
        let h = (self.chrome_cell_height + pad * 2.0).round();
        let x = ((self.width as f32 - w) / 2.0).round();
        let y = ((self.height as f32 - h) / 2.0).round();
        let panel = LayoutRect { x, y, w, h };
        rects.push(
            self.px_rect(&panel, th.card, 0.96, (CARD_RADIUS * scale).round())
                .shadow(Shadow::Card),
        );
        vec![LabelSpec {
            text: text.to_string(),
            color: color(th.ink, 1.0),
            left: x + pad,
            top: (y + (h - self.chrome_cell_height) / 2.0).round(),
            clip: panel,
            size: None,
        }]
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
        cursor: Option<(f32, f32)>,
        hot: &mut Vec<LayoutRect>,
    ) -> (Vec<Quad>, Vec<PaneText>, Vec<Quad>, Vec<LabelSpec>) {
        let th = self.theme();
        let scale = self.scale;
        // Resolve the terminal scheme exactly as `build_frame` does for tile
        // cards, so the flyover follows the Appearance-page terminal colors:
        // scheme bg/fg drive the card and tab chrome when one is selected.
        let scheme = crate::term_theme::selected(crate::theme::dark_active());
        let term_palette = crate::term_theme::build(scheme, th.term_bg);
        let (pane_bg, pane_ink, pane_ink_dim, pane_divider, pane_pill) = match scheme {
            Some(t) => (t.bg, (t.fg, 1.0), (t.fg, 0.55), (t.fg, 0.15), (t.fg, 0.12)),
            None => (
                th.term_bg,
                (th.text_bright, 1.0),
                (th.text_dim, 1.0),
                (th.card_divider, 1.0),
                ((255u8, 255u8, 255u8), 0.09f32),
            ),
        };

        let tab_bar = crate::workspace::flyover_tab_bar(panel_rect, scale);
        let content = crate::workspace::flyover_content(panel_rect, scale);
        let n = tabs.len();

        let mut quads: Vec<Quad> = Vec::new();
        let mut fg_quads: Vec<Quad> = Vec::new();
        let mut panes: Vec<PaneText> = Vec::new();
        let mut labels: Vec<LabelSpec> = Vec::new();

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

        // Tab strip: highlight pill for the active tab, inset like the tile
        // strips' pill so the bar shows around it.
        if n > 0 {
            let tr = crate::workspace::flyover_tab_rect(panel_rect, active, n, scale, maximized);
            let m = (4.0 * scale).round();
            let pill = crate::workspace::LayoutRect {
                x: tr.x + m,
                y: tr.y + m,
                w: (tr.w - 2.0 * m).max(0.0),
                h: (tr.h - 2.0 * m).max(0.0),
            };
            quads.push(self.px_rect(&pill, pane_pill.0, pane_pill.1, (7.0 * scale).round()));
        }

        // Tab labels + per-tab × close button (mirrors the tile tab strip).
        let tab_text_pad = (8.0 * scale).round();
        for (i, tab) in tabs.iter().enumerate() {
            let tr = crate::workspace::flyover_tab_rect(panel_rect, i, n, scale, maximized);
            let close = crate::workspace::flyover_tab_close_rect(panel_rect, i, n, scale, maximized);
            let close_hov = hover(cursor, &close);
            // Same hover language as the tile strips: dim pill on an inactive
            // tab, rounded chip + brightened glyph on the ×.
            if i != active && hover(cursor, &tr) && !close_hov {
                let m = (4.0 * scale).round();
                let pill = crate::workspace::LayoutRect {
                    x: tr.x + m,
                    y: tr.y + m,
                    w: (tr.w - 2.0 * m).max(0.0),
                    h: (tr.h - 2.0 * m).max(0.0),
                };
                quads.push(self.px_rect(&pill, pane_pill.0, pane_pill.1 * 0.55, (7.0 * scale).round()));
            }
            if close_hov {
                let inset = (3.0 * scale).round();
                let chip = crate::workspace::LayoutRect {
                    x: close.x + inset,
                    y: close.y + inset,
                    w: (close.w - 2.0 * inset).max(0.0),
                    h: (close.h - 2.0 * inset).max(0.0),
                };
                quads.push(self.px_rect(
                    &chip,
                    pane_pill.0,
                    (pane_pill.1 * 2.0).min(1.0),
                    (4.0 * scale).round(),
                ));
            }
            // Close after its tab so reverse iteration (topmost wins)
            // resolves × over the tab it sits in.
            hot.push(tr);
            hot.push(close);
            let title = tab.session.title();
            let text = if title.is_empty() { "shell".to_string() } else { title };
            let mut text_left = tr.x + tab_text_pad;
            // Unread dot.
            if tab.unread {
                let ds = (6.0 * scale).round();
                let dot = crate::workspace::LayoutRect {
                    x: text_left,
                    y: (tr.y + (tr.h - ds) / 2.0).round(),
                    w: ds,
                    h: ds,
                };
                fg_quads.push(self.px_rect(&dot, th.accent, 1.0, ds / 2.0));
                text_left += ds + (5.0 * scale).round();
            }
            labels.push(LabelSpec {
                text,
                color: if i == active {
                    color(pane_ink.0, pane_ink.1)
                } else {
                    color(pane_ink_dim.0, pane_ink_dim.1)
                },
                left: text_left,
                top: (tr.y + (tr.h - self.chrome_cell_height) / 2.0).round(),
                clip: crate::workspace::LayoutRect {
                    w: (close.x - tr.x - tab_text_pad).max(0.0),
                    ..tr
                },
                size: None,
            });
            labels.push(LabelSpec {
                text: "×".to_string(),
                color: if close_hov {
                    color(pane_ink.0, pane_ink.1)
                } else {
                    color(pane_ink_dim.0, pane_ink_dim.1)
                },
                left: close.x + ((close.w - self.chrome_cell_width) / 2.0).round(),
                top: (tr.y + (tr.h - self.chrome_cell_height) / 2.0).round(),
                clip: tr,
                size: None,
            });
        }

        // Minimize / maximize buttons at the bar's right edge.
        if show_window_buttons {
            let bar_h = tab_bar.h;
            for (rect, glyph) in [
                (crate::workspace::flyover_minimize_rect(panel_rect, scale), "–"),
                (crate::workspace::flyover_maximize_rect(panel_rect, scale), "□"),
            ] {
                let hov = hover(cursor, &rect);
                if hov {
                    let inset = (3.0 * scale).round();
                    let chip = crate::workspace::LayoutRect {
                        x: rect.x + inset,
                        y: rect.y + inset,
                        w: (rect.w - 2.0 * inset).max(0.0),
                        h: (rect.h - 2.0 * inset).max(0.0),
                    };
                    quads.push(self.px_rect(
                        &chip,
                        pane_pill.0,
                        (pane_pill.1 * 2.0).min(1.0),
                        (4.0 * scale).round(),
                    ));
                }
                hot.push(rect);
                labels.push(LabelSpec {
                    text: glyph.to_string(),
                    color: if hov {
                        color(pane_ink.0, pane_ink.1)
                    } else {
                        color(pane_ink_dim.0, pane_ink_dim.1)
                    },
                    left: rect.x + ((rect.w - self.chrome_cell_width) / 2.0).round(),
                    top: (rect.y + (bar_h - self.chrome_cell_height) / 2.0).round(),
                    clip: rect,
                    size: None,
                });
            }
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
        if let Some(tab) = tabs.get(active) {
            let origin = self.content_origin(&content);
            let rows = self.snapshot_pane(
                &tab.session,
                &term_palette,
                origin,
                draw_cursor && focused,
                None,
                &mut quads,
                &mut fg_quads,
            );
            panes.push(PaneText { origin, rows });
            self.selection_rects(&tab.session, origin, &mut fg_quads);
        }

        (quads, panes, fg_quads, labels)
    }

    /// Snapshot one pane's grid into per-row text spans + geometry quads,
    /// offset to `origin`. One `Vec<TextSpan>` per grid row (so the caller can
    /// shape each row independently). Holds the terminal lock only for the walk.
    fn snapshot_pane(
        &self,
        session: &Session,
        palette: &ColorPalette,
        origin: (f32, f32),
        draw_cursor: bool,
        hover: Option<(usize, usize)>,
        bg_rects: &mut Vec<Quad>,
        rects: &mut Vec<Quad>,
    ) -> Vec<Vec<TextSpan>> {
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
        for (row, line) in lines.iter().enumerate() {
            let mut spans: Vec<TextSpan> = Vec::new();
            for cell in line.visible_cells() {
                let col = cell.cell_index();
                let attrs = cell.attrs();
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

        rows_spans
    }

    /// Slim ink line along `rect` plus a centered grip pill (~28 logical px).
    /// `vertical` is true for a row-split divider / the sidebar edge (pill is tall).
    fn push_resize_grip(
        &self,
        quads: &mut Vec<Quad>,
        rect: &LayoutRect,
        vertical: bool,
        ink: (u8, u8, u8),
    ) {
        let thickness = if vertical { rect.w } else { rect.h };
        let radius = thickness / 2.0;
        quads.push(self.px_rect(rect, ink, 0.14, radius));
        let pill_len = (28.0 * self.scale).round();
        let pill = if vertical {
            let h = pill_len.min(rect.h);
            LayoutRect {
                x: rect.x,
                y: rect.y + ((rect.h - h) / 2.0).max(0.0),
                w: rect.w,
                h,
            }
        } else {
            let w = pill_len.min(rect.w);
            LayoutRect {
                x: rect.x + ((rect.w - w) / 2.0).max(0.0),
                y: rect.y,
                w,
                h: rect.h,
            }
        };
        quads.push(self.px_rect(&pill, ink, 0.45, radius));
    }

    /// A quad straight from layout coordinates (already physical px).
    /// A tool's ribbon glyph, drawn as vector quads inside a 16×16 logical-px
    /// box centered on the slot — fonts can't be trusted to carry
    /// octicon-style symbols, so like `rect.rs` we build them from geometry.
    fn ribbon_icon(
        &self,
        tool: pages::Tool,
        slot: &LayoutRect,
        rgb: (u8, u8, u8),
        quads: &mut Vec<Quad>,
        carets: &mut Vec<CaretSpec>,
    ) {
        let px = |v: f32| (v * self.scale).round();
        let (ix, iy) = (
            (slot.x + (slot.w - px(16.0)) / 2.0).round(),
            (slot.y + (slot.h - px(16.0)) / 2.0).round(),
        );
        let t = px(1.5).max(1.0);
        match tool {
            pages::Tool::Pr => {
                // Pull-request mark: two branch endpoints joined to a merge
                // target — hollow circles, a spine, and an elbow.
                let d = px(6.0);
                let circle = |cx: f32, cy: f32| LayoutRect {
                    x: ix + px(cx) - d / 2.0,
                    y: iy + px(cy) - d / 2.0,
                    w: d,
                    h: d,
                };
                for (cx, cy) in [(3.5, 3.5), (3.5, 12.5), (12.5, 12.5)] {
                    quads.push(
                        self.px_rect(&circle(cx, cy), rgb, 0.0, d / 2.0)
                            .border(t, color(rgb, 1.0)),
                    );
                }
                // Left spine between the two branch endpoints.
                quads.push(self.px_rect(
                    &LayoutRect {
                        x: ix + px(3.5) - t / 2.0,
                        y: iy + px(6.5),
                        w: t,
                        h: px(3.0),
                    },
                    rgb,
                    1.0,
                    0.0,
                ));
                // Elbow from the top endpoint over and down into the target.
                quads.push(self.px_rect(
                    &LayoutRect {
                        x: ix + px(6.5),
                        y: iy + px(3.5) - t / 2.0,
                        w: px(6.0) + t / 2.0,
                        h: t,
                    },
                    rgb,
                    1.0,
                    0.0,
                ));
                quads.push(self.px_rect(
                    &LayoutRect {
                        x: ix + px(12.5) - t / 2.0,
                        y: iy + px(3.5),
                        w: t,
                        h: px(6.0),
                    },
                    rgb,
                    1.0,
                    0.0,
                ));
            },
            pages::Tool::LocalDiff => {
                // Diff mark: a `+` over a `−` (an added line above a removed
                // one) — the working-tree review glyph.
                // Plus: horizontal bar…
                quads.push(self.px_rect(
                    &LayoutRect { x: ix + px(3.0), y: iy + px(5.0) - t / 2.0, w: px(10.0), h: t },
                    rgb,
                    1.0,
                    0.0,
                ));
                // …crossed by a vertical bar.
                quads.push(self.px_rect(
                    &LayoutRect { x: ix + px(8.0) - t / 2.0, y: iy + px(2.0), w: t, h: px(6.0) },
                    rgb,
                    1.0,
                    0.0,
                ));
                // Minus below.
                quads.push(self.px_rect(
                    &LayoutRect { x: ix + px(3.0), y: iy + px(12.0) - t / 2.0, w: px(10.0), h: t },
                    rgb,
                    1.0,
                    0.0,
                ));
            },
            pages::Tool::Launch => {
                // Terminal-prompt mark (>_): launch runs a command in a shell.
                carets.push(CaretSpec {
                    cx: ix + px(4.5),
                    cy: iy + px(8.0),
                    size: px(3.5),
                    angle: -std::f32::consts::FRAC_PI_2,
                    color: color(rgb, 1.0),
                });
                quads.push(self.px_rect(
                    &LayoutRect {
                        x: ix + px(9.5),
                        y: iy + px(12.5) - t / 2.0,
                        w: px(5.0),
                        h: t,
                    },
                    rgb,
                    1.0,
                    0.0,
                ));
            },
        }
    }

    /// A glassy sidebar capsule: translucent card fill with the radius
    /// clamped to a true capsule for the rect, plus a hairline ink rim so the
    /// pill reads as glass on the blurred vibrancy ground (iTerm2-style).
    fn pill(&self, r: &LayoutRect, th: &Theme, alpha: f32, radius: f32) -> Quad {
        let rim = (0.5 * self.scale).round().max(1.0);
        self.px_rect(r, th.card, alpha, radius.min(r.w / 2.0).min(r.h / 2.0))
            .border(rim, color(th.ink, 0.22))
    }

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
    use crate::cleanup::Cleanup;

    /// Long profile details truncate with an ellipsis instead of overrunning
    /// the row; degenerate widths drop the detail entirely.
    #[test]
    fn truncate_chars_caps_length_with_ellipsis() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("exactly-ten", 11), "exactly-ten");
        assert_eq!(truncate_chars("a long description · user", 10), "a long de…");
        assert_eq!(truncate_chars("ab", 1), "");
        assert_eq!(truncate_chars("ab", 0), "");
        assert_eq!(truncate_chars("", 0), "");
    }

    fn cleanup_chrome(cleanup: &Cleanup) -> ChromeState<'_> {
        ChromeState {
            page: Page::Cleanup,
            section: Section::ALL[0],
            dot_anim: &[0.0, 0.0, 0.0],
            sections: &[],
            editing_section: None,
            cleanup,
            notes_enabled: false,
            notes_vaults: &[],
            notes_active_vault: 0,
            notes_doc_rels: &[],
            notes_selected_doc: None,
            ribbon_tools: &[],
            open_tool: None,
            tool_panel_w: 0.0,
        tool_panel_floating: false,
            cursor: None,
            settings_query: "",
            settings_search_focus: false,
            preview_dark: false,
            appearance_menu: None,
        }
    }


    /// The tool ribbon renders on every frame; opening a tool adds the panel
    /// card (header + placeholder) and narrows the tile area to make room.
    #[test]
    fn tool_ribbon_and_panel_render() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let tile = crate::workspace::Tile::new(1, crate::term::Session::placeholder());
        let wss = [crate::workspace::Workspace::new("g".into(), tile, None)];

        // Closed: the ribbon icon is there, the panel is not.
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;
        chrome.ribbon_tools = &pages::Tool::ALL;
        let frame = renderer.build_frame(
            &wss, 0, 240.0, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        let texts: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        assert!(!texts.contains(&"Pull Request"));
        let slot = crate::workspace::ribbon_slot_rect(0, 1600, scale);
        // The PR mark is vector geometry: hollow (bordered, zero-alpha)
        // circles inside the slot.
        let d = (6.0 * scale).round();
        assert_eq!(
            frame
                .bg_quads
                .iter()
                .filter(|q| q.w == d
                    && q.radius == d / 2.0
                    && q.border > 0.0
                    && q.x >= slot.x
                    && q.x + q.w <= slot.x + slot.w
                    && q.y >= slot.y
                    && q.y + q.h <= slot.y + slot.h)
                .count(),
            3,
            "ribbon PR icon draws its three branch/merge circles"
        );
        assert!(
            frame.hot.iter().any(|r| r.x == slot.x && r.y == slot.y),
            "ribbon slot is a hover target"
        );
        // Local diff stacks in slot 1 (a hover target), Launch in slot 2 with
        // its prompt chevron.
        let slot1 = crate::workspace::ribbon_slot_rect(1, 1600, scale);
        assert!(frame.hot.iter().any(|r| r.x == slot1.x && r.y == slot1.y));
        let slot2 = crate::workspace::ribbon_slot_rect(2, 1600, scale);
        assert!(frame.hot.iter().any(|r| r.x == slot2.x && r.y == slot2.y));
        assert!(
            frame.carets.iter().any(|c| c.cx >= slot2.x
                && c.cx <= slot2.x + slot2.w
                && c.cy >= slot2.y
                && c.cy <= slot2.y + slot2.h),
            "launch icon chevron renders in the third slot"
        );

        // Open: the panel card paints with header + placeholder, and the tile
        // card stops left of the panel. The PR / Local-diff tools render as
        // gpui element trees (not canvas), so the Launch tool — which still
        // uses the canvas placeholder — exercises the panel-paint path here.
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;
        chrome.ribbon_tools = &pages::Tool::ALL;
        chrome.open_tool = Some(pages::Tool::Launch);
        chrome.tool_panel_w = crate::workspace::TOOL_PANEL_DEFAULT_W;
        let frame = renderer.build_frame(
            &wss, 0, 240.0, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        let texts: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.contains(&"Launch"));
        assert!(texts.contains(&"Launch view coming soon"));
        let panel =
            crate::workspace::tool_panel(1600, 1000, scale, crate::workspace::TOOL_PANEL_DEFAULT_W, false);
        assert!(
            frame.bg_quads.iter().any(|q| q.x == panel.x && q.w == panel.w),
            "panel card quad renders"
        );
        let area = crate::workspace::terminal_area(
            1600,
            1000,
            scale,
            240.0,
            crate::workspace::RIBBON_W + crate::workspace::TOOL_PANEL_DEFAULT_W,
        );
        assert!(
            frame.bg_quads.iter().any(|q| q.x == area.x && q.w == area.w),
            "tile card fills the narrowed area"
        );
        assert!(area.x + area.w <= panel.x, "tiles stop left of the panel");

        // Hovering the panel's left edge paints the same ink-line grip the
        // sidebar edge and dividers get.
        let frame = renderer.build_frame(
            &wss,
            0,
            240.0,
            None,
            Some(&crate::workspace::ResizeHover::ToolPanel),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &chrome,
        );
        let line_w = (2.0 * scale).round();
        assert!(
            frame
                .bg_quads
                .iter()
                .any(|q| q.w == line_w && q.h == panel.h && q.x == panel.x - line_w / 2.0),
            "panel edge grip line renders on hover"
        );

        // No tools registered (non-Sessions pages, or a group without the
        // tool's context): ribbon and panel hide — even with a stale
        // open_tool — and the tiles reclaim the full width.
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;
        chrome.open_tool = Some(pages::Tool::Pr);
        chrome.tool_panel_w = crate::workspace::TOOL_PANEL_DEFAULT_W;
        let frame = renderer.build_frame(
            &wss, 0, 240.0, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        let texts: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        assert!(!texts.contains(&"Pull Request"));
        assert!(
            !frame.bg_quads.iter().any(|q| q.w == d && q.radius == d / 2.0 && q.border > 0.0),
            "no ribbon icons without registered tools"
        );
        let full = crate::workspace::terminal_area(1600, 1000, scale, 240.0, 0.0);
        assert!(
            frame.bg_quads.iter().any(|q| q.x == full.x && q.w == full.w),
            "tile card reclaims the ribbon's width"
        );
    }

    /// The Settings sidebar renders the search box's placeholder in the top
    /// slot and the section tabs one slot down, off the same rect helpers
    /// main.rs hit-tests.
    #[test]
    fn settings_sidebar_shows_search_placeholder_above_tabs() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Settings;
        let ws = crate::workspace::Workspace::new(
            "g".into(),
            crate::workspace::Tile::empty(1),
            None,
        );
        let sidebar_w = 240.0;
        let frame = renderer.build_frame(
            &[ws], 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        let search = crate::workspace::settings_search_rect(scale, sidebar_w);
        let search_y = (search.y + (search.h - renderer.chrome_cell_height) / 2.0).round();
        assert!(
            frame.labels.iter().any(|l| l.text == "Search settings" && l.top == search_y),
            "placeholder sits in the top sidebar slot"
        );
        let tab = crate::workspace::tab_rect(1, scale, sidebar_w);
        let tab_y = (tab.y + (tab.h - renderer.chrome_cell_height) / 2.0).round();
        assert!(
            frame.labels.iter().any(|l| l.text == "Sessions" && l.top == tab_y),
            "first section tab shifts down one slot below the search box"
        );
    }

    /// The Settings content area is the gpui overlay in settings_ui — the
    /// canvas must not paint the terminal tiles (or their hot rects)
    /// underneath it, same as on Cleanup.
    #[test]
    fn settings_page_paints_sidebar_only() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Settings;
        let ws = crate::workspace::Workspace::new(
            "g".into(),
            crate::workspace::Tile::empty(1),
            None,
        );
        let sidebar_w = 240.0;
        let settings_frame = renderer.build_frame(
            &[ws], 0, sidebar_w, None, None, None, None, None, None, None, None, None, None,
            &chrome,
        );
        chrome.page = Page::Sessions;
        let ws = crate::workspace::Workspace::new(
            "g".into(),
            crate::workspace::Tile::empty(1),
            None,
        );
        let sessions_frame = renderer.build_frame(
            &[ws], 0, sidebar_w, None, None, None, None, None, None, None, None, None, None,
            &chrome,
        );
        assert!(
            settings_frame.bg_quads.len() < sessions_frame.bg_quads.len(),
            "Settings ({} quads) must skip the tile cards the Sessions page paints ({} quads)",
            settings_frame.bg_quads.len(),
            sessions_frame.bg_quads.len()
        );
    }

    #[test]
    fn appearance_page_draws_dropdowns_previews_and_footer() {
        let renderer = Renderer::new(2.0, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Settings;
        chrome.section = Section::Appearance;
        let tile = crate::workspace::Tile::new(1, crate::term::Session::placeholder());
        let wss = [crate::workspace::Workspace::new("g".into(), tile, None)];
        let frame = renderer.build_frame(
            &wss, 0, 240.0, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        let texts: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        for expected in [
            "Mode",
            "Preview",
            "App Theme",
            "Terminal Colors",
            "Light Theme",
            "Dark Theme",
            "Light Profile",
            "Dark Profile",
            "Import from Clipboard",
            "Copy Theme String",
            "changes apply live",
        ] {
            assert!(texts.contains(&expected), "missing label {expected:?}");
        }
        // Both preview cards announce the scheme they render.
        assert!(
            texts.iter().filter(|t| t.ends_with("· Preview")).count() >= 2,
            "expected two preview captions, got {texts:?}"
        );
    }

    #[test]
    fn appearance_preview_toggle_switches_polarity() {
        let renderer = Renderer::new(2.0, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let tile = crate::workspace::Tile::new(1, crate::term::Session::placeholder());
        let wss = [crate::workspace::Workspace::new("g".into(), tile, None)];
        for (dark, label) in [(false, "Arc Light · Preview"), (true, "Midnight · Preview")] {
            let mut chrome = cleanup_chrome(&state);
            chrome.page = Page::Settings;
            chrome.section = Section::Appearance;
            chrome.preview_dark = dark;
            let frame = renderer.build_frame(
                &wss, 0, 240.0, None, None, None, None, None, None, None, None, None, None,
                &chrome,
            );
            assert!(
                frame.labels.iter().any(|l| l.text == label),
                "preview_dark={dark} should render {label:?}"
            );
        }
    }

    #[test]
    fn appearance_menu_lists_polarity_options_above_the_page() {
        let renderer = Renderer::new(2.0, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Settings;
        chrome.section = Section::Appearance;
        chrome.appearance_menu = Some(crate::pages::AppearanceDropdown::ThemeDark);
        let tile = crate::workspace::Tile::new(1, crate::term::Session::placeholder());
        let wss = [crate::workspace::Workspace::new("g".into(), tile, None)];
        let frame = renderer.build_frame(
            &wss, 0, 240.0, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        let menu_texts: Vec<&str> = frame.picker_labels.iter().map(|l| l.text.as_str()).collect();
        for t in crate::theme::ALL {
            assert!(menu_texts.contains(&t.label), "menu missing {:?}", t.label);
        }
        // The dark slot's menu sorts dark themes above light ones.
        let pos = |label: &str| menu_texts.iter().position(|t| *t == label).unwrap();
        assert!(pos("Midnight") < pos("Arc Light"), "own polarity should sort first");
        assert!(!frame.picker_quads.is_empty(), "menu panel should paint in the overlay layer");
    }


    /// The sidebar unread dot sits in the row's left padding gutter, not at
    /// the card's right edge.
    #[test]
    fn sidebar_unread_dot_sits_in_left_gutter() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;

        let mut tile = crate::workspace::Tile::new(1, crate::term::Session::placeholder());
        if let Some(tab) = tile.active_tab_mut() {
            tab.unread = true;
        }
        let wss = [crate::workspace::Workspace::new("g".into(), tile, None)];

        let sidebar_w = 240.0;
        let frame = renderer.build_frame(
            &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );

        let rows = crate::workspace::sidebar_rows(&wss, &[]);
        let row = crate::workspace::sidebar_row_rect(&rows, 0, &wss, scale, sidebar_w);
        let group_pad = (12.0 * scale).round();
        let ds = (7.0 * scale).round();
        let expected_x = (row.x + (group_pad - ds) / 2.0).round();
        let expected_y = (row.y + (row.h - ds) / 2.0).round();
        assert!(
            frame.bg_quads.iter().any(|q| q.w == ds
                && q.h == ds
                && q.radius == ds / 2.0
                && q.x == expected_x
                && q.y == expected_y),
            "unread dot should sit in the left gutter, centered on the group row"
        );
    }

    /// The section-header unread dot sits in the left gutter when any member
    /// workspace has an unread tab — for both collapsed and expanded sections.
    #[test]
    fn sidebar_section_header_unread_dot_sits_in_left_gutter() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();

        let section_id: u64 = 42;

        for &collapsed in &[false, true] {
            let mut chrome = cleanup_chrome(&state);
            chrome.page = Page::Sessions;

            // Build a workspace assigned to the section with an unread tab.
            let mut tile = crate::workspace::Tile::new(1, crate::term::Session::placeholder());
            if let Some(tab) = tile.active_tab_mut() {
                tab.unread = true;
            }
            let mut ws = crate::workspace::Workspace::new("g".into(), tile, None);
            ws.section = Some(section_id);
            let wss = [ws];

            let sections = [crate::workspace::Section {
                id: section_id,
                name: "MySection".into(),
                emoji: "🔥".into(),
                collapsed,
                anchor: None,
            }];
            chrome.sections = &sections;

            let sidebar_w = 240.0;
            let frame = renderer.build_frame(
                &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
            );

            // The section header is always row 0.
            let rows = crate::workspace::sidebar_rows(&wss, &sections);
            let header_row = crate::workspace::sidebar_row_rect(&rows, 0, &wss, scale, sidebar_w);
            let group_pad = (12.0 * scale).round();
            let ds = (7.0 * scale).round();
            let expected_x = (header_row.x + (group_pad - ds) / 2.0).round();

            assert!(
                frame.bg_quads.iter().any(|q| q.w == ds
                    && q.h == ds
                    && q.radius == ds / 2.0
                    && q.x == expected_x
                    && q.y >= header_row.y
                    && q.y + q.h <= header_row.y + header_row.h),
                "unread dot should sit in the left gutter of the section header row (collapsed={collapsed})"
            );
        }
    }

    /// Hovering the "+ group" button brightens it and adds the soft shadow;
    /// without a cursor the button stays in its resting style.
    #[test]
    fn new_group_button_hover_brightens_and_soft_shadows() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;
        let sidebar_w = 240.0;
        let btn = crate::workspace::new_group_button(scale, sidebar_w);
        let wss = [crate::workspace::Workspace::new(
            "g".into(),
            crate::workspace::Tile::empty(1),
            None,
        )];

        let quad_at_btn = |frame: &Frame| {
            frame
                .bg_quads
                .iter()
                .find(|q| q.x == btn.x && q.y == btn.y && q.w == btn.w && q.h == btn.h)
                .map(|q| q.shadow == Shadow::Soft)
        };

        let resting = renderer.build_frame(
            &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        assert_eq!(quad_at_btn(&resting), Some(false), "resting button has no soft shadow");
        assert!(resting.hot.iter().any(|r| r.x == btn.x && r.y == btn.y), "button is hot");

        chrome.cursor = Some((btn.x + btn.w / 2.0, btn.y + btn.h / 2.0));
        let hovered = renderer.build_frame(
            &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        assert_eq!(quad_at_btn(&hovered), Some(true), "hovered button gains the soft shadow");
    }

    /// A modal overlay owns the frame's hot list: only its elements register,
    /// never the chrome underneath.
    #[test]
    fn frame_hot_scopes_to_overlay_when_modal() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;
        let sidebar_w = 240.0;
        let btn = crate::workspace::new_group_button(scale, sidebar_w);
        let wss = [crate::workspace::Workspace::new(
            "g".into(),
            crate::workspace::Tile::empty(1),
            None,
        )];

        let plain = renderer.build_frame(
            &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        assert!(!plain.hot.is_empty());
        assert!(plain.hot.iter().any(|r| r.x == btn.x && r.y == btn.y));

        let confirm = renderer.build_frame(
            &wss,
            0,
            sidebar_w,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(("Close tab?", "Close")),
            &chrome,
        );
        // Exactly the dialog's Cancel and accept buttons are interactive.
        assert_eq!(confirm.hot.len(), 2, "confirm dialog exposes only its two buttons");
        assert!(!confirm.hot.iter().any(|r| r.x == btn.x && r.y == btn.y));
    }

    /// A hovered inactive sidebar group row gains the shadowless hover pill;
    /// without a cursor no quad is painted for it at all.
    #[test]
    fn hovered_inactive_sidebar_row_gains_pill() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let state = Cleanup::default();
        let mut chrome = cleanup_chrome(&state);
        chrome.page = Page::Sessions;
        let sidebar_w = 240.0;
        let wss = [
            crate::workspace::Workspace::new("a".into(), crate::workspace::Tile::empty(1), None),
            crate::workspace::Workspace::new("b".into(), crate::workspace::Tile::empty(2), None),
        ];
        let rows = crate::workspace::sidebar_rows(&wss, &[]);
        let row = crate::workspace::sidebar_row_rect(&rows, 1, &wss, scale, sidebar_w);
        let row_quad = |frame: &Frame| {
            frame
                .bg_quads
                .iter()
                .find(|q| q.x == row.x && q.y == row.y && q.w == row.w && q.h == row.h)
                .map(|q| q.shadow)
        };

        let resting = renderer.build_frame(
            &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        assert_eq!(row_quad(&resting), None, "inactive row paints no pill at rest");

        chrome.cursor = Some((row.x + row.w / 2.0, row.y + row.h / 2.0));
        let hovered = renderer.build_frame(
            &wss, 0, sidebar_w, None, None, None, None, None, None, None, None, None, None, &chrome,
        );
        assert_eq!(
            row_quad(&hovered),
            Some(Shadow::None),
            "hovered inactive row gains the shadowless pill"
        );
    }

    /// Hovering a flyover tab's × registers it hot and paints the chip; with
    /// no cursor the strip stays in its resting style.
    #[test]
    fn flyover_close_hover_paints_chip_and_registers_hot() {
        let scale = 2.0;
        let renderer = Renderer::new(scale, 18.0, 1600, 1000);
        let panel = crate::workspace::flyover_rect(1600, 1000, scale, 1.0, 0.35, false);
        let tabs = [crate::workspace::Tab::new(crate::term::Session::placeholder())];
        let close = crate::workspace::flyover_tab_close_rect(&panel, 0, 1, scale, false);

        let mut hot = Vec::new();
        let (resting_quads, ..) = renderer
            .flyover_overlay(&tabs, 0, &panel, true, false, true, false, None, &mut hot);
        // Tab, its ×, and the two window buttons are all interactive.
        assert_eq!(hot.len(), 4, "tab + close + minimize + maximize are hot");
        assert!(hot.iter().any(|r| r.x == close.x && r.y == close.y));

        let cursor = Some((close.x + close.w / 2.0, close.y + close.h / 2.0));
        let mut hot2 = Vec::new();
        let (hovered_quads, ..) = renderer
            .flyover_overlay(&tabs, 0, &panel, true, false, true, false, cursor, &mut hot2);
        assert_eq!(
            hovered_quads.len(),
            resting_quads.len() + 1,
            "hovering the × adds exactly the chip quad"
        );
    }


}
