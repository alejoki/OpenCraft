//! Sky pass GPU layer: sun, moon and star billboards.
//!
//! All celestial geometry is static and camera-relative: `build_sky_vertices`
//! expands the sun and moon quads and every star into billboard vertices once
//! at renderer construction, and the vertex shader positions them on a sphere
//! around `camera.camera_pos`. Per-frame state is a single
//! 48-byte [`SkyUniform`] (the sun/moon directions, their brightness fades,
//! the star visibility and the star rotation angle) written with
//! `queue.write_buffer`.
//!
//! The pure math that fills [`SkyUniform`] lives headlessly in
//! [`crate::world::daycycle`]; this module only uploads it and draws
//! billboards.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::render::context::RenderContext;
use crate::world::daycycle;

/// Distance from the camera to the sky billboards. Must stay inside
/// `Camera::z_far` (1200 in `src/player/camera.rs`) so the quads survive the
/// clip-volume test; 1,000 leaves comfortable margin.
pub const SKY_DIST: f32 = 1_000.0;

/// Quad kind tags carried per vertex so the shader knows where to read the
/// per-frame direction and brightness from.
pub const KIND_STAR: u32 = 0;
pub const KIND_SUN: u32 = 1;
pub const KIND_MOON: u32 = 2;

/// Sun billboard tint (warm white).
const SUN_COLOR: [f32; 4] = [1.0, 0.96, 0.80, 1.0];
/// Moon billboard tint: a cool, dimmed blue-gray so the moon reads as itself
/// even at a glance (silhouette plus size plus tint — UAT-H3).
const MOON_COLOR: [f32; 4] = [0.62, 0.68, 0.82, 1.0];
/// Star billboard tint.
const STAR_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Six corners of a quad in `-1..1`, expanded to two triangles. Same
/// no-index-buffer expansion precedent as `src/ui/quad.rs`.
const QUAD_CORNERS: [[f32; 2]; 6] = [
    [-1.0, -1.0],
    [1.0, -1.0],
    [1.0, 1.0],
    [-1.0, -1.0],
    [1.0, 1.0],
    [-1.0, 1.0],
];

/// Fixed vertex count of the static moon quad (two triangles).
///
/// The moon is a square billboard like the sun; size and the cool dim tint
/// carry its identity instead of a crescent silhouette. Kept as a named
/// constant because the static-buffer layout tests index past it.
pub const MOON_VERTEX_COUNT: usize = 6;

/// Per-frame sky state shared by every billboard.
///
/// Layout: three `vec4`s (48 bytes, 16-aligned). Deliberately separate from
/// `CameraUniform`, which is pinned at 112 bytes / seven vec4s (MEM159).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct SkyUniform {
    pub sun_dir: [f32; 4],
    pub moon_dir: [f32; 4],
    /// `(sun_brightness, moon_brightness, star_visibility, star_angle)`.
    pub params: [f32; 4],
}

impl SkyUniform {
    /// Derive the whole uniform from a game time via the pure day-cycle math.
    pub fn from_time(time_of_day: u32) -> Self {
        let sun = daycycle::sun_direction(time_of_day);
        let moon = daycycle::moon_direction(time_of_day);
        // Same rotation the sun follows, recovered from its direction so the
        // formula lives in exactly one place (daycycle).
        let star_angle = sun.y.atan2(sun.x);
        Self {
            sun_dir: [sun.x, sun.y, sun.z, 0.0],
            moon_dir: [moon.x, moon.y, moon.z, 0.0],
            params: [
                daycycle::sun_brightness(time_of_day),
                daycycle::moon_brightness(time_of_day),
                daycycle::star_visibility(time_of_day),
                star_angle,
            ],
        }
    }
}

/// Static sky billboard vertex.
///
/// `dir` is the star direction (ignored for sun/moon, whose direction arrives
/// per-frame in [`SkyUniform`]). `corner` spans `-1..1`; `size` is an angular
/// half-size in radians, scaled by [`SKY_DIST`] in the shader.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct SkyVertex {
    pub dir: [f32; 3],
    pub corner: [f32; 2],
    pub size: f32,
    pub color: [f32; 4],
    /// One of [`KIND_STAR`], [`KIND_SUN`], [`KIND_MOON`].
    pub kind: u32,
}

