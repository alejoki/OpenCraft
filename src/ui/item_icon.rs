//! Pure block-item cube icon geometry and texture-array face mapping.
//!
//! Everything above the renderer (added in a later task) is GPU-free: given a
//! pixel-space slot rect it emits textured triangles in clip space, exactly
//! like `ui::quad` and `ui::logo` do for their own vertices. Keeping this
//! module's geometry pure means the three-face cube can be proven headless,
//! before any wgpu device exists.
//!
//! Coordinates are physical pixels, top-left origin, y down, converted to NDC
//! with the codebase-wide convention (`x_ndc = 2x/sw - 1`,
//! `y_ndc = 1 - 2y/sh`, surface dimensions guarded with `.max(1)`).

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::inventory::Rect;
use crate::player::inventory::{Item, ItemId};
use crate::render::context::RenderContext;
use crate::world::block::BlockId;

/// Fraction of the icon's smaller edge kept as empty padding on each side.
pub const ICON_INSET_FRACTION: f32 = 0.15;

/// One textured UI icon vertex: clip-space position, texture UV, and the
/// `texture_2d_array` layer to sample.
///
/// The trailing `_pad` keeps the struct free of implicit padding while still
/// matching the 32-byte stride a `Float32x4 + Float32x2 + Uint32` GPU layout
/// expects (offsets 0, 16, 24; stride 32). It is always zero.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct IconVertex {
    pub clip_pos: [f32; 4],
    pub uv: [f32; 2],
    pub layer: u32,
    _pad: [u8; 4],
}

impl IconVertex {
    const fn new(clip_pos: [f32; 4], uv: [f32; 2], layer: u32) -> Self {
        Self {
            clip_pos,
            uv,
            layer,
            _pad: [0; 4],
        }
    }
}

/// Pure byte-offset description of [`IconVertex`], consumed by the icon
/// pipeline's `VertexBufferLayout`. Kept GPU-free so tests can pin the ABI
/// without constructing a device.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IconVertexLayout {
    pub stride: usize,
    pub clip_pos_offset: usize,
    pub uv_offset: usize,
    pub layer_offset: usize,
}

/// The one and only vertex layout for [`IconVertex`].
pub const ICON_VERTEX_LAYOUT: IconVertexLayout = IconVertexLayout {
    stride: std::mem::size_of::<IconVertex>(),
    clip_pos_offset: std::mem::offset_of!(IconVertex, clip_pos),
    uv_offset: std::mem::offset_of!(IconVertex, uv),
    layer_offset: std::mem::offset_of!(IconVertex, layer),
};

/// Returns [`ICON_VERTEX_LAYOUT`]; a function form keeps call sites reading
/// like the pure geometry helpers around it.
pub const fn icon_vertex_layout() -> IconVertexLayout {
    ICON_VERTEX_LAYOUT
}

/// Texture-array layers for the three faces of a block icon cube.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlockIconLayers {
    pub top: u8,
    pub left: u8,
    pub right: u8,
}

/// Maps a block id onto the three visible cube-face layers.
///
/// The faces reuse the existing block texture-array layers: top is face
/// direction 2 (+Y), left is 4 (+Z), and right is 0 (+X). A later slice that
/// adds a real crafting-table texture only has to teach
/// [`BlockId::face_tex_layer`] the new layer; every icon path already routes
/// through this function, so the mapping never needs duplicating.
#[inline]
pub const fn block_icon_layers(block: BlockId) -> BlockIconLayers {
    BlockIconLayers {
        top: block.face_tex_layer(2),
        left: block.face_tex_layer(4),
        right: block.face_tex_layer(0),
    }
}

/// Inset one slot rect into the icon area.
///
/// The inset is a deterministic fraction of the smaller edge, so square and
/// non-square slots both get breathing room. Degenerate input (zero, negative,
/// or non-finite dimensions) collapses to a finite empty rect instead of
/// producing an inverted one.
pub fn icon_rect((x, y, w, h): Rect) -> Rect {
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
    let h = if h.is_finite() { h.max(0.0) } else { 0.0 };
    if w == 0.0 || h == 0.0 {
        return (x, y, 0.0, 0.0);
    }

    let inset = ICON_INSET_FRACTION * w.min(h);
    (
        x + inset,
        y + inset,
        (w - 2.0 * inset).max(0.0),
        (h - 2.0 * inset).max(0.0),
    )
}

