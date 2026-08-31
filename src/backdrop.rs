//! Impression blur — the backdrop behind liquid-glass surfaces.
//!
//! gpui has no backdrop filter, so the glass overlays can't ask the GPU to
//! blur what's beneath them. But everything under those overlays is canvas
//! content we build ourselves ([`crate::renderer::Frame`]: tile fills,
//! terminal text runs, cursors), so this module rasterizes the frame at a
//! coarse resolution, blurs it on the CPU, and hands back a gpui
//! [`RenderImage`] the overlays paint under their translucent fill. At the
//! blur radius the GANTRY mock uses (40px) real backdrop blur reduces text to
//! coloured streaks anyway, so sampling each glyph run as a soft block of its
//! foreground colour is visually indistinguishable — and costs microseconds.
//!
//! Everything here is pure data → data so it can be unit tested; `main.rs`
//! decides when to rebuild (only while a glass surface is on screen, and only
//! when the impression actually changed).

use std::hash::{Hash, Hasher};

use gpui::{Hsla, RenderImage, Rgba};
use image::{Delay, Frame as ImageFrame, RgbaImage};

use crate::renderer::{Frame, PaneText, Quad};

/// Physical pixels per impression sample. 8 physical px is 4 logical px on
/// retina — coarse enough to be cheap, fine enough that the blur has
/// structure to smear.
pub const DOWNSAMPLE: f32 = 8.0;

/// Box-blur radius in samples, run as three passes (≈ gaussian σ ≈ 9.5
/// samples ≈ 38 logical px, matching the mock's `blur(40px)`).
pub const BLUR_RADIUS: usize = 9;

/// Fraction of a text cell a glyph is assumed to cover. Terminal glyphs are
/// mostly strokes, so their run reads as a tint, not a solid bar.
const GLYPH_COVERAGE: f32 = 0.30;

/// The mock's `saturate(1.8) brightness(1.15)` backdrop grading.
pub const SATURATE: f32 = 1.8;
pub const BRIGHTNESS: f32 = 1.15;

/// A coarse RGB rasterization of the canvas, premultiplied over an opaque
/// ground so blurring never bleeds transparency.
#[derive(Clone, Debug, PartialEq)]
pub struct Impression {
    pub w: usize,
    pub h: usize,
    /// Row-major RGB triples in 0..=1 sRGB.
    pub px: Vec<[f32; 3]>,
}

impl Impression {
    pub fn solid(w: usize, h: usize, c: [f32; 3]) -> Self {
        Self { w, h, px: vec![c; w * h] }
    }

