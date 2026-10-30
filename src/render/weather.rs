//! Precipitation presentation layer: deterministic camera-relative rain
//! streaks and snowfall flakes.
//!
//! Presentation follows the [`crate::render::sky`] static-billboard precedent
//! exactly, because that pattern is deterministic by construction: a fixed-seed
//! pattern of drop quads fills a camera-relative box and is uploaded once at
//! renderer construction. Per-frame state is a single 32-byte
//! [`WeatherUniform`] (the live kind, the continuous clock phase and the
//! kind's fall/stretch parameters) written with `queue.write_buffer`.
//!
//! Falling is pure shader math: `y = mod(base.y - fall_speed * phase,
//! BOX_HEIGHT)`. The same static buffer carries a rain pattern and a snowfall
//! pattern; the uniform's kind selects one, and the inactive half is collapsed
//! to a degenerate transparent quad. Nothing here reads or writes blocks — the
//! pattern is biome-agnostic, so precipitation renders over liquid oceans too.
//!
//! The clock phase comes from [`crate::world::daycycle::GameClock::cycle_phase`]
//! (game time, never wall clock), so two runs with the same calendar render the
//! same animation and a paused game freezes it.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::render::context::{RenderContext, DEPTH_FORMAT};
use crate::world::season::Weather;

/// Fixed seed for [`build_weather_vertices`]; no wall-clock or global state.
///
/// Mirrors the `STAR_SEED` precedent in [`crate::world::daycycle`]: the drop
/// pattern is identical on every launch and for every seed/world, because it is
/// presentation only and must not depend on world state.
pub const WEATHER_PATTERN_SEED: u32 = 0x5EED_7A1D;

/// Camera-relative precipitation box, in blocks. The box moves with the
/// camera, so horizontal movement never runs out of drops and no block data is
/// ever consulted.
pub const WEATHER_BOX_WIDTH: f32 = 32.0;
/// Box height; also the wrap period of the fall animation.
pub const WEATHER_BOX_HEIGHT: f32 = 24.0;
/// Box depth.
pub const WEATHER_BOX_DEPTH: f32 = 32.0;

/// Quads per precipitation kind. Rain and snowfall each own a contiguous
/// segment of the static buffer ([`RAIN_VERTEX_COUNT`] and
/// [`SNOW_VERTEX_COUNT`]), so the draw can either use
/// [`weather_vertex_range`] or rely on the shader's kind gate.
pub const WEATHER_DROP_COUNT: usize = 800;

/// One of the two segments: rain streak quads.
pub const KIND_RAIN: u32 = 0;
/// One of the two segments: snowfall flake quads.
pub const KIND_SNOWFLAKE: u32 = 1;

/// Fall speed in blocks per clock tick. Rain is fast; snowfall drifts.
pub const RAIN_FALL_SPEED: f32 = 1.2;
/// Fall speed in blocks per clock tick for snowfall.
pub const SNOW_FALL_SPEED: f32 = 0.25;

/// Vertical stretch applied to a rain streak (narrow and tall).
pub const RAIN_STRETCH: f32 = 6.0;
/// Vertical stretch applied to a snowfall flake (roughly square).
pub const SNOW_STRETCH: f32 = 1.0;

/// Half-width of a rain streak in blocks (the long axis is `RAIN_STRETCH`).
pub const RAIN_HALF_SIZE: f32 = 0.035;
/// Half-width of a snowfall flake in blocks.
pub const SNOW_HALF_SIZE: f32 = 0.045;

/// Rain tint written into [`WeatherUniform`] (cool, translucent blue-grey).
pub const RAIN_COLOR: [f32; 4] = [0.60, 0.70, 0.85, 0.55];
/// Snowfall tint written into [`WeatherUniform`] (pale, brighter, more opaque).
pub const SNOW_COLOR: [f32; 4] = [0.96, 0.98, 1.00, 0.85];

/// Six corners of a quad in `-1..1`, expanded to two triangles. Same
/// no-index-buffer expansion precedent as `src/render/sky.rs` and
/// `src/ui/quad.rs`.
const QUAD_CORNERS: [[f32; 2]; 6] = [
    [-1.0, -1.0],
    [1.0, -1.0],
    [1.0, 1.0],
    [-1.0, -1.0],
    [1.0, 1.0],
    [-1.0, 1.0],
];

