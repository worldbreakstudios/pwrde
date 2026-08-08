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

use crate::pages::{self, Action, Page, Section};
use crate::picker::{ForkPicker, Picker, PickerLayout, PickerRow};
use crate::rect::char_rects;
use crate::term::Session;
use crate::theme::Theme;
use crate::workspace::{self, LayoutRect, Workspace};

const FONT_SIZE: f32 = 15.0;
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
/// Corner radius of the sidebar's rounded rows, logical px.
const ROW_RADIUS: f32 = 9.0;

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
#[derive(Clone, Copy, PartialEq)]
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

/// Button labels of the confirm dialog (shared by layout and drawing).
const CONFIRM_CANCEL: &str = "Cancel";
const CONFIRM_CLOSE: &str = "Close group";

/// The confirm dialog's rects, in physical px.
pub struct ConfirmLayout {
    pub panel: LayoutRect,
    pub cancel: LayoutRect,
    pub close: LayoutRect,
}

/// Per-frame page/navigation state the renderer needs beyond the workspaces:
/// which page is up, which settings section, the dot-strip animation
/// progresses (0..1 per page), and the keyboard row being rebound, if any.
pub struct ChromeState<'a> {
    pub page: Page,
    pub section: Section,
    pub dot_anim: &'a [f32],
    pub recording: Option<Action>,
    /// In-progress edit buffer for the primary-command settings row, if the
    /// row is being edited.
    pub editing_command: Option<&'a str>,
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
    /// Picker overlay fills painted over everything else (scrim, panel, rows).
    pub picker_quads: Vec<Quad>,
    /// Picker overlay labels, painted last.
    pub picker_labels: Vec<LabelSpec>,
}

/// Stateless renderer: owns only cell metrics and scale. All measurements
/// come from gpui's text system (see `main.rs`), so `new` takes them as
/// arguments instead of creating a GPU surface. The terminal color palette is
/// resolved from settings once per frame in [`Renderer::build_frame`].
pub struct Renderer {
    width: u32,
    height: u32,

    pub scale: f32,
    pub cell_width: f32,
    pub cell_height: f32,
}

