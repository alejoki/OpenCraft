//! Pure box-model mob geometry (M009/S01-T03, D136).
//!
//! This module is deliberately split into two layers:
//!
//! 1. [`build_mob_vertices`] is a pure model-space builder. It needs no world,
//!    no GPU, and no session: feet sit at `y = 0` and yaw 0 faces `+Z`
//!    (matching [`crate::world::mob`]'s yaw convention).
//! 2. [`build_mob_batch`] is a thin scene assembler that residency-gates,
//!    frustum-culls, lights, and transforms those model vertices into world
//!    space.
//!
//! Every mob is exactly six axis-aligned cuboids (body, head, and four limbs)
//! emitted as `6 parts x 6 faces x 2 triangles x 3 vertices = 216` vertices.
//! The sub-block part sizes are the reason [`crate::mesh::quad::PackedQuad`]
//! cannot represent a mob: that format only stores integer chunk-local block
//! coordinates. [`build_mob_vertices`] and [`build_mob_batch`] stay pure CPU
//! data so the model contract remains headlessly testable; the wgpu-facing
//! [`create_mob_pipeline`] factory below only translates that contract into a
//! pipeline descriptor (one camera bind group, no per-mob state).
//!
//! Arrows share that one batch: [`build_arrow_vertices`] is the pure
//! model-space arrow box (36 vertices, long axis `+Z`, already-authored layer
//! [`crate::render::texture::MOB_ARROW_TEX_LAYER`]) and [`build_mob_batch`]
//! appends one oriented copy per visible projectile. The shader, vertex
//! layout, stride and texture layers 0..=43 are untouched, so a projectile
//! costs vertices in the existing draw call, never a second pipeline or
//! `rpass.draw`.

use glam::{Quat, Vec3};

use crate::player::camera::{Camera, Frustum};
use crate::render::context::{RenderContext, DEPTH_FORMAT};
use crate::render::texture::{MOB_ARROW_TEX_LAYER, MOB_LAYER_BASE};
use crate::world::mob::MobKind;
use crate::world::session::WorldSession;

/// Bytes in one [`MobVertex`]. Pinned by D136 so the WGSL layout cannot drift.
pub const MOB_VERTEX_SIZE: usize = 32;

/// The five vertex attributes of [`MobVertex`], in shader-location order.
/// Offsets are 0 / 12 / 20 / 24 / 28 and depend on the `#[repr(C)]` field
/// order above; a test pins every value.
pub const MOB_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x2,
    2 => Uint32,
    3 => Float32,
    4 => Float32
];

/// One mob geometry vertex (D136).
///
/// `shade` carries the face-shading constant multiplied by the per-mob light
/// factor on the CPU, so world light needs no extra attribute. `flash` is the
/// hurt tint (0 normal, 1 fully red). Distance fog is derived from the shared
/// camera uniform in the vertex shader, so it needs no attribute either.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MobVertex {
    position: [f32; 3],
    uv: [f32; 2],
    tex_layer: u32,
    shade: f32,
    flash: f32,
}

/// The mob vertex buffer layout (stride 32, one vertex per step).
pub fn mob_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: MOB_VERTEX_SIZE as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MOB_VERTEX_ATTRIBUTES,
    }
}

/// Mob shader (WGSL): one batched, textured box-model pass.
///
/// The vertex stage consumes the pinned [`MobVertex`] attributes, applies the
/// shared camera view-projection, and derives the distance fog factor from the
/// existing [`crate::render::pipeline::CameraUniform`] (`camera_pos` plus
/// `fog_start_end.xy`) so the 112-byte uniform layout is untouched. The
/// fragment stage samples the same 256-layer block texture array from the
/// camera bind group (`uv`, flat `tex_layer`), multiplies by the CPU-baked
/// face shade, applies the hurt flash tint, and fades into `fog_color`.
///
/// Group 0 is the existing camera bind group: binding 0 uniform, binding 1
/// `texture_2d_array<f32>`, binding 2 sampler. There is deliberately no group
/// 1: the whole scene draws with a single bind group and a single draw call.
pub const MOB_SHADER_WGSL: &str = r#"
struct CameraUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    fog_color: vec4<f32>,
    fog_start_end: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(0) @binding(1) var block_texture: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) tex_layer: u32,
    @location(2) shade: f32,
    @location(3) flash: f32,
    @location(4) fog: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tex_layer: u32,
    @location(3) shade: f32,
    @location(4) flash: f32,
) -> VertexOutput {
    let cam_dist = distance(position, camera.camera_pos.xyz);
    let fog_start = camera.fog_start_end.x;
    let fog_end = camera.fog_start_end.y;
    let fog_t = clamp((cam_dist - fog_start) / max(fog_end - fog_start, 1.0), 0.0, 1.0);

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    out.tex_layer = tex_layer;
    out.shade = shade;
    out.flash = flash;
    out.fog = fog_t * fog_t;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(block_texture, block_sampler, in.uv, i32(in.tex_layer));
    var rgb = texel.rgb * in.shade;
    rgb = mix(rgb, vec3<f32>(1.0, 0.15, 0.1), in.flash);
    let final_rgb = mix(rgb, camera.fog_color.rgb, in.fog);
    return vec4<f32>(final_rgb, 1.0);
}
"#;

/// Create the single batched mob pipeline.
///
/// The pipeline layout is `&[camera_bgl]` ONLY: mobs reuse the existing camera
/// uniform + block texture array + sampler bind group, so there is no per-mob
/// bind group, no chunk uniform, and no second draw call per mob. The pipeline
/// is opaque (REPLACE blending) and writes depth with `LessEqual`, so opaque
/// terrain occludes mobs and mobs occlude the translucent water drawn after
/// them (D136).
pub fn create_mob_pipeline(
    ctx: &RenderContext,
    camera_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mob_shader"),
            source: wgpu::ShaderSource::Wgsl(MOB_SHADER_WGSL.into()),
        });

    let layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mob_pipeline_layout"),
            bind_group_layouts: &[camera_bgl],
            push_constant_ranges: &[],
        });

    ctx.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mob_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[mob_vertex_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: ctx.config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Mob cuboids are emitted with outward CCW winding, but the
                // model is open (no back faces needed) and death squashes can
                // flip thin parts; hardware culling only costs visibility.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        })
}

/// Animation inputs for one model-space build.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MobAnim {
    /// Walk-cycle phase in radians. Advanced by the 20 Hz mob runtime.
    pub walk_phase: f32,
    /// 0 = standing, 1 = full walk swing. Typically derived from velocity.
    pub walk_amount: f32,
    /// Normalised world light in `0..=1` (sky light / 15).
    pub light: f32,
}

impl Default for MobAnim {
    fn default() -> Self {
        MobAnim {
            walk_phase: 0.0,
            walk_amount: 0.0,
            light: 1.0,
        }
    }
}

/// The three texture-array layers one kind uses. The arrow layer
/// ([`crate::render::texture::MOB_ARROW_TEX_LAYER`]) is reserved for
/// projectiles and no mob uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MobLayers {
    pub body: u32,
    pub head: u32,
    pub limb: u32,
}