impl SkyVertex {
    fn quad(dir: Vec3, size: f32, color: [f32; 4], kind: u32) -> [SkyVertex; 6] {
        QUAD_CORNERS.map(|corner| SkyVertex {
            dir: [dir.x, dir.y, dir.z],
            corner,
            size,
            color,
            kind,
        })
    }
}

/// Expand the sun, moon and fixed-seed star field into static billboard
/// vertices. Called once at renderer construction; the result never changes.
pub fn build_sky_vertices(star_field: &[Vec3]) -> Vec<SkyVertex> {
    let mut verts = Vec::with_capacity(6 + MOON_VERTEX_COUNT + star_field.len() * 6);

    // Sun and moon directions are placeholders: the shader replaces them with
    // the per-frame uniform directions, so the static buffer stays valid all
    // day. Their colors and angular sizes are fixed.
    for v in SkyVertex::quad(Vec3::X, daycycle::SUN_ANGULAR_SIZE, SUN_COLOR, KIND_SUN) {
        verts.push(v);
    }
    for v in SkyVertex::quad(-Vec3::X, daycycle::MOON_ANGULAR_SIZE, MOON_COLOR, KIND_MOON) {
        verts.push(v);
    }
    for star in star_field {
        for v in SkyVertex::quad(*star, daycycle::STAR_ANGULAR_SIZE, STAR_COLOR, KIND_STAR) {
            verts.push(v);
        }
    }

    verts
}

/// WGSL for the sky billboard pass.
///
/// Group 0 is the existing camera bind group (only `view_proj` and
/// `camera_pos` are declared); group 1 is the per-frame [`SkyUniform`].
pub const SKY_SHADER_WGSL: &str = r#"
const SKY_DIST: f32 = 1000.0;

struct CameraUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    fog_color: vec4<f32>,
    fog_start_end: vec4<f32>,
};

struct SkyUniform {
    sun_dir: vec4<f32>,
    moon_dir: vec4<f32>,
    params: vec4<f32>,
};

struct SkyVertex {
    @location(0) dir: vec3<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) size: f32,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> sky: SkyUniform;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) kind: f32,
};

fn rotate_z(dir: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec3<f32>(c * dir.x - s * dir.y, s * dir.x + c * dir.y, dir.z);
}

@vertex
fn vs_main(vertex: SkyVertex) -> VertexOutput {
    var body_dir = vertex.dir;
    var alpha = 1.0;

    if (vertex.kind == 1u) {
        body_dir = sky.sun_dir.xyz;
        alpha = sky.params.x;
    } else if (vertex.kind == 2u) {
        body_dir = sky.moon_dir.xyz;
        alpha = sky.params.y;
    } else {
        // The celestial sphere turns with the sun; sun/moon directions are
        // already rotated on the CPU, so only stars rotate in-shader.
        body_dir = rotate_z(vertex.dir, sky.params.w);
        // Stars fade out near and below the horizon instead of popping.
        alpha = sky.params.z * smoothstep(-0.12, 0.12, body_dir.y);
    }

    let dir = normalize(body_dir);
    let center = camera.camera_pos.xyz + dir * SKY_DIST;

    // Billboard basis in the plane perpendicular to `dir`, with a fallback
    // axis for near-pole directions where cross(dir, +Z) degenerates.
    var up = vec3<f32>(0.0, 0.0, 1.0);
    if (abs(dot(dir, up)) > 0.99) {
        up = vec3<f32>(1.0, 0.0, 0.0);
    }
    let right = normalize(cross(up, dir));
    let billboard_up = cross(dir, right);

    let offset = (right * vertex.corner.x + billboard_up * vertex.corner.y)
        * (vertex.size * SKY_DIST);
    let world = center + offset;

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(world, 1.0);
    out.color = vec4<f32>(vertex.color.rgb, vertex.color.a * alpha);
    // Pass the quad corner through for the per-fragment star mask. The kind
    // tags are 0/1/2, exact in f32, so plain interpolation is safe.
    out.corner = vertex.corner;
    out.kind = f32(vertex.kind);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var color = in.color;
    // Stars are hard square quads; carve a soft circular disc out of their
    // alpha instead of tessellating 600 CPU discs (D057). The smoothstep edge
    // anti-aliases the boundary on the 1-sample sky pipeline. Sun (1) and
    // moon (2) bypass the mask entirely: both stay hard square quads.
    if (in.kind < 0.5) {
        let mask = 1.0 - smoothstep(0.85, 1.0, length(in.corner));
        color.a = color.a * mask;
    }
    return color;
}
"#;

