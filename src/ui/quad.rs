use wgpu::util::DeviceExt;

use crate::render::context::RenderContext;
use crate::render::pipeline::OverlayVertex;

/// Pass-through overlay shader (same shape as the line overlay pipeline's):
/// vertices arrive already projected to clip space on the CPU, with a
/// per-vertex color the fragment stage passes through untouched. Defined
/// locally to avoid cross-module churn in `render::pipeline`.
const QUAD_SHADER_WGSL: &str = r#"
struct OverlayOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) clip_pos: vec4<f32>,
    @location(1) color: vec4<f32>,
) -> OverlayOutput {
    var out: OverlayOutput;
    out.clip_pos = clip_pos;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: OverlayOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// A filled rectangle in physical pixels (top-left origin, y down).
///
/// This is the pixel-space drawing API that S04's settings panels and
/// buttons will consume.
#[derive(Copy, Clone, Debug)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [f32; 4],
}

/// Convert one pixel-space quad to 6 `OverlayVertex`s: two triangles with
/// corner indices 0,1,2 and 2,3,0 expanded (no shared index buffer needed
/// at this scale).
///
/// NDC conversion happens on the CPU exactly like the codebase's hand-rolled
/// CPU-vertex style (hotbar/crosshair): `x_ndc = 2x/surface_w - 1` and
/// `y_ndc = 1 - 2y/surface_h` — note the y flip versus pixel space (pixel y
/// grows downward, NDC y grows upward). Pure function, no GPU.
pub fn quad_to_vertices(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [f32; 4],
    surface_w: u32,
    surface_h: u32,
) -> Vec<OverlayVertex> {
    // Guard degenerate surface dimensions so a zero-sized surface cannot
    // divide by zero.
    let sw = surface_w.max(1) as f32;
    let sh = surface_h.max(1) as f32;

    let nx0 = 2.0 * x / sw - 1.0;
    let nx1 = 2.0 * (x + w) / sw - 1.0;
    let ny0 = 1.0 - 2.0 * y / sh;
    let ny1 = 1.0 - 2.0 * (y + h) / sh;

    let corner = |nx: f32, ny: f32| OverlayVertex {
        clip_pos: [nx, ny, 0.0, 1.0],
        color,
    };

    let c0 = corner(nx0, ny0); // top-left
    let c1 = corner(nx1, ny0); // top-right
    let c2 = corner(nx1, ny1); // bottom-right
    let c3 = corner(nx0, ny1); // bottom-left

    vec![c0, c1, c2, c2, c3, c0]
}

/// Convert a slice of pixel-space quads into one interleaved vertex list
/// (6 vertices per quad, in input order). An empty slice yields no vertices.
pub fn quads_to_vertices(quads: &[Quad], surface_w: u32, surface_h: u32) -> Vec<OverlayVertex> {
    let mut verts = Vec::with_capacity(quads.len() * 6);
    for q in quads {
        verts.extend(quad_to_vertices(
            q.x, q.y, q.w, q.h, q.color, surface_w, surface_h,
        ));
    }
    verts
}

/// Draws filled quads into the color-only UI pass using a TriangleList
/// pipeline with alpha blending and `depth_stencil: None` (the UI pass has
/// no depth attachment, and a mismatched depth-stencil pipeline would fail
/// render-pass validation).
///
/// Per frame, `set_quads` (pure CPU: build vertices, upload one vertex
/// buffer) runs before the render pass; `draw` issues the draw call inside
/// the pass. Draw quads BEFORE glyphon text so text sits on top of panels.
pub struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_count: u32,
}