/// Texture-array layers per mob kind (32..=43; the arrow is layer 44).
pub fn mob_texture_layers(kind: MobKind) -> MobLayers {
    let base = match kind {
        MobKind::Pig => MOB_LAYER_BASE,
        MobKind::Cow => MOB_LAYER_BASE + 3,
        MobKind::Zombie => MOB_LAYER_BASE + 6,
        MobKind::Skeleton => MOB_LAYER_BASE + 9,
    };
    MobLayers {
        body: base,
        head: base + 1,
        limb: base + 2,
    }
}

/// Number of cuboid parts per mob (body, head, four limbs).
pub const MOB_PART_COUNT: usize = 6;
/// Vertices per cuboid: 6 faces x 2 triangles x 3 vertices.
pub const MOB_VERTS_PER_PART: usize = 36;
/// Total vertices per mob model.
pub const MOB_VERTS_PER_MOB: usize = MOB_PART_COUNT * MOB_VERTS_PER_PART;

/// Face shading constants (top brightest, bottom darkest).
const SHADE_TOP: f32 = 1.0;
const SHADE_BOTTOM: f32 = 0.5;
const SHADE_X: f32 = 0.65;
const SHADE_Z: f32 = 0.8;

/// Maximum walk swing in radians at `walk_amount = 1`.
const WALK_SWING_RAD: f32 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PartTex {
    Body,
    Head,
    Limb,
}

/// One axis-aligned cuboid in model space. `swing` is 0 for body/head and
/// `+1`/`-1` for limbs; the sign selects the diagonal gait phase.
#[derive(Clone, Copy, Debug)]
struct Part {
    center: Vec3,
    size: Vec3,
    tex: PartTex,
    swing: f32,
}

const fn part(center: Vec3, size: Vec3, tex: PartTex, swing: f32) -> Part {
    Part {
        center,
        size,
        tex,
        swing,
    }
}

/// Per-kind part table. The fixed order is part 0 body, 1 head,
/// 2 left-front, 3 right-front, 4 left-back, 5 right-back. Quadrupeds use all
/// four entries as legs; bipeds use parts 2/3 as legs and 4/5 as arms.
///
/// Every part extent is `< 1.0` block, and the union of the six parts spans
/// exactly `x = [-width/2, width/2]`, `y = [0, height]`,
/// `z = [-width/2, width/2]`. Tests pin both properties.
fn kind_parts(kind: MobKind) -> [Part; MOB_PART_COUNT] {
    match kind {
        // Pig: 0.7 wide, 0.9 tall. Body provides the -Z extent, the head the
        // +Z extent, and the legs the exact +-X extent.
        MobKind::Pig => [
            part(
                Vec3::new(0.0, 0.47, -0.10),
                Vec3::new(0.44, 0.34, 0.50),
                PartTex::Body,
                0.0,
            ),
            part(
                Vec3::new(0.0, 0.70, 0.25),
                Vec3::new(0.34, 0.40, 0.20),
                PartTex::Head,
                0.0,
            ),
            part(
                Vec3::new(-0.27, 0.15, 0.0),
                Vec3::new(0.16, 0.30, 0.16),
                PartTex::Limb,
                1.0,
            ),
            part(
                Vec3::new(0.27, 0.15, 0.0),
                Vec3::new(0.16, 0.30, 0.16),
                PartTex::Limb,
                -1.0,
            ),
            part(
                Vec3::new(-0.27, 0.15, -0.25),
                Vec3::new(0.16, 0.30, 0.16),
                PartTex::Limb,
                -1.0,
            ),
            part(
                Vec3::new(0.27, 0.15, -0.25),
                Vec3::new(0.16, 0.30, 0.16),
                PartTex::Limb,
                1.0,
            ),
        ],
        // Cow: 0.9 wide, 1.3 tall.
        MobKind::Cow => [
            part(
                Vec3::new(0.0, 0.735, -0.15),
                Vec3::new(0.58, 0.57, 0.60),
                PartTex::Body,
                0.0,
            ),
            part(
                Vec3::new(0.0, 1.09, 0.30),
                Vec3::new(0.44, 0.42, 0.30),
                PartTex::Head,
                0.0,
            ),
            part(
                Vec3::new(-0.36, 0.225, 0.02),
                Vec3::new(0.18, 0.45, 0.18),
                PartTex::Limb,
                1.0,
            ),
            part(
                Vec3::new(0.36, 0.225, 0.02),
                Vec3::new(0.18, 0.45, 0.18),
                PartTex::Limb,
                -1.0,
            ),
            part(
                Vec3::new(-0.36, 0.225, -0.32),
                Vec3::new(0.18, 0.45, 0.18),
                PartTex::Limb,
                -1.0,
            ),
            part(
                Vec3::new(0.36, 0.225, -0.32),
                Vec3::new(0.18, 0.45, 0.18),
                PartTex::Limb,
                1.0,
            ),
        ],
        // Zombie / Skeleton: 0.7 wide, 1.8 tall. Legs reach the ground; arms
        // hang from the shoulders and provide the exact +-X extent.
        MobKind::Zombie | MobKind::Skeleton => [
            part(
                Vec3::new(0.0, 1.20, 0.0),
                Vec3::new(0.50, 0.60, 0.70),
                PartTex::Body,
                0.0,
            ),
            part(
                Vec3::new(0.0, 1.65, 0.0),
                Vec3::new(0.40, 0.30, 0.40),
                PartTex::Head,
                0.0,
            ),
            part(
                Vec3::new(-0.15, 0.45, 0.0),
                Vec3::new(0.20, 0.90, 0.20),
                PartTex::Limb,
                1.0,
            ),
            part(
                Vec3::new(0.15, 0.45, 0.0),
                Vec3::new(0.20, 0.90, 0.20),
                PartTex::Limb,
                -1.0,
            ),
            part(
                Vec3::new(-0.25, 1.15, 0.0),
                Vec3::new(0.20, 0.70, 0.20),
                PartTex::Limb,
                -1.0,
            ),
            part(
                Vec3::new(0.25, 1.15, 0.0),
                Vec3::new(0.20, 0.70, 0.20),
                PartTex::Limb,
                1.0,
            ),
        ],
    }
}

/// One cuboid face: a fixed outward winding (indices into the 8 corners) and
/// its shading constant.
struct Face {
    corners: [usize; 4],
    shade: f32,
}

/// Corners are `(x0|y0|z0)` bit triples: bit 0 = x1, bit 1 = y1, bit 2 = z1.
fn corner(index: usize, min: Vec3, max: Vec3) -> Vec3 {
    Vec3::new(
        if index & 1 != 0 { max.x } else { min.x },
        if index & 2 != 0 { max.y } else { min.y },
        if index & 4 != 0 { max.z } else { min.z },
    )
}

