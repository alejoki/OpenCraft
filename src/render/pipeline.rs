use bytemuck::{Pod, Zeroable};
use glam::{IVec3, Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::mesh::quad::PackedQuad;
use crate::player::camera::{visible_face_mask, Camera, Frustum};
use crate::player::inventory::HOTBAR_SLOTS;
use crate::render::buffer::{ChunkRenderStore, QUAD_VERTICES};
use crate::render::context::{RenderContext, DEPTH_FORMAT};
use crate::render::sky;
use crate::render::texture::{
    block_layers_match_shape, create_block_texture_array, upload_block_layers,
};
use crate::world::chunk::CHUNK_SIZE;
use crate::world::daycycle;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    pub fog_color: [f32; 4],
    pub fog_start_end: [f32; 4],
}

/// Per-frame sky inputs for the world pass, derived from
/// [`crate::world::daycycle::sky_palette`].
///
/// Grouped into one type so `render_frame`'s long signature grows by a single
/// parameter rather than four. The day factor rides in the otherwise unused
/// `CameraUniform::fog_start_end.z` slot, so the 112-byte uniform layout and the
/// WGSL struct stay untouched; the celestial inputs (sun/moon direction,
/// brightness fades, star visibility and rotation) travel in the separate
/// 48-byte [`sky::SkyUniform`] derived from `time_of_day`.
#[derive(Copy, Clone, Debug)]
pub struct SkyParams {
    /// Multiplier applied to terrain light: 1.0 at noon down to the moonlit
    /// floor at midnight.
    pub day_factor: f32,
    /// Linear RGBA fog color the terrain fades into.
    pub fog_color: [f32; 4],
    /// Background clear color for the sky pass (and thus the horizon).
    pub clear_color: wgpu::Color,
    /// Game time in day-cycle ticks (`0..DAY_TICKS`), the input the pure
    /// [`daycycle`] helpers turn into sun/moon/star placement.
    pub time_of_day: u32,
    /// True when the camera's eye is inside a liquid block. The world pass
    /// then swaps the palette fog for the fixed deep-water shift below. This
    /// is a plain flag so `SkyParams` stays `Copy` and `render_frame`'s
    /// signature does not grow.
    pub underwater: bool,
}

/// Deep-water fog color while the camera's eye is submerged. Uses the same
/// sRGB-value convention as [`daycycle::SkyPalette::fog_color`] (plain 0..1
/// channel values written straight into the camera uniform), just darker and
/// bluer than any palette keyframe.
pub const UNDERWATER_FOG_COLOR: [f32; 4] = [0.05, 0.18, 0.32, 1.0];
/// Fog start distance (blocks) while submerged: visibility collapses fast.
pub const UNDERWATER_FOG_START: f32 = 8.0;
/// Fog end distance (blocks) while submerged.
pub const UNDERWATER_FOG_END: f32 = 48.0;

/// Effective world-pass fog as `(color, start, end)`.
///
/// Submersion swaps the palette fog color and the render-distance-derived
/// range for the fixed deep-water shift; above water the caller's values pass
/// through untouched, so the dry path stays byte-identical. Pure so headless
/// tests can pin both branches without a `wgpu::Device`.
pub fn effective_fog(
    underwater: bool,
    fog_color: [f32; 4],
    fog_start: f32,
    fog_end: f32,
) -> ([f32; 4], f32, f32) {
    if underwater {
        (
            UNDERWATER_FOG_COLOR,
            UNDERWATER_FOG_START,
            UNDERWATER_FOG_END,
        )
    } else {
        (fog_color, fog_start, fog_end)
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct OverlayVertex {
    pub clip_pos: [f32; 4],
    pub color: [f32; 4],
}

/// Vertex layout shared by the opaque voxel and translucent water pipelines:
/// one [`PackedQuad`] (two `u32` words) per instance. Keeping it in one place
/// makes it impossible for the water pass to drift from the opaque decode.
const PACKED_QUAD_ATTRIBUTES: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
    offset: 0,
    shader_location: 0,
    format: wgpu::VertexFormat::Uint32x2,
}];

fn packed_quad_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<PackedQuad>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &PACKED_QUAD_ATTRIBUTES,
    }
}

const VOXEL_SHADER_WGSL: &str = r#"
struct CameraUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    fog_color: vec4<f32>,
    fog_start_end: vec4<f32>,
};

struct ChunkUniform {
    origin: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(0) @binding(1) var block_tex: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

@group(1) @binding(0) var<uniform> chunk: ChunkUniform;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) tex_layer: u32,
    @location(2) light: f32,
    @location(3) cam_dist: f32,
};