/// Measure the advance width of a monospace cell (physical px) by shaping a
/// representative glyph at the current font size in gpui's text system.
pub fn measure_cell_width(window: &mut gpui::Window, scale: f32) -> f32 {
    let font_size = gpui::px(FONT_SIZE * scale);
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
        };
        renderer.update_scale(scale, cell_width);
        renderer
    }

    /// Recompute cell metrics for a new display scale (the window moved to a
    /// monitor with a different backing scale factor). `cell_width` must be
    /// re-measured by the caller at the new scale via [`measure_cell_width`].
    pub fn update_scale(&mut self, scale: f32, cell_width: f32) {
        self.scale = scale;
        self.cell_width = cell_width.round();
        self.cell_height = (FONT_SIZE * scale * LINE_HEIGHT_FACTOR).round();
    }

    /// Physical-px font size for shaping (logical size × scale).
    pub fn font_size(&self) -> f32 {
        FONT_SIZE * self.scale
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
        picker: Option<&Picker>,
        fork: Option<&ForkPicker>,
        message: Option<&(String, bool)>,
        confirm: Option<&str>,
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
        let area = workspace::terminal_area(width, height, self.scale, sidebar_w);
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

        // ── Sidebar chrome (identical geometry on every page) ──────────
        // The window gradient is painted by `main.rs` before these quads;
        // the sidebar itself is transparent — its rounded rows float on it.
        let row_r = (ROW_RADIUS * self.scale).round();
        let group_pad = (12.0 * self.scale).round();
        // Traffic lights are the native macOS buttons now (transparent titlebar),
        // so we no longer draw our own here.
        match chrome.page {
            Page::Sessions => {
                // "+" new-group button: a translucent rounded field below the
                // titlebar, then one row per group.
                let new_group = workspace::new_group_button(self.scale, sidebar_w);
                bg_quads.push(self.px_rect(&new_group, th.card, 0.55, row_r));
                labels.push(LabelSpec {
                    text: "+ new group".into(),
                    color: color(th.ink_dim, 1.0),
                    left: (new_group.x + group_pad).round(),
                    top: (new_group.y + (new_group.h - self.cell_height) / 2.0).round(),
                    clip: new_group,
                    size: None,
                });
                if empty {
                    // Empty state: a centered CTA instead of group rows.
                    let cta = workspace::empty_state_cta(width, height, self.scale, sidebar_w);
                    let hint = workspace::empty_state_hint(width, height, self.scale, sidebar_w);
                    bg_quads.push(self.px_rect(&cta, th.card, 0.62, row_r).shadow(Shadow::Soft));
                    let cta_text = "New group";
                    let cta_w = cta_text.chars().count() as f32 * self.cell_width;
                    labels.push(LabelSpec {
                        text: cta_text.into(),
                        color: color(th.ink, 1.0),
                        left: (cta.x + ((cta.w - cta_w) / 2.0).max(0.0)).round(),
                        top: (cta.y + (cta.h - self.cell_height) / 2.0).round(),
                        clip: cta,
                        size: None,
                    });
                    let hint_text = "press ⇧⌘T";
                    let hint_w = hint_text.chars().count() as f32 * self.cell_width;
                    labels.push(LabelSpec {
                        text: hint_text.into(),
                        color: color(th.ink_dim, 0.9),
                        left: (hint.x + ((hint.w - hint_w) / 2.0).max(0.0)).round(),
                        top: (hint.y + (hint.h - self.cell_height) / 2.0).round(),
                        clip: hint,
                        size: None,
                    });
                } else {
                    // Group cards are two lines: the primary pane's title on
                    // top, the group's cwd below in smaller, dimmer text.
                    let cwd_size = self.font_size() * 0.85;
                    let cwd_line_h = self.cell_height * 0.85;
                    for (i, ws_item) in workspaces.iter().enumerate() {
                        let tab = workspace::tab_rect(i, self.scale, sidebar_w);
                        if i == active {
                            // Active row: a raised rounded pill with a subtle shadow.
                            bg_quads
                                .push(self.px_rect(&tab, th.card, 0.78, row_r).shadow(Shadow::Soft));
                        }
                        let inset = ((tab.h - (self.cell_height + cwd_line_h)) / 2.0).max(0.0);
                        labels.push(LabelSpec {
                            text: ws_item.title(),
                            color: color(if i == active { th.ink } else { th.ink_dim }, 1.0),
                            left: tab.x + group_pad,
                            top: (tab.y + inset).round(),
                            clip: LayoutRect { w: tab.w - group_pad, ..tab },
                            size: None,
                        });
                        labels.push(LabelSpec {
                            text: workspace::display_cwd(ws_item.cwd.as_deref()),
                            color: color(th.ink_dim, 0.8),
                            left: tab.x + group_pad,
                            top: (tab.y + inset + self.cell_height).round(),
                            clip: LayoutRect { w: tab.w - group_pad, ..tab },
                            size: Some(cwd_size),
                        });
                    }
                }
            },
            Page::Settings => {
                // Settings sections as sidebar tabs, in the same rows the
                // groups occupy on Sessions so the chrome reads as one.
                for (i, section) in Section::ALL.iter().enumerate() {
                    let tab = workspace::tab_rect(i, self.scale, sidebar_w);
                    let active_row = *section == chrome.section;
                    if active_row {
                        bg_quads.push(self.px_rect(&tab, th.card, 0.78, row_r).shadow(Shadow::Soft));
                    }
                    labels.push(LabelSpec {
                        text: section.label().into(),
                        color: color(if active_row { th.ink } else { th.ink_dim }, 1.0),
                        left: tab.x + group_pad,
                        top: (tab.y + (tab.h - self.cell_height) / 2.0).round(),
                        clip: LayoutRect { w: tab.w - group_pad, ..tab },
                        size: None,
                    });
                }
            },
        }

        // ── Page-dot strip (bottom of the sidebar, every page) ─────────
        // Each slot crossfades between a subtle dot and the page's glyph as
        // its animation progress moves 0 → 1 (hovered or active page).
        let n_pages = Page::ALL.len();
        for (i, page) in Page::ALL.iter().enumerate() {
            let slot = workspace::page_slot_rect(i, n_pages, height, self.scale, sidebar_w);
            let p = chrome.dot_anim.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
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
                let gw = glyph.chars().count() as f32 * self.cell_width;
                labels.push(LabelSpec {
                    text: glyph.into(),
                    color: color(th.ink, p),
                    left: (slot.x + (slot.w - gw) / 2.0).round(),
                    // Glyphs may be a hair wider than the slot ("<>"): allow
                    // a small clip overhang so they aren't shaved.
                    top: (slot.y + (slot.h - self.cell_height) / 2.0).round(),
                    clip: slot.inflate((4.0 * self.scale).round()),
                    size: None,
                });
            }
        }

        let card_r = (CARD_RADIUS * self.scale).round();

        if chrome.page == Page::Settings {
            // ── Settings page: one tile-style card in the content area ──
            self.settings_page(&area, chrome, workspaces, active, &mut bg_quads, &mut labels);
        } else {
            let hair = (1.0 * self.scale).round().max(1.0);
            for (id, rect) in &tiles {
                // Each tile is a floating dark card: rounded, shadowed, with its
                // tab strip inside the card above a hairline divider.
                bg_quads.push(self.px_rect(rect, pane_bg, 1.0, card_r).shadow(Shadow::Card));
                let bar = workspace::tile_tab_bar(rect, self.scale);
                let divider = LayoutRect { x: rect.x, y: bar.y + bar.h - hair, w: rect.w, h: hair };
                bg_quads.push(self.px_rect(&divider, pane_divider.0, pane_divider.1, 0.0));
                if let Some(tile) = ws.root.find_tile(*id) {
                    // Active tab: a subtle rounded pill inside the strip (white
                    // works on every theme's dark card).
                    let tr = workspace::tile_tab_rect(rect, tile.active, tile.tabs.len(), self.scale);
                    let m = (4.0 * self.scale).round();
                    let pill = LayoutRect {
                        x: tr.x + m,
                        y: tr.y + m,
                        w: (tr.w - 2.0 * m).max(0.0),
                        h: (tr.h - 2.0 * m).max(0.0),
                    };
                    bg_quads.push(self.px_rect(&pill, pane_pill.0, pane_pill.1, (7.0 * self.scale).round()));
                }
            }

            // ── Terminal snapshots + per-tile chrome (tab strips) ──────────
            let focused_tile = Some(ws.focused_tile);
            for (id, rect) in &tiles {
                let Some(tile) = ws.root.find_tile(*id) else { continue };
                let content = workspace::tile_content(rect, self.scale);
                let origin = self.content_origin(&content);
                if let Some(session) = tile.tabs.get(tile.active).map(|t| &t.session) {
                    let draw_cursor = Some(*id) == focused_tile
                        && picker.is_none()
                        && fork.is_none()
                        && message.is_none()
                        && confirm.is_none();
                    let rows = self.snapshot_pane(
                        session,
                        &term_palette,
                        origin,
                        draw_cursor,
                        &mut bg_quads,
                        &mut fg_quads,
                    );
                    panes.push(PaneText { origin, rows });
                    self.selection_rects(session, origin, &mut fg_quads);
                }

                // Tab labels for this tile's tab strip.
                let tab_text_pad = (8.0 * self.scale).round();
                for (ti, tab) in tile.tabs.iter().enumerate() {
                    let tr = workspace::tile_tab_rect(rect, ti, tile.tabs.len(), self.scale);
                    let title = tab.session.title();
                    let text = if title.is_empty() { "shell".to_string() } else { title };
                    labels.push(LabelSpec {
                        text,
                        color: if ti == tile.active {
                            color(pane_ink.0, pane_ink.1)
                        } else {
                            color(pane_ink_dim.0, pane_ink_dim.1)
                        },
                        left: tr.x + tab_text_pad,
                        top: (tr.y + (tr.h - self.cell_height) / 2.0).round(),
                        clip: LayoutRect { w: tr.w - tab_text_pad, ..tr },
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
            let w = text.chars().count() as f32 * self.cell_width;
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
        let mut picker_quads: Vec<Quad> = Vec::new();
        let mut picker_labels: Vec<LabelSpec> = Vec::new();
        if let Some(text) = confirm {
            picker_labels = self.confirm_overlay(text, &mut picker_quads);
        } else if let Some(p) = picker {
            let layout = PickerLayout::compute(width, height, self.scale, p.rows.len(), p.selected);
            picker_labels = self.picker_overlay(p, &layout, &mut picker_quads);
        } else if let Some(f) = fork {
            picker_labels = self.fork_overlay(f, &mut picker_quads);
        } else if let Some((text, _)) = message {
            picker_labels = self.message_overlay(text, &mut picker_quads);
        }

        Frame { bg_quads, panes, fg_quads, labels, picker_quads, picker_labels }
    }

    /// The Settings page: a single card styled exactly like a terminal tile
    /// (same fill, radius, shadow) filling the content area, holding the
    /// active section's rows. Row geometry comes from
    /// `workspace::settings_row_rect` so `main.rs` hit-tests the same pixels.
    fn settings_page(
        &self,
        area: &LayoutRect,
        chrome: &ChromeState,
        workspaces: &[Workspace],
        active: usize,
        bg_quads: &mut Vec<Quad>,
        labels: &mut Vec<LabelSpec>,
    ) {
        let th = self.theme();
        let scale = self.scale;
        let pad = (14.0 * scale).round();
        let pill_r = (7.0 * scale).round();
        bg_quads.push(self.px_rect(area, th.term_bg, 1.0, (CARD_RADIUS * scale).round()).shadow(Shadow::Card));

        let header_h = (workspace::SETTINGS_HEADER_H * scale).round();
        labels.push(LabelSpec {
            text: chrome.section.label().into(),
            color: color(th.text_bright, 1.0),
            left: area.x + pad,
            top: (area.y + (header_h - self.cell_height) / 2.0).round(),
            clip: *area,
            size: None,
        });

        // Rows that would spill past the card bottom are dropped, not clipped
        // mid-glyph.
        let fits = |row: &LayoutRect| row.y + row.h <= area.y + area.h - pad;
        let mid = |row: &LayoutRect| (row.y + (row.h - self.cell_height) / 2.0).round();

        match chrome.section {
            Section::Sessions => {
                let row = workspace::settings_row_rect(area, 0, scale);
                if fits(&row) {
                    let editing = chrome.editing_command.is_some();
                    if editing {
                        bg_quads.push(self.px_rect(&row, th.accent, 0.18, pill_r));
                    }
                    labels.push(LabelSpec {
                        text: "Primary command".into(),
                        color: color(th.text_bright, 1.0),
                        left: row.x + pad,
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                    let value = match chrome.editing_command {
                        Some(buf) => buf.to_string(),
                        None => crate::settings::primary_command(),
                    };
                    let caret_w = (2.0 * scale).round().max(1.0);
                    let w = value.chars().count() as f32 * self.cell_width;
                    let right = row.x + row.w - pad - if editing { caret_w + 2.0 } else { 0.0 };
                    labels.push(LabelSpec {
                        text: value,
                        color: color(if editing { th.text_bright } else { th.text_dim }, 1.0),
                        left: (right - w).round(),
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                    if editing {
                        let caret = LayoutRect {
                            x: right.round(),
                            y: mid(&row),
                            w: caret_w,
                            h: self.cell_height,
                        };
                        bg_quads.push(self.px_rect(&caret, th.accent, 1.0, 0.0));
                    }
                }
                let hint = workspace::settings_row_rect(area, 1, scale);
                if fits(&hint) {
                    let text = if chrome.editing_command.is_some() {
                        "type a command… (enter saves, esc cancels)"
                    } else {
                        "runs in the primary pane when a group opens"
                    };
                    labels.push(LabelSpec {
                        text: text.into(),
                        color: color(th.text_dim, 1.0),
                        left: hint.x + pad,
                        top: mid(&hint),
                        clip: hint,
                        size: None,
                    });
                }
            },
            Section::Keyboard => {
                for (i, action) in Action::ALL.iter().enumerate() {
                    let row = workspace::settings_row_rect(area, i, scale);
                    if !fits(&row) {
                        break;
                    }
                    let recording = chrome.recording == Some(*action);
                    if recording {
                        bg_quads.push(self.px_rect(&row, th.accent, 0.18, pill_r));
                    }
                    labels.push(LabelSpec {
                        text: action.label().into(),
                        color: color(th.text_bright, 1.0),
                        left: row.x + pad,
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                    let value = if recording {
                        "press keys… (esc cancels)".to_string()
                    } else {
                        action.binding().display()
                    };
                    let w = value.chars().count() as f32 * self.cell_width;
                    labels.push(LabelSpec {
                        text: value,
                        color: color(if recording { th.text_bright } else { th.text_dim }, 1.0),
                        left: (row.x + row.w - pad - w).round(),
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                }
            },
            Section::Appearance => {
                let dark_now = crate::theme::dark_active();
                let mode = crate::theme::mode();
                for (row_i, col, item) in pages::appearance_layout() {
                    let slot =
                        workspace::appearance_slot_rect(area, row_i, col, item.full_width(), scale);
                    if !fits(&slot) {
                        break;
                    }
                    match item {
                        pages::AppearanceItem::Mode => {
                            labels.push(LabelSpec {
                                text: "Mode".into(),
                                color: color(th.text_bright, 1.0),
                                left: slot.x + pad,
                                top: mid(&slot),
                                clip: slot,
                                size: None,
                            });
                            for (i, m) in crate::theme::Mode::ALL.iter().enumerate() {
                                let seg = workspace::mode_segment_rect(
                                    &slot,
                                    i,
                                    self.cell_width,
                                    scale,
                                );
                                let on = *m == mode;
                                bg_quads.push(self.px_rect(
                                    &seg,
                                    if on { th.accent } else { (255, 255, 255) },
                                    if on { 0.9 } else { 0.12 },
                                    seg.h / 2.0,
                                ));
                                let lw = m.label().chars().count() as f32 * self.cell_width;
                                labels.push(LabelSpec {
                                    text: m.label().into(),
                                    color: color(if on { (255, 255, 255) } else { th.text_dim }, 1.0),
                                    left: (seg.x + (seg.w - lw) / 2.0).round(),
                                    top: mid(&slot),
                                    clip: seg,
                                    size: None,
                                });
                            }
                        },
                        pages::AppearanceItem::Header(text) => {
                            labels.push(LabelSpec {
                                text: text.into(),
                                color: color(th.text_dim, 1.0),
                                left: slot.x + pad,
                                top: mid(&slot),
                                clip: slot,
                                size: None,
                            });
                        },
                        pages::AppearanceItem::Theme(t) => {
                            let picked = crate::theme::selected(t.dark).name == t.name;
                            self.appearance_slot(
                                &slot,
                                t.label,
                                picked,
                                picked && t.dark == dark_now,
                                None,
                                bg_quads,
                                labels,
                            );
                        },
                        pages::AppearanceItem::TermDefault => {
                            let picked = crate::term_theme::selected(dark_now).is_none();
                            self.appearance_slot(
                                &slot, "Default", picked, picked, None, bg_quads, labels,
                            );
                        },
                        pages::AppearanceItem::Term(t) => {
                            let picked = crate::term_theme::selected(t.dark)
                                .is_some_and(|s| s.name == t.name);
                            self.appearance_slot(
                                &slot,
                                t.label,
                                picked,
                                picked && t.dark == dark_now,
                                Some(&t.ansi),
                                bg_quads,
                                labels,
                            );
                        },
                    }
                }
            },
            Section::Terminal => {
                let row = workspace::settings_row_rect(area, pages::PERSIST_TOGGLE_ROW, scale);
                if fits(&row) {
                    let on = crate::settings::get_bool("terminal.persist", false);
                    labels.push(LabelSpec {
                        text: "Persist sessions".into(),
                        color: color(th.text_bright, 1.0),
                        left: row.x + pad,
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                    let state = if on { "on" } else { "off" };
                    let w = state.chars().count() as f32 * self.cell_width;
                    let pill_pad = (10.0 * scale).round();
                    let inset = (4.0 * scale).round();
                    let pill = LayoutRect {
                        x: (row.x + row.w - pad - w - 2.0 * pill_pad).round(),
                        y: row.y + inset,
                        w: w + 2.0 * pill_pad,
                        h: (row.h - 2.0 * inset).max(0.0),
                    };
                    bg_quads.push(self.px_rect(
                        &pill,
                        if on { th.accent } else { (255, 255, 255) },
                        if on { 0.9 } else { 0.12 },
                        pill.h / 2.0,
                    ));
                    labels.push(LabelSpec {
                        text: state.into(),
                        color: color(if on { (255, 255, 255) } else { th.text_dim }, 1.0),
                        left: (pill.x + pill_pad).round(),
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                }
            },
            Section::Debug => {
                let diags: Vec<(&str, String)> = vec![
                    ("settings file", crate::settings::path().to_string_lossy().into_owned()),
                    ("theme", th.label.into()),
                    ("scale", format!("{:.2}", self.scale)),
                    ("surface", format!("{}×{} px", self.width, self.height)),
                    ("cell", format!("{}×{} px", self.cell_width, self.cell_height)),
                    ("workspaces", workspaces.len().to_string()),
                    ("tiles (active group)", workspaces[active].root.tiles().len().to_string()),
                ];
                let value_col = (180.0 * scale).round();
                for (i, (key, value)) in diags.iter().enumerate() {
                    let row = workspace::settings_row_rect(area, i, scale);
                    if !fits(&row) {
                        break;
                    }
                    labels.push(LabelSpec {
                        text: (*key).into(),
                        color: color(th.text_dim, 1.0),
                        left: row.x + pad,
                        top: mid(&row),
                        clip: LayoutRect { w: (value_col - 2.0 * pad).max(0.0), ..row },
                        size: None,
                    });
                    labels.push(LabelSpec {
                        text: value.clone(),
                        color: color(th.text_bright, 1.0),
                        left: row.x + value_col,
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                }
                // The one functional toggle, separated from the diagnostics.
                let row = workspace::settings_row_rect(area, pages::DEBUG_TOGGLE_ROW, scale);
                if fits(&row) {
                    let on = crate::settings::get_bool("debug.overlay", false);
                    labels.push(LabelSpec {
                        text: "Show frame stats".into(),
                        color: color(th.text_bright, 1.0),
                        left: row.x + pad,
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                    let state = if on { "on" } else { "off" };
                    let w = state.chars().count() as f32 * self.cell_width;
                    let pill_pad = (10.0 * scale).round();
                    let inset = (4.0 * scale).round();
                    let pill = LayoutRect {
                        x: (row.x + row.w - pad - w - 2.0 * pill_pad).round(),
                        y: row.y + inset,
                        w: w + 2.0 * pill_pad,
                        h: (row.h - 2.0 * inset).max(0.0),
                    };
                    bg_quads.push(self.px_rect(
                        &pill,
                        if on { th.accent } else { (255, 255, 255) },
                        if on { 0.9 } else { 0.12 },
                        pill.h / 2.0,
                    ));
                    labels.push(LabelSpec {
                        text: state.into(),
                        color: color(if on { (255, 255, 255) } else { th.text_dim }, 1.0),
                        left: (pill.x + pill_pad).round(),
                        top: mid(&row),
                        clip: row,
                        size: None,
                    });
                }
            },
        }
    }

    /// One half-width Appearance slot: a pill with the entry's label, ANSI
    /// preview chips for terminal schemes, and an accent dot on the entry the
    /// resolved mode is actually applying. `picked` marks the entry its own
    /// polarity slot points at (both slots stay visible at once).
    #[allow(clippy::too_many_arguments)]
    fn appearance_slot(
        &self,
        slot: &LayoutRect,
        label: &str,
        picked: bool,
        applied: bool,
        chips: Option<&[(u8, u8, u8); 8]>,
        bg_quads: &mut Vec<Quad>,
        labels: &mut Vec<LabelSpec>,
    ) {
        let th = self.theme();
        let scale = self.scale;
        let pad = (10.0 * scale).round();
        let inset = (2.0 * scale).round();
        let mid = (slot.y + (slot.h - self.cell_height) / 2.0).round();
        let pill = LayoutRect {
            y: slot.y + inset,
            h: (slot.h - 2.0 * inset).max(0.0),
            ..*slot
        };
        bg_quads.push(self.px_rect(
            &pill,
            if picked { th.accent } else { (255, 255, 255) },
            if picked { 0.14 } else { 0.06 },
            (7.0 * scale).round(),
        ));
        // The dot column is always reserved so chips align across rows.
        if applied {
            labels.push(LabelSpec {
                text: "●".into(),
                color: color(th.accent, 1.0),
                left: (slot.x + slot.w - pad - self.cell_width).round(),
                top: mid,
                clip: *slot,
                size: None,
            });
        }
        let mut right = slot.x + slot.w - pad - self.cell_width - (6.0 * scale).round();
        if let Some(ansi) = chips {
            let cw = (8.0 * scale).round();
            let gap = (2.0 * scale).round();
            let x0 = right - (8.0 * cw + 7.0 * gap);
            let y = (slot.y + (slot.h - cw) / 2.0).round();
            for (i, c) in ansi.iter().enumerate() {
                let chip = LayoutRect { x: (x0 + i as f32 * (cw + gap)).round(), y, w: cw, h: cw };
                bg_quads.push(self.px_rect(&chip, *c, 1.0, (2.0 * scale).round()));
            }
            right = x0 - pad;
        }
        labels.push(LabelSpec {
            text: label.into(),
            color: color(if picked { th.text_bright } else { th.text_dim }, 1.0),
            left: slot.x + pad,
            top: mid,
            clip: LayoutRect { w: (right - slot.x - pad).max(0.0), ..*slot },
            size: None,
        });
    }

    fn picker_overlay(
        &self,
        picker: &Picker,
        layout: &PickerLayout,
        rects: &mut Vec<Quad>,
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
        let search_top = (layout.search.y + (layout.search.h - self.cell_height) / 2.0).round();
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
        let caret_x = layout.search.x + pad + picker.query.chars().count() as f32 * self.cell_width;
        let caret = LayoutRect {
            x: caret_x,
            y: search_top,
            w: (2.0 * scale).round().max(1.0),
            h: self.cell_height,
        };
        rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));

        // Visible rows: headers, selected-row highlight, labels, glyphs.
        for i in layout.first_visible..(layout.first_visible + layout.visible) {
            let (Some(row), Some(prow)) = (layout.row_rect(i), picker.rows.get(i)) else {
                continue;
            };
            let top = (row.y + (row.h - self.cell_height) / 2.0).round();
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
                    if i == picker.selected {
                        // Accent-tinted rounded pill, inset from the panel edges.
                        let m = (6.0 * scale).round();
                        let pill =
                            LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..row };
                        rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
                    }
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
                            left: gx + (layout.row_h - self.cell_width) / 2.0,
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
                            left: star.x + (layout.row_h - self.cell_width) / 2.0,
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

    /// Step-2 fork-source overlay: a centered, filterable list of the branch /
    /// worktree choices for the group being forked. Styled like the dir picker.
    /// TODO(gpui-port): per-scope tag colors and scroll-to-selection.
    fn fork_overlay(&self, fork: &ForkPicker, rects: &mut Vec<Quad>) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (12.0 * scale).round();
        let mut labels = Vec::new();

        // Dimming scrim behind the popover (light, matching the dir picker).
        let scrim = LayoutRect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 };
        rects.push(self.px_rect(&scrim, th.scrim, 0.30, 0.0));

        // Centered panel sized to the (capped) row count: a floating white
        // card matching the Arc chrome.
        let row_h = (self.cell_height + 8.0 * scale).round();
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
            top: (header.y + (row_h - self.cell_height) / 2.0).round(),
            clip: header,
            size: None,
        });

        // Filter box + caret.
        let search =
            LayoutRect { x: panel_x + pad, y: panel_y + pad + row_h, w: panel_w - 2.0 * pad, h: row_h };
        rects.push(self.px_rect(&search, th.ink, 0.06, (7.0 * scale).round()));
        let search_top = (search.y + (search.h - self.cell_height) / 2.0).round();
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
        let caret_x = search.x + pad + fork.query.chars().count() as f32 * self.cell_width;
        let caret = LayoutRect {
            x: caret_x,
            y: search_top,
            w: (2.0 * scale).round().max(1.0),
            h: self.cell_height,
        };
        rects.push(self.px_rect(&caret, th.accent, 1.0, 0.0));

        // Rows.
        let rows_top = panel_y + pad + row_h * 2.0;
        for (i, entry) in fork.rows.iter().take(visible).enumerate() {
            let row = LayoutRect { x: panel_x, y: rows_top + row_h * i as f32, w: panel_w, h: row_h };
            let top = (row.y + (row.h - self.cell_height) / 2.0).round();
            if i == fork.selected {
                // Accent-tinted rounded pill, inset from the panel edges.
                let m = (6.0 * scale).round();
                let pill = LayoutRect { x: row.x + m, w: (row.w - 2.0 * m).max(0.0), ..row };
                rects.push(self.px_rect(&pill, th.accent, 0.10, (7.0 * scale).round()));
            }
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

    /// Geometry of the confirm dialog (panel + buttons), shared by drawing and
    /// `main.rs` hit-testing so clicks always agree with pixels.
    pub fn confirm_layout(&self, text: &str) -> ConfirmLayout {
        let scale = self.scale;
        let pad = (16.0 * scale).round();
        let gap = (10.0 * scale).round();
        let btn_pad = (14.0 * scale).round();
        let btn_h = (self.cell_height + 10.0 * scale).round();
        let text_w = text.chars().count() as f32 * self.cell_width;
        let cancel_w =
            (CONFIRM_CANCEL.chars().count() as f32 * self.cell_width + 2.0 * btn_pad).round();
        let close_w =
            (CONFIRM_CLOSE.chars().count() as f32 * self.cell_width + 2.0 * btn_pad).round();
        let w = (text_w.max(cancel_w + gap + close_w) + 2.0 * pad)
            .min(self.width as f32 - 2.0 * pad);
        let h = (self.cell_height + gap + btn_h + 2.0 * pad).round();
        let x = ((self.width as f32 - w) / 2.0).round();
        let y = ((self.height as f32 - h) / 2.0).round();
        let by = (y + pad + self.cell_height + gap).round();
        let close_x = (x + w - pad - close_w).round();
        let cancel_x = (close_x - gap - cancel_w).round();
        ConfirmLayout {
            panel: LayoutRect { x, y, w, h },
            cancel: LayoutRect { x: cancel_x, y: by, w: cancel_w, h: btn_h },
            close: LayoutRect { x: close_x, y: by, w: close_w, h: btn_h },
        }
    }

    /// Centered confirm dialog: a message line over Cancel / Close-group
    /// buttons. Styled like the message panel; the destructive button carries
    /// the accent fill.
    fn confirm_overlay(&self, text: &str, rects: &mut Vec<Quad>) -> Vec<LabelSpec> {
        let th = self.theme();
        let scale = self.scale;
        let pad = (16.0 * scale).round();
        let btn_r = (7.0 * scale).round();
        let layout = self.confirm_layout(text);

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
            [(&layout.cancel, CONFIRM_CANCEL, false), (&layout.close, CONFIRM_CLOSE, true)]
        {
            rects.push(self.px_rect(
                rect,
                if danger { th.accent } else { th.ink },
                if danger { 0.9 } else { 0.08 },
                btn_r,
            ));
            let w = label.chars().count() as f32 * self.cell_width;
            labels.push(LabelSpec {
                text: label.into(),
                color: color(if danger { (255, 255, 255) } else { th.ink }, 1.0),
                left: (rect.x + (rect.w - w) / 2.0).round(),
                top: (rect.y + (rect.h - self.cell_height) / 2.0).round(),
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

        let w = (text.chars().count() as f32 * self.cell_width + pad * 2.0)
            .min(self.width as f32 - pad * 2.0);
        let h = (self.cell_height + pad * 2.0).round();
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
            top: (y + (h - self.cell_height) / 2.0).round(),
            clip: panel,
            size: None,
        }]
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
        let links = crate::links::links_in_lines(&lines);
        for l in &links {
            let span = (l.end_col - l.start_col + 1) as f32;
            rects.push(self.cell_rect(
                origin, l.start_col, l.row, 0.0, 0.92, span, 0.06, th.accent, 1.0,
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
                let rgb = if links.iter().any(|l| l.contains(row, col)) {
                    th.accent
                } else {
                    (r, g, b)
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
