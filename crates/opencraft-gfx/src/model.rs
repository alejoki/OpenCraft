//! Neutral model vertex ABI and single-camera-bind-group pipeline factory.
//!
//! The host owns every piece of content identity (box tables, animation,
//! lighting, culling and batch assembly); this module owns only the
//! device-facing part: the pinned 32-byte vertex layout, the WGSL text and the
//! render-pipeline factory that consumes one camera bind group. The vertex
//! struct is a plain public data carrier so the host can build batches field
//! by field without constructors.

use crate::context::{RenderContext, DEPTH_FORMAT};

/// Bytes in one [`ModelVertex`]. Pinned so the WGSL layout cannot drift.
pub const MODEL_VERTEX_SIZE: usize = 32;

/// The five vertex attributes of [`ModelVertex`], in shader-location order.
/// Offsets are 0 / 12 / 20 / 24 / 28 and depend on the `#[repr(C)]` field
/// order below; a test pins every value.
pub const MODEL_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x2,
    2 => Uint32,
    3 => Float32,
    4 => Float32
];

/// Bytes in the camera uniform the model shader reads: seven 16-byte vec4s.
///
/// The host camera uniform must stay byte-compatible with the WGSL
/// `CameraUniform` struct: one 64-byte view-projection matrix plus
/// `camera_pos`, `fog_color` and `fog_start_end`.
pub const MODEL_CAMERA_UNIFORM_BYTES: usize = 112;

/// One model geometry vertex.
///
/// `shade` carries the face-shading constant multiplied by the per-instance
/// light factor on the CPU, so world light needs no extra attribute. `flash`
/// is the hurt tint (0 normal, 1 fully red). Distance fog is derived from the
/// shared camera uniform in the vertex shader, so it needs no attribute
/// either.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub tex_layer: u32,
    pub shade: f32,
    pub flash: f32,
}

/// The model vertex buffer layout (stride 32, one vertex per step).
pub fn model_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: MODEL_VERTEX_SIZE as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MODEL_VERTEX_ATTRIBUTES,
    }
}

/// Model shader (WGSL): one batched, textured box-model pass.
///
/// The vertex stage consumes the pinned [`ModelVertex`] attributes, applies
/// the shared camera view-projection, and derives the distance fog factor from
/// the host camera uniform (`camera_pos` plus `fog_start_end.xy`) so the
/// 112-byte uniform layout is untouched. The fragment stage samples the
/// 256-layer block texture array from the camera bind group (`uv`, flat
/// `tex_layer`), multiplies by the CPU-baked face shade, applies the hurt
/// flash tint, and fades into `fog_color`.
///
/// Group 0 is the existing camera bind group: binding 0 uniform, binding 1
/// `texture_2d_array<f32>`, binding 2 sampler. There is deliberately no group
/// 1: the whole scene draws with a single bind group and a single draw call.
pub const MODEL_SHADER_WGSL: &str = r#"
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

/// Create the single batched model pipeline.
///
/// The pipeline layout is `&[camera_bgl]` ONLY: the model reuses the existing
/// camera uniform + block texture array + sampler bind group, so there is no
/// per-instance bind group, no chunk uniform, and no second draw call per
/// model. The pipeline is opaque (REPLACE blending) and writes depth with
/// `LessEqual`, so opaque terrain occludes it and it occludes the translucent
/// water drawn after it.
pub fn create_model_pipeline(
    ctx: &RenderContext,
    camera_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model_shader"),
            source: wgpu::ShaderSource::Wgsl(MODEL_SHADER_WGSL.into()),
        });

    let layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model_pipeline_layout"),
            bind_group_layouts: &[camera_bgl],
            push_constant_ranges: &[],
        });

    ctx.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("model_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[model_vertex_layout()],
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
                // Box cuboids are emitted with outward CCW winding, but the
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_vertex_is_32_bytes_with_the_pinned_attribute_offsets() {
        assert_eq!(std::mem::size_of::<ModelVertex>(), MODEL_VERTEX_SIZE);
        assert_eq!(MODEL_VERTEX_SIZE, 32);

        let expected = [
            (wgpu::VertexFormat::Float32x3, 0u64, 0u32),
            (wgpu::VertexFormat::Float32x2, 12, 1),
            (wgpu::VertexFormat::Uint32, 20, 2),
            (wgpu::VertexFormat::Float32, 24, 3),
            (wgpu::VertexFormat::Float32, 28, 4),
        ];
        for (index, (format, offset, location)) in expected.iter().enumerate() {
            let attr = MODEL_VERTEX_ATTRIBUTES[index];
            assert_eq!(attr.format, *format, "attribute {index} format");
            assert_eq!(attr.offset, *offset, "attribute {index} offset");
            assert_eq!(
                attr.shader_location, *location,
                "attribute {index} location"
            );
        }
    }

    #[test]
    fn model_vertex_layout_is_one_32_byte_vertex_per_step() {
        let layout = model_vertex_layout();
        assert_eq!(layout.array_stride, MODEL_VERTEX_SIZE as u64);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Vertex);
        assert_eq!(layout.attributes.len(), 5);
        assert_eq!(layout.attributes, &MODEL_VERTEX_ATTRIBUTES);
    }

    #[test]
    fn model_pipeline_is_opaque_and_camera_only() {
        // Source pin on the model pipeline factory: opaque REPLACE blending,
        // depth writes with LessEqual, and a layout that carries the camera
        // bind group ONLY (no chunk uniform, no per-instance bind group).
        let src = include_str!("model.rs");
        let code = &src[..src.find("#[cfg(test)]").expect("test module marker")];
        assert!(
            code.contains("depth_write_enabled: true"),
            "the model must write depth so water behind it is occluded"
        );
        assert!(code.contains("CompareFunction::LessEqual"));
        assert!(
            code.contains("BlendState::REPLACE"),
            "the model is opaque; no alpha blending"
        );
        assert!(
            code.contains("bind_group_layouts: &[camera_bgl]"),
            "the model pipeline layout must be the camera bind group alone"
        );
        assert!(
            code.contains("buffers: &[model_vertex_layout()]"),
            "the model pipeline must consume the pinned 32-byte vertex layout"
        );
        assert!(
            !code.contains("chunk_bind_group") && !code.contains("@group(1)"),
            "the model must not bind a chunk uniform or a second group"
        );
        // Shader contract: shared camera view-projection, array sample by flat
        // layer, shade multiply, hurt flash tint, distance fog.
        assert!(code.contains("camera.view_proj * vec4<f32>(position, 1.0)"));
        assert!(code.contains("camera.fog_start_end.x") && code.contains("camera.fog_start_end.y"));
        assert!(
            code.contains("textureSample(block_texture, block_sampler, in.uv, i32(in.tex_layer))")
        );
        assert!(code.contains("texel.rgb * in.shade"));
        assert!(code.contains("vec3<f32>(1.0, 0.15, 0.1), in.flash"));
        assert!(code.contains("mix(rgb, camera.fog_color.rgb, in.fog)"));
    }

    #[test]
    fn model_camera_uniform_contract_is_seven_vec4s() {
        assert_eq!(MODEL_CAMERA_UNIFORM_BYTES, 112);
        assert_eq!(MODEL_CAMERA_UNIFORM_BYTES % 16, 0);
        assert_eq!(
            MODEL_CAMERA_UNIFORM_BYTES,
            7 * std::mem::size_of::<[f32; 4]>()
        );
    }
}