fn face_normal(face_dir: u32) -> vec3<f32> {
    switch face_dir {
        case 0u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 1u: { return vec3<f32>(-1.0, 0.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 3u: { return vec3<f32>(0.0, -1.0, 0.0); }
        case 4u: { return vec3<f32>(0.0, 0.0, 1.0); }
        default: { return vec3<f32>(0.0, 0.0, -1.0); }
    }
}

fn face_sun_factor(face_dir: u32) -> f32 {
    switch face_dir {
        case 2u: { return 1.0; }
        case 0u, 1u: { return 0.82; }
        case 4u, 5u: { return 0.70; }
        default: { return 0.50; }
    }
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @location(0) quad_words: vec2<u32>,
) -> VertexOutput {
    let w0 = quad_words.x;
    let w1 = quad_words.y;

    let qx = f32(w0 & 31u);
    let qy = f32((w0 >> 5u) & 31u);
    let qz = f32((w0 >> 10u) & 31u);
    let qw = f32(((w0 >> 15u) & 31u) + 1u);
    let qh = f32(((w0 >> 20u) & 31u) + 1u);
    let face_dir = (w0 >> 25u) & 7u;

    let tex_layer = w1 & 255u;
    let ao_packed = (w1 >> 8u) & 255u;
    let light_packed = (w1 >> 16u) & 0xFFFFu;

    let ao00 = ao_packed & 3u;
    let ao10 = (ao_packed >> 2u) & 3u;
    let ao01 = (ao_packed >> 4u) & 3u;
    let ao11 = (ao_packed >> 6u) & 3u;

    let l00 = light_packed & 15u;
    let l10 = (light_packed >> 4u) & 15u;
    let l01 = (light_packed >> 8u) & 15u;
    let l11 = (light_packed >> 12u) & 15u;

    let flip_diag = (ao00 + ao11) > (ao10 + ao01);
    var corner: u32 = 0u;
    if !flip_diag {
        switch vid {
            case 0u: { corner = 0u; }
            case 1u: { corner = 1u; }
            case 2u: { corner = 2u; }
            case 3u: { corner = 2u; }
            case 4u: { corner = 1u; }
            default: { corner = 3u; }
        }
    } else {
        switch vid {
            case 0u: { corner = 0u; }
            case 1u: { corner = 1u; }
            case 2u: { corner = 3u; }
            case 3u: { corner = 0u; }
            case 4u: { corner = 3u; }
            default: { corner = 2u; }
        }
    }

    let u_bit = f32(corner & 1u);
    let v_bit = f32((corner >> 1u) & 1u);
    let u_off = u_bit * qw;
    let v_off = v_bit * qh;
    let d_off = select(0.0, 1.0, (face_dir & 1u) == 0u);

    var local_pos: vec3<f32>;
    var uv: vec2<f32>;
    let axis = face_dir >> 1u;
    if axis == 0u {
        local_pos = vec3<f32>(qx + d_off, qy + v_off, qz + u_off);
        uv = vec2<f32>(u_off, qh - v_off);
    } else if axis == 1u {
        local_pos = vec3<f32>(qx + u_off, qy + d_off, qz + v_off);
        uv = vec2<f32>(u_off, v_off);
    } else {
        local_pos = vec3<f32>(qx + u_off, qy + v_off, qz + d_off);
        uv = vec2<f32>(u_off, qh - v_off);
    }

    let world_pos = chunk.origin.xyz + local_pos;
    let normal = face_normal(face_dir);

    var out: VertexOutput;
    // Exact planar backface check: cull if camera is on or behind the quad plane
    if dot(world_pos - camera.camera_pos.xyz, normal) >= 0.0 {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 0.0);
        out.uv = vec2<f32>(0.0, 0.0);
        out.tex_layer = 0u;
        out.light = 0.0;
        out.cam_dist = 0.0;
        return out;
    }

    var ao_val: u32 = ao00;
    var light_val: u32 = l00;
    if corner == 1u {
        ao_val = ao10;
        light_val = l10;
    } else if corner == 2u {
        ao_val = ao01;
        light_val = l01;
    } else if corner == 3u {
        ao_val = ao11;
        light_val = l11;
    }
    let ao_factor = 0.45 + (f32(ao_val) / 3.0) * 0.55;
    let light_factor = f32(light_val) / 15.0;

    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = uv;
    out.tex_layer = tex_layer;
    // Day factor rides in the unused fog_start_end.z slot: keeps CameraUniform
    // at seven vec4s while still dimming lit terrain at night. Cave vertices
    // stay dark regardless because their light_factor is already ~0.
    let day_factor = camera.fog_start_end.z;
    out.light = face_sun_factor(face_dir) * ao_factor * light_factor * day_factor;
    out.cam_dist = distance(world_pos, camera.camera_pos.xyz);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(block_tex, block_sampler, in.uv, i32(in.tex_layer));
    let lit_rgb = tex_color.rgb * in.light;
    let fog_start = camera.fog_start_end.x;
    let fog_end = camera.fog_start_end.y;
    let fog_t = clamp((in.cam_dist - fog_start) / max(fog_end - fog_start, 1.0), 0.0, 1.0);
    let final_rgb = mix(lit_rgb, camera.fog_color.rgb, fog_t * fog_t);
    return vec4<f32>(final_rgb, 1.0);
}
"#;

/// Translucent water shader.
///
/// The vertex stage is a copy of the voxel `vs_main` PackedQuad decode (same
/// bitfields, same analytic planar backface cull, same day factor carried in
/// `fog_start_end.z`) so water geometry lands exactly where the opaque mesher
/// put it. The fragment stage is deliberately texture-free: it outputs a
/// procedural deep-blue RGBA and never samples the block texture array
/// (MEM230 shader-colored water; MEM232 all 16 layers are consumed, no 17th
/// layer). The water quads carry placeholder tex layer 0 (Grass Top) purely
/// because the packed format requires a byte — it must never be sampled.
///
/// Only `group(0) binding(0)` (camera) and `group(1) binding(0)` (chunk
/// origin) are declared. The pipeline layout still carries the camera bind
/// group's texture/sampler entries, which is legal for undeclared bindings
/// (same precedent as `sky::create_sky_pipeline`).
const WATER_SHADER_WGSL: &str = r#"
struct CameraUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    fog_color: vec4<f32>,
    fog_start_end: vec4<f32>,
};

struct ChunkUniform {
    origin: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> chunk: ChunkUniform;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) light: f32,
    @location(1) cam_dist: f32,
};