const VERTICES_PER_DROP: usize = 6;

/// Vertex count of the rain segment (all rain quads).
pub const RAIN_VERTEX_COUNT: u32 = (WEATHER_DROP_COUNT * VERTICES_PER_DROP) as u32;
/// Vertex count of the snowfall segment (all snowfall quads).
pub const SNOW_VERTEX_COUNT: u32 = (WEATHER_DROP_COUNT * VERTICES_PER_DROP) as u32;
/// Total static precipitation vertices in the one uploaded buffer.
pub const WEATHER_VERTEX_COUNT: u32 = RAIN_VERTEX_COUNT + SNOW_VERTEX_COUNT;

/// Static precipitation vertex.
///
/// `base` is the drop's place in the [`WEATHER_BOX_WIDTH`] x
/// [`WEATHER_BOX_HEIGHT`] x [`WEATHER_BOX_DEPTH`] camera-relative box (x/z are
/// centred on zero, y runs `0..WEATHER_BOX_HEIGHT`). `corner` spans `-1..1`;
/// `size` is the billboard half-width in blocks. `color.rgb` is a per-drop
/// multiplier and `color.a` a per-drop alpha variation; the kind tint lives in
/// [`WeatherUniform`]. The field order and offsets deliberately mirror
/// [`crate::render::sky::SkyVertex`], so the vertex buffer layout is the same
/// shape.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct WeatherVertex {
    pub base: [f32; 3],
    pub corner: [f32; 2],
    pub size: f32,
    pub color: [f32; 4],
    /// One of [`KIND_RAIN`], [`KIND_SNOWFLAKE`].
    pub kind: u32,
}

impl WeatherVertex {
    fn quad(base: Vec3, size: f32, color: [f32; 4], kind: u32) -> [WeatherVertex; 6] {
        QUAD_CORNERS.map(|corner| WeatherVertex {
            base: [base.x, base.y, base.z],
            corner,
            size,
            color,
            kind,
        })
    }
}

/// Per-frame precipitation state.
///
/// Layout: two `vec4`s (32 bytes, 16-aligned). `params` is
/// `(kind, phase, fall_speed, stretch)`; `tint` is the kind's RGBA color.
/// Deliberately separate from `CameraUniform`, which is pinned at 112 bytes /
/// seven vec4s (MEM159).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct WeatherUniform {
    /// `(kind, phase, fall_speed, stretch)`. `kind` is [`KIND_RAIN`] or
    /// [`KIND_SNOWFLAKE`] as an `f32` (0/1 are exact); `phase` is the
    /// continuous clock phase in ticks.
    pub params: [f32; 4],
    /// Kind tint, RGBA.
    pub tint: [f32; 4],
}

impl WeatherUniform {
    /// Derive the whole uniform from the day's weather and the clock phase.
    ///
    /// Clear writes an invisible rain configuration (zero alpha): the draw is
    /// skipped upstream, and if it ever runs anyway the fragment still fades
    /// to nothing instead of showing a stray streak. Pure and total — every
    /// input yields finite fields.
    pub fn from_weather(weather: Weather, phase: f32) -> Self {
        match weather {
            Weather::Clear => Self {
                params: [KIND_RAIN as f32, phase, RAIN_FALL_SPEED, RAIN_STRETCH],
                tint: [0.0; 4],
            },
            Weather::Rain => Self {
                params: [KIND_RAIN as f32, phase, RAIN_FALL_SPEED, RAIN_STRETCH],
                tint: RAIN_COLOR,
            },
            Weather::Snowfall => Self {
                params: [KIND_SNOWFLAKE as f32, phase, SNOW_FALL_SPEED, SNOW_STRETCH],
                tint: SNOW_COLOR,
            },
        }
    }
}

