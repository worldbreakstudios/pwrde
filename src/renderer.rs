//! GPU renderer: wgpu (Metal on macOS) + glyphon glyph atlas.
//!
//! Same rendering model as iTerm2's Metal renderer:
//! - Glyphs are rasterized once (cosmic-text/swash) into a texture atlas kept
//!   on the GPU; frames draw instanced textured quads — no per-frame font
//!   rasterization.
//! - The surface presents with Fifo (vsync), so redraws are naturally capped
//!   at display refresh; PTY floods never outpace the display.
//!
//! Frame structure: chrome rects (sidebar, tab strips, dividers) → text
//! (terminal grids + labels) → foreground rects (block glyph geometry,
//! cursor, focus border, drag-drop hint).

use std::sync::Arc;

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color, ColorMode, Family, FontSystem, Metrics, Resolution,
    Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use termwiz::surface::CursorVisibility;
use wezterm_term::color::ColorPalette;
use winit::window::Window;

use crate::rect::{RectInstance, RectRenderer, char_rects};
use crate::term::Session;
use crate::workspace::{self, LayoutRect, Workspace};

const FONT_SIZE: f32 = 15.0;
const LINE_HEIGHT_FACTOR: f32 = 1.25;
/// Background color, in sRGB (as you'd write it in CSS).
const BG_SRGB: [f64; 3] = [0.086, 0.09, 0.11];

/// Content inset inside each tile's terminal region, logical px.
const PANE_PAD: f32 = 5.0;
/// Window corner radius, logical px (macOS-native look).
const CORNER_RADIUS: f32 = 12.0;
/// UI colors (sRGB u8).
const TERM_BG: (u8, u8, u8) = (22, 23, 28);
const SIDEBAR_BG: (u8, u8, u8) = (30, 33, 41);
const TAB_ACTIVE_BG: (u8, u8, u8) = (48, 53, 66);
const DIVIDER_BG: (u8, u8, u8) = (45, 49, 61);
const ACCENT: (u8, u8, u8) = (122, 162, 247);
const TEXT_BRIGHT: Color = Color::rgb(220, 222, 228);
const TEXT_DIM: Color = Color::rgb(130, 135, 148);

/// sRGB electro-optical transfer function: gamma-encoded → linear.
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

struct LabelSpec {
    text: String,
    color: Color,
    left: f32,
    top: f32,
    bounds: TextBounds,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,

    /// Reusable per-tile text buffers (index-aligned with the visible tiles;
    /// grown on demand).
    pane_buffers: Vec<TextBuffer>,
    /// Reusable buffers for sidebar/tile tab labels + hints.
    label_buffers: Vec<TextBuffer>,
    rect_renderer: RectRenderer,

    metrics: Metrics,
    clear_color: wgpu::Color,
    palette: ColorPalette,
    srgb: bool,