fn face_normal(face_dir: u32) -> vec3<f32> {
    switch face_dir {
        case 0u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 1u: { return vec3<f32>(-1.0, 0.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 3u: { return vec3<f32>(0.0, -1.0, 0.0); }
        case 4u: { return vec3<f32>(0.0, 0.0, 1.0); }
        default: { return vec3<f32>(0.0, 0.0, -1.0); }
    }
}

fn face_sun_factor(face_dir: u32) -> f32 {
    switch face_dir {
        case 2u: { return 1.0; }
        case 0u, 1u: { return 0.82; }
        case 4u, 5u: { return 0.70; }
        default: { return 0.50; }
    }
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @location(0) quad_words: vec2<u32>,
) -> VertexOutput {
    let w0 = quad_words.x;
    let w1 = quad_words.y;

    let qx = f32(w0 & 31u);
    let qy = f32((w0 >> 5u) & 31u);
    let qz = f32((w0 >> 10u) & 31u);
    let qw = f32(((w0 >> 15u) & 31u) + 1u);
    let qh = f32(((w0 >> 20u) & 31u) + 1u);
    let face_dir = (w0 >> 25u) & 7u;

    // `w1`'s low byte is the placeholder tex layer; water never reads it.
    let ao_packed = (w1 >> 8u) & 255u;
    let light_packed = (w1 >> 16u) & 0xFFFFu;

    let ao00 = ao_packed & 3u;
    let ao10 = (ao_packed >> 2u) & 3u;
    let ao01 = (ao_packed >> 4u) & 3u;
    let ao11 = (ao_packed >> 6u) & 3u;

    let l00 = light_packed & 15u;
    let l10 = (light_packed >> 4u) & 15u;
    let l01 = (light_packed >> 8u) & 15u;
    let l11 = (light_packed >> 12u) & 15u;

    let flip_diag = (ao00 + ao11) > (ao10 + ao01);
    var corner: u32 = 0u;
    if !flip_diag {
        switch vid {
            case 0u: { corner = 0u; }
            case 1u: { corner = 1u; }
            case 2u: { corner = 2u; }
            case 3u: { corner = 2u; }
            case 4u: { corner = 1u; }
            default: { corner = 3u; }
        }
    } else {
        switch vid {
            case 0u: { corner = 0u; }
            case 1u: { corner = 1u; }
            case 2u: { corner = 3u; }
            case 3u: { corner = 0u; }
            case 4u: { corner = 3u; }
            default: { corner = 2u; }
        }
    }

    let u_bit = f32(corner & 1u);
    let v_bit = f32((corner >> 1u) & 1u);
    let u_off = u_bit * qw;
    let v_off = v_bit * qh;
    let d_off = select(0.0, 1.0, (face_dir & 1u) == 0u);

    var local_pos: vec3<f32>;
    let axis = face_dir >> 1u;
    if axis == 0u {
        local_pos = vec3<f32>(qx + d_off, qy + v_off, qz + u_off);
    } else if axis == 1u {
        local_pos = vec3<f32>(qx + u_off, qy + d_off, qz + v_off);
    } else {
        local_pos = vec3<f32>(qx + u_off, qy + v_off, qz + d_off);
    }

    let world_pos = chunk.origin.xyz + local_pos;
    let normal = face_normal(face_dir);

    var out: VertexOutput;
    // Same exact planar backface check as the opaque shader: hardware culling
    // stays off and a quad is rejected when the camera is on/behind its plane.
    if dot(world_pos - camera.camera_pos.xyz, normal) >= 0.0 {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 0.0);
        out.light = 0.0;
        out.cam_dist = 0.0;
        return out;
    }

    var ao_val: u32 = ao00;
    var light_val: u32 = l00;
    if corner == 1u {
        ao_val = ao10;
        light_val = l10;
    } else if corner == 2u {
        ao_val = ao01;
        light_val = l01;
    } else if corner == 3u {
        ao_val = ao11;
        light_val = l11;
    }
    let ao_factor = 0.45 + (f32(ao_val) / 3.0) * 0.55;
    let light_factor = f32(light_val) / 15.0;

    // Day factor rides in the unused fog_start_end.z slot (see the opaque
    // shader); water dims at night with the rest of the world.
    let day_factor = camera.fog_start_end.z;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.light = face_sun_factor(face_dir) * ao_factor * light_factor * day_factor;
    out.cam_dist = distance(world_pos, camera.camera_pos.xyz);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Procedural deep-blue water, no texture sample (MEM230/MEM232). The
    // `light` term already includes the day factor from the vertex stage.
    let base = vec4<f32>(0.15, 0.35, 0.70, 0.65);
    let water_rgb = base.rgb * in.light;
    let fog_start = camera.fog_start_end.x;
    let fog_end = camera.fog_start_end.y;
    let fog_t = clamp((in.cam_dist - fog_start) / max(fog_end - fog_start, 1.0), 0.0, 1.0);
    let fog = fog_t * fog_t;
    let final_rgb = mix(water_rgb, camera.fog_color.rgb, fog);
    // Fade the translucent surface out with distance too, so far water cannot
    // sit as a fixed 65%-opaque sheet over the fogged horizon.
    let final_alpha = base.a * (1.0 - fog);
    return vec4<f32>(final_rgb, final_alpha);
}
"#;

const OVERLAY_SHADER_WGSL: &str = r#"
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

/// Layout of hotbar slot `index` in normalised device coordinates.
///
/// Returns `(x0, y0, x1, y1)` with y pointing up in NDC. The bar is centered
/// on x = 0 at the bottom of the screen. `index >= HOTBAR_SLOTS` returns a
/// zero-sized rect at the bar's left edge instead of panicking, so defensive
/// callers can use it without a bounds check.
pub fn hotbar_slot_rect_ndc(index: usize, aspect: f32) -> (f32, f32, f32, f32) {
    let aspect = aspect.max(0.5);
    let slot_w = 0.09 / aspect;
    let slot_h = 0.085;
    let gap = 0.012 / aspect;
    let total_w = HOTBAR_SLOTS as f32 * slot_w + (HOTBAR_SLOTS - 1) as f32 * gap;
    let start_x = -total_w * 0.5;
    let y0 = -0.92;
    let y1 = y0 + slot_h;

    if index >= HOTBAR_SLOTS {
        return (start_x, y0, start_x, y0);
    }

    let x0 = start_x + index as f32 * (slot_w + gap);
    (x0, y0, x0 + slot_w, y1)
}

pub fn build_overlay_line_vertices(
    view_proj: &Mat4,
    aspect: f32,
    target_block: Option<IVec3>,
    selected_slot: usize,
    show_overlays: bool,
) -> Vec<OverlayVertex> {
    // Single gate for every gameplay overlay (3D targeted-block wireframe,
    // center crosshair, hotbar outlines). Menus draw through UiPass instead.
    if !show_overlays {
        return Vec::new();
    }

    let mut verts = Vec::with_capacity(160);

    // 1. 3D Wireframe around targeted block (12 edges = 24 vertices)
    if let Some(bpos) = target_block {
        let pad = 0.003;
        let min = Vec3::new(
            bpos.x as f32 - pad,
            bpos.y as f32 - pad,
            bpos.z as f32 - pad,
        );
        let max = Vec3::new(
            (bpos.x + 1) as f32 + pad,
            (bpos.y + 1) as f32 + pad,
            (bpos.z + 1) as f32 + pad,
        );
        let corners = [
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            Vec3::new(max.x, max.y, max.z),
            Vec3::new(min.x, max.y, max.z),
        ];
        let edges = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        let color = [0.05, 0.05, 0.05, 1.0];
        for (a, b) in edges {
            let ca = *view_proj * corners[a].extend(1.0);
            let cb = *view_proj * corners[b].extend(1.0);
            verts.push(OverlayVertex {
                clip_pos: ca.to_array(),
                color,
            });
            verts.push(OverlayVertex {
                clip_pos: cb.to_array(),
                color,
            });
        }
    }

    // 2. Center 2D Crosshair
    let ch_x = 0.018 / aspect.max(0.5);
    let ch_y = 0.018;
    let white = [0.95, 0.95, 0.95, 1.0];
    verts.push(OverlayVertex {
        clip_pos: [-ch_x, 0.0, 0.0, 1.0],
        color: white,
    });
    verts.push(OverlayVertex {
        clip_pos: [ch_x, 0.0, 0.0, 1.0],
        color: white,
    });
    verts.push(OverlayVertex {
        clip_pos: [0.0, -ch_y, 0.0, 1.0],
        color: white,
    });
    verts.push(OverlayVertex {
        clip_pos: [0.0, ch_y, 0.0, 1.0],
        color: white,
    });

    // 3. 9-Slot Block Hotbar at bottom of screen. Geometry comes from the
    // same helper the HUD count labels use, so outlines and labels cannot
    // drift with aspect ratio or resolution.
    for i in 0..HOTBAR_SLOTS {
        let (x0, y0, x1, y1) = hotbar_slot_rect_ndc(i, aspect);
        let is_sel = i == selected_slot;
        let color = if is_sel {
            [1.0, 0.92, 0.25, 1.0]
        } else {
            [0.65, 0.65, 0.65, 1.0]
        };
        for (p0, p1) in [
            ([x0, y0], [x1, y0]),
            ([x1, y0], [x1, y1]),
            ([x1, y1], [x0, y1]),
            ([x0, y1], [x0, y0]),
        ] {
            verts.push(OverlayVertex {
                clip_pos: [p0[0], p0[1], 0.0, 1.0],
                color,
            });
            verts.push(OverlayVertex {
                clip_pos: [p1[0], p1[1], 0.0, 1.0],
                color,
            });
        }
    }

    verts
}

/// Hook for drawing UI content in the color-only pass that `render_frame`
/// opens after the world pass.
///
/// The `&'pass self` receiver matches glyphon's `render<'pass>(&'pass self,
/// atlas: &'pass TextAtlas, pass: &mut RenderPass<'pass>)` tie: with wgpu
/// 0.19's lifetime-carrying `RenderPass`, a higher-ranked closure hook would
/// force the captured atlas/renderer borrows to be `'static`.
pub trait UiPass {
    fn draw_ui<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>);
}