/// Draw range `(base_vertex, vertex_count)` for a weather state, or `None`
/// when there is nothing to draw.
///
/// The static buffer stores the rain segment first and the snowfall segment
/// second. A renderer can either use this range or draw the whole buffer and
/// rely on the shader's kind gate; both are correct, and the range form avoids
/// processing the inactive half.
pub fn weather_vertex_range(weather: Weather) -> Option<(u32, u32)> {
    match weather {
        Weather::Clear => None,
        Weather::Rain => Some((0, RAIN_VERTEX_COUNT)),
        Weather::Snowfall => Some((RAIN_VERTEX_COUNT, SNOW_VERTEX_COUNT)),
    }
}

/// Uniform `[0, 1)` sample from a fixed-seed stream.
#[inline]
fn rng_unit(rng: &mut Xorshift32) -> f32 {
    (rng.next() >> 8) as f32 / (1u32 << 24) as f32
}

/// Deterministic per-pattern xorshift32 stream. No wall-clock or global state,
/// so the drop pattern is byte-identical on every launch.
struct Xorshift32(u32);

impl Xorshift32 {
    #[inline]
    fn new(seed: u32) -> Self {
        // xorshift32 locks up at zero, so force a non-zero state.
        Self(seed | 1)
    }

    #[inline]
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

/// Expand the fixed-seed rain and snowfall patterns into static billboard
/// vertices. Called once at renderer construction; the result never changes.
///
/// Rain quads occupy `0..RAIN_VERTEX_COUNT`, snowfall quads the rest. The
/// function is total: bounded loops, no unwrap, no panic on any input.
pub fn build_weather_vertices() -> Vec<WeatherVertex> {
    let mut verts = Vec::with_capacity(WEATHER_VERTEX_COUNT as usize);
    let mut rng = Xorshift32::new(WEATHER_PATTERN_SEED);

    for kind in [KIND_RAIN, KIND_SNOWFLAKE] {
        for _ in 0..WEATHER_DROP_COUNT {
            let base = Vec3::new(
                (rng_unit(&mut rng) - 0.5) * WEATHER_BOX_WIDTH,
                rng_unit(&mut rng) * WEATHER_BOX_HEIGHT,
                (rng_unit(&mut rng) - 0.5) * WEATHER_BOX_DEPTH,
            );
            // Per-drop alpha variation makes the fixed pattern read less
            // uniform without adding any per-frame state.
            let alpha = 0.60 + 0.40 * rng_unit(&mut rng);
            let (size, color) = if kind == KIND_RAIN {
                (RAIN_HALF_SIZE, [1.0, 1.0, 1.0, alpha])
            } else {
                (SNOW_HALF_SIZE, [1.0, 1.0, 1.0, alpha])
            };
            for v in WeatherVertex::quad(base, size, color, kind) {
                verts.push(v);
            }
        }
    }

    verts
}

/// WGSL for the precipitation billboard pass.
///
/// Group 0 is the existing camera bind group (only `view_proj`, `camera_pos`
/// and the fog lanes are declared); group 1 is the per-frame
/// [`WeatherUniform`]. The day factor (`fog_start_end.z`) is intentionally not
/// read — the palette tint already carries the weather's dimming.
pub const WEATHER_SHADER_WGSL: &str = r#"
const BOX_HEIGHT: f32 = 24.0;

struct CameraUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    fog_color: vec4<f32>,
    fog_start_end: vec4<f32>,
};

struct WeatherUniform {
    params: vec4<f32>,
    tint: vec4<f32>,
};

struct WeatherVertex {
    @location(0) base: vec3<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) size: f32,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> weather: WeatherUniform;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) kind: f32,
    @location(3) cam_dist: f32,
};