/// Six faces with outward normals (CCW seen from outside) and the D136 shade
/// constants.
const FACES: [Face; 6] = [
    // +X: 5 (x1,y0,z1), 1 (x1,y0,z0), 3 (x1,y1,z0), 7 (x1,y1,z1)
    Face {
        corners: [5, 1, 3, 7],
        shade: SHADE_X,
    },
    // -X: 0, 4, 6, 2
    Face {
        corners: [0, 4, 6, 2],
        shade: SHADE_X,
    },
    // +Y: 6, 7, 3, 2
    Face {
        corners: [6, 7, 3, 2],
        shade: SHADE_TOP,
    },
    // -Y: 0, 1, 5, 4
    Face {
        corners: [0, 1, 5, 4],
        shade: SHADE_BOTTOM,
    },
    // +Z: 4, 5, 7, 6
    Face {
        corners: [4, 5, 7, 6],
        shade: SHADE_Z,
    },
    // -Z: 1, 0, 2, 3
    Face {
        corners: [1, 0, 2, 3],
        shade: SHADE_Z,
    },
];

/// Corner UVs paired with [`Face::corners`] order: (0,0), (1,0), (1,1), (0,1).
const FACE_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

#[inline]
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

#[inline]
fn rotate_x_about(point: Vec3, pivot: Vec3, angle: f32) -> Vec3 {
    let (sin, cos) = angle.sin_cos();
    let rel = point - pivot;
    pivot + Vec3::new(rel.x, rel.y * cos - rel.z * sin, rel.y * sin + rel.z * cos)
}

/// Build one mob's model-space vertices.
///
/// Model space: feet at `y = 0`, yaw 0 faces `+Z`. The result is always
/// exactly [`MOB_VERTS_PER_MOB`] vertices in six fixed 36-vertex part blocks.
/// Non-finite inputs are sanitised to their neutral value so a malformed
/// caller can never poison the vertex buffer with NaNs.
///
/// `hurt` is clamped to `0..=1` and copied to every vertex's `flash`.
/// `death` (0..=1) squashes the model toward its feet, adds a forward lean,
/// and clamps every final `y` to `>= 0`, so `death = 1.0` leaves a collapsed
/// silhouette.
pub fn build_mob_vertices(kind: MobKind, anim: MobAnim, hurt: f32, death: f32) -> Vec<MobVertex> {
    let light = finite_or(anim.light, 0.0).clamp(0.0, 1.0);
    let shade_mul = 0.35 + 0.65 * light;
    let flash = finite_or(hurt, 0.0).clamp(0.0, 1.0);
    let death = finite_or(death, 0.0).clamp(0.0, 1.0);
    let walk_phase = finite_or(anim.walk_phase, 0.0);
    let walk_amount = finite_or(anim.walk_amount, 0.0).clamp(0.0, 1.0);
    let swing = walk_phase.sin() * walk_amount * WALK_SWING_RAD;

    // Death: squash toward the feet, then lean forward about X through the
    // origin (the feet plane).
    let height_scale = 1.0 - 0.7 * death;
    let lean = death * 0.5;
    let (lean_sin, lean_cos) = lean.sin_cos();

    let layers = mob_texture_layers(kind);
    let parts = kind_parts(kind);
    let mut out = Vec::with_capacity(MOB_VERTS_PER_MOB);

    for part in parts {
        let tex_layer = match part.tex {
            PartTex::Body => layers.body,
            PartTex::Head => layers.head,
            PartTex::Limb => layers.limb,
        };
        let half = part.size * 0.5;
        let min = part.center - half;
        let max = part.center + half;
        // Limbs rotate around their top-centre (hip or shoulder) X axis.
        let pivot = Vec3::new(part.center.x, max.y, part.center.z);
        let angle = part.swing * swing;

        for face in &FACES {
            let shade = face.shade * shade_mul;
            let corners = [
                corner(face.corners[0], min, max),
                corner(face.corners[1], min, max),
                corner(face.corners[2], min, max),
                corner(face.corners[3], min, max),
            ];
            // Two triangles: (0,1,2) and (0,2,3). With the winding above this
            // is CCW from outside, so the face normal points outward.
            for &index in &[0usize, 1, 2, 0, 2, 3] {
                let mut position = corners[index];
                if angle != 0.0 {
                    position = rotate_x_about(position, pivot, angle);
                }
                // Squash toward the feet, then lean forward. Clamp to the
                // ground so no part ever pokes below y = 0.
                let squashed_y = position.y * height_scale;
                let y = squashed_y * lean_cos - position.z * lean_sin;
                let z = squashed_y * lean_sin + position.z * lean_cos;
                position = Vec3::new(position.x, y.max(0.0), z);
                out.push(MobVertex {
                    position: position.to_array(),
                    uv: FACE_UVS[index],
                    tex_layer,
                    shade,
                    flash,
                });
            }
        }
    }

    debug_assert_eq!(out.len(), MOB_VERTS_PER_MOB);
    out
}

/// Vertices in one arrow box: 6 faces x 2 triangles x 3 vertices.
///
/// The arrow reuses the mob vertex format and pipeline; it is a single thin
/// cuboid, not a six-part model, so it costs exactly this many vertices on top
/// of the mob batch.
pub const ARROW_VERTS: usize = 36;

/// Half length of the arrow's long axis, in blocks (0.6 blocks tip to tail).
const ARROW_HALF_LEN: f32 = 0.3;
/// Half extent of the arrow's square cross-section, in blocks (0.06 wide).
const ARROW_HALF_THICK: f32 = 0.03;

/// Build the model-space vertices of one arrow.
///
/// Model space: a single axis-aligned cuboid **centred on the origin** whose
/// long axis is `+Z` and whose cross-section is `x`/`y`. Centring matters:
/// [`build_mob_batch`] rotates about the midpoint, so offsetting the box would
/// swing it in an arc instead of aiming it along the velocity.
///
/// Every vertex uses [`MOB_ARROW_TEX_LAYER`] (already authored on the block
/// texture array) and `flash = 0.0`. `light` is baked into `shade` with the
/// exact same face-shading constants as [`build_mob_vertices`], so arrows and
/// mobs respond identically to world light. Non-finite light is sanitised to
/// `0.0` and clamped to `0..=1`.
pub fn build_arrow_vertices(light: f32) -> Vec<MobVertex> {
    let light = finite_or(light, 0.0).clamp(0.0, 1.0);
    let shade_mul = 0.35 + 0.65 * light;
    let min = Vec3::new(-ARROW_HALF_THICK, -ARROW_HALF_THICK, -ARROW_HALF_LEN);
    let max = Vec3::new(ARROW_HALF_THICK, ARROW_HALF_THICK, ARROW_HALF_LEN);
    let mut out = Vec::with_capacity(ARROW_VERTS);

    for face in &FACES {
        let shade = face.shade * shade_mul;
        let corners = [
            corner(face.corners[0], min, max),
            corner(face.corners[1], min, max),
            corner(face.corners[2], min, max),
            corner(face.corners[3], min, max),
        ];
        for &index in &[0usize, 1, 2, 0, 2, 3] {
            out.push(MobVertex {
                position: corners[index].to_array(),
                uv: FACE_UVS[index],
                tex_layer: MOB_ARROW_TEX_LAYER,
                shade,
                flash: 0.0,
            });
        }
    }

    debug_assert_eq!(out.len(), ARROW_VERTS);
    out
}

