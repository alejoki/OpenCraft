//! Title/logo/tint helpers and the textured-quad logo pipeline for the
//! vista front door.
//!
//! The geometry at the top of this file is GPU-free and headless-testable;
//! `LogoRenderer` at the bottom consumes those helpers but never the other
//! way around, so the pure math can be proven without a device.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::quad::Quad;
use crate::pack::PackLogo;
use crate::render::context::RenderContext;

/// Engine-styled title text. "Engine-styled" is the same monospace family
/// every other UI string uses (see `ui::text`).
pub const TITLE_TEXT: &str = "OpenCraft";

/// Large title font size: 44 px versus the 18 px UI font (>2x).
pub const TITLE_FONT_SIZE: f32 = 44.0;

/// Line height matching [`TITLE_FONT_SIZE`] plus padding.
pub const TITLE_LINE_HEIGHT: f32 = 56.0;

/// Top edge of the title strip, in physical pixels (y down).
pub const TITLE_STRIP_TOP: f32 = 4.0;

/// Height of the title strip. The no-churn decision: the strip ends where
/// every fixed panel begins (`menu::MENU_TOP == 60.0`), so no screen
/// geometry or hit test moves.
pub const TITLE_STRIP_HEIGHT: f32 = super::menu::MENU_TOP - 8.0;

/// A full-surface tint quad from straight RGBA bytes (`u8 / 255 -> f32`,
/// the same convention the pack manifest documents for `overlay_tint`).
pub fn tint_quad(surface_w: u32, surface_h: u32, tint: [u8; 4]) -> Quad {
    Quad {
        x: 0.0,
        y: 0.0,
        w: surface_w as f32,
        h: surface_h as f32,
        color: [
            tint[0] as f32 / 255.0,
            tint[1] as f32 / 255.0,
            tint[2] as f32 / 255.0,
            tint[3] as f32 / 255.0,
        ],
    }
}

/// Pixel-space rect `(x, y, w, h)` for a native-size logo scaled
/// aspect-preserving so its height exactly fits `max_h`, centered
/// horizontally and top-anchored at [`TITLE_STRIP_TOP`].
///
/// `max_h` is clamped to at least 1.0 so degenerate input cannot produce
/// zero or negative dimensions; degenerate logo dimensions (0 x 0) yield an
/// empty, centered rect rather than a panic or a division by zero.
pub fn logo_rect(surface_w: u32, logo_w: u32, logo_h: u32, max_h: f32) -> (f32, f32, f32, f32) {
    if logo_w == 0 || logo_h == 0 {
        return ((surface_w as f32 / 2.0).max(0.0), TITLE_STRIP_TOP, 0.0, 0.0);
    }

    let max_h = max_h.max(1.0);
    let scale = max_h / logo_h as f32;
    let w = logo_w as f32 * scale;
    let h = max_h;
    let x = ((surface_w as f32 - w) / 2.0).max(0.0);
    (x, TITLE_STRIP_TOP, w, h)
}

/// One textured-quad vertex: clip-space position plus image UV.
///
/// Same `clip_pos` shape as `render::pipeline::OverlayVertex`, but carries a
/// texture coordinate instead of a color.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct LogoVertex {
    pub clip_pos: [f32; 4],
    pub uv: [f32; 2],
}

/// Convert one pixel-space logo rect to 6 [`LogoVertex`]s with the same
/// corner order and NDC math as `quad::quad_to_vertices` (`x_ndc = 2x/sw - 1`,
/// `y_ndc = 1 - 2y/sh`, y-down -> y-up flip, surface dims guarded with
/// `.max(1)`).
///
/// UVs map the image top-left to pixel-space top-left: `(0,0)` top-left,
/// `(1,0)` top-right, `(1,1)` bottom-right, `(0,1)` bottom-left — matching
/// `queue.write_texture` row order (row 0 is the image top).
pub fn logo_to_vertices(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Vec<LogoVertex> {
    let sw = surface_w.max(1) as f32;
    let sh = surface_h.max(1) as f32;

    let nx0 = 2.0 * x / sw - 1.0;
    let nx1 = 2.0 * (x + w) / sw - 1.0;
    let ny0 = 1.0 - 2.0 * y / sh;
    let ny1 = 1.0 - 2.0 * (y + h) / sh;

    let corner = |nx: f32, ny: f32, u: f32, v: f32| LogoVertex {
        clip_pos: [nx, ny, 0.0, 1.0],
        uv: [u, v],
    };

    let c0 = corner(nx0, ny0, 0.0, 0.0); // top-left
    let c1 = corner(nx1, ny0, 1.0, 0.0); // top-right
    let c2 = corner(nx1, ny1, 1.0, 1.0); // bottom-right
    let c3 = corner(nx0, ny1, 0.0, 1.0); // bottom-left

    vec![c0, c1, c2, c2, c3, c0]
}

/// Left edge for a horizontally centered title, clamped at 0 with the same
/// convention as `screens::centered_x`.
pub fn centered_title_left(surface_w: f32, measured_w: f32) -> f32 {
    ((surface_w - measured_w) / 2.0).max(0.0)
}

/// Straight-alpha textured-quad shader for the pack logo.
///
/// One texture + sampler at group 0, no uniforms: the GPU cost is a single
/// two-triangle draw per frame when a logo is present.
const LOGO_SHADER_WGSL: &str = r#"
struct LogoOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@vertex
fn vs_main(
    @location(0) clip_pos: vec4<f32>,
    @location(1) uv: vec2<f32>,
) -> LogoOutput {
    var out: LogoOutput;
    out.clip_pos = clip_pos;
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: LogoOutput) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;

/// Draws the optional pack logo as one textured quad inside the color-only
/// UI pass.
///
/// Texture creation and upload happen only in [`set_logo`](Self::set_logo),
/// i.e. at startup and when the Texture Pack selection changes — never on
/// the render hot path. [`set_logo_frame`](Self::set_logo_frame) rebuilds a
/// small CPU vertex buffer per frame (same pattern as `QuadRenderer`).
///
/// Draw this between the quad renderer (panels/tint) and glyphon text so the
/// logo sits above the panorama tint and below the UI text.
pub struct LogoRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    // The bind group owns the texture view, which keeps the texture alive;
    // dropping the bind group (set_logo(None)/replacement) releases both.
    bind_group: Option<wgpu::BindGroup>,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_count: u32,
}