pub struct VoxelRenderer {
    /// The 16-layer block texture array retained so live pack changes can
    /// re-upload layer data into the existing texture (MEM111). The view and
    /// bind group above stay valid because only the contents change.
    pub block_texture: wgpu::Texture,
    pub voxel_pipeline: wgpu::RenderPipeline,
    /// Alpha-blended, depth-write-disabled pipeline for `ChunkMesh::water_quads`.
    /// Drawn inside the main pass strictly after the opaque chunk loop.
    pub water_pipeline: wgpu::RenderPipeline,
    pub overlay_pipeline: wgpu::RenderPipeline,
    /// Color-only sky pass pipeline (sun/moon/star billboards).
    pub sky_pipeline: wgpu::RenderPipeline,
    pub camera_buffer: wgpu::Buffer,
    pub camera_bind_group: wgpu::BindGroup,
    pub chunk_bind_group_layout: wgpu::BindGroupLayout,
    /// Per-frame 48-byte sky state (directions, fades, star angle).
    pub sky_uniform_buffer: wgpu::Buffer,
    pub sky_bind_group: wgpu::BindGroup,
    /// Static sun/moon/star billboard vertices, uploaded once at construction.
    pub sky_vertex_buffer: wgpu::Buffer,
    pub sky_vertex_count: u32,
}