/// Orientation for a projectile moving along `velocity`.
///
/// The model long axis is `+Z`, so the quaternion first yaws about `Y`
/// (`yaw = atan2(v.x, v.z)`) to face the horizontal direction and then pitches
/// about `X` (`pitch = -asin(v.y / |v|)`) to follow the climb or dive. The
/// inverse sign on the pitch is required because a positive rotation about
/// `X` maps `+Z` toward `-Y`; the test suite pins that a downward velocity
/// ends up pointing down.
///
/// A zero-length or non-finite velocity cannot define a direction, so it falls
/// back to [`Quat::IDENTITY`] (the flat `+Z` pose). Embedded arrows keep their
/// impact velocity by design, so they render angled into the block they hit.
fn arrow_orientation(velocity: Vec3) -> Quat {
    let length_sq = velocity.length_squared();
    if !velocity.is_finite() || length_sq <= 1e-12 {
        return Quat::IDENTITY;
    }
    let length = length_sq.sqrt();
    let yaw = velocity.x.atan2(velocity.z);
    let pitch = -(velocity.y / length).clamp(-1.0, 1.0).asin();
    Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch)
}

/// Places `model` arrow vertices into world space and appends them to `out`.
///
/// Shared by [`build_mob_batch`] and the geometry tests so both exercise the
/// same rotate-then-translate order.
fn place_arrow_vertices(
    out: &mut Vec<MobVertex>,
    model: &[MobVertex],
    rotation: Quat,
    position: Vec3,
) {
    for vertex in model {
        let mut world = *vertex;
        world.position = (rotation * Vec3::from(vertex.position) + position).to_array();
        out.push(world);
    }
}