impl QuadRenderer {
    pub fn new(ctx: &RenderContext) -> Self {
        let shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ui_quad_shader"),
                source: wgpu::ShaderSource::Wgsl(QUAD_SHADER_WGSL.into()),
            });

        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ui_quad_pipeline_layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        // Same vertex layout as the line overlay pipeline, but TriangleList.
        let pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ui_quad_pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<OverlayVertex>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                            wgpu::VertexAttribute {
                                offset: 16,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        Self {
            pipeline,
            vertex_buffer: None,
            vertex_count: 0,
        }
    }

    /// Rebuild the per-frame quad vertex buffer. Pure CPU work; call once per
    /// frame before the UI pass opens. An empty quad list clears the buffer
    /// so nothing is drawn.
    pub fn set_quads(
        &mut self,
        device: &wgpu::Device,
        quads: &[Quad],
        surface_w: u32,
        surface_h: u32,
    ) {
        let verts = quads_to_vertices(quads, surface_w, surface_h);
        self.vertex_count = verts.len() as u32;
        self.vertex_buffer = if verts.is_empty() {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ui_quad_vbuf"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            )
        };
    }

    /// Draw the current quads into the UI pass. Call before glyphon text so
    /// the text sits on top of the panels.
    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if let Some(buffer) = &self.vertex_buffer {
            if self.vertex_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..self.vertex_count, 0..1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ndc(v: &OverlayVertex) -> (f32, f32) {
        (v.clip_pos[0], v.clip_pos[1])
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: {actual} != {expected}"
        );
    }

    #[test]
    fn full_surface_quad_maps_to_unit_corners_with_y_flip() {
        let color = [0.0, 0.0, 0.0, 0.4];
        let verts = quad_to_vertices(0.0, 0.0, 100.0, 50.0, color, 100, 50);

        // 6 vertices per quad: triangles (0,1,2) and (2,3,0).
        assert_eq!(verts.len(), 6);
        // Every vertex is a valid clip-space point with w == 1.0.
        assert!(verts.iter().all(|v| v.clip_pos[3] == 1.0));
        assert!(verts.iter().all(|v| v.color == color));

        // Expected corner NDC positions for a full-surface quad.
        let corners = [
            (-1.0, 1.0),  // top-left pixel (0, 0)
            (1.0, 1.0),   // top-right pixel (100, 0)
            (1.0, -1.0),  // bottom-right pixel (100, 50)
            (-1.0, -1.0), // bottom-left pixel (0, 50)
        ];
        let order = [0usize, 1, 2, 2, 3, 0];
        for (i, &corner) in order.iter().enumerate() {
            let (ex, ey) = corners[corner];
            let (vx, vy) = ndc(&verts[i]);
            assert_close(vx, ex, &format!("vertex {i} x"));
            assert_close(vy, ey, &format!("vertex {i} y"));
        }

        // Y-flip direction: pixel y = 0 maps to NDC +1 (top), pixel y = h
        // maps to NDC -1 (bottom) — pixel space is y-down, NDC is y-up.
        assert!(ndc(&verts[0]).1 > ndc(&verts[2]).1);
    }

    #[test]
    fn partial_quad_maps_to_offset_corners() {
        // Centered half-size quad on a 100x100 surface: (25,25)-(75,75).
        let verts = quad_to_vertices(25.0, 25.0, 50.0, 50.0, [1.0; 4], 100, 100);
        assert_eq!(verts.len(), 6);

        // top-left -> (-0.5, +0.5)
        let (x, y) = ndc(&verts[0]);
        assert_close(x, -0.5, "partial top-left x");
        assert_close(y, 0.5, "partial top-left y");
        // bottom-right -> (+0.5, -0.5)
        let (x, y) = ndc(&verts[2]);
        assert_close(x, 0.5, "partial bottom-right x");
        assert_close(y, -0.5, "partial bottom-right y");
        // Every vertex carries w == 1.0.
        assert!(verts.iter().all(|v| v.clip_pos[3] == 1.0));
    }

    #[test]
    fn degenerate_surface_dims_do_not_divide_by_zero() {
        let verts = quad_to_vertices(0.0, 0.0, 10.0, 10.0, [1.0; 4], 0, 0);
        assert_eq!(verts.len(), 6);
        assert!(verts.iter().all(|v| v.clip_pos[3] == 1.0
            && v.clip_pos[0].is_finite()
            && v.clip_pos[1].is_finite()));
    }

    #[test]
    fn quads_to_vertices_concatenates_in_input_order() {
        let quads = [
            Quad {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Quad {
                x: 20.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
                color: [0.0, 1.0, 0.0, 1.0],
            },
        ];
        let verts = quads_to_vertices(&quads, 100, 100);
        assert_eq!(verts.len(), 12);
        assert!(verts[..6].iter().all(|v| v.color == [1.0, 0.0, 0.0, 1.0]));
        assert!(verts[6..].iter().all(|v| v.color == [0.0, 1.0, 0.0, 1.0]));
    }

    #[test]
    fn empty_quad_list_yields_no_vertices() {
        assert!(quads_to_vertices(&[], 100, 100).is_empty());
    }
}