    pub scale: f32,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Self {
        let scale = window.scale_factor() as f32;
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window).expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("no gpu adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        // Prefer an sRGB format: the hardware then handles gamma encoding, and
        // glyphon's ColorMode::Accurate linearizes glyph colors to match.
        // Clear colors are given to wgpu in *linear* space, so convert.
        let format =
            caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let (color_mode, clear_color) = if format.is_srgb() {
            let [r, g, b] = BG_SRGB.map(srgb_to_linear);
            (ColorMode::Accurate, wgpu::Color { r, g, b, a: 1.0 })
        } else {
            let [r, g, b] = BG_SRGB;
            (ColorMode::Web, wgpu::Color { r, g, b, a: 1.0 })
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            // Non-opaque so the corner mask can show the desktop through the
            // rounded corners of our borderless window.
            alpha_mode: caps
                .alpha_modes
                .iter()
                .copied()
                .find(|m| *m != wgpu::CompositeAlphaMode::Opaque)
                .unwrap_or(caps.alpha_modes[0]),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::with_color_mode(&device, &queue, &cache, format, color_mode);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        // Measure the monospace cell once (iTerm2 does the same with Core Text
        // metrics): shape a reference glyph and take its advance width.
        let font_size = FONT_SIZE * scale;
        let line_height = (font_size * LINE_HEIGHT_FACTOR).round();
        let metrics = Metrics::new(font_size, line_height);
        let mut probe = TextBuffer::new(&mut font_system, metrics);
        probe.set_text("M", &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
        probe.shape_until_scroll(&mut font_system, false);
        let cell_width = probe
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| g.w))
            .unwrap_or(font_size * 0.6)
            .round();

        let rect_renderer = RectRenderer::new(&device, format);

        Self {
            surface,
            device,
            queue,
            config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            pane_buffers: Vec::new(),
            label_buffers: Vec::new(),
            rect_renderer,
            metrics,
            clear_color,
            palette: ColorPalette::default(),
            srgb: format.is_srgb(),
            scale,
            cell_width,
            cell_height: line_height,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
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

    pub fn draw(
        &mut self,
        workspaces: &[Workspace],
        active: usize,
        sidebar_w: f32,
        drop_hint: Option<LayoutRect>,
    ) {
        let ws = &workspaces[active];
        let (width, height) = (self.config.width, self.config.height);
        let sidebar = workspace::sidebar(height, self.scale, sidebar_w);
        let area = workspace::terminal_area(width, height, self.scale, sidebar_w);
        let (tiles, dividers) = workspace::layout_tiles(&ws.root, area, self.scale);

        let mut rects: Vec<RectInstance> = Vec::new();

        // ── Chrome (under text) ────────────────────────────────────────
        rects.push(self.px_rect(&sidebar, SIDEBAR_BG, 1.0));
        // Custom traffic lights (window is borderless).
        const LIGHTS: [(u8, u8, u8); 3] = [(255, 95, 87), (254, 188, 46), (40, 200, 64)];
        for (i, color) in LIGHTS.iter().enumerate() {
            let r = workspace::traffic_light(i, self.scale);
            let mut inst = self.px_rect(&r, *color, 1.0);
            inst.radius = r.w / 2.0;
            rects.push(inst);
        }
        for (i, _) in workspaces.iter().enumerate() {
            if i == active {
                let tab = workspace::tab_rect(i, self.scale, sidebar_w);
                rects.push(self.px_rect(&tab, TAB_ACTIVE_BG, 1.0));
                let bar = LayoutRect { w: (3.0 * self.scale).round(), ..tab };
                rects.push(self.px_rect(&bar, ACCENT, 1.0));
            }
        }
        for d in &dividers {
            rects.push(self.px_rect(&d.rect, DIVIDER_BG, 1.0));
        }
        for (id, rect) in &tiles {
            let bar = workspace::tile_tab_bar(rect, self.scale);
            rects.push(self.px_rect(&bar, SIDEBAR_BG, 1.0));
            if let Some(tile) = ws.root.find_tile(*id) {
                // Active tab slot in terminal-bg so it merges with content.
                let tr = workspace::tile_tab_rect(rect, tile.active, tile.tabs.len(), self.scale);
                rects.push(self.px_rect(&tr, TERM_BG, 1.0));
            }
        }
        let bg_rects = rects.len() as u32;

        // ── Terminal snapshots (short critical sections) ───────────────
        let mut pane_spans: Vec<Vec<(String, Color)>> = Vec::with_capacity(tiles.len());
        for (id, rect) in &tiles {
            let Some(tab) = ws.root.find_tile(*id).and_then(|t| t.active_tab()) else {
                pane_spans.push(Vec::new());
                continue;
            };
            tab.session.begin_frame();
            let content = workspace::tile_content(rect, self.scale);
            let origin = self.content_origin(&content);
            let draw_cursor = *id == ws.focused_tile;
            pane_spans.push(self.snapshot_pane(&tab.session, origin, draw_cursor, &mut rects));
        }

        // Focused tile border (only interesting with multiple tiles).
        if tiles.len() > 1
            && let Some((_, rect)) = tiles.iter().find(|(id, _)| *id == ws.focused_tile)
        {
            let t = (1.5 * self.scale).round();
            let sides = [
                LayoutRect { x: rect.x, y: rect.y, w: rect.w, h: t },
                LayoutRect { x: rect.x, y: rect.y + rect.h - t, w: rect.w, h: t },
                LayoutRect { x: rect.x, y: rect.y, w: t, h: rect.h },
                LayoutRect { x: rect.x + rect.w - t, y: rect.y, w: t, h: rect.h },
            ];
            rects.extend(sides.iter().map(|s| self.px_rect(s, ACCENT, 1.0)));
        }

        // Drag-and-drop target hint, on top of everything.
        if let Some(hint) = drop_hint {
            rects.push(self.px_rect(&hint, ACCENT, 0.3));
        }

        // ── Labels ─────────────────────────────────────────────────────
        let label_pad = (12.0 * self.scale).round();
        let mut labels: Vec<LabelSpec> = Vec::new();
        for (i, w) in workspaces.iter().enumerate() {
            let tab = workspace::tab_rect(i, self.scale, sidebar_w);
            labels.push(LabelSpec {
                text: format!("{}  {}", i + 1, w.name),
                color: if i == active { TEXT_BRIGHT } else { TEXT_DIM },
                left: tab.x + label_pad,
                top: (tab.y + (tab.h - self.cell_height) / 2.0).round(),
                bounds: bounds_of(&tab),
            });
        }
        labels.push(LabelSpec {
            text: "⌘T tab · ⌘D split · ⇧⌘T group".into(),
            color: TEXT_DIM,
            left: label_pad,
            top: sidebar.h - self.cell_height - label_pad,
            bounds: bounds_of(&sidebar),
        });
        let tab_text_pad = (8.0 * self.scale).round();
        for (id, rect) in &tiles {
            let Some(tile) = ws.root.find_tile(*id) else { continue };
            let n = tile.tabs.len();
            for (ti, tab) in tile.tabs.iter().enumerate() {
                let tr = workspace::tile_tab_rect(rect, ti, n, self.scale);
                let title = tab.session.title();
                let text = if title.is_empty() { "shell".to_string() } else { title };
                labels.push(LabelSpec {
                    text,
                    color: if ti == tile.active { TEXT_BRIGHT } else { TEXT_DIM },
                    left: tr.x + tab_text_pad,
                    top: (tr.y + (tr.h - self.cell_height) / 2.0).round(),
                    bounds: bounds_of(&LayoutRect { w: tr.w - tab_text_pad, ..tr }),
                });
            }
        }

        // ── Shape text into pooled buffers ─────────────────────────────
        let attrs = Attrs::new().family(Family::Monospace);
        while self.pane_buffers.len() < pane_spans.len() {
            let mut buf = TextBuffer::new(&mut self.font_system, self.metrics);
            buf.set_wrap(Wrap::None);
            self.pane_buffers.push(buf);
        }
        for (i, spans) in pane_spans.iter().enumerate() {
            let rich =
                spans.iter().map(|(text, color)| (text.as_str(), attrs.clone().color(*color)));
            let buf = &mut self.pane_buffers[i];
            buf.set_rich_text(rich, &attrs, Shaping::Advanced, None);
            buf.set_size(Some(tiles[i].1.w), Some(tiles[i].1.h));
            buf.shape_until_scroll(&mut self.font_system, false);
        }
        while self.label_buffers.len() < labels.len() {
            let mut buf = TextBuffer::new(&mut self.font_system, self.metrics);
            buf.set_wrap(Wrap::None);
            self.label_buffers.push(buf);
        }
        for (i, spec) in labels.iter().enumerate() {
            let buf = &mut self.label_buffers[i];
            buf.set_text(&spec.text, &attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(&mut self.font_system, false);
        }

        // ── Assemble text areas (immutable borrows of the pools) ───────
        let mut text_areas: Vec<TextArea> = Vec::new();
        for (i, (_, rect)) in tiles.iter().enumerate() {
            let content = workspace::tile_content(rect, self.scale);
            let (left, top) = self.content_origin(&content);
            text_areas.push(TextArea {
                buffer: &self.pane_buffers[i],
                left,
                top,
                scale: 1.0,
                bounds: bounds_of(&content),
                default_color: TEXT_BRIGHT,
                custom_glyphs: &[],
            });
        }
        for (i, spec) in labels.iter().enumerate() {
            text_areas.push(TextArea {
                buffer: &self.label_buffers[i],
                left: spec.left,
                top: spec.top,
                scale: 1.0,
                bounds: spec.bounds,
                default_color: spec.color,
                custom_glyphs: &[],
            });
        }

        self.rect_renderer.prepare(
            &self.device,
            &self.queue,
            &rects,
            self.config.width,
            self.config.height,
        );
        self.rect_renderer.prepare_mask(
            &self.queue,
            self.config.width,
            self.config.height,
            CORNER_RADIUS * self.scale,
        );
        self.viewport.update(
            &self.queue,
            Resolution { width: self.config.width, height: self.config.height },
        );
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("prepare text");

        use wgpu::CurrentSurfaceTexture::*;
        let frame = match self.surface.get_current_texture() {
            Success(frame) | Suboptimal(frame) => frame,
            Outdated | Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            },
            Timeout | Occluded | Validation => return,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Chrome under the text…
            self.rect_renderer.render_range(&mut pass, 0..bg_rects);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render text");
            // …blocks/cursor/border/drop-hint on top (blocks never overlap
            // glyphs: their cells hold spaces).
            self.rect_renderer.render_range(&mut pass, bg_rects..u32::MAX);
            // Finally, punch out the rounded window corners.
            self.rect_renderer.render_mask(&mut pass);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
    }

    /// Snapshot one pane's grid into text spans + geometry rects, offset to
    /// `origin`. Holds the terminal lock only for the walk.
    fn snapshot_pane(
        &self,
        session: &Session,
        origin: (f32, f32),
        draw_cursor: bool,
        rects: &mut Vec<RectInstance>,
    ) -> Vec<(String, Color)> {
        // Box-drawing line thickness in px, and in cell-relative units.
        let thickness = (self.cell_width / 8.0).round().max(1.0);
        let (tx, ty) = (thickness / self.cell_width, thickness / self.cell_height);

        let term = session.term.lock().unwrap();
        let screen = term.screen();
        let rows = screen.physical_rows;
        let lines = screen.lines_in_phys_range(screen.phys_range(&(0..rows as i64)));

        // Coalesce per-cell colors into runs: one (String, Color) span per
        // same-colored stretch keeps the shaping input small.
        // Links get the accent color + an underline rect; ⌘-click opens.
        // Detection is wrap-aware: a URL broken across rows is one link.
        let links = crate::links::links_in_lines(&lines);
        for l in &links {
            let span = (l.end_col - l.start_col + 1) as f32;
            rects.push(self.cell_rect(
                origin, l.start_col, l.row, 0.0, 0.92, span, 0.06, ACCENT, 1.0,
            ));
        }

        let mut spans: Vec<(String, Color)> = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            if row > 0 {
                spans.push(("\n".into(), Color::rgb(0, 0, 0)));
            }
            for cell in line.visible_cells() {
                let col = cell.cell_index();
                let attrs = cell.attrs();
                // No bg quads yet: reversed cells draw in their bg color.
                let srgba = if attrs.reverse() {
                    self.palette.resolve_bg(attrs.background())
                } else {
                    self.palette.resolve_fg(attrs.foreground())
                };
                let (r, g, b, _) = srgba.to_srgb_u8();
                let (r, g, b) = if links.iter().any(|l| l.contains(row, col)) {
                    ACCENT
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
                        self.cell_rect(origin, col, row, u.x, u.y, u.w, u.h, (r, g, b), u.alpha)
                    }));
                    // Keep column alignment in the text run.
                    match spans.last_mut() {
                        Some((text, _)) if !text.ends_with('\n') => text.push(' '),
                        _ => spans.push((" ".into(), Color::rgb(r, g, b))),
                    }
                    continue;
                }

                let color = Color::rgb(r, g, b);
                match spans.last_mut() {
                    Some((text, c)) if *c == color && !text.ends_with('\n') => {
                        text.push_str(cell.str())
                    },
                    _ => spans.push((cell.str().to_string(), color)),
                }
            }
        }

