//! Cell-sized geometry for block elements and box-drawing characters.
//!
//! Terminals can't trust fonts to render block elements (U+2580–U+259F) and
//! box-drawing lines (U+2500–U+257F) seamlessly: glyphs rarely fill the whole
//! line box, leaving gaps between rows. iTerm2, WezTerm, and Alacritty all
//! special-case these characters and draw them as geometry sized exactly to
//! the cell — this is that mapping. gpui paints the geometry as quads (see
//! `renderer.rs`), so this module is now pure math: it turns a char into a set
//! of cell-relative rects, and turns those into gpui pixel `Bounds` the
//! renderer fills. Also used for the cursor.
//!
//! Ported from a custom wgpu SDF pipeline to gpui's `fill`/`paint_quad`
//! primitives; the char→geometry mapping is unchanged, only the drawing
//! backend moved. Rounded window corners were a wgpu-only full-surface
//! alpha-mask trick; gpui windows get their rounding from the platform / the
//! root element's corner radii, so that mask has no analogue here.
//! TODO(gpui-port): if a rounded outer window frame is wanted, apply
//! `corner_radii` on the root element in `main.rs` rather than masking pixels.

/// A rect in cell-relative unit coordinates (0..1, y-down), with an alpha
/// multiplier (for the ░▒▓ shades).
pub struct UnitRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

const fn r(x: f32, y: f32, w: f32, h: f32) -> UnitRect {
    UnitRect { x, y, w, h, alpha: 1.0 }
}

/// Decompose a block-element or box-drawing character into cell-relative
/// rects. Returns `None` for characters the font should render.
///
/// `tx`/`ty` are the box-drawing line thickness in *cell-relative* units of
/// the horizontal/vertical cell dimension; the caller converts to pixels.
pub fn char_rects(ch: char, tx: f32, ty: f32) -> Option<Vec<UnitRect>> {
    // Half-line segments from cell center toward each edge, overlapping the
    // center so junctions are seamless.
    let cx = 0.5 - tx / 2.0;
    let cy = 0.5 - ty / 2.0;
    let left = r(0.0, cy, 0.5 + tx / 2.0, ty);
    let right = r(cx, cy, 0.5 + tx / 2.0, ty);
    let up = r(cx, 0.0, tx, 0.5 + ty / 2.0);
    let down = r(cx, cy, tx, 0.5 + ty / 2.0);

    let rects = match ch {
        // ── Block elements (U+2580–U+259F) ─────────────────────────────
        '\u{2580}' => vec![r(0.0, 0.0, 1.0, 0.5)], // upper half
        // Lower eighth blocks ▁▂▃▄▅▆▇█
        '\u{2581}'..='\u{2588}' => {
            let k = (ch as u32 - 0x2580) as f32 / 8.0;
            vec![r(0.0, 1.0 - k, 1.0, k)]
        },
        // Left blocks ▉▊▋▌▍▎▏ (7/8 down to 1/8)
        '\u{2589}'..='\u{258F}' => {
            let k = (0x2590 - ch as u32) as f32 / 8.0;
            vec![r(0.0, 0.0, k, 1.0)]
        },
        '\u{2590}' => vec![r(0.5, 0.0, 0.5, 1.0)], // right half
        '\u{2591}' => vec![UnitRect { alpha: 0.25, ..r(0.0, 0.0, 1.0, 1.0) }], // light shade
        '\u{2592}' => vec![UnitRect { alpha: 0.5, ..r(0.0, 0.0, 1.0, 1.0) }],  // medium shade
        '\u{2593}' => vec![UnitRect { alpha: 0.75, ..r(0.0, 0.0, 1.0, 1.0) }], // dark shade
        '\u{2594}' => vec![r(0.0, 0.0, 1.0, 0.125)], // upper eighth
        '\u{2595}' => vec![r(0.875, 0.0, 0.125, 1.0)], // right eighth
        '\u{2596}' => vec![r(0.0, 0.5, 0.5, 0.5)],   // ▖
        '\u{2597}' => vec![r(0.5, 0.5, 0.5, 0.5)],   // ▗
        '\u{2598}' => vec![r(0.0, 0.0, 0.5, 0.5)],   // ▘
        '\u{2599}' => vec![r(0.0, 0.0, 0.5, 1.0), r(0.5, 0.5, 0.5, 0.5)], // ▙
        '\u{259A}' => vec![r(0.0, 0.0, 0.5, 0.5), r(0.5, 0.5, 0.5, 0.5)], // ▚
        '\u{259B}' => vec![r(0.0, 0.0, 1.0, 0.5), r(0.0, 0.5, 0.5, 0.5)], // ▛
        '\u{259C}' => vec![r(0.0, 0.0, 1.0, 0.5), r(0.5, 0.5, 0.5, 0.5)], // ▜
        '\u{259D}' => vec![r(0.5, 0.0, 0.5, 0.5)],   // ▝
        '\u{259E}' => vec![r(0.5, 0.0, 0.5, 0.5), r(0.0, 0.5, 0.5, 0.5)], // ▞
        '\u{259F}' => vec![r(0.0, 0.5, 1.0, 0.5), r(0.5, 0.0, 0.5, 0.5)], // ▟

        // ── Light box drawing (U+2500–U+257F subset) ───────────────────
        '\u{2500}' => vec![left, right],             // ─
        '\u{2502}' => vec![up, down],                // │
        '\u{250C}' | '\u{256D}' => vec![right, down], // ┌ ╭
        '\u{2510}' | '\u{256E}' => vec![left, down],  // ┐ ╮
        '\u{2514}' | '\u{2570}' => vec![right, up],   // └ ╰
        '\u{2518}' | '\u{256F}' => vec![left, up],    // ┘ ╯
        '\u{251C}' => vec![up, down, right],         // ├
        '\u{2524}' => vec![up, down, left],          // ┤
        '\u{252C}' => vec![left, right, down],       // ┬
        '\u{2534}' => vec![left, right, up],         // ┴
        '\u{253C}' => vec![left, right, up, down],   // ┼
        '\u{2574}' => vec![left],                    // ╴
        '\u{2575}' => vec![up],                      // ╵
        '\u{2576}' => vec![right],                   // ╶
        '\u{2577}' => vec![down],                    // ╷
        _ => return None,
    };
    Some(rects)
}