@vertex
fn vs_main(vertex: WeatherVertex) -> VertexOutput {
    let kind = weather.params.x;
    let phase = weather.params.y;
    let fall_speed = weather.params.z;
    let stretch = weather.params.w;

    var out: VertexOutput;

    // The static buffer holds both patterns; the uniform selects one. The
    // inactive half collapses to a degenerate, transparent triangle behind
    // the far plane, so an unfiltered full-buffer draw stays correct.
    if (f32(vertex.kind) != kind) {
        out.clip_pos = vec4<f32>(0.0, 0.0, 2.0, 1.0);
        out.color = vec4<f32>(0.0);
        out.corner = vec2<f32>(0.0);
        out.kind = 0.0;
        out.cam_dist = 0.0;
        return out;
    }

    // Camera-relative falling: the drop's base sits in a fixed box around the
    // camera and its height wraps modulo BOX_HEIGHT as the clock phase
    // advances. The animation is therefore a pure function of
    // (pattern seed, phase) — no CPU mutation and no wall clock.
    let y_unwrapped = vertex.base.y - fall_speed * phase;
    let y_wrapped = y_unwrapped - floor(y_unwrapped / BOX_HEIGHT) * BOX_HEIGHT;
    let center = camera.camera_pos.xyz + vec3<f32>(
        vertex.base.x,
        y_wrapped - BOX_HEIGHT * 0.5,
        vertex.base.z,
    );

    // Billboard basis facing the camera, with the sky.rs fallback axis for
    // drops directly above or below it.
    let to_camera = normalize(camera.camera_pos.xyz - center);
    var up = vec3<f32>(0.0, 1.0, 0.0);
    if (abs(dot(to_camera, up)) > 0.99) {
        up = vec3<f32>(1.0, 0.0, 0.0);
    }
    let right = normalize(cross(up, to_camera));
    let billboard_up = cross(to_camera, right);

    // Kind + stretch: rain is a narrow tall streak, snowfall a small flake.
    let offset = right * (vertex.corner.x * vertex.size)
        + billboard_up * (vertex.corner.y * vertex.size * stretch);
    let world_pos = center + offset;

    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = vec4<f32>(
        weather.tint.rgb * vertex.color.rgb,
        vertex.color.a * weather.tint.a,
    );
    out.corner = vertex.corner;
    out.kind = f32(vertex.kind);
    out.cam_dist = distance(world_pos, camera.camera_pos.xyz);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var color = in.color;
    // Snowflakes are soft discs carved out of their quads; rain streaks keep
    // the hard quad silhouette.
    if (in.kind > 0.5) {
        let mask = 1.0 - smoothstep(0.75, 1.0, length(in.corner));
        color.a = color.a * mask;
    }
    // Fog lanes come from the camera uniform exactly like the voxel and water
    // shaders; day_factor (.z) is deliberately untouched.
    let fog_start = camera.fog_start_end.x;
    let fog_end = camera.fog_start_end.y;
    let fog_t = clamp((in.cam_dist - fog_start) / max(fog_end - fog_start, 1.0), 0.0, 1.0);
    color.a = color.a * (1.0 - fog_t * fog_t);
    return color;
}
"#;

/// Bind group layout for the 32-byte [`WeatherUniform`].
pub fn create_weather_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("weather_bgl"),
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

/// Create the weather uniform buffer initialised to the invisible Clear state.
pub fn create_weather_uniform_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    let uniform = WeatherUniform::from_weather(Weather::Clear, 0.0);
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("weather_uniform_buffer"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

pub fn create_weather_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("weather_bg"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    })
}