        // Cursor: a solid rect, drawn on top of the text (focused tile only).
        let cur = term.cursor_pos();
        if draw_cursor && cur.visibility == CursorVisibility::Visible && cur.y >= 0 {
            let (r, g, b, _) = self.palette.foreground.to_srgb_u8();
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

        spans
    }

    /// A rect straight from layout coordinates (already physical px).
    fn px_rect(&self, r: &LayoutRect, (cr, cg, cb): (u8, u8, u8), alpha: f32) -> RectInstance {
        let comp = |v: u8| {
            let c = v as f64 / 255.0;
            if self.srgb { srgb_to_linear(c) as f32 } else { c as f32 }
        };
        RectInstance {
            pos: [r.x, r.y],
            size: [r.w, r.h],
            color: [comp(cr), comp(cg), comp(cb), alpha],
            radius: 0.0,
            _pad: [0.0; 3],
        }
    }

    /// Build a pixel-space rect for a sub-region of a cell, with edges snapped
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
        (r, g, b): (u8, u8, u8),
        alpha: f32,
    ) -> RectInstance {
        let base_x = origin.0 + col as f32 * self.cell_width;
        let base_y = origin.1 + row as f32 * self.cell_height;
        // Round each edge (not pos+size) so neighbors share exact edges.
        let x0 = (base_x + ux * self.cell_width).round();
        let y0 = (base_y + uy * self.cell_height).round();
        let x1 = (base_x + (ux + uw) * self.cell_width).round();
        let y1 = (base_y + (uy + uh) * self.cell_height).round();
        let comp = |v: u8| {
            let c = v as f64 / 255.0;
            if self.srgb { srgb_to_linear(c) as f32 } else { c as f32 }
        };
        RectInstance {
            pos: [x0, y0],
            size: [x1 - x0, y1 - y0],
            color: [comp(r), comp(g), comp(b), alpha],
            radius: 0.0,
            _pad: [0.0; 3],
        }
    }
}

fn bounds_of(rect: &LayoutRect) -> TextBounds {
    TextBounds {
        left: rect.x as i32,
        top: rect.y as i32,
        right: (rect.x + rect.w) as i32,
        bottom: (rect.y + rect.h) as i32,
    }
}