impl VoxelRenderer {
    pub fn new(ctx: &RenderContext) -> Self {
        let (block_texture, tex_view, sampler) =
            create_block_texture_array(&ctx.device, &ctx.queue);

        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0; 4],
            fog_color: [0.53, 0.76, 0.96, 1.0],
            // z is the day factor: full brightness before the first frame.
            fog_start_end: [300.0, 512.0, 1.0, 0.0],
        };

        let camera_buffer = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("camera_uniform_buffer"),
                contents: bytemuck::bytes_of(&camera_uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let camera_bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("camera_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let camera_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera_bg"),
            layout: &camera_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let chunk_bind_group_layout =
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("chunk_bgl"),
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

        // Sky pass resources: the pipeline reused the camera bind group for
        // group 0 and a small dedicated 48-byte uniform for group 1. The
        // billboard geometry (sun, moon, fixed-seed star field) is static.
        let sky_bind_group_layout = sky::create_sky_bind_group_layout(&ctx.device);
        let sky_uniform_buffer = sky::create_sky_uniform_buffer(&ctx.device);
        let sky_bind_group =
            sky::create_sky_bind_group(&ctx.device, &sky_bind_group_layout, &sky_uniform_buffer);
        let sky_pipeline = sky::create_sky_pipeline(ctx, &camera_bgl, &sky_bind_group_layout);
        let (sky_vertex_buffer, sky_vertex_count) =
            sky::create_sky_vertex_buffer(&ctx.device, &daycycle::generate_star_field());

        let voxel_shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("voxel_shader"),
                source: wgpu::ShaderSource::Wgsl(VOXEL_SHADER_WGSL.into()),
            });

        let voxel_layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("voxel_pipeline_layout"),
                bind_group_layouts: &[&camera_bgl, &chunk_bind_group_layout],
                push_constant_ranges: &[],
            });

        let voxel_pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("voxel_pipeline"),
                layout: Some(&voxel_layout),
                vertex: wgpu::VertexState {
                    module: &voxel_shader,
                    entry_point: "vs_main",
                    buffers: &[packed_quad_vertex_layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &voxel_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None, // Planar backface culling is performed analytically in vs_main
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
            });

        // Water pass: same PackedQuad vertex layout as the opaque pipeline but
        // alpha-blended with depth writes disabled, so translucent water is
        // occluded by opaque geometry (shared depth buffer, LessEqual) without
        // ever occluding what is drawn after it. The pipeline layout is the
        // voxel layout: the shader declares only group 0 binding 0 and group 1
        // binding 0, while the layout may carry the camera bind group's
        // texture/sampler entries it never uses (sky.rs precedent).
        let water_shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("water_shader"),
                source: wgpu::ShaderSource::Wgsl(WATER_SHADER_WGSL.into()),
            });

        let water_pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("water_pipeline"),
                layout: Some(&voxel_layout),
                vertex: wgpu::VertexState {
                    module: &water_shader,
                    entry_point: "vs_main",
                    buffers: &[packed_quad_vertex_layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &water_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None, // analytic planar backface cull in vs_main
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        let overlay_shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("overlay_shader"),
                source: wgpu::ShaderSource::Wgsl(OVERLAY_SHADER_WGSL.into()),
            });

        let overlay_layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay_pipeline_layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        let overlay_pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay_pipeline"),
                layout: Some(&overlay_layout),
                vertex: wgpu::VertexState {
                    module: &overlay_shader,
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
                    module: &overlay_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::LineList,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        Self {
            block_texture,
            voxel_pipeline,
            water_pipeline,
            overlay_pipeline,
            sky_pipeline,
            camera_buffer,
            camera_bind_group,
            chunk_bind_group_layout,
            sky_uniform_buffer,
            sky_bind_group,
            sky_vertex_buffer,
            sky_vertex_count,
        }
    }

    /// Re-upload all 16 block texture layers (base mip plus mip chain) into
    /// the existing texture array. Never recreates the texture, its view, or
    /// the camera bind group (MEM111).
    pub fn write_block_layers(&self, queue: &wgpu::Queue, layers: &[Vec<u8>]) {
        debug_assert!(
            block_layers_match_shape(layers),
            "block texture layers must be 16 layers of 16x16 RGBA bytes"
        );
        upload_block_layers(&self.block_texture, queue, layers);
    }

    // 10 args: the existing world-pass parameters plus the T01 `ui_pass`
    // hook, the T02 `show_overlays` gate, and the S02-T03 grouped `sky`
    // inputs (which since S03 also carry `time_of_day` for the sky pass, so
    // the signature did not grow). Kept flat rather than grouped further to
    // avoid churn at the single call site in src/main.rs; this is the
    // frame-draw entry point, not a general-purpose API.
    //
    // Pass order on the encoder: sky (clears color) -> world (loads color,
    // clears depth) -> overlay -> optional UI (loads color).
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &self,
        ctx: &RenderContext,
        store: &ChunkRenderStore,
        camera: &Camera,
        render_distance: i32,
        target_block: Option<IVec3>,
        selected_slot: usize,
        show_overlays: bool,
        sky: SkyParams,
        ui_pass: Option<&dyn UiPass>,
    ) -> Result<usize, wgpu::SurfaceError> {
        let output = ctx.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let aspect = ctx.config.width as f32 / ctx.config.height.max(1) as f32;
        let vp = camera.view_proj(aspect);
        let frustum = Frustum::from_view_proj(&vp);

        let max_dist = (render_distance * CHUNK_SIZE as i32) as f32;
        // Submersion swaps the palette fog color and the render-distance range
        // for the fixed deep-water shift. Only values change; the 112-byte
        // CameraUniform layout (and day_factor in fog_start_end.z) is intact.
        let (fog_color, fog_start, fog_end) =
            effective_fog(sky.underwater, sky.fog_color, max_dist * 0.65, max_dist);
        let cam_uniform = CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            camera_pos: [camera.position.x, camera.position.y, camera.position.z, 1.0],
            fog_color,
            fog_start_end: [fog_start, fog_end, sky.day_factor, 0.0],
        };
        ctx.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam_uniform));

        // Per-frame celestial state: three vec4s derived from the game clock by
        // the pure day-cycle helpers (directions, brightness fades, star
        // visibility and rotation). One 48-byte write per frame; all sky
        // geometry itself is static.
        let sky_uniform = sky::SkyUniform::from_time(sky.time_of_day);
        ctx.queue.write_buffer(
            &self.sky_uniform_buffer,
            0,
            bytemuck::bytes_of(&sky_uniform),
        );

        let overlay_verts =
            build_overlay_line_vertices(&vp, aspect, target_block, selected_slot, show_overlays);
        let overlay_buf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay_vbuf"),
                contents: bytemuck::cast_slice(&overlay_verts),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame_encoder"),
            });

        let mut visible_chunks = 0usize;

        // Sky pass: FIRST pass on the encoder, before the world pass. It is
        // color-only (no depth attachment) and clears the color target to the
        // palette sky color, then draws the camera-relative sun/moon/star
        // billboards. It is deliberately UNCONDITIONAL: the world pass below
        // loads (not clears) the color attachment, so skipping this pass would
        // present stale or undefined content. Terrain then overwrites sky
        // pixels wherever geometry lands (occlusion by draw order).
        {
            let mut sky_rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sky_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(sky.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            sky_rpass.set_pipeline(&self.sky_pipeline);
            // Group 0 is the shared camera bind group; the sky shader declares
            // only the view_proj/camera_pos members it reads.
            sky_rpass.set_bind_group(0, &self.camera_bind_group, &[]);
            sky_rpass.set_bind_group(1, &self.sky_bind_group, &[]);
            sky_rpass.set_vertex_buffer(0, self.sky_vertex_buffer.slice(..));
            sky_rpass.draw(0..self.sky_vertex_count, 0..1);
        }

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Load: the sky pass already cleared and painted the
                        // color target above. Depth is cleared here as before.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &ctx.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_pipeline(&self.voxel_pipeline);
            rpass.set_bind_group(0, &self.camera_bind_group, &[]);

            let cs = CHUNK_SIZE as f32;
            for gpu_chunk in store.chunks.values() {
                let min = Vec3::new(
                    gpu_chunk.pos.x as f32 * cs,
                    gpu_chunk.pos.y as f32 * cs,
                    gpu_chunk.pos.z as f32 * cs,
                );
                let max = min + Vec3::splat(cs);

                if !frustum.intersects_aabb(min, max) {
                    continue;
                }

                let face_mask = visible_face_mask(camera.position, min, max);
                if face_mask == 0 {
                    continue;
                }

                visible_chunks += 1;
                rpass.set_bind_group(1, &gpu_chunk.bind_group, &[]);
                rpass.set_vertex_buffer(0, gpu_chunk.vertex_buffer.slice(..));

                for dir in 0..6 {
                    if (face_mask & (1 << dir)) != 0 {
                        let range = &gpu_chunk.bucket_ranges[dir];
                        if range.start < range.end {
                            rpass.draw(0..6, range.clone());
                        }
                    }
                }
            }

            // Translucent water stage: strictly AFTER the opaque chunk loop
            // (so the depth buffer written there occludes water behind
            // terrain) and BEFORE the overlay block (so the crosshair and
            // hotbar draw on top). The pipeline writes no depth, so water can
            // never occlude anything drawn later. Same frustum + face-mask
            // gating as the opaque loop; chunks without water are skipped
            // entirely, which makes an absent water stage a no-op. Direct
            // statements, no closures: wgpu 0.19 RenderPass lifetimes (MEM032).
            rpass.set_pipeline(&self.water_pipeline);
            rpass.set_bind_group(0, &self.camera_bind_group, &[]);
            for gpu_chunk in store.chunks.values() {
                let Some(water_buffer) = gpu_chunk.water_vertex_buffer.as_ref() else {
                    continue;
                };
                if gpu_chunk.water_quad_count == 0 {
                    continue;
                }

                let min = Vec3::new(
                    gpu_chunk.pos.x as f32 * cs,
                    gpu_chunk.pos.y as f32 * cs,
                    gpu_chunk.pos.z as f32 * cs,
                );
                let max = min + Vec3::splat(cs);

                if !frustum.intersects_aabb(min, max) {
                    continue;
                }

                let face_mask = visible_face_mask(camera.position, min, max);
                if face_mask == 0 {
                    continue;
                }

                rpass.set_bind_group(1, &gpu_chunk.bind_group, &[]);
                rpass.set_vertex_buffer(0, water_buffer.slice(..));
                // Six vertices per instance (PackedQuad step mode), one
                // instance per water quad in the contiguous water buffer.
                rpass.draw(0..QUAD_VERTICES, 0..gpu_chunk.water_quad_count as u32);
            }

            if !overlay_verts.is_empty() {
                rpass.set_pipeline(&self.overlay_pipeline);
                rpass.set_vertex_buffer(0, overlay_buf.slice(..));
                rpass.draw(0..overlay_verts.len() as u32, 0..1);
            }
        }

        // UI pass: second color-only render pass on the same encoder and view,
        // drawn after the world pass and before submit/present. It loads (not
        // clears) the color attachment so the world pass output stays visible.
        if let Some(ui) = ui_pass {
            let mut ui_rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            ui.draw_ui(&mut ui_rpass);
        }

        ctx.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(visible_chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_uniform_stays_seven_vec4s() {
        // WGSL uniform members must be 16-byte aligned, and the day factor
        // rides in the unused `fog_start_end.z` slot precisely so this stays
        // true. The struct is one mat4x4 (4 vec4s) plus camera_pos, fog_color
        // and fog_start_end (3 vec4s) = 7 vec4s = 112 bytes. Any new field
        // here would change the CPU/GPU layout contract.
        assert_eq!(std::mem::size_of::<CameraUniform>(), 112);
        assert_eq!(std::mem::size_of::<CameraUniform>() % 16, 0);
        assert_eq!(
            std::mem::size_of::<CameraUniform>(),
            7 * std::mem::size_of::<[f32; 4]>()
        );
        assert_eq!(std::mem::offset_of!(CameraUniform, fog_start_end), 96);
    }

    #[test]
    fn voxel_shader_applies_day_factor_from_fog_start_end_z() {
        assert!(
            VOXEL_SHADER_WGSL.contains("let day_factor = camera.fog_start_end.z;"),
            "vs_main must read the day factor from the unused fog_start_end.z slot"
        );
        assert!(
            VOXEL_SHADER_WGSL.contains("ao_factor * light_factor * day_factor"),
            "terrain vertex light must be scaled by the day factor"
        );
    }

    #[test]
    fn test_overlay_vertices_include_wireframe_crosshair_and_hotbar() {
        let verts =
            build_overlay_line_vertices(&Mat4::IDENTITY, 1.77, Some(IVec3::new(1, 2, 3)), 0, true);
        // 24 (wireframe) + 4 (crosshair) + 9 * 8 (hotbar) = 100 vertices
        assert_eq!(verts.len(), 100);
    }

    #[test]
    fn test_overlay_vertices_empty_when_overlays_hidden() {
        // Non-Playing screens must suppress the wireframe, crosshair, AND
        // hotbar even when the inputs still carry a target and a selected
        // slot — the flag alone gates all three.
        let verts =
            build_overlay_line_vertices(&Mat4::IDENTITY, 1.77, Some(IVec3::new(1, 2, 3)), 4, false);
        assert!(verts.is_empty(), "hidden overlays must emit no vertices");
    }

    #[test]
    fn sky_params_carries_time_of_day_into_the_sky_uniform() {
        // The same grouped struct that carries the palette also carries the
        // game time, so render_frame's 10-arg signature did not grow. The
        // time is what the pure day-cycle helpers turn into sun/moon/star
        // placement for the sky pass.
        let params = SkyParams {
            day_factor: 0.35,
            fog_color: [0.1, 0.12, 0.2, 1.0],
            clear_color: wgpu::Color {
                r: 0.1,
                g: 0.12,
                b: 0.2,
                a: 1.0,
            },
            time_of_day: 18_000,
            underwater: false,
        };
        assert_eq!(params.time_of_day, 18_000);
        assert_eq!(params.day_factor, 0.35);
        assert!(!params.underwater);

        // Midnight: moon up, stars out, sun below the horizon.
        let uniform = sky::SkyUniform::from_time(params.time_of_day);
        assert!(uniform.sun_dir[1] < 0.0, "sun is below the horizon");
        assert!(uniform.moon_dir[1] > 0.0, "moon is above the horizon");
        assert!(uniform.params[1] > 0.0, "moon brightness is non-zero");
        assert!(uniform.params[2] > 0.0, "stars are visible at midnight");
    }

    #[test]
    fn underwater_fog_shift_round_trips_and_keeps_the_dry_path_identical() {
        let palette_fog = [0.53, 0.76, 0.96, 1.0];
        let dry = effective_fog(false, palette_fog, 300.0, 512.0);
        assert_eq!(dry.0, palette_fog, "dry fog color passes through");
        assert_eq!((dry.1, dry.2), (300.0, 512.0), "dry range is untouched");

        let wet = effective_fog(true, palette_fog, 300.0, 512.0);
        assert_eq!(wet.0, UNDERWATER_FOG_COLOR);
        assert_eq!((wet.1, wet.2), (UNDERWATER_FOG_START, UNDERWATER_FOG_END));
        assert!(
            wet.1 < wet.2 && UNDERWATER_FOG_START < 300.0,
            "submersion pulls fog in much closer than the render distance"
        );
        // Same sRGB-value convention as palette.fog_color: plain 0..1 channel
        // values, blue-dominant and darker than daytime air fog.
        assert!(wet.0[2] > wet.0[0] && wet.0[2] > wet.0[1]);
        assert!(wet.0[0] >= 0.0 && wet.0[2] <= 1.0 && wet.0[3] == 1.0);

        // SkyParams stays Copy so render_frame keeps its flat 10-arg signature.
        fn assert_copy<T: Copy>() {}
        assert_copy::<SkyParams>();
        let params = SkyParams {
            day_factor: 1.0,
            fog_color: palette_fog,
            clear_color: wgpu::Color::BLACK,
            time_of_day: 6_000,
            underwater: true,
        };
        assert!(params.underwater);
    }

    #[test]
    fn water_shader_ignores_the_block_texture_and_tex_layer() {
        // MEM230/MEM232: water is shader-colored. The packed quads carry a
        // placeholder layer 0 (Grass Top) only because the format has a byte
        // for it, so the water shader must never sample the block array.
        assert!(
            WATER_SHADER_WGSL.contains("vec4<f32>(0.15, 0.35, 0.70, 0.65)"),
            "water fragment output must be the procedural deep-blue RGBA"
        );
        assert!(
            !WATER_SHADER_WGSL.contains("textureSample")
                && !WATER_SHADER_WGSL.contains("block_tex")
                && !WATER_SHADER_WGSL.contains("tex_layer"),
            "water must not sample the block texture array or read a layer"
        );
        // Only group 0 binding 0 (camera) and group 1 binding 0 (chunk origin)
        // are declared; the pipeline layout may carry unused camera entries.
        assert!(!WATER_SHADER_WGSL.contains("@binding(1)"));
        assert!(!WATER_SHADER_WGSL.contains("@binding(2)"));

        // The vertex stage is the voxel decode: same day factor source, same
        // analytic planar backface cull, same fog formula in the fragment.
        assert!(WATER_SHADER_WGSL.contains("let day_factor = camera.fog_start_end.z;"));
        assert!(WATER_SHADER_WGSL.contains("dot(world_pos - camera.camera_pos.xyz, normal) >= 0.0"));
        assert!(WATER_SHADER_WGSL.contains("let fog_start = camera.fog_start_end.x;"));
        assert!(WATER_SHADER_WGSL.contains("let fog_end = camera.fog_start_end.y;"));
    }

    #[test]
    fn water_pipeline_is_alpha_blended_and_does_not_write_depth() {
        // Source pin on the water pipeline descriptor: translucent blending,
        // no depth writes (water never occludes later draws), and LessEqual
        // against the opaque depth buffer (terrain occludes water).
        let src = include_str!("pipeline.rs");
        let water_at = src
            .find("label: Some(\"water_pipeline\")")
            .expect("water pipeline exists");
        let overlay_shader_at = src
            .find("label: Some(\"overlay_shader\")")
            .expect("overlay shader follows the water pipeline");
        assert!(water_at < overlay_shader_at);
        let water_block = &src[water_at..overlay_shader_at];
        assert!(
            water_block.contains("blend: Some(wgpu::BlendState::ALPHA_BLENDING)"),
            "water must blend translucently"
        );
        assert!(
            water_block.contains("depth_write_enabled: false"),
            "water must not write depth"
        );
        assert!(water_block.contains("depth_compare: wgpu::CompareFunction::LessEqual"));
        assert!(
            water_block.contains("packed_quad_vertex_layout()"),
            "water must share the opaque PackedQuad vertex layout"
        );
    }

    #[test]
    fn water_draw_sits_after_the_opaque_loop_and_before_the_overlay() {
        // Source pin on main_pass stage order: the opaque chunk loop writes the
        // depth buffer first, the translucent water draw consumes it second,
        // and the overlay lines draw last. The water block must reuse the same
        // frustum + face-mask gating and skip absent/empty water buffers.
        let src = include_str!("pipeline.rs");
        let opaque_loop_at = src
            .find("for gpu_chunk in store.chunks.values()")
            .expect("opaque chunk loop exists");
        let water_draw_at = src
            .find("set_pipeline(&self.water_pipeline)")
            .expect("water draw exists");
        let overlay_draw_at = src
            .find("set_pipeline(&self.overlay_pipeline)")
            .expect("overlay draw exists");
        assert!(
            opaque_loop_at < water_draw_at && water_draw_at < overlay_draw_at,
            "stage order must be opaque loop -> water draw -> overlay"
        );

        let water_block = &src[water_draw_at..overlay_draw_at];
        assert!(water_block.contains("water_vertex_buffer"));
        assert!(water_block.contains("water_quad_count"));
        assert!(water_block.contains("frustum.intersects_aabb"));
        assert!(water_block.contains("visible_face_mask"));
        assert!(water_block.contains("QUAD_VERTICES"));
        assert!(
            water_block.contains("continue"),
            "absent or empty water buffers must be skipped, not drawn"
        );
    }

    #[test]
    fn main_pass_still_loads_color_and_the_water_stage_never_clears() {
        let src = include_str!("pipeline.rs");
        let main_at = src
            .find("label: Some(\"main_pass\")")
            .expect("main pass exists");
        let ui_at = src
            .find("label: Some(\"ui_pass\")")
            .expect("ui pass exists");
        let main_block = &src[main_at..ui_at];
        assert!(
            main_block.contains("load: wgpu::LoadOp::Load,"),
            "the world pass must keep loading the sky pass output"
        );
        assert!(
            !main_block.contains("LoadOp::Clear(sky.clear_color)"),
            "no stage inside main_pass may clear the color target"
        );
    }

    #[test]
    fn vista_sky_params_use_the_pinned_golden_hour() {
        // Source pin: the vista arm must pass VISTA_GOLDEN_HOUR (S02's single
        // home for the constant) rather than the world clock, and the session
        // arm must pass the live clock. Both go through `sky_params`, whose
        // third argument is the S04-T03 underwater flag: the vista panorama
        // anchors over land so it pins `false`, while the session arm feeds the
        // per-frame eye-submersion lookup through.
        let src = include_str!("../main.rs");
        assert!(
            src.contains("sky_params(palette, daycycle::VISTA_GOLDEN_HOUR, false)"),
            "vista frames must pin the sky time to golden hour and stay dry"
        );
        assert!(
            src.contains("sky_params(palette, self.session.clock.time_of_day, underwater)"),
            "session frames must follow the live clock and the submersion lookup"
        );
        assert!(
            src.contains("eye_is_submerged(&self.session.world, self.session.camera.position)"),
            "the session arm must derive the flag from the live eye position"
        );
    }

    #[test]
    fn sky_pass_is_first_and_world_pass_loads_its_color() {
        // Static source pin on render_frame's pass order. The sky pass clears
        // the target and must run before the world pass; the world pass then
        // Loads (never Clears) color so the sky survives behind terrain. The
        // sky pass is unconditional: no guard may sit between the encoder and
        // the pass that clears the target.
        let src = include_str!("pipeline.rs");
        let encoder_at = src
            .find("label: Some(\"frame_encoder\")")
            .expect("encoder exists");
        let sky_at = src
            .find("label: Some(\"sky_pass\")")
            .expect("sky pass exists");
        let main_at = src
            .find("label: Some(\"main_pass\")")
            .expect("main pass exists");
        assert!(
            encoder_at < sky_at && sky_at < main_at,
            "pass order must be encoder -> sky_pass -> main_pass"
        );

        let before_sky = &src[encoder_at..sky_at];
        assert!(
            !before_sky.contains("return") && !before_sky.contains("if "),
            "the sky pass must be unconditional (it owns the color clear)"
        );

        let sky_block = &src[sky_at..main_at];
        assert!(
            sky_block.contains("load: wgpu::LoadOp::Clear(sky.clear_color)"),
            "the sky pass must clear the color target"
        );
        assert!(
            sky_block.contains("depth_stencil_attachment: None"),
            "the sky pass is color-only"
        );
        assert!(sky_block.contains("set_pipeline(&self.sky_pipeline)"));
        assert!(
            sky_block.contains("draw(0..self.sky_vertex_count, 0..1)"),
            "the sky pass must draw the static billboard buffer"
        );

        let main_block = &src[main_at..src.len().min(main_at + 700)];
        assert!(
            main_block.contains("load: wgpu::LoadOp::Load,"),
            "the world pass must load the sky pass output"
        );
        assert!(
            !main_block.contains("LoadOp::Clear(sky.clear_color)"),
            "the world pass must not clear the color the sky pass wrote"
        );
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: {actual} != {expected}"
        );
    }

    #[test]
    fn hotbar_slots_are_uniform_adjacent_and_centered() {
        let rects: Vec<_> = (0..HOTBAR_SLOTS)
            .map(|i| hotbar_slot_rect_ndc(i, 1.77))
            .collect();

        let (first_x0, y0, _, y1) = rects[0];
        let (_, _, last_x1, _) = rects[HOTBAR_SLOTS - 1];
        assert_close(first_x0, -last_x1, "bar centered on x=0");
        assert_close(y0, -0.92, "bar bottom");
        assert_close(y1, -0.835, "bar top");

        let (w0, h0) = (rects[0].2 - rects[0].0, rects[0].3 - rects[0].1);
        let gap = rects[1].0 - rects[0].2;
        assert!(gap > 0.0, "gap must be positive, got {gap}");

        for (i, rect) in rects.iter().enumerate() {
            assert_close(rect.2 - rect.0, w0, &format!("slot {i} width"));
            assert_close(rect.3 - rect.1, h0, &format!("slot {i} height"));
            assert_close(rect.1, y0, &format!("slot {i} bottom"));
            assert_close(rect.3, y1, &format!("slot {i} top"));
            if i > 0 {
                assert_close(rect.0 - rects[i - 1].2, gap, &format!("slot {i} gap"));
            }
        }
    }

    #[test]
    fn hotbar_layout_clamps_narrow_aspects() {
        for i in 0..HOTBAR_SLOTS {
            assert_eq!(
                hotbar_slot_rect_ndc(i, 0.1),
                hotbar_slot_rect_ndc(i, 0.5),
                "aspect below 0.5 clamps to 0.5"
            );
        }
    }

    #[test]
    fn hotbar_out_of_range_index_is_degenerate_and_finite() {
        let (x0, y0, x1, y1) = hotbar_slot_rect_ndc(HOTBAR_SLOTS, 1.77);
        assert_eq!((x0, y0, x1, y1), (x0, y0, x0, y0));
        assert!(x0.is_finite() && y0.is_finite());
        assert_eq!(x0, hotbar_slot_rect_ndc(0, 1.77).0, "returns at left edge");
    }
}