/// Build the static precipitation vertex buffer (rain segment then snowfall
/// segment) once at renderer construction.
pub fn create_weather_vertex_buffer(device: &wgpu::Device) -> (wgpu::Buffer, u32) {
    use wgpu::util::DeviceExt;
    let vertices = build_weather_vertices();
    let count = vertices.len() as u32;
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("weather_vertex_buffer"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    (buffer, count)
}

/// Weather pipeline: alpha-blended, depth-write-disabled billboards drawn
/// inside the existing main pass (overlay/water pipeline template).
///
/// `LessEqual` against the shared depth buffer lets terrain occlude far drops
/// while near drops render in front of it; never writing depth keeps the
/// selection/hotbar overlay crisp on top.
pub fn create_weather_pipeline(
    ctx: &RenderContext,
    camera_bgl: &wgpu::BindGroupLayout,
    weather_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("weather_shader"),
            source: wgpu::ShaderSource::Wgsl(WEATHER_SHADER_WGSL.into()),
        });

    let layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("weather_pipeline_layout"),
            bind_group_layouts: &[camera_bgl, weather_bgl],
            push_constant_ranges: &[],
        });

    ctx.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("weather_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WeatherVertex>() as wgpu::BufferAddress,
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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_finite(values: impl Iterator<Item = f32>, context: &str) {
        for value in values {
            assert!(value.is_finite(), "non-finite value in {context}");
        }
    }

    #[test]
    fn weather_uniform_is_two_16_byte_aligned_vec4s() {
        assert_eq!(std::mem::size_of::<WeatherUniform>(), 32);
        assert_eq!(std::mem::size_of::<WeatherUniform>() % 16, 0);
        assert_eq!(std::mem::offset_of!(WeatherUniform, params), 0);
        assert_eq!(std::mem::offset_of!(WeatherUniform, tint), 16);
    }

    #[test]
    fn weather_vertex_layout_is_pod_and_stable() {
        assert_eq!(std::mem::size_of::<WeatherVertex>(), 44);
        assert_eq!(std::mem::offset_of!(WeatherVertex, base), 0);
        assert_eq!(std::mem::offset_of!(WeatherVertex, corner), 12);
        assert_eq!(std::mem::offset_of!(WeatherVertex, size), 20);
        assert_eq!(std::mem::offset_of!(WeatherVertex, color), 24);
        assert_eq!(std::mem::offset_of!(WeatherVertex, kind), 40);

        let vertex = WeatherVertex {
            base: Vec3::Y.to_array(),
            corner: [0.0; 2],
            size: RAIN_HALF_SIZE,
            color: RAIN_COLOR,
            kind: KIND_RAIN,
        };
        let bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(bytes.len(), 44);
        assert_eq!(
            bytemuck::from_bytes::<WeatherVertex>(bytes).kind,
            KIND_RAIN,
            "WeatherVertex must round-trip through Pod casts"
        );
    }

    #[test]
    fn weather_consts_hold_their_sanity_invariants() {
        // MEM183: top-level `const { assert!(..) }` is rejected by rustc 1.99,
        // so compile-time invariants live inside a function body.
        const {
            assert!(WEATHER_BOX_WIDTH > 0.0, "box width must be positive");
            assert!(WEATHER_BOX_HEIGHT > 0.0, "box height must be positive");
            assert!(WEATHER_BOX_DEPTH > 0.0, "box depth must be positive");
            assert!(WEATHER_DROP_COUNT > 0, "the pattern must not be empty");
            assert!(KIND_RAIN != KIND_SNOWFLAKE, "kind tags must be distinct");
            assert!(
                RAIN_FALL_SPEED > SNOW_FALL_SPEED,
                "rain must fall faster than snow"
            );
            assert!(RAIN_STRETCH > SNOW_STRETCH, "rain streaks must be taller");
            assert!(RAIN_HALF_SIZE > 0.0 && SNOW_HALF_SIZE > 0.0);
            assert!(RAIN_COLOR[3] > 0.0 && SNOW_COLOR[3] > 0.0);
            assert!(
                WEATHER_VERTEX_COUNT == 2 * RAIN_VERTEX_COUNT,
                "both segments have the same drop count"
            );
        }
    }

    #[test]
    fn build_weather_vertices_is_deterministic() {
        let first = build_weather_vertices();
        let second = build_weather_vertices();
        assert_eq!(first.len(), second.len());
        assert_eq!(
            bytemuck::cast_slice::<WeatherVertex, u8>(&first),
            bytemuck::cast_slice::<WeatherVertex, u8>(&second),
            "the fixed-seed pattern must be byte-identical across calls"
        );
    }

    #[test]
    fn build_weather_vertices_is_non_empty_and_segmented() {
        let verts = build_weather_vertices();
        assert_eq!(verts.len(), WEATHER_VERTEX_COUNT as usize);
        assert_eq!(
            verts.iter().filter(|v| v.kind == KIND_RAIN).count(),
            RAIN_VERTEX_COUNT as usize
        );
        assert_eq!(
            verts.iter().filter(|v| v.kind == KIND_SNOWFLAKE).count(),
            SNOW_VERTEX_COUNT as usize
        );
        assert!(verts[..RAIN_VERTEX_COUNT as usize]
            .iter()
            .all(|v| v.kind == KIND_RAIN));
        assert!(verts[RAIN_VERTEX_COUNT as usize..]
            .iter()
            .all(|v| v.kind == KIND_SNOWFLAKE));
    }

    #[test]
    fn build_weather_vertices_stays_inside_the_camera_relative_box() {
        let verts = build_weather_vertices();
        let half_w = WEATHER_BOX_WIDTH * 0.5;
        let half_d = WEATHER_BOX_DEPTH * 0.5;
        for (i, v) in verts.iter().enumerate() {
            assert!(
                (-half_w..=half_w).contains(&v.base[0]),
                "drop {i} x out of box: {}",
                v.base[0]
            );
            assert!(
                (0.0..=WEATHER_BOX_HEIGHT).contains(&v.base[1]),
                "drop {i} y out of box: {}",
                v.base[1]
            );
            assert!(
                (-half_d..=half_d).contains(&v.base[2]),
                "drop {i} z out of box: {}",
                v.base[2]
            );
            assert!(
                QUAD_CORNERS.contains(&v.corner),
                "drop {i} uses the shared quad corners"
            );
            assert!(v.size > 0.0, "drop {i} needs a positive size");
            assert!(
                (0.0..=1.0).contains(&v.color[3]),
                "drop {i} alpha out of range: {}",
                v.color[3]
            );
            assert!(
                v.kind == KIND_RAIN || v.kind == KIND_SNOWFLAKE,
                "drop {i} has an unknown kind"
            );
            assert_finite(
                v.base
                    .into_iter()
                    .chain(v.corner)
                    .chain([v.size])
                    .chain(v.color),
                "WeatherVertex",
            );
        }
    }

    #[test]
    fn per_kind_vertices_use_their_own_size() {
        let verts = build_weather_vertices();
        for v in &verts[..RAIN_VERTEX_COUNT as usize] {
            assert_eq!(v.size, RAIN_HALF_SIZE);
        }
        for v in &verts[RAIN_VERTEX_COUNT as usize..] {
            assert_eq!(v.size, SNOW_HALF_SIZE);
        }
    }

    #[test]
    fn weather_uniform_derives_kind_speed_stretch_and_tint() {
        let rain = WeatherUniform::from_weather(Weather::Rain, 123.5);
        assert_eq!(rain.params[0], KIND_RAIN as f32);
        assert_eq!(rain.params[1], 123.5);
        assert_eq!(rain.params[2], RAIN_FALL_SPEED);
        assert_eq!(rain.params[3], RAIN_STRETCH);
        assert_eq!(rain.tint, RAIN_COLOR);

        let snow = WeatherUniform::from_weather(Weather::Snowfall, 9.0);
        assert_eq!(snow.params[0], KIND_SNOWFLAKE as f32);
        assert_eq!(snow.params[1], 9.0);
        assert_eq!(snow.params[2], SNOW_FALL_SPEED);
        assert_eq!(snow.params[3], SNOW_STRETCH);
        assert_eq!(snow.tint, SNOW_COLOR);

        assert!(
            snow.params[2] < rain.params[2],
            "snowfall must fall slower than rain"
        );
        assert!(
            rain.params[3] > snow.params[3],
            "rain must be stretched into streaks"
        );
    }

    #[test]
    fn clear_weather_is_invisible_even_if_it_is_drawn() {
        let clear = WeatherUniform::from_weather(Weather::Clear, 42.0);
        assert_eq!(clear.tint[3], 0.0, "Clear must never paint a drop");
        assert_eq!(weather_vertex_range(Weather::Clear), None);
    }

    #[test]
    fn weather_uniform_is_finite_across_a_long_phase_sweep() {
        for phase in [0.0, 0.5, 24_000.0, 96_000.0, 1_000_000.0] {
            for weather in [Weather::Clear, Weather::Rain, Weather::Snowfall] {
                let u = WeatherUniform::from_weather(weather, phase);
                assert_finite(u.params.into_iter().chain(u.tint), "WeatherUniform");
            }
        }
    }

    #[test]
    fn weather_vertex_range_covers_both_segments() {
        assert_eq!(
            weather_vertex_range(Weather::Rain),
            Some((0, RAIN_VERTEX_COUNT))
        );
        assert_eq!(
            weather_vertex_range(Weather::Snowfall),
            Some((RAIN_VERTEX_COUNT, SNOW_VERTEX_COUNT))
        );
        let (_, rain_count) = weather_vertex_range(Weather::Rain).unwrap();
        let (base, snow_count) = weather_vertex_range(Weather::Snowfall).unwrap();
        assert_eq!(base + snow_count, WEATHER_VERTEX_COUNT);
        assert_eq!(rain_count + snow_count, WEATHER_VERTEX_COUNT);
    }

    #[test]
    fn weather_shader_positions_billboards_camera_relative() {
        assert!(
            WEATHER_SHADER_WGSL.contains("camera.camera_pos.xyz + vec3<f32>("),
            "drops must be placed relative to the camera, never in world space"
        );
        assert!(WEATHER_SHADER_WGSL.contains("camera.view_proj * vec4<f32>(world_pos, 1.0)"));
        assert!(
            WEATHER_SHADER_WGSL.contains("abs(dot(to_camera, up)) > 0.99"),
            "degenerate pole directions need a fallback billboard axis"
        );
        assert!(WEATHER_SHADER_WGSL.contains("let world_pos = center + offset;"));
    }

    #[test]
    fn weather_shader_wraps_the_fall_modulo_the_box_height() {
        assert!(
            WEATHER_SHADER_WGSL.contains("let y_unwrapped = vertex.base.y - fall_speed * phase;"),
            "fall must be a pure function of the clock phase"
        );
        assert!(
            WEATHER_SHADER_WGSL.contains(
                "let y_wrapped = y_unwrapped - floor(y_unwrapped / BOX_HEIGHT) * BOX_HEIGHT;"
            ),
            "the drop height must wrap inside the box instead of leaving it"
        );
        assert!(WEATHER_SHADER_WGSL.contains("const BOX_HEIGHT: f32 = 24.0;"));

        // Keep the WGSL wrap period in lock-step with the Rust box constant.
        let height_literal = format!("const BOX_HEIGHT: f32 = {:.1};", WEATHER_BOX_HEIGHT);
        assert!(
            WEATHER_SHADER_WGSL.contains(&height_literal),
            "WGSL BOX_HEIGHT must match WEATHER_BOX_HEIGHT"
        );
    }

    #[test]
    fn weather_shader_reads_the_camera_fog_lanes_and_leaves_day_factor_alone() {
        assert!(WEATHER_SHADER_WGSL.contains("let fog_start = camera.fog_start_end.x;"));
        assert!(WEATHER_SHADER_WGSL.contains("let fog_end = camera.fog_start_end.y;"));
        assert!(
            WEATHER_SHADER_WGSL.contains("color.a = color.a * (1.0 - fog_t * fog_t);"),
            "distant drops must fade into the fog"
        );
        assert!(
            !WEATHER_SHADER_WGSL.contains("fog_start_end.z"),
            "the day factor lane belongs to the palette path and must stay untouched"
        );
    }

    #[test]
    fn weather_shader_switches_streak_and_flake_by_kind() {
        // The uniform's kind selects the live pattern segment.
        assert!(WEATHER_SHADER_WGSL.contains("if (f32(vertex.kind) != kind)"));
        // Speed and stretch are kind-derived parameters from Rust.
        assert!(WEATHER_SHADER_WGSL.contains("let fall_speed = weather.params.z;"));
        assert!(WEATHER_SHADER_WGSL.contains("let stretch = weather.params.w;"));
        assert!(WEATHER_SHADER_WGSL.contains("vertex.size * stretch"));
        // Snowflakes get a soft circular mask; rain keeps its quad silhouette.
        assert!(WEATHER_SHADER_WGSL.contains("smoothstep(0.75, 1.0, length(in.corner))"));
        assert!(WEATHER_SHADER_WGSL.contains("if (in.kind > 0.5)"));
    }

    #[test]
    fn weather_shader_multiplies_the_kind_tint_by_per_drop_alpha() {
        assert!(
            WEATHER_SHADER_WGSL.contains("weather.tint.rgb * vertex.color.rgb"),
            "the uniform tint carries the kind color"
        );
        assert!(
            WEATHER_SHADER_WGSL.contains("vertex.color.a * weather.tint.a"),
            "per-drop alpha variation must combine with the kind alpha"
        );
    }
}