    /// Alpha-blend `color` over the samples covering the physical-pixel rect.
    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Hsla) {
        if color.a <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let rgba = Rgba::from(color);
        let a = rgba.a.clamp(0.0, 1.0);
        let x0 = ((x / DOWNSAMPLE).floor().max(0.0)) as usize;
        let y0 = ((y / DOWNSAMPLE).floor().max(0.0)) as usize;
        let x1 = (((x + w) / DOWNSAMPLE).ceil() as usize).min(self.w);
        let y1 = (((y + h) / DOWNSAMPLE).ceil() as usize).min(self.h);
        for sy in y0..y1 {
            for sx in x0..x1 {
                let p = &mut self.px[sy * self.w + sx];
                p[0] += (rgba.r - p[0]) * a;
                p[1] += (rgba.g - p[1]) * a;
                p[2] += (rgba.b - p[2]) * a;
            }
        }
    }

    fn quads(&mut self, quads: &[Quad]) {
        for q in quads {
            self.fill(q.x, q.y, q.w, q.h, q.color);
        }
    }

    /// Each glyph run becomes a block of its foreground colour at glyph
    /// coverage; blank cells leave the ground alone.
    fn panes(&mut self, panes: &[PaneText], cell_w: f32, cell_h: f32) {
        for pane in panes {
            let (ox, oy) = pane.origin;
            for (ri, row) in pane.rows.iter().enumerate() {
                let y = oy + ri as f32 * cell_h;
                let mut col = 0usize;
                for span in row {
                    let mut run_start: Option<usize> = None;
                    let mut n = 0usize;
                    for ch in span.text.chars() {
                        if ch.is_whitespace() {
                            if let Some(s) = run_start.take() {
                                let x = ox + s as f32 * cell_w;
                                let w = (col + n - s) as f32 * cell_w;
                                self.fill(x, y, w, cell_h, tint(span.color, GLYPH_COVERAGE));
                            }
                        } else if run_start.is_none() {
                            run_start = Some(col + n);
                        }
                        n += 1;
                    }
                    if let Some(s) = run_start {
                        let x = ox + s as f32 * cell_w;
                        let w = (col + n - s) as f32 * cell_w;
                        self.fill(x, y, w, cell_h, tint(span.color, GLYPH_COVERAGE));
                    }
                    col += n;
                }
            }
        }
    }

    /// Three separable box passes ≈ gaussian. Edges clamp, so the blur never
    /// darkens toward the window border.
    pub fn blur(&mut self, radius: usize) {
        if radius == 0 || self.w == 0 || self.h == 0 {
            return;
        }
        let mut tmp = vec![[0.0f32; 3]; self.px.len()];
        for _ in 0..3 {
            box_pass(&self.px, &mut tmp, self.w, self.h, radius, true);
            box_pass(&tmp, &mut self.px, self.w, self.h, radius, false);
        }
    }

    /// Subtract the ground's colour cast so the material reads neutral: a
    /// navy terminal scheme (`#0d1117`) would otherwise tint every panel
    /// blue once `grade` saturates it, where the mock's glass sits on a
    /// neutral `#18181b`. Content colours keep their hue — only the constant
    /// offset shared by the ground and the tile fills is removed.
    pub fn neutralize(&mut self, ground: Hsla) {
        let g = Rgba::from(ground);
        let luma = 0.2126 * g.r + 0.7152 * g.g + 0.0722 * g.b;
        let cast = [g.r - luma, g.g - luma, g.b - luma];
        if cast.iter().all(|c| c.abs() < 1e-4) {
            return;
        }
        for p in &mut self.px {
            for (c, k) in p.iter_mut().zip(cast) {
                *c = (*c - k).clamp(0.0, 1.0);
            }
        }
    }

    /// Saturation + brightness grading (the mock's backdrop-filter tail).
    pub fn grade(&mut self, saturate: f32, brightness: f32) {
        for p in &mut self.px {
            let luma = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            for c in p.iter_mut() {
                let s = luma + (*c - luma) * saturate;
                *c = (s * brightness).clamp(0.0, 1.0);
            }
        }
    }

    /// Stable fingerprint of the raw samples, so a static frame doesn't
    /// re-blur and re-upload every paint.
    pub fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.w.hash(&mut h);
        self.h.hash(&mut h);
        for p in &self.px {
            for c in p {
                ((c * 255.0) as u8).hash(&mut h);
            }
        }
        h.finish()
    }

    /// Upload-ready image. gpui's `RenderImage` frames are BGRA.
    pub fn to_render_image(&self) -> RenderImage {
        let mut buf = RgbaImage::new(self.w.max(1) as u32, self.h.max(1) as u32);
        for (i, p) in self.px.iter().enumerate() {
            let x = (i % self.w) as u32;
            let y = (i / self.w) as u32;
            let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            buf.put_pixel(x, y, image::Rgba([to8(p[2]), to8(p[1]), to8(p[0]), 255]));
        }
        RenderImage::new(vec![ImageFrame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(0, 1))])
    }
}

fn tint(c: Hsla, a: f32) -> Hsla {
    Hsla { a: c.a * a, ..c }
}