impl LogoRenderer {
    pub fn new(ctx: &RenderContext) -> Self {
        let shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ui_logo_shader"),
                source: wgpu::ShaderSource::Wgsl(LOGO_SHADER_WGSL.into()),
            });

        let bind_group_layout =
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("ui_logo_bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ui_logo_pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ui_logo_pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<LogoVertex>() as wgpu::BufferAddress,
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
                                format: wgpu::VertexFormat::Float32x2,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        // Straight (non-premultiplied) RGBA, same convention as
                        // QuadRenderer: transparent PNG logos composite correctly.
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
            bind_group_layout,
            bind_group: None,
            vertex_buffer: None,
            vertex_count: 0,
        }
    }

    /// Replace the logo texture. Call on pack load/switch only.
    ///
    /// `None` clears the texture, bind group, and any pending frame geometry
    /// so the caller falls back to the text title. A logo whose byte payload
    /// does not match its declared dimensions is treated as absent rather
    /// than handed to `write_texture` (which would panic on the size
    /// mismatch).
    pub fn set_logo(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        logo: Option<&PackLogo>,
    ) {
        self.vertex_buffer = None;
        self.vertex_count = 0;

        let Some(logo) = logo else {
            self.bind_group = None;
            return;
        };

        let width = logo.width.max(1);
        let height = logo.height.max(1);
        let expected_len = width as usize * height as usize * 4;
        if logo.rgba.len() != expected_len {
            self.bind_group = None;
            return;
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui_logo_texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB texture matching the sRGB surface: pure-color logos are
            // not gamma-shifted. NPOT/non-square logos need no mipmaps.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &logo.rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                // RGBA8 is 4 bytes per texel, so rows are always 4-aligned.
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui_logo_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_logo_bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        }));
    }

    /// Rebuild the per-frame logo vertex buffer. Pure CPU work (no texture
    /// upload); call once per frame before the UI pass opens.
    ///
    /// The vertex buffer is cleared when there is no logo or no rect, so a
    /// `None` call is the explicit "do not draw a logo this frame" signal
    /// (used to keep the logo off Pause/Playing screens).
    pub fn set_logo_frame(
        &mut self,
        device: &wgpu::Device,
        rect: Option<(f32, f32, f32, f32)>,
        surface_w: u32,
        surface_h: u32,
    ) {
        let verts = match rect {
            Some((x, y, w, h))
                if self.bind_group.is_some()
                    && w > 0.0
                    && h > 0.0
                    && x.is_finite()
                    && y.is_finite() =>
            {
                logo_to_vertices(x, y, w, h, surface_w, surface_h)
            }
            _ => Vec::new(),
        };

        self.vertex_count = verts.len() as u32;
        self.vertex_buffer = if verts.is_empty() {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ui_logo_vbuf"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            )
        };
    }

    /// Draw the logo into the UI pass. No-op when no texture or frame
    /// geometry is pending. Call after quads and before glyphon text.
    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.vertex_count == 0 {
            return;
        }
        let (Some(bind_group), Some(buffer)) = (&self.bind_group, &self.vertex_buffer) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..self.vertex_count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: {actual} != {expected}"
        );
    }

    #[test]
    fn tint_quad_covers_full_surface_and_converts_channels() {
        let quad = tint_quad(1280, 720, [0, 51, 255, 128]);
        assert_eq!((quad.x, quad.y, quad.w, quad.h), (0.0, 0.0, 1280.0, 720.0));
        assert_close(quad.color[0], 0.0, "red");
        assert_close(quad.color[1], 0.2, "green");
        assert_close(quad.color[2], 1.0, "blue");
        assert_close(quad.color[3], 128.0 / 255.0, "alpha");
    }

    #[test]
    fn logo_rect_preserves_aspect_and_fills_strip_height() {
        // 64x32 logo: 2:1 aspect, fitted to a 52 px strip.
        let (x, y, w, h) = logo_rect(800, 64, 32, TITLE_STRIP_HEIGHT);
        assert_close(h, 52.0, "height");
        assert_close(w, 104.0, "width keeps 2:1 aspect");
        assert_close(w / h, 2.0, "aspect");
        assert_close(y, TITLE_STRIP_TOP, "top anchored");
        assert_close(x, (800.0 - 104.0) / 2.0, "centered horizontally");
    }

    #[test]
    fn logo_rect_non_square_tall_logo_keeps_aspect() {
        let (_, _, w, h) = logo_rect(400, 20, 64, 50.0);
        assert_close(h, 50.0, "height");
        assert_close(w, 20.0 * 50.0 / 64.0, "width preserves aspect");
    }

    #[test]
    fn logo_rect_clamps_max_height_to_at_least_one() {
        for max_h in [0.0, -10.0] {
            let (_, y, w, h) = logo_rect(100, 10, 10, max_h);
            assert!(h >= 1.0, "height must stay positive, got {h}");
            assert!(w >= 1.0, "width must stay positive, got {w}");
            assert_close(y, TITLE_STRIP_TOP, "top anchored");
        }
    }

    #[test]
    fn logo_rect_degenerate_logo_dims_do_not_panic_or_divide() {
        for (lw, lh) in [(0, 0), (0, 32), (32, 0)] {
            let (x, y, w, h) = logo_rect(640, lw, lh, 52.0);
            assert!(w.is_finite() && h.is_finite());
            assert!(w >= 0.0 && h >= 0.0);
            assert!(x.is_finite() && x >= 0.0);
            assert_close(y, TITLE_STRIP_TOP, "top anchored");
        }
    }

    #[test]
    fn logo_rect_wider_than_surface_clamps_left_to_zero() {
        let (x, _, w, _) = logo_rect(100, 1000, 10, 52.0);
        assert!(w > 100.0);
        assert_close(x, 0.0, "left clamps to zero");
    }

    #[test]
    fn logo_to_vertices_has_six_vertices_with_clip_corners_and_uvs() {
        // Full-surface rect on a 100x50 surface.
        let verts = logo_to_vertices(0.0, 0.0, 100.0, 50.0, 100, 50);
        assert_eq!(verts.len(), 6);
        assert!(verts.iter().all(|v| v.clip_pos[3] == 1.0));

        // Corner order: 0,1,2 then 2,3,0.
        let order = [0usize, 1, 2, 2, 3, 0];
        let corners = [
            (-1.0, 1.0),  // top-left
            (1.0, 1.0),   // top-right
            (1.0, -1.0),  // bottom-right
            (-1.0, -1.0), // bottom-left
        ];
        let uvs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for (i, &corner) in order.iter().enumerate() {
            let v = &verts[i];
            assert_close(v.clip_pos[0], corners[corner].0, &format!("vertex {i} x"));
            assert_close(v.clip_pos[1], corners[corner].1, &format!("vertex {i} y"));
            assert_close(v.uv[0], uvs[corner].0, &format!("vertex {i} u"));
            assert_close(v.uv[1], uvs[corner].1, &format!("vertex {i} v"));
        }

        // Y-flip direction: image top (pixel y = 0) maps to NDC +1 (top).
        assert!(verts[0].clip_pos[1] > verts[2].clip_pos[1]);
    }

    #[test]
    fn logo_to_vertices_offset_rect_maps_correctly() {
        // Centered half-size rect on 100x100: (25,25)-(75,75).
        let verts = logo_to_vertices(25.0, 25.0, 50.0, 50.0, 100, 100);
        assert_eq!(verts.len(), 6);
        assert_close(verts[0].clip_pos[0], -0.5, "top-left x");
        assert_close(verts[0].clip_pos[1], 0.5, "top-left y");
        assert_close(verts[2].clip_pos[0], 0.5, "bottom-right x");
        assert_close(verts[2].clip_pos[1], -0.5, "bottom-right y");
        assert_eq!(verts[0].uv, [0.0, 0.0]);
        assert_eq!(verts[2].uv, [1.0, 1.0]);
    }

    #[test]
    fn logo_to_vertices_degenerate_surface_dims_stay_finite() {
        let verts = logo_to_vertices(0.0, 0.0, 10.0, 10.0, 0, 0);
        assert_eq!(verts.len(), 6);
        assert!(verts.iter().all(|v| {
            v.clip_pos[0].is_finite()
                && v.clip_pos[1].is_finite()
                && v.clip_pos[3] == 1.0
                && v.uv.iter().all(|c| c.is_finite())
        }));
    }

    #[test]
    fn centered_title_left_centers_and_clamps() {
        assert_close(centered_title_left(800.0, 200.0), 300.0, "centered");
        assert_close(centered_title_left(100.0, 400.0), 0.0, "clamped at zero");
        assert_close(centered_title_left(0.0, 0.0), 0.0, "degenerate");
    }
}
