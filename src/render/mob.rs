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
//! coordinates. T04 adds the wgpu pipeline and draw; this module is pure CPU
//! data so the model contract stays headlessly testable.

use glam::{Quat, Vec3};

use crate::player::camera::{Camera, Frustum};
use crate::render::texture::MOB_LAYER_BASE;
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

/// Build the world-space vertex batch for every visible mob in `session`.
///
/// A mob is included only when its feet chunk is resident and its AABB
/// intersects the camera frustum. Light is read once from the feet block,
/// folded into `shade`, and the model vertices are rotated by the mob yaw and
/// translated to the world position. This is intentionally pure: no wgpu
/// calls and no allocations beyond the returned output vector.
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
        for vertex in build_mob_vertices(mob.kind, anim, mob.hurt, 0.0) {
            let mut world = vertex;
            world.position = (rotation * Vec3::from(vertex.position) + mob.position).to_array();
            out.push(world);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::player::survival::GameMode;
    use crate::world::block::BlockId;
    use crate::world::chunk::{Chunk, ChunkPos};
    use crate::world::mob::Mob;

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
}