fn box_pass(src: &[[f32; 3]], dst: &mut [[f32; 3]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { o * w + i } else { i * w + o };
    let win = (2 * r + 1) as f32;
    for o in 0..outer {
        // Running sum with clamped edges.
        let at = |i: isize| src[idx(o, i.clamp(0, inner as isize - 1) as usize)];
        let mut sum = [0.0f32; 3];
        for i in -(r as isize)..=(r as isize) {
            let p = at(i);
            sum[0] += p[0];
            sum[1] += p[1];
            sum[2] += p[2];
        }
        for i in 0..inner {
            dst[idx(o, i)] = [sum[0] / win, sum[1] / win, sum[2] / win];
            let out = at(i as isize - r as isize);
            let inn = at(i as isize + r as isize + 1);
            sum[0] += inn[0] - out[0];
            sum[1] += inn[1] - out[1];
            sum[2] += inn[2] - out[2];
        }
    }
}

/// Rasterize a built frame (physical-pixel coordinates) over `ground` into an
/// impression `phys_w × phys_h` physical pixels wide, in paint order: tile
/// fills, terminal text, foreground quads, then the flyover's layers.
pub fn rasterize(frame: &Frame, ground: Hsla, phys_w: f32, phys_h: f32, cell_w: f32, cell_h: f32) -> Impression {
    let w = ((phys_w / DOWNSAMPLE).ceil() as usize).max(1);
    let h = ((phys_h / DOWNSAMPLE).ceil() as usize).max(1);
    let g = Rgba::from(ground);
    let mut imp = Impression::solid(w, h, [g.r, g.g, g.b]);
    imp.quads(&frame.bg_quads);
    imp.panes(&frame.panes, cell_w, cell_h);
    imp.quads(&frame.fg_quads);
    imp.quads(&frame.flyover_quads);
    imp.panes(&frame.flyover_panes, cell_w, cell_h);
    imp.quads(&frame.flyover_fg_quads);
    imp.neutralize(ground);
    imp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hsla(r: f32, g: f32, b: f32, a: f32) -> Hsla {
        Hsla::from(Rgba { r, g, b, a })
    }

    #[test]
    fn fill_blends_over_ground_in_sample_space() {
        let mut imp = Impression::solid(4, 4, [0.0, 0.0, 0.0]);
        imp.fill(DOWNSAMPLE, DOWNSAMPLE, DOWNSAMPLE, DOWNSAMPLE, hsla(1.0, 1.0, 1.0, 0.5));
        assert!((imp.px[1 * 4 + 1][0] - 0.5).abs() < 1e-3, "covered sample blended");
        assert_eq!(imp.px[0], [0.0, 0.0, 0.0], "neighbour untouched");
    }

    #[test]
    fn blur_preserves_a_flat_field_and_spreads_a_spike() {
        let mut flat = Impression::solid(20, 20, [0.4, 0.4, 0.4]);
        flat.blur(3);
        assert!(flat.px.iter().all(|p| (p[0] - 0.4).abs() < 1e-4));

        let mut spike = Impression::solid(21, 21, [0.0; 3]);
        spike.px[10 * 21 + 10] = [1.0; 3];
        spike.blur(2);
        assert!(spike.px[10 * 21 + 10][0] < 1.0, "centre spread out");
        assert!(spike.px[10 * 21 + 12][0] > 0.0, "neighbour picked some up");
        let total: f32 = spike.px.iter().map(|p| p[0]).sum();
        assert!((total - 1.0).abs() < 0.05, "energy roughly conserved, got {total}");
    }

    #[test]
    fn neutralize_turns_a_navy_ground_grey_and_keeps_content_hue() {
        let navy = hsla(13.0 / 255.0, 17.0 / 255.0, 23.0 / 255.0, 1.0);
        let mut imp = Impression::solid(2, 1, [13.0 / 255.0, 17.0 / 255.0, 23.0 / 255.0]);
        imp.px[1] = [0.9, 0.2, 0.2]; // a red glyph run
        imp.neutralize(navy);
        let g = imp.px[0];
        assert!((g[0] - g[1]).abs() < 1e-3 && (g[1] - g[2]).abs() < 1e-3, "ground is grey: {g:?}");
        assert!(imp.px[1][0] > imp.px[1][1] + 0.5, "red stays red: {:?}", imp.px[1]);
    }

    #[test]
    fn grade_brightens_and_clamps() {
        let mut imp = Impression::solid(1, 1, [0.9, 0.5, 0.5]);
        imp.grade(SATURATE, BRIGHTNESS);
        assert!(imp.px[0][0] <= 1.0 && imp.px[0][0] > 0.9);
        assert!(imp.px[0][1] < 0.5, "saturation pushes the low channels down");
    }

    #[test]
    fn fingerprint_changes_with_content() {
        let a = Impression::solid(3, 3, [0.1; 3]);
        let mut b = a.clone();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.px[4] = [0.9; 3];
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn render_image_is_bgra_and_sized() {
        let imp = Impression::solid(2, 1, [1.0, 0.0, 0.0]);
        let img = imp.to_render_image();
        let size = img.size(0);
        assert_eq!((size.width.0, size.height.0), (2, 1));
        let bytes = img.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[0, 0, 255, 255], "red lands in the B-G-R-A red slot");
    }

    #[test]
    fn text_runs_tint_only_their_cells() {
        let panes = vec![PaneText {
            origin: (0.0, 0.0),
            rows: vec![vec![crate::renderer::TextSpan { text: "ab  ".into(), color: hsla(1.0, 1.0, 1.0, 1.0) }]],
        }];
        let mut imp = Impression::solid(4, 1, [0.0; 3]);
        imp.panes(&panes, DOWNSAMPLE, DOWNSAMPLE);
        assert!((imp.px[0][0] - GLYPH_COVERAGE).abs() < 1e-3);
        assert!((imp.px[1][0] - GLYPH_COVERAGE).abs() < 1e-3);
        assert_eq!(imp.px[2], [0.0; 3], "blank cell stays ground");
    }
}