/// Build the world-space vertex batch for every visible mob **and arrow** in
/// `session`.
///
/// A mob is included only when its feet chunk is resident and its AABB
/// intersects the camera frustum. A dying mob is still included (it remains a
/// record until the session reaper removes it) and is rendered collapsed by
/// its own [`crate::world::mob::Mob::death_progress`]. Light is read once from
/// the feet block, folded into `shade`, and the model vertices are rotated by
/// the mob yaw and translated to the world position.
///
/// Arrows are appended after the mob loop with exactly the same gates: a
/// non-finite position is skipped, a non-resident arrow is skipped (the chunk
/// has not streamed in yet), and an arrow whose
/// [`crate::world::arrow::Arrow::aabb`] misses the camera frustum is skipped.
/// Each survivor contributes [`ARROW_VERTS`] vertices oriented from its
/// velocity via [`arrow_orientation`] and translated to its centre (embedded
/// arrows keep their impact velocity and rest position).
///
/// Everything lands in one output vector, so the renderer still uploads one
/// buffer and issues the existing single `rpass.draw(0..mob_vertices.len())`:
/// no pipeline, uniform, stride or texture-layer change is involved. This is
/// intentionally pure: no wgpu calls and no allocations beyond the returned
/// output vector.
pub fn build_mob_batch(session: &WorldSession, camera: &Camera, aspect: f32) -> Vec<MobVertex> {
    let frustum = Frustum::from_view_proj(&camera.view_proj(aspect));
    let mut out = Vec::new();

    for mob in session.mobs() {
        if !mob.position.is_finite() || !mob.yaw.is_finite() {
            continue;
        }
        if !session.mob_is_resident(mob) {
            continue;
        }
        let (min, max) = mob.aabb();
        if !frustum.intersects_aabb(min, max) {
            continue;
        }

        let light = session.world.light_at_world(
            mob.position.x.floor() as i32,
            mob.position.y.floor() as i32,
            mob.position.z.floor() as i32,
        );
        let anim = MobAnim {
            walk_phase: mob.walk_phase,
            walk_amount: if mob.velocity.length_squared() > 1e-6 {
                1.0
            } else {
                0.0
            },
            light: light as f32 / 15.0,
        };
        let rotation = Quat::from_rotation_y(mob.yaw);
        for vertex in build_mob_vertices(mob.kind, anim, mob.hurt, mob.death_progress()) {
            let mut world = vertex;
            world.position = (rotation * Vec3::from(vertex.position) + mob.position).to_array();
            out.push(world);
        }
    }

    // Arrows: same gates as mobs, one 36-vertex box each. A zero or non-finite
    // velocity cannot orient the box, so `arrow_orientation` falls back to the
    // flat +Z pose rather than dropping a projectile that is still alive.
    for arrow in session.arrows() {
        if !arrow.position.is_finite() {
            continue;
        }
        if !session.arrow_is_resident(arrow) {
            continue;
        }
        let (min, max) = arrow.aabb();
        if !frustum.intersects_aabb(min, max) {
            continue;
        }

        let light = session.world.light_at_world(
            arrow.position.x.floor() as i32,
            arrow.position.y.floor() as i32,
            arrow.position.z.floor() as i32,
        );
        let rotation = arrow_orientation(arrow.velocity);
        place_arrow_vertices(
            &mut out,
            &build_arrow_vertices(light as f32 / 15.0),
            rotation,
            arrow.position,
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::player::survival::GameMode;
    use crate::world::arrow::Arrow;
    use crate::world::block::BlockId;
    use crate::world::chunk::{Chunk, ChunkPos};
    use crate::world::mob::{Mob, MOB_DEATH_SECS};

    fn rest_anim() -> MobAnim {
        MobAnim {
            walk_phase: 0.0,
            walk_amount: 0.0,
            light: 1.0,
        }
    }

    fn verts(kind: MobKind) -> Vec<MobVertex> {
        build_mob_vertices(kind, rest_anim(), 0.0, 0.0)
    }

    fn part(vertices: &[MobVertex], index: usize) -> &[MobVertex] {
        let start = index * MOB_VERTS_PER_PART;
        &vertices[start..start + MOB_VERTS_PER_PART]
    }

    fn aabb(vertices: &[MobVertex]) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for vertex in vertices {
            let position = Vec3::from(vertex.position);
            min = min.min(position);
            max = max.max(position);
        }
        (min, max)
    }

    fn min_y(vertices: &[MobVertex]) -> f32 {
        vertices
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::INFINITY, f32::min)
    }

    fn max_y(vertices: &[MobVertex]) -> f32 {
        vertices
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// A session with chunk (0, 0, 0) resident, the given mobs pushed onto
    /// the list, and the batch those mobs produce from a camera at `eye`
    /// looking toward +Z.
    fn scene_and_batch(mobs: &[Mob], eye: Vec3) -> (WorldSession, Vec<MobVertex>) {
        let mut session = session();
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        for mob in mobs {
            session.mobs_mut().push(mob.clone());
        }
        let camera = camera_looking_at_plus_z(eye);
        let batch = build_mob_batch(&session, &camera, 16.0 / 9.0);
        (session, batch)
    }

    fn session() -> WorldSession {
        WorldSession::new_session_with_mode(
            7,
            PathBuf::from("mob-geometry-test.mcvx"),
            28,
            GameMode::Creative,
        )
    }

    fn camera_looking_at_plus_z(eye: Vec3) -> Camera {
        let mut camera = Camera::new(eye);
        camera.yaw = std::f32::consts::FRAC_PI_2;
        camera.pitch = 0.0;
        camera
    }

    #[test]
    fn mob_vertex_is_32_bytes_with_the_pinned_attribute_offsets() {
        assert_eq!(std::mem::size_of::<MobVertex>(), MOB_VERTEX_SIZE);
        assert_eq!(MOB_VERTEX_SIZE, 32);

        let expected = [
            (wgpu::VertexFormat::Float32x3, 0u64, 0u32),
            (wgpu::VertexFormat::Float32x2, 12, 1),
            (wgpu::VertexFormat::Uint32, 20, 2),
            (wgpu::VertexFormat::Float32, 24, 3),
            (wgpu::VertexFormat::Float32, 28, 4),
        ];
        for (index, (format, offset, location)) in expected.iter().enumerate() {
            let attr = MOB_VERTEX_ATTRIBUTES[index];
            assert_eq!(attr.format, *format, "attribute {index} format");
            assert_eq!(attr.offset, *offset, "attribute {index} offset");
            assert_eq!(
                attr.shader_location, *location,
                "attribute {index} location"
            );
        }
    }

    #[test]
    fn mob_vertex_layout_is_one_32_byte_vertex_per_step() {
        let layout = mob_vertex_layout();
        assert_eq!(layout.array_stride, MOB_VERTEX_SIZE as u64);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Vertex);
        assert_eq!(layout.attributes.len(), 5);
        assert_eq!(layout.attributes, &MOB_VERTEX_ATTRIBUTES);
    }

    #[test]
    fn every_kind_emits_six_36_vertex_part_blocks() {
        for kind in MobKind::ALL {
            let vertices = verts(kind);
            assert_eq!(vertices.len(), MOB_VERTS_PER_MOB, "{kind:?}");
            assert_eq!(MOB_VERTS_PER_MOB, 216);
            assert_eq!(MOB_PART_COUNT, 6);
            assert_eq!(MOB_VERTS_PER_PART, 36);
        }
    }

    #[test]
    fn every_part_is_a_sub_block_cuboid() {
        for kind in MobKind::ALL {
            let vertices = verts(kind);
            for index in 0..MOB_PART_COUNT {
                let (min, max) = aabb(part(&vertices, index));
                let extent = max - min;
                assert!(
                    extent.x < 1.0 && extent.y < 1.0 && extent.z < 1.0,
                    "{kind:?} part {index} extent {extent:?} must stay sub-block"
                );
            }
        }
    }

    #[test]
    fn model_aabb_matches_the_kind_dimensions_exactly() {
        for kind in MobKind::ALL {
            let (min, max) = aabb(&verts(kind));
            assert!(
                (max.x - min.x - kind.width()).abs() < 1e-3,
                "{kind:?} x span {} != {}",
                max.x - min.x,
                kind.width()
            );
            assert!(
                (max.z - min.z - kind.width()).abs() < 1e-3,
                "{kind:?} z span {} != {}",
                max.z - min.z,
                kind.width()
            );
            assert!(
                max.y - min.y <= kind.height() + 1e-3
                    && (max.y - min.y - kind.height()).abs() < 1e-3,
                "{kind:?} y span {} != {}",
                max.y - min.y,
                kind.height()
            );
            assert!(min.y.abs() < 1e-3, "{kind:?} feet must sit on y = 0");
        }
    }

    #[test]
    fn uvs_are_unit_range_and_layers_match_the_kind_table() {
        for kind in MobKind::ALL {
            let layers = mob_texture_layers(kind);
            let vertices = verts(kind);
            for (index, vertex) in vertices.iter().enumerate() {
                let part_index = index / MOB_VERTS_PER_PART;
                let expected_layer = match part_index {
                    0 => layers.body,
                    1 => layers.head,
                    _ => layers.limb,
                };
                assert_eq!(
                    vertex.tex_layer, expected_layer,
                    "{kind:?} part {part_index}"
                );
                assert!(
                    (MOB_LAYER_BASE..=crate::render::texture::MOB_ARROW_TEX_LAYER)
                        .contains(&vertex.tex_layer),
                    "{kind:?} layer {} outside the mob range",
                    vertex.tex_layer
                );
                assert!((0.0..=1.0).contains(&vertex.uv[0]));
                assert!((0.0..=1.0).contains(&vertex.uv[1]));
            }
        }
    }

    #[test]
    fn texture_layer_table_is_pinned_per_kind() {
        for (kind, body, head, limb) in [
            (MobKind::Pig, 32, 33, 34),
            (MobKind::Cow, 35, 36, 37),
            (MobKind::Zombie, 38, 39, 40),
            (MobKind::Skeleton, 41, 42, 43),
        ] {
            let layers = mob_texture_layers(kind);
            assert_eq!((layers.body, layers.head, layers.limb), (body, head, limb));
        }
        assert_eq!(crate::render::texture::MOB_ARROW_TEX_LAYER, 44);
    }

    #[test]
    fn diagonal_quadruped_pairs_swing_in_phase() {
        let anim = MobAnim {
            walk_phase: std::f32::consts::FRAC_PI_2,
            walk_amount: 1.0,
            light: 1.0,
        };
        for kind in [MobKind::Pig, MobKind::Cow] {
            let vertices = build_mob_vertices(kind, anim, 0.0, 0.0);
            let left_front = min_y(part(&vertices, 2));
            let right_front = min_y(part(&vertices, 3));
            let left_back = min_y(part(&vertices, 4));
            let right_back = min_y(part(&vertices, 5));
            assert!((left_front - right_back).abs() < 1e-3, "{kind:?} pair 2/5");
            assert!((right_front - left_back).abs() < 1e-3, "{kind:?} pair 3/4");
            // A +-X swing is vertically symmetric, so the diagonal pose
            // difference shows up in Z, not in the lowest Y.
            let mean_z = |index: usize| {
                let slice = part(&vertices, index);
                slice.iter().map(|v| v.position[2]).sum::<f32>() / slice.len() as f32
            };
            assert!(
                (mean_z(2) - mean_z(3)).abs() > 0.01,
                "{kind:?} diagonal limbs must differ"
            );
        }
    }

    #[test]
    fn every_limb_swings_antisymmetrically_about_its_pivot() {
        let anim = MobAnim {
            walk_phase: std::f32::consts::FRAC_PI_2,
            walk_amount: 1.0,
            light: 1.0,
        };
        for kind in MobKind::ALL {
            let rest = verts(kind);
            let walking = build_mob_vertices(kind, anim, 0.0, 0.0);
            let mean_z = |source: &[MobVertex], index: usize| {
                let slice = part(source, index);
                slice.iter().map(|v| v.position[2]).sum::<f32>() / slice.len() as f32
            };
            let dz2 = mean_z(&walking, 2) - mean_z(&rest, 2);
            let dz3 = mean_z(&walking, 3) - mean_z(&rest, 3);
            let dz4 = mean_z(&walking, 4) - mean_z(&rest, 4);
            let dz5 = mean_z(&walking, 5) - mean_z(&rest, 5);
            assert!(dz2 * dz5 > 0.0, "{kind:?} parts 2/5 must swing together");
            assert!(dz3 * dz4 > 0.0, "{kind:?} parts 3/4 must swing together");
            assert!(dz2 * dz3 < 0.0, "{kind:?} diagonal parts must oppose");
        }
    }

    #[test]
    fn death_one_collapses_the_model_onto_the_ground() {
        for kind in MobKind::ALL {
            let vertices = build_mob_vertices(kind, rest_anim(), 0.0, 1.0);
            let (min, max) = aabb(&vertices);
            assert!(min.y >= -1e-3, "{kind:?} min y {} below ground", min.y);
            assert!(
                max.y - min.y <= 0.5 * kind.height() + 1e-3,
                "{kind:?} death height {} too tall",
                max.y - min.y
            );
        }
    }

    #[test]
    fn hurt_flash_is_clamped_and_copied_to_every_vertex() {
        for kind in MobKind::ALL {
            let vertices = build_mob_vertices(kind, rest_anim(), 0.35, 0.0);
            assert!(vertices.iter().all(|v| v.flash == 0.35));
            let clamped = build_mob_vertices(kind, rest_anim(), 3.0, 0.0);
            assert!(clamped.iter().all(|v| v.flash == 1.0));
            let negative = build_mob_vertices(kind, rest_anim(), -2.0, 0.0);
            assert!(negative.iter().all(|v| v.flash == 0.0));
        }
    }

    #[test]
    fn non_finite_anim_inputs_are_sanitised() {
        let anim = MobAnim {
            walk_phase: f32::NAN,
            walk_amount: f32::INFINITY,
            light: f32::NAN,
        };
        let vertices = build_mob_vertices(MobKind::Pig, anim, f32::NAN, f32::INFINITY);
        for vertex in &vertices {
            assert!(vertex.position.iter().all(|v| v.is_finite()));
            assert!(vertex.shade.is_finite());
            assert_eq!(vertex.flash, 0.0);
        }
    }

    #[test]
    fn batch_drops_a_non_resident_mob() {
        let mut session = session();
        // Chunk (0, 0, 0) is deliberately absent, so the mob is non-resident.
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, Vec3::new(0.5, 0.0, 0.5), 0.0));
        let camera = camera_looking_at_plus_z(Vec3::new(0.5, 1.6, -3.0));
        assert!(build_mob_batch(&session, &camera, 16.0 / 9.0).is_empty());
    }

    #[test]
    fn batch_drops_a_resident_mob_behind_the_camera() {
        let mut session = session();
        // The mob is resident in chunk (0, 0, -1) but sits behind the camera.
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, -1), Chunk::new_uniform(BlockId::Air));
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Cow, Vec3::new(0.5, 0.0, -6.5), 0.0));
        let camera = camera_looking_at_plus_z(Vec3::new(0.5, 1.6, -3.0));
        assert!(build_mob_batch(&session, &camera, 16.0 / 9.0).is_empty());
    }

    #[test]
    fn batch_emits_216_vertices_for_one_visible_resident_mob() {
        let mut session = session();
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        let position = Vec3::new(0.5, 0.0, 0.5);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, position, 0.0));

        let camera = camera_looking_at_plus_z(position + Vec3::new(0.0, 1.6, -3.5));
        let batch = build_mob_batch(&session, &camera, 16.0 / 9.0);
        assert_eq!(batch.len(), MOB_VERTS_PER_MOB);
        // The batch is world-space: the model feet moved to the mob position.
        let (min, max) = aabb(&batch);
        assert!((min.x - (position.x - MobKind::Pig.width() * 0.5)).abs() < 1e-3);
        assert!((max.x - (position.x + MobKind::Pig.width() * 0.5)).abs() < 1e-3);
        assert!((min.y - position.y).abs() < 1e-3);
        assert!((max.y - (position.y + MobKind::Pig.height())).abs() < 1e-3);
    }

    #[test]
    fn batch_renders_a_half_collapsed_cow_shorter_than_its_living_pose() {
        let position = Vec3::new(0.5, 0.0, 0.5);
        let eye = position + Vec3::new(0.0, 1.6, -3.5);
        let living = Mob::new(MobKind::Cow, position, 0.0);
        let (_, living_batch) = scene_and_batch(std::slice::from_ref(&living), eye);
        assert_eq!(living_batch.len(), MOB_VERTS_PER_MOB);
        let living_max_y = max_y(&living_batch);
        assert!((living_max_y - (position.y + MobKind::Cow.height())).abs() < 1e-3);

        let mut dying = Mob::new(MobKind::Cow, position, 0.0);
        dying.health = 0;
        dying.death = MOB_DEATH_SECS * 0.5;
        assert_eq!(dying.death_progress(), 0.5);
        let (_, dying_batch) = scene_and_batch(std::slice::from_ref(&dying), eye);
        assert_eq!(dying_batch.len(), MOB_VERTS_PER_MOB);

        // Halfway through the collapse the head is still the tallest part.
        // The squash curve pins its ceiling at
        // `0.65 * height * cos(0.25)` (~60% of the standing height), so the
        // proof bound is 0.7x — strictly enough to show the pose is squashed,
        // not a copy of the standing silhouette.
        let dying_max_y = max_y(&dying_batch);
        assert!(
            dying_max_y <= 0.7 * living_max_y,
            "half-collapse max y {dying_max_y} must stay below 0.7x the living {living_max_y}"
        );
        assert!(
            living_max_y - dying_max_y > 0.2,
            "the body must visibly drop: living {living_max_y}, collapsing {dying_max_y}"
        );
        // Some vertex must actually have moved: the standing pose is not reused.
        assert!(dying_batch
            .iter()
            .zip(living_batch.iter())
            .any(|(collapsing, standing)| collapsing.position != standing.position));
    }

    #[test]
    fn batch_renders_a_finished_corpse_flat_on_the_ground() {
        let position = Vec3::new(0.5, 0.0, 0.5);
        let eye = position + Vec3::new(0.0, 1.6, -3.5);
        let living = Mob::new(MobKind::Cow, position, 0.0);
        let (_, living_batch) = scene_and_batch(std::slice::from_ref(&living), eye);
        let living_max_y = max_y(&living_batch);

        let mut corpse = Mob::new(MobKind::Cow, position, 0.0);
        corpse.health = 0;
        corpse.death = 0.0;
        assert_eq!(corpse.death_progress(), 1.0);
        let (_, corpse_batch) = scene_and_batch(std::slice::from_ref(&corpse), eye);
        assert_eq!(corpse_batch.len(), MOB_VERTS_PER_MOB);

        let corpse_max_y = max_y(&corpse_batch);
        assert!(
            corpse_max_y < living_max_y - 0.5,
            "a finished corpse must collapse: living {living_max_y}, corpse {corpse_max_y}"
        );
        assert!(
            corpse_max_y >= position.y,
            "the corpse must still have height above the feet: {corpse_max_y}"
        );
        assert!(
            corpse_batch
                .iter()
                .all(|vertex| vertex.position[1] >= position.y - 1e-3),
            "no corpse vertex may sink below the ground plane"
        );
    }

    #[test]
    fn batch_leaves_a_living_mob_at_the_standing_pose() {
        let position = Vec3::new(0.5, 0.0, 0.5);
        let mob = Mob::new(MobKind::Pig, position, 0.0);
        let eye = position + Vec3::new(0.0, 1.6, -3.5);
        let (session, batch) = scene_and_batch(std::slice::from_ref(&mob), eye);
        assert_eq!(batch.len(), MOB_VERTS_PER_MOB);

        let light = session.world.light_at_world(
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        );
        let anim = MobAnim {
            walk_phase: mob.walk_phase,
            walk_amount: 0.0,
            light: light as f32 / 15.0,
        };
        let standing = build_mob_vertices(mob.kind, anim, mob.hurt, 0.0);
        assert_eq!(batch.len(), standing.len());
        for (actual, model) in batch.iter().zip(standing.iter()) {
            let expected =
                Quat::from_rotation_y(mob.yaw) * Vec3::from(model.position) + mob.position;
            assert!(
                (Vec3::from(actual.position) - expected).length() < 1e-5,
                "living vertex {:?} != standing {:?}",
                actual.position,
                expected
            );
            assert_eq!(actual.uv, model.uv);
            assert_eq!(actual.tex_layer, model.tex_layer);
            assert_eq!(actual.shade, model.shade);
            assert_eq!(actual.flash, model.flash);
        }
    }

    #[test]
    fn batch_emits_216_vertices_per_included_mob_including_a_corpse() {
        let living = Mob::new(MobKind::Pig, Vec3::new(0.2, 0.0, 0.5), 0.0);
        let mut corpse = Mob::new(MobKind::Cow, Vec3::new(0.8, 0.0, 0.5), 0.0);
        corpse.health = 0;
        corpse.death = MOB_DEATH_SECS * 0.5;

        let (_, batch) = scene_and_batch(&[living, corpse], Vec3::new(0.5, 1.6, -3.0));
        assert_eq!(batch.len(), 2 * MOB_VERTS_PER_MOB);
        assert_eq!(MOB_VERTS_PER_MOB, 216);
    }

    #[test]
    fn batch_skips_non_finite_mob_positions() {
        let mut session = session();
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, Vec3::new(f32::NAN, 0.0, 0.5), 0.0));
        let camera = camera_looking_at_plus_z(Vec3::new(0.5, 1.6, -3.0));
        assert!(build_mob_batch(&session, &camera, 16.0 / 9.0).is_empty());
    }

    // -----------------------------------------------------------------------
    // Arrow geometry (S07-T04)
    // -----------------------------------------------------------------------

    /// Builds an oriented copy of the model arrow, exactly as
    /// [`build_mob_batch`] places it (rotate about the centre, then translate).
    fn placed_arrow(velocity: Vec3, position: Vec3) -> Vec<MobVertex> {
        let mut placed = Vec::new();
        place_arrow_vertices(
            &mut placed,
            &build_arrow_vertices(1.0),
            arrow_orientation(velocity),
            position,
        );
        placed
    }

    /// Spawns a player arrow through the real session seam, panicking if the
    /// session refuses it (which would make the render assertion meaningless).
    fn spawn_arrow(session: &mut WorldSession, position: Vec3, dir: Vec3, charge: f32) -> Arrow {
        let arrow = Arrow::player(position, dir, charge).expect("valid test arrow");
        session.spawn_arrow(arrow).expect("session accepts arrow");
        arrow
    }

    #[test]
    fn arrow_geometry_is_36_vertices_on_layer_44() {
        let vertices = build_arrow_vertices(1.0);
        assert_eq!(vertices.len(), ARROW_VERTS);
        assert_eq!(ARROW_VERTS, 36);
        assert_eq!(MOB_ARROW_TEX_LAYER, 44);
        for vertex in &vertices {
            assert_eq!(vertex.tex_layer, MOB_ARROW_TEX_LAYER);
            assert_eq!(vertex.flash, 0.0);
            assert!(vertex.position.iter().all(|value| value.is_finite()));
            assert!(vertex.shade.is_finite());
            assert!((0.0..=1.0).contains(&vertex.uv[0]));
            assert!((0.0..=1.0).contains(&vertex.uv[1]));
        }
        // The same six face-shading constants as a mob: top is the brightest
        // and bottom the darkest at full light.
        let darkest = vertices
            .iter()
            .map(|vertex| vertex.shade)
            .fold(f32::INFINITY, f32::min);
        let brightest = vertices
            .iter()
            .map(|vertex| vertex.shade)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((darkest - SHADE_BOTTOM).abs() < 1e-6);
        assert!((brightest - SHADE_TOP).abs() < 1e-6);
    }

    #[test]
    fn arrow_geometry_is_light_sensitive_and_sanitises_non_finite_light() {
        let bright = build_arrow_vertices(1.0);
        let dark = build_arrow_vertices(0.0);
        let nan = build_arrow_vertices(f32::NAN);
        assert!(dark.iter().all(|vertex| vertex.shade.is_finite()));
        assert!(nan.iter().all(|vertex| vertex.shade.is_finite()));
        assert_eq!(dark, nan);
        assert!(bright
            .iter()
            .zip(dark.iter())
            .all(|(lit, unlit)| lit.shade > unlit.shade));
    }

    #[test]
    fn arrow_long_axis_is_positive_z() {
        let (min, max) = aabb(&build_arrow_vertices(1.0));
        let extent = max - min;
        assert!(
            extent.z > extent.x * 5.0 && extent.z > extent.y * 5.0,
            "the long axis must be Z: {extent:?}"
        );
        assert!((extent.z - 0.6).abs() < 1e-5);
        assert!((extent.x - 0.06).abs() < 1e-5);
        assert!((extent.y - 0.06).abs() < 1e-5);
        // Centred on the origin so rotation about the mid-point is correct,
        // and the tip points at +Z.
        assert!(min.z < 0.0 && max.z > 0.0);
        assert!((min.z + 0.3).abs() < 1e-5);
        assert!((max.z - 0.3).abs() < 1e-5);
    }

    #[test]
    fn horizontal_arrow_lies_flat() {
        let placed = placed_arrow(Vec3::new(0.0, 0.0, 18.0), Vec3::ZERO);
        let (min, max) = aabb(&placed);
        let extent = max - min;
        assert!((extent.z - 0.6).abs() < 1e-4, "forward span {extent:?}");
        assert!(extent.y < 0.1, "a flat arrow is thin in Y: {extent:?}");
        assert!(extent.x < 0.1, "a flat arrow is thin in X: {extent:?}");
    }

    #[test]
    fn downward_arrow_points_down() {
        let rotation = arrow_orientation(Vec3::new(0.0, -28.0, 0.0));
        let mut placed = Vec::new();
        place_arrow_vertices(
            &mut placed,
            &build_arrow_vertices(1.0),
            rotation,
            Vec3::ZERO,
        );
        let (min, max) = aabb(&placed);
        let extent = max - min;
        // The long axis is now vertical within 1e-4.
        assert!((extent.y - 0.6).abs() < 1e-4, "vertical span {extent:?}");
        assert!(extent.x < 0.1 && extent.z < 0.1);
        // The tip (model +Z) maps to the lowest point, so the highest vertex
        // must be the tail: un-rotating it lands at model z = -0.3.
        assert!((max.y - 0.3).abs() < 1e-4);
        let tail = placed
            .iter()
            .max_by(|a, b| a.position[1].total_cmp(&b.position[1]))
            .expect("arrow has vertices");
        let model = rotation.inverse() * Vec3::from(tail.position);
        assert!(
            (-0.31..-0.29).contains(&model.z),
            "the highest vertex must be the tail, model z {}",
            model.z
        );
    }

    #[test]
    fn nan_or_zero_velocity_falls_back_to_a_positive_z_box() {
        for velocity in [
            Vec3::ZERO,
            Vec3::new(f32::NAN, 1.0, 0.0),
            Vec3::new(f32::INFINITY, 0.0, 0.0),
        ] {
            let rotation = arrow_orientation(velocity);
            assert_eq!(rotation, Quat::IDENTITY, "velocity {velocity:?}");
            let placed = placed_arrow(velocity, Vec3::new(0.5, 1.0, 0.5));
            assert!(placed
                .iter()
                .all(|vertex| vertex.position.iter().all(|value| value.is_finite())));
            let (min, max) = aabb(&placed);
            assert!((min.z - 0.2).abs() < 1e-4 && (max.z - 0.8).abs() < 1e-4);
        }
    }

    #[test]
    fn batch_drops_a_non_resident_arrow() {
        let mut session = session();
        // Chunk (0, 0, 0) is deliberately absent, so the arrow is non-resident.
        spawn_arrow(
            &mut session,
            Vec3::new(0.5, 1.0, 0.5),
            Vec3::new(0.0, 0.0, 1.0),
            1.0,
        );
        let camera = camera_looking_at_plus_z(Vec3::new(0.5, 1.6, -3.0));
        assert!(build_mob_batch(&session, &camera, 16.0 / 9.0).is_empty());
    }

    #[test]
    fn batch_drops_a_resident_arrow_behind_the_camera() {
        let mut session = session();
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, -1), Chunk::new_uniform(BlockId::Air));
        spawn_arrow(
            &mut session,
            Vec3::new(0.5, 1.0, -6.5),
            Vec3::new(0.0, 0.0, 1.0),
            1.0,
        );
        let camera = camera_looking_at_plus_z(Vec3::new(0.5, 1.6, -3.0));
        assert!(build_mob_batch(&session, &camera, 16.0 / 9.0).is_empty());
    }

    #[test]
    fn an_empty_arrow_list_leaves_the_mob_batch_unchanged() {
        let mob = Mob::new(MobKind::Pig, Vec3::new(0.5, 0.0, 0.5), 0.0);
        let (session, batch) =
            scene_and_batch(std::slice::from_ref(&mob), Vec3::new(0.5, 1.6, -3.0));
        assert!(session.arrows().is_empty());
        assert_eq!(batch.len(), MOB_VERTS_PER_MOB);

        let camera = camera_looking_at_plus_z(Vec3::new(0.5, 1.6, -3.0));
        let rebuilt = build_mob_batch(&session, &camera, 16.0 / 9.0);
        assert_eq!(batch, rebuilt, "no arrows means no extra vertices");
    }

    #[test]
    fn one_visible_arrow_appends_exactly_36_vertices() {
        let eye = Vec3::new(0.5, 1.6, -3.0);
        let mob = Mob::new(MobKind::Pig, Vec3::new(0.5, 0.0, 0.5), 0.0);
        let (mut session, mob_batch) = scene_and_batch(std::slice::from_ref(&mob), eye);
        assert_eq!(mob_batch.len(), MOB_VERTS_PER_MOB);

        let arrow = spawn_arrow(
            &mut session,
            Vec3::new(0.5, 1.0, 0.5),
            Vec3::new(0.0, 0.0, 1.0),
            1.0,
        );
        let camera = camera_looking_at_plus_z(eye);
        let batch = build_mob_batch(&session, &camera, 16.0 / 9.0);
        assert_eq!(batch.len(), MOB_VERTS_PER_MOB + ARROW_VERTS);
        // Mobs come first and are byte-identical to the arrow-free batch.
        assert_eq!(&batch[..MOB_VERTS_PER_MOB], mob_batch.as_slice());

        let appended = &batch[MOB_VERTS_PER_MOB..];
        assert_eq!(appended.len(), ARROW_VERTS);
        assert!(appended
            .iter()
            .all(|vertex| vertex.tex_layer == MOB_ARROW_TEX_LAYER));

        let light = session.world.light_at_world(
            arrow.position.x.floor() as i32,
            arrow.position.y.floor() as i32,
            arrow.position.z.floor() as i32,
        );
        let mut expected = Vec::new();
        place_arrow_vertices(
            &mut expected,
            &build_arrow_vertices(light as f32 / 15.0),
            Quat::IDENTITY,
            arrow.position,
        );
        assert_eq!(appended, expected.as_slice());

        let (min, max) = aabb(appended);
        assert!((min.x - 0.47).abs() < 1e-4 && (max.x - 0.53).abs() < 1e-4);
        assert!((min.z - 0.2).abs() < 1e-4 && (max.z - 0.8).abs() < 1e-4);
    }

    #[test]
    fn an_embedded_arrow_renders_at_its_rest_position_with_impact_heading() {
        let eye = Vec3::new(0.5, 1.6, -3.0);
        let mut session = session();
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        let mut arrow = Arrow::player(Vec3::new(0.5, 1.0, 0.5), Vec3::new(0.0, -1.0, 0.0), 0.5)
            .expect("valid test arrow");
        let velocity = arrow.velocity;
        arrow.embed();
        session.spawn_arrow(arrow).expect("embedded arrow accepted");

        let camera = camera_looking_at_plus_z(eye);
        let batch = build_mob_batch(&session, &camera, 16.0 / 9.0);
        assert_eq!(batch.len(), ARROW_VERTS);
        let (min, max) = aabb(&batch);
        // Still centred on the impact point, but oriented along the downward
        // impact velocity, so its long axis is vertical.
        assert!((min.x - 0.47).abs() < 1e-4 && (max.x - 0.53).abs() < 1e-4);
        assert!((min.y - 0.7).abs() < 1e-3 && (max.y - 1.3).abs() < 1e-3);
        assert!(max.z - min.z < 0.1, "downward heading is thin in Z");
        assert!(velocity.y < 0.0);
    }
}