/// Bind group layout for the 48-byte [`SkyUniform`].
pub fn create_sky_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sky_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Create the sky uniform buffer initialised to the default noon sky.
pub fn create_sky_uniform_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    let uniform = SkyUniform::from_time(daycycle::DEFAULT_TIME_OF_DAY);
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sky_uniform_buffer"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

pub fn create_sky_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sky_bg"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    })
}

/// Build the static sky vertex buffer (sun quad + moon quad + every star
/// quad) once at renderer construction.
pub fn create_sky_vertex_buffer(device: &wgpu::Device, star_field: &[Vec3]) -> (wgpu::Buffer, u32) {
    use wgpu::util::DeviceExt;
    let vertices = build_sky_vertices(star_field);
    let count = vertices.len() as u32;
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sky_vertex_buffer"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    (buffer, count)
}

/// Sky pipeline: alpha-blended, double-sided camera-relative billboards with
/// no depth attachment (the sky pass runs first and clears the color target).
pub fn create_sky_pipeline(
    ctx: &RenderContext,
    camera_bgl: &wgpu::BindGroupLayout,
    sky_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky_shader"),
            source: wgpu::ShaderSource::Wgsl(SKY_SHADER_WGSL.into()),
        });

    let layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky_pipeline_layout"),
            bind_group_layouts: &[camera_bgl, sky_bgl],
            push_constant_ranges: &[],
        });

    ctx.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SkyVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 20,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 40,
                            shader_location: 4,
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // Billboards are double-sided.
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::camera::Camera;

    #[test]
    fn sky_uniform_is_three_16_byte_aligned_vec4s() {
        assert_eq!(std::mem::size_of::<SkyUniform>(), 48);
        assert_eq!(std::mem::size_of::<SkyUniform>() % 16, 0);
        assert_eq!(std::mem::offset_of!(SkyUniform, sun_dir), 0);
        assert_eq!(std::mem::offset_of!(SkyUniform, moon_dir), 16);
        assert_eq!(std::mem::offset_of!(SkyUniform, params), 32);
    }

    #[test]
    fn sky_uniform_from_time_tracks_the_pure_day_cycle() {
        let noon = SkyUniform::from_time(daycycle::DEFAULT_TIME_OF_DAY);
        assert!(
            (noon.sun_dir[1] - 1.0).abs() < 1e-5,
            "noon sun is at zenith"
        );
        assert!((noon.params[0] - 1.0).abs() < 1e-5, "noon sun is bright");
        assert_eq!(noon.params[2], 0.0, "no stars at noon");

        let midnight =
            SkyUniform::from_time(daycycle::DAY_TICKS / 2 + daycycle::DEFAULT_TIME_OF_DAY);
        assert!(
            (midnight.moon_dir[1] - 1.0).abs() < 1e-5,
            "midnight moon is at zenith"
        );
        assert!(
            (midnight.params[1] - 1.0).abs() < 1e-5,
            "midnight moon is bright"
        );
        assert!(
            (midnight.params[2] - 1.0).abs() < 1e-5,
            "midnight star visibility is full"
        );
    }

    #[test]
    fn sky_vertex_layout_is_pod_and_stable() {
        assert_eq!(std::mem::size_of::<SkyVertex>(), 44);
        assert_eq!(std::mem::offset_of!(SkyVertex, dir), 0);
        assert_eq!(std::mem::offset_of!(SkyVertex, corner), 12);
        assert_eq!(std::mem::offset_of!(SkyVertex, size), 20);
        assert_eq!(std::mem::offset_of!(SkyVertex, color), 24);
        assert_eq!(std::mem::offset_of!(SkyVertex, kind), 40);

        let vertex = SkyVertex {
            dir: Vec3::Y.to_array(),
            corner: [0.0; 2],
            size: 0.02,
            color: STAR_COLOR,
            kind: KIND_STAR,
        };
        let bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(bytes.len(), 44);
        assert_eq!(
            bytemuck::from_bytes::<SkyVertex>(bytes).kind,
            KIND_STAR,
            "SkyVertex must round-trip through Pod casts"
        );
    }

    #[test]
    fn build_sky_vertices_handles_an_empty_star_field() {
        let verts = build_sky_vertices(&[]);
        assert_eq!(
            verts.len(),
            6 + MOON_VERTEX_COUNT,
            "sun and moon still render with no stars"
        );
        assert_eq!(verts.iter().filter(|v| v.kind == KIND_SUN).count(), 6);
        assert_eq!(
            verts.iter().filter(|v| v.kind == KIND_MOON).count(),
            MOON_VERTEX_COUNT
        );
    }

    #[test]
    fn sky_uniform_from_time_wraps_and_stays_finite() {
        let dawn = SkyUniform::from_time(0);
        assert_eq!(
            bytemuck::bytes_of(&dawn),
            bytemuck::bytes_of(&SkyUniform::from_time(daycycle::DAY_TICKS))
        );
        assert_eq!(
            bytemuck::bytes_of(&SkyUniform::from_time(1_000)),
            bytemuck::bytes_of(&SkyUniform::from_time(daycycle::DAY_TICKS + 1_000))
        );

        // Sweep the whole cycle plus out-of-range values: no NaN/inf may reach
        // the uniform, and every field stays in its documented range.
        for time in (0..=daycycle::DAY_TICKS * 2).step_by(250) {
            let u = SkyUniform::from_time(time);
            for value in u
                .sun_dir
                .iter()
                .chain(u.moon_dir.iter())
                .chain(u.params.iter())
            {
                assert!(value.is_finite(), "non-finite uniform field at t={time}");
            }
            for brightness in &u.params[0..3] {
                assert!(
                    (0.0..=1.0).contains(brightness),
                    "brightness out of range at t={time}: {brightness}"
                );
            }
            assert!((u.sun_dir[0].hypot(u.sun_dir[1]) - 1.0).abs() < 1e-4);
            assert!((u.moon_dir[0].hypot(u.moon_dir[1]) - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn build_sky_vertices_emits_body_geometry_and_star_quads() {
        let stars = daycycle::generate_star_field();
        let verts = build_sky_vertices(&stars);
        let star_base = 6 + MOON_VERTEX_COUNT;

        assert_eq!(stars.len(), daycycle::STAR_COUNT);
        assert_eq!(verts.len(), star_base + stars.len() * 6);
        assert_eq!(
            &verts[0..6].iter().map(|v| v.kind).collect::<Vec<_>>(),
            &[KIND_SUN; 6]
        );
        assert_eq!(
            &verts[6..star_base]
                .iter()
                .map(|v| v.kind)
                .collect::<Vec<_>>(),
            &[KIND_MOON; MOON_VERTEX_COUNT]
        );
        assert!(verts[star_base..].iter().all(|v| v.kind == KIND_STAR));

        for (i, star) in stars.iter().enumerate() {
            let base = star_base + i * 6;
            for v in &verts[base..base + 6] {
                assert_eq!(Vec3::from(v.dir), *star);
                assert_eq!(v.size, daycycle::STAR_ANGULAR_SIZE);
            }
        }
    }

    #[test]
    fn moon_geometry_is_a_static_square_quad() {
        let verts = build_sky_vertices(&[]);
        let moon: Vec<SkyVertex> = verts
            .iter()
            .copied()
            .filter(|v| v.kind == KIND_MOON)
            .collect();

        assert_eq!(moon.len(), MOON_VERTEX_COUNT);
        assert_eq!(MOON_VERTEX_COUNT, 6, "the moon is a single quad");

        // The square must use exactly the six shared quad corners, including
        // the four diagonal (-1,-1) style corners a crescent never reaches.
        for corner in QUAD_CORNERS {
            assert!(
                moon.iter().any(|v| v.corner == corner),
                "missing square corner {corner:?}"
            );
        }

        // Identity is belt-and-braces: square silhouette plus smaller size
        // plus a cool, dim tint that is clearly not the warm-white sun.
        for v in &moon {
            assert_eq!(v.color, MOON_COLOR);
            assert_eq!(v.size, daycycle::MOON_ANGULAR_SIZE);
            assert_eq!(
                Vec3::from(v.dir),
                -Vec3::X,
                "moon dir is a shader placeholder"
            );
        }
        const {
            assert!(MOON_COLOR[2] > MOON_COLOR[0], "moon tint must be cool");
            assert!(
                MOON_COLOR[0] < SUN_COLOR[0],
                "moon tint must be dimmer than the sun"
            );
            assert!(
                daycycle::MOON_ANGULAR_SIZE < daycycle::SUN_ANGULAR_SIZE,
                "the moon must be smaller than the sun"
            );
        }
    }

    #[test]
    fn sky_dist_stays_inside_the_camera_far_plane() {
        let camera = Camera::new(Vec3::ZERO);
        assert!(SKY_DIST < camera.z_far, "sky must be closer than z_far");
        assert!(
            SKY_DIST > camera.z_far * 0.5,
            "sky should be far enough to sit behind terrain"
        );
        assert!(
            SKY_SHADER_WGSL.contains("const SKY_DIST: f32 = 1000.0;"),
            "WGSL distance must match the Rust SKY_DIST constant"
        );
    }

    #[test]
    fn sky_shader_positions_billboards_camera_relative() {
        assert!(SKY_SHADER_WGSL.contains("camera.camera_pos.xyz + dir * SKY_DIST"));
        assert!(SKY_SHADER_WGSL.contains("camera.view_proj * vec4<f32>(world, 1.0)"));
        assert!(
            SKY_SHADER_WGSL.contains("abs(dot(dir, up)) > 0.99"),
            "degenerate pole directions need a fallback billboard axis"
        );
    }

    #[test]
    fn sky_shader_rotates_stars_around_z() {
        assert!(SKY_SHADER_WGSL.contains("fn rotate_z"));
        assert!(
            SKY_SHADER_WGSL.contains("c * dir.x - s * dir.y"),
            "star rotation must be a 2D rotation in the XY plane around Z"
        );
        assert!(SKY_SHADER_WGSL.contains("rotate_z(vertex.dir, sky.params.w)"));
    }

    #[test]
    fn sky_shader_multiplies_vertex_alpha_by_kind_brightness() {
        assert!(SKY_SHADER_WGSL.contains("sky.params.x"));
        assert!(SKY_SHADER_WGSL.contains("sky.params.y"));
        assert!(SKY_SHADER_WGSL.contains("sky.params.z"));
        assert!(SKY_SHADER_WGSL.contains("smoothstep(-0.12, 0.12, body_dir.y)"));
        assert!(SKY_SHADER_WGSL.contains("vertex.color.a * alpha"));
    }

    #[test]
    fn sky_shader_masks_stars_with_a_kind_gated_circular_alpha() {
        // D057: stars stay 6-vertex quads and the disc is carved per fragment.
        // The soft edge doubles as anti-aliasing on the 1-sample pipeline.
        assert!(
            SKY_SHADER_WGSL.contains("smoothstep(0.85, 1.0, length(in.corner))"),
            "fs_main must carve a soft circular disc out of the star quad"
        );
        assert!(
            SKY_SHADER_WGSL.contains("if (in.kind < 0.5)"),
            "the mask must be gated to KIND_STAR (0) so sun/moon bypass it"
        );
        assert!(
            SKY_SHADER_WGSL.contains("@location(1) corner: vec2<f32>"),
            "VertexOutput must carry the quad corner for the fragment mask"
        );
        assert!(
            SKY_SHADER_WGSL.contains("@location(2) kind: f32"),
            "VertexOutput must carry the interpolated kind as an f32 pass-through"
        );
        assert!(SKY_SHADER_WGSL.contains("out.corner = vertex.corner;"));
        assert!(
            SKY_SHADER_WGSL.contains("out.kind = f32(vertex.kind);"),
            "kind 0/1/2 is exact in f32, so no flat interpolation is needed"
        );
        // The mask must actually reach the returned alpha, not just exist.
        assert!(SKY_SHADER_WGSL.contains("color.a = color.a * mask;"));
        // Sun (1) and moon (2) bypass the mask: the mask test only names the
        // star kind lower bound.
        assert!(
            !SKY_SHADER_WGSL.contains("in.kind != 0.0")
                && !SKY_SHADER_WGSL.contains("in.kind > 0.5"),
            "the mask must be star-only, never applied to sun/moon"
        );
    }

    #[test]
    fn vista_golden_hour_never_shows_stars() {
        // The menu arm renders the pinned golden hour, where the sun is still
        // up: star visibility (and therefore the materialised mask) is zero.
        assert_eq!(
            SkyUniform::from_time(daycycle::VISTA_GOLDEN_HOUR).params[2],
            0.0,
            "the vista never shows stars, so the mask is a no-op there"
        );
    }
}