/// Emit one pixel-space quad as two triangles (corner order 0,1,2 then 2,3,0),
/// carrying full 0..=1 UVs and the given texture-array layer.
fn push_quad(
    out: &mut Vec<IconVertex>,
    corners: [(f32, f32); 4],
    layer: u32,
    surface_w: f32,
    surface_h: f32,
) {
    let uvs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    let vertex = |index: usize| {
        let (px, py) = corners[index];
        IconVertex::new(
            [
                2.0 * px / surface_w - 1.0,
                1.0 - 2.0 * py / surface_h,
                0.0,
                1.0,
            ],
            [uvs[index].0, uvs[index].1],
            layer,
        )
    };
    let c0 = vertex(0);
    let c1 = vertex(1);
    let c2 = vertex(2);
    let c3 = vertex(3);
    out.extend([c0, c1, c2, c2, c3, c0]);
}

/// Build the 18 vertices of a three-face block cube icon inside `slot`.
///
/// Face order is top, left, right (6 vertices each). The cube silhouette fills
/// [`icon_rect`] exactly: a top rhombus over two side parallelograms, sharing
/// edges but never overlapping. Every coordinate is finite even for 0x0 and
/// 1x1 surfaces because the surface dimensions are clamped to at least one.
pub fn cube_icon_vertices(
    slot: Rect,
    layers: BlockIconLayers,
    surface_w: u32,
    surface_h: u32,
) -> Vec<IconVertex> {
    let (ix, iy, iw, ih) = icon_rect(slot);
    let sw = surface_w.max(1) as f32;
    let sh = surface_h.max(1) as f32;

    let cx = ix + iw * 0.5;
    let half_w = iw * 0.5;
    let top_h = ih * 0.25;
    let side_h = ih * 0.5;

    let top = (cx, iy);
    let right = (cx + half_w, iy + top_h);
    let bottom = (cx, iy + 2.0 * top_h);
    let left = (cx - half_w, iy + top_h);
    let bottom_left = (cx - half_w, iy + top_h + side_h);
    let bottom_right = (cx + half_w, iy + top_h + side_h);
    let bottom_mid = (cx, iy + 2.0 * top_h + side_h);

    let mut verts = Vec::with_capacity(18);
    // Top rhombus.
    push_quad(
        &mut verts,
        [top, right, bottom, left],
        layers.top as u32,
        sw,
        sh,
    );
    // Left side (faces +Z / south).
    push_quad(
        &mut verts,
        [left, bottom, bottom_mid, bottom_left],
        layers.left as u32,
        sw,
        sh,
    );
    // Right side (faces +X / east).
    push_quad(
        &mut verts,
        [bottom, right, bottom_right, bottom_mid],
        layers.right as u32,
        sw,
        sh,
    );
    verts
}

/// Icon vertices for one inventory item, or `None` when the item has no cube
/// icon in this slice.
///
/// Only carryable block identities produce a cube; materials and tools return
/// `None` until their own icon path lands. Empty or uncarryable items also
/// return `None`, so a malformed caller can never draw a ghost cube.
pub fn icon_vertices_for_item(
    item: Item,
    slot: Rect,
    surface_w: u32,
    surface_h: u32,
) -> Option<Vec<IconVertex>> {
    match item.id() {
        ItemId::Block(block) if item.count() > 0 && block.is_carryable() => Some(
            cube_icon_vertices(slot, block_icon_layers(block), surface_w, surface_h),
        ),
        _ => None,
    }
}

/// Filter a CPU icon batch down to the vertices the GPU can safely consume.
///
/// Inputs are already in clip space, so the surface dimensions do not affect
/// the result; they stay in the signature to mirror the other UI upload
/// helpers and make the caller's "this batch belongs to this surface"
/// contract explicit. Non-finite vertices are dropped rather than uploaded,
/// which keeps a malformed caller from poisoning the whole buffer.
pub fn sanitize_icon_vertices(
    icons: &[IconVertex],
    _surface_w: u32,
    _surface_h: u32,
) -> Vec<IconVertex> {
    icons
        .iter()
        .copied()
        .filter(|v| v.clip_pos.iter().all(|c| c.is_finite()))
        .collect()
}

/// Textured UI icon shader: `IconVertex` in, one `texture_2d_array` sample
/// out, with the texture-array layer carried per vertex.
const ICON_SHADER_WGSL: &str = r#"
struct IconOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@group(0) @binding(0) var tex: texture_2d_array<f32>;
@group(0) @binding(1) var samp: sampler;

@vertex
fn vs_main(
    @location(0) clip_pos: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) layer: u32,
) -> IconOutput {
    var out: IconOutput;
    out.clip_pos = clip_pos;
    out.uv = uv;
    out.layer = layer;
    return out;
}

@fragment
fn fs_main(in: IconOutput) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv, i32(in.layer));
}
"#;

