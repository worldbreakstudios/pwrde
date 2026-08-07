//! Instanced solid-rect pipeline.
//!
//! Terminals can't trust fonts to render block elements (U+2580–U+259F) and
//! box-drawing lines (U+2500–U+257F) seamlessly: glyphs rarely fill the whole
//! line box, leaving gaps between rows. iTerm2, WezTerm, and Alacritty all
//! special-case these characters and draw them as geometry sized exactly to
//! the cell — this is that pipeline. Also used for the cursor (and later,
//! cell background colors).

const SHADER: &str = r#"
struct Params {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> params: Params;

struct VsIn {
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radius: f32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
    @location(2) size: vec2<f32>,
    @location(3) radius: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let corner = corners[in.vi];
    let p = in.pos + corner * in.size;
    let ndc = vec2(p.x / params.screen.x * 2.0 - 1.0, 1.0 - p.y / params.screen.y * 2.0);
    var out: VsOut;
    out.pos = vec4(ndc, 0.0, 1.0);
    out.color = in.color;
    out.local = corner * in.size;
    out.size = in.size;
    out.radius = in.radius;
    return out;
}

fn coverage(in: VsOut) -> f32 {
    // Rounded-rect SDF (radius = min(size)/2 turns the rect into a circle).
    let half = in.size * 0.5;
    let q = abs(in.local - half) - (half - vec2(in.radius));
    let d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - in.radius;
    return 1.0 - smoothstep(-0.75, 0.75, d);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if in.radius <= 0.0 {
        return in.color;
    }
    return vec4(in.color.rgb, in.color.a * coverage(in));
}

// Window-corner mask: multiplies the already-rendered frame by the rounded
// coverage (blend: dst *= src alpha), zeroing color+alpha outside the radius
// so the transparent window shows the desktop through the corners.
@fragment
fn fs_mask(in: VsOut) -> @location(0) vec4<f32> {
    return vec4(coverage(in));
}
"#;

/// One rectangle: position/size in physical pixels, color in the surface's
/// working space (already linearized for sRGB surfaces). `radius` rounds the
/// corners; `radius = min(size)/2` yields a circle.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RectInstance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub color: [f32; 4],
    pub radius: f32,
    pub _pad: [f32; 3],
}

pub struct RectRenderer {
    pipeline: wgpu::RenderPipeline,
    mask_pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    mask_instance: wgpu::Buffer,
    capacity: usize,
    count: usize,
}

impl RectRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rect bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rect bind group"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rect layout"),
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let make_pipeline = |label: &str, entry: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<RectInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline("rect pipeline", "fs_main", wgpu::BlendState::ALPHA_BLENDING);
        // dst *= src-alpha: multiplies the whole frame by rounded coverage.
        let multiply = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::SrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let mask_pipeline = make_pipeline(
            "corner mask pipeline",
            "fs_mask",
            wgpu::BlendState { color: multiply, alpha: multiply },
        );

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect instances"),
            size: (256 * std::mem::size_of::<RectInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mask_instance = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("corner mask instance"),
            size: std::mem::size_of::<RectInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            mask_pipeline,
            uniform,
            bind_group,
            instances,
            mask_instance,
            capacity: 256,
            count: 0,
        }
    }

    /// Upload the window-corner mask: a full-surface rounded rect.
    pub fn prepare_mask(&mut self, queue: &wgpu::Queue, width: u32, height: u32, radius: f32) {
        let inst = RectInstance {
            pos: [0.0, 0.0],
            size: [width as f32, height as f32],
            color: [1.0, 1.0, 1.0, 1.0],
            radius,
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.mask_instance, 0, bytemuck::bytes_of(&inst));
    }

    /// Multiply the frame by the rounded-corner coverage. Call last.
    pub fn render_mask(&self, pass: &mut wgpu::RenderPass) {
        pass.set_pipeline(&self.mask_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.mask_instance.slice(..));
        pass.draw(0..6, 0..1);
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rects: &[RectInstance],
        width: u32,
        height: u32,
    ) {
        self.count = rects.len();
        if rects.is_empty() {
            return;
        }
        if rects.len() > self.capacity {
            self.capacity = rects.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rect instances"),
                size: (self.capacity * std::mem::size_of::<RectInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(rects));
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::cast_slice(&[width as f32, height as f32, 0.0, 0.0]),
        );
    }

    /// Render a sub-range of the prepared instances; lets callers interleave
    /// rect layers with other pipelines (e.g. chrome below text, cursor above).
    pub fn render_range(&self, pass: &mut wgpu::RenderPass, range: std::ops::Range<u32>) {
        if range.is_empty() || self.count == 0 {
            return;
        }
        let end = range.end.min(self.count as u32);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, range.start..end);
    }
}

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
/// `t` is the box-drawing line thickness in *cell-relative* units of the
/// smaller cell dimension; the caller converts to pixels.
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