/// Draws textured item icons (block cube faces) inside the color-only UI
/// pass, sampling the world's block texture array.
///
/// The texture view/sampler are borrowed once at construction (they come from
/// [`crate::render::pipeline::VoxelRenderer`]) and then owned by the bind
/// group. Only the per-frame vertex buffer is rebuilt in
/// [`set_icons`](Self::set_icons), the same pattern as `QuadRenderer` and
/// `LogoRenderer`.
///
/// Draw this strictly after the quad and logo renderers and strictly before
/// glyphon text, so icons sit above panels/tint and below labels/counts.
pub struct IconRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: Option<wgpu::BindGroup>,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_count: u32,
}

impl IconRenderer {
    pub fn new(
        ctx: &RenderContext,
        texture_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ui_item_icon_shader"),
                source: wgpu::ShaderSource::Wgsl(ICON_SHADER_WGSL.into()),
            });

        let bind_group_layout =
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("ui_item_icon_bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
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
                label: Some("ui_item_icon_pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ui_item_icon_pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: ICON_VERTEX_LAYOUT.stride as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: ICON_VERTEX_LAYOUT.clip_pos_offset as wgpu::BufferAddress,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                            wgpu::VertexAttribute {
                                offset: ICON_VERTEX_LAYOUT.uv_offset as wgpu::BufferAddress,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: ICON_VERTEX_LAYOUT.layer_offset as wgpu::BufferAddress,
                                shader_location: 2,
                                format: wgpu::VertexFormat::Uint32,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        // Straight-alpha blending, same convention as quads
                        // and the logo: pack art composites over panels.
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

        let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_item_icon_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        Self {
            pipeline,
            bind_group: Some(bind_group),
            vertex_buffer: None,
            vertex_count: 0,
        }
    }

    /// Rebuild the per-frame icon vertex buffer. Pure CPU work (no texture
    /// upload); call once per frame before the UI pass opens. An empty slice
    /// clears the buffer so nothing is drawn.
    pub fn set_icons(
        &mut self,
        device: &wgpu::Device,
        icons: &[IconVertex],
        surface_w: u32,
        surface_h: u32,
    ) {
        let verts = sanitize_icon_vertices(icons, surface_w, surface_h);
        self.vertex_count = verts.len() as u32;
        self.vertex_buffer = if verts.is_empty() {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ui_item_icon_vbuf"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            )
        };
    }

    /// Draw pending icons into the UI pass. No-op when nothing is queued.
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
    use crate::player::inventory::{Item, ItemId, Material, ToolKind, ToolTier};

    const SLOT: Rect = (10.0, 20.0, 40.0, 40.0);

    fn item(id: ItemId) -> Item {
        Item::new(id, 1).expect("test item is valid")
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: {actual} != {expected}"
        );
    }

    /// Convert a clip-space vertex back to pixel space for rect containment
    /// checks (inverse of the module's NDC math).
    fn pixel(v: &IconVertex, surface_w: u32, surface_h: u32) -> (f32, f32) {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        (
            (v.clip_pos[0] + 1.0) * 0.5 * sw,
            (1.0 - v.clip_pos[1]) * 0.5 * sh,
        )
    }

    #[test]
    fn cube_has_18_vertices_in_top_left_right_face_order() {
        let layers = block_icon_layers(BlockId::Grass);
        let verts = cube_icon_vertices(SLOT, layers, 800, 600);
        assert_eq!(verts.len(), 18);

        // Six vertices per face, in top / left / right order.
        for v in &verts[..6] {
            assert_eq!(v.layer, layers.top as u32, "top face layer");
        }
        for v in &verts[6..12] {
            assert_eq!(v.layer, layers.left as u32, "left face layer");
        }
        for v in &verts[12..] {
            assert_eq!(v.layer, layers.right as u32, "right face layer");
        }
    }

    #[test]
    fn cube_vertices_are_finite_inside_icon_rect_and_uvs_are_unit_range() {
        let (sx, sy, sw, sh) = SLOT;
        let (ix, iy, iw, ih) = icon_rect(SLOT);
        assert!(ix >= sx && iy >= sy);
        assert!(iw <= sw && ih <= sh);

        let verts = cube_icon_vertices(SLOT, block_icon_layers(BlockId::OakLog), 800, 600);
        for (index, v) in verts.iter().enumerate() {
            assert!(v.clip_pos.iter().all(|c| c.is_finite()), "vertex {index}");
            assert_eq!(v.clip_pos[2], 0.0);
            assert_eq!(v.clip_pos[3], 1.0);
            for (axis, uv) in v.uv.iter().enumerate() {
                assert!((0.0..=1.0).contains(uv), "vertex {index} uv[{axis}] = {uv}");
            }

            let (px, py) = pixel(v, 800, 600);
            assert!(
                px >= ix - 1e-4 && px <= ix + iw + 1e-4,
                "vertex {index} x={px} outside [{ix}, {}]",
                ix + iw
            );
            assert!(
                py >= iy - 1e-4 && py <= iy + ih + 1e-4,
                "vertex {index} y={py} outside [{iy}, {}]",
                iy + ih
            );
        }
    }

    #[test]
    fn cube_faces_are_distinct_regions_in_top_left_right_order() {
        // Grass has a distinct top texture and two side textures, so face
        // labels plus centroid ordering prove the quads are laid out as a
        // top rhombus over left/right parallelograms rather than stacked
        // copies of one rect.
        let layers = block_icon_layers(BlockId::Grass);
        let verts = cube_icon_vertices(SLOT, layers, 800, 600);
        assert!(verts[..6].iter().all(|v| v.layer == layers.top as u32));
        assert!(verts[6..].iter().all(|v| v.layer == layers.left as u32));

        let centroid = |slice: &[IconVertex]| {
            let (mut x, mut y) = (0.0, 0.0);
            for v in slice {
                let (px, py) = pixel(v, 800, 600);
                x += px;
                y += py;
            }
            (x / slice.len() as f32, y / slice.len() as f32)
        };
        let (top_x, top_y) = centroid(&verts[..6]);
        let (left_x, left_y) = centroid(&verts[6..12]);
        let (right_x, right_y) = centroid(&verts[12..]);

        assert!(top_x > left_x && top_x < right_x);
        assert!(top_y < left_y && top_y < right_y);
        assert!(left_x < right_x);
    }

    #[test]
    fn icon_vertex_layout_is_pinned_for_the_gpu_stride() {
        let layout = icon_vertex_layout();
        assert_eq!(layout.stride, 32);
        assert_eq!(layout.clip_pos_offset, 0);
        assert_eq!(layout.uv_offset, 16);
        assert_eq!(layout.layer_offset, 24);
        assert_eq!(std::mem::size_of::<IconVertex>(), 32);
    }

    #[test]
    fn block_icon_layers_match_face_texture_mapping() {
        let grass = block_icon_layers(BlockId::Grass);
        assert_eq!((grass.top, grass.left, grass.right), (0, 1, 1));

        let log = block_icon_layers(BlockId::OakLog);
        assert_eq!((log.top, log.left, log.right), (6, 7, 7));

        let table = block_icon_layers(BlockId::CraftingTable);
        assert_eq!((table.top, table.left, table.right), (9, 9, 9));
    }

    #[test]
    fn same_input_yields_identical_vertices() {
        let layers = block_icon_layers(BlockId::Stone);
        let a = cube_icon_vertices(SLOT, layers, 640, 480);
        let b = cube_icon_vertices(SLOT, layers, 640, 480);
        assert_eq!(a.len(), b.len());
        for (va, vb) in a.iter().zip(&b) {
            assert_eq!(va.clip_pos, vb.clip_pos);
            assert_eq!(va.uv, vb.uv);
            assert_eq!(va.layer, vb.layer);
        }
    }

    #[test]
    fn degenerate_surfaces_stay_finite() {
        let layers = block_icon_layers(BlockId::Grass);
        for (sw, sh) in [(0, 0), (1, 1), (0, 480), (640, 0)] {
            let verts = cube_icon_vertices(SLOT, layers, sw, sh);
            assert_eq!(verts.len(), 18);
            for v in &verts {
                assert!(
                    v.clip_pos.iter().all(|c| c.is_finite()),
                    "surface {sw}x{sh} produced a non-finite vertex"
                );
                assert_eq!(v.clip_pos[3], 1.0);
            }
        }
    }

    #[test]
    fn icon_rect_insets_and_never_inverts() {
        let rect = icon_rect((10.0, 20.0, 40.0, 40.0));
        assert_close(rect.0, 16.0, "inset x");
        assert_close(rect.1, 26.0, "inset y");
        assert_close(rect.2, 28.0, "inset width");
        assert_close(rect.3, 28.0, "inset height");

        for slot in [
            (0.0, 0.0, 0.0, 0.0),
            (5.0, 5.0, 1.0, 1.0),
            (5.0, 5.0, -10.0, 4.0),
            (f32::NAN, f32::INFINITY, 10.0, 10.0),
        ] {
            let (x, y, w, h) = icon_rect(slot);
            assert!(x.is_finite() && y.is_finite());
            assert!(w >= 0.0 && h >= 0.0, "never inverted: {w} x {h}");
        }
    }

    #[test]
    fn icon_vertices_for_item_accepts_blocks_and_rejects_other_identities() {
        let grass = item(ItemId::Block(BlockId::Grass));
        let cube = icon_vertices_for_item(grass, SLOT, 800, 600);
        assert_eq!(cube.expect("block has a cube icon").len(), 18);

        let stick = item(ItemId::Material(Material::Stick));
        assert!(icon_vertices_for_item(stick, SLOT, 800, 600).is_none());

        let pickaxe = item(ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Wood,
        });
        assert!(icon_vertices_for_item(pickaxe, SLOT, 800, 600).is_none());
    }

    #[test]
    fn icon_shader_samples_the_texture_array_with_a_per_vertex_layer() {
        assert!(
            ICON_SHADER_WGSL.contains("texture_2d_array<f32>"),
            "the icon pipeline must bind the world block texture array"
        );
        assert!(
            ICON_SHADER_WGSL.contains("textureSample(tex, samp, in.uv, i32(in.layer))"),
            "the fragment stage must sample the layer carried by the vertex"
        );
        assert!(
            ICON_SHADER_WGSL.contains("@interpolate(flat) layer: u32"),
            "the integer layer must be flat-interpolated"
        );
    }

    #[test]
    fn ui_draw_order_is_quads_then_logo_then_icons_then_text() {
        // Source pin on UiRenderer::draw_ui: icons must sit above panel/tint
        // quads and the pack logo, but below glyphon text (counts/labels).
        let src = include_str!("mod.rs");
        let quads_at = src
            .find("self.quad_renderer.draw(pass)")
            .expect("quad draw exists");
        let logo_at = src
            .find("self.logo_renderer.draw(pass)")
            .expect("logo draw exists");
        let icons_at = src
            .find("self.icon_renderer.draw(pass)")
            .expect("icon draw exists");
        let text_at = src
            .find("self.text_renderer.render(&self.atlas, pass)")
            .expect("text draw exists");
        assert!(
            quads_at < logo_at && logo_at < icons_at && icons_at < text_at,
            "UI order must be quads -> logo -> icons -> text"
        );
    }

    #[test]
    fn empty_icon_batches_clear_the_gpu_buffer() {
        // Source pin: an empty slice must leave vertex_count 0 and no buffer,
        // so closing every inventory never uploads or draws anything.
        let src = include_str!("item_icon.rs");
        let set_icons_at = src.find("pub fn set_icons").expect("set_icons exists");
        let set_icons_block = &src[set_icons_at..];
        assert!(set_icons_block.contains("self.vertex_count = verts.len() as u32"));
        assert!(set_icons_block.contains("if verts.is_empty()"));
        assert!(set_icons_block.contains("None"));
    }

    #[test]
    fn sanitize_icons_drops_non_finite_vertices_and_accepts_empty() {
        assert!(sanitize_icon_vertices(&[], 1280, 720).is_empty());

        let good = IconVertex::new([0.0, 0.0, 0.0, 1.0], [0.0, 0.0], 3);
        let bad = IconVertex::new([f32::NAN, 0.0, 0.0, 1.0], [0.0, 0.0], 3);
        let kept = sanitize_icon_vertices(&[good, bad], 1280, 720);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].layer, 3);
    }

    #[test]
    fn zero_and_non_finite_surfaces_still_produce_finite_icon_vertices() {
        let layers = block_icon_layers(BlockId::Stone);
        for (sw, sh) in [(0, 0), (1, 1)] {
            let verts =
                cube_icon_vertices((f32::NAN, f32::NEG_INFINITY, 32.0, 32.0), layers, sw, sh);
            assert_eq!(verts.len(), 18);
            assert!(verts
                .iter()
                .all(|v| v.clip_pos.iter().all(|c| c.is_finite())));
        }
    }

    #[test]
    fn main_wires_the_world_block_texture_into_the_ui_icon_renderer() {
        // Source pin: the UI is built after the renderer and handed the same
        // block texture view/sampler, so icons always match the active pack.
        let src = include_str!("../main.rs");
        let ui_at = src
            .find("let mut ui = UiRenderer::new(")
            .expect("UiRenderer::new call exists");
        let ui_block = &src[ui_at..];
        assert!(ui_block.contains("&renderer.block_texture_view"));
        assert!(ui_block.contains("&renderer.block_texture_sampler"));
        let renderer_at = src
            .find("let renderer = VoxelRenderer::new(&ctx);")
            .expect("renderer construction exists");
        assert!(
            renderer_at < ui_at,
            "the renderer must be constructed before the UI so its texture handles exist"
        );
    }
}
