use bytemuck::{Pod, Zeroable};
use glam::{IVec3, Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::mesh::quad::PackedQuad;
use crate::player::camera::{visible_face_mask, Camera, Frustum};
use crate::render::buffer::ChunkRenderStore;
use crate::render::context::{RenderContext, DEPTH_FORMAT};
use crate::render::texture::create_block_texture_array;
use crate::world::chunk::CHUNK_SIZE;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    pub fog_color: [f32; 4],
    pub fog_start_end: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct OverlayVertex {
    pub clip_pos: [f32; 4],
    pub color: [f32; 4],
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

    let ao00 = ao_packed & 3u;
    let ao10 = (ao_packed >> 2u) & 3u;
    let ao01 = (ao_packed >> 4u) & 3u;
    let ao11 = (ao_packed >> 6u) & 3u;

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
    if corner == 1u {
        ao_val = ao10;
    } else if corner == 2u {
        ao_val = ao01;
    } else if corner == 3u {
        ao_val = ao11;
    }
    let ao_factor = 0.45 + (f32(ao_val) / 3.0) * 0.55;

    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = uv;
    out.tex_layer = tex_layer;
    out.light = face_sun_factor(face_dir) * ao_factor;
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

pub fn build_overlay_line_vertices(
    view_proj: &Mat4,
    aspect: f32,
    target_block: Option<IVec3>,
    selected_slot: usize,
) -> Vec<OverlayVertex> {
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

    // 3. 9-Slot Block Hotbar at bottom of screen
    let slot_w = 0.09 / aspect.max(0.5);
    let slot_h = 0.085;
    let gap = 0.012 / aspect.max(0.5);
    let total_w = 9.0 * slot_w + 8.0 * gap;
    let start_x = -total_w * 0.5;
    let y0 = -0.92;
    let y1 = y0 + slot_h;

    for i in 0..9 {
        let x0 = start_x + i as f32 * (slot_w + gap);
        let x1 = x0 + slot_w;
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
    pub voxel_pipeline: wgpu::RenderPipeline,
    pub overlay_pipeline: wgpu::RenderPipeline,
    pub camera_buffer: wgpu::Buffer,
    pub camera_bind_group: wgpu::BindGroup,
    pub chunk_bind_group_layout: wgpu::BindGroupLayout,
}

impl VoxelRenderer {
    pub fn new(ctx: &RenderContext) -> Self {
        let (tex_view, sampler) = create_block_texture_array(&ctx.device, &ctx.queue);

        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0; 4],
            fog_color: [0.53, 0.76, 0.96, 1.0],
            fog_start_end: [300.0, 512.0, 0.0, 0.0],
        };

        let camera_buffer = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("camera_uniform_buffer"),
                contents: bytemuck::bytes_of(&camera_uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let camera_bgl =
            ctx.device
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

        let voxel_shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("voxel_shader"),
                source: wgpu::ShaderSource::Wgsl(VOXEL_SHADER_WGSL.into()),
            });

        let voxel_layout =
            ctx.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("voxel_pipeline_layout"),
                    bind_group_layouts: &[&camera_bgl, &chunk_bind_group_layout],
                    push_constant_ranges: &[],
                });

        let voxel_pipeline =
            ctx.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("voxel_pipeline"),
                    layout: Some(&voxel_layout),
                    vertex: wgpu::VertexState {
                        module: &voxel_shader,
                        entry_point: "vs_main",
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<PackedQuad>() as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &[wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Uint32x2,
                            }],
                        }],
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

        let overlay_shader =
            ctx.device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("overlay_shader"),
                    source: wgpu::ShaderSource::Wgsl(OVERLAY_SHADER_WGSL.into()),
                });

        let overlay_layout =
            ctx.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("overlay_pipeline_layout"),
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

        let overlay_pipeline =
            ctx.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("overlay_pipeline"),
                    layout: Some(&overlay_layout),
                    vertex: wgpu::VertexState {
                        module: &overlay_shader,
                        entry_point: "vs_main",
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<OverlayVertex>()
                                as wgpu::BufferAddress,
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
            voxel_pipeline,
            overlay_pipeline,
            camera_buffer,
            camera_bind_group,
            chunk_bind_group_layout,
        }
    }

    pub fn render_frame(
        &self,
        ctx: &RenderContext,
        store: &ChunkRenderStore,
        camera: &Camera,
        render_distance: i32,
        target_block: Option<IVec3>,
        selected_slot: usize,
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
        let cam_uniform = CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            camera_pos: [camera.position.x, camera.position.y, camera.position.z, 1.0],
            fog_color: [0.53, 0.76, 0.96, 1.0],
            fog_start_end: [max_dist * 0.65, max_dist, 0.0, 0.0],
        };
        ctx.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam_uniform));

        let overlay_verts =
            build_overlay_line_vertices(&vp, aspect, target_block, selected_slot);
        let overlay_buf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay_vbuf"),
                contents: bytemuck::cast_slice(&overlay_verts),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut encoder =
            ctx.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("frame_encoder"),
                });

        let mut visible_chunks = 0usize;
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.53,
                            g: 0.76,
                            b: 0.96,
                            a: 1.0,
                        }),
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
    fn test_overlay_vertices_include_wireframe_crosshair_and_hotbar() {
        let verts = build_overlay_line_vertices(
            &Mat4::IDENTITY,
            1.77,
            Some(IVec3::new(1, 2, 3)),
            0,
        );
        // 24 (wireframe) + 4 (crosshair) + 9 * 8 (hotbar) = 100 vertices
        assert_eq!(verts.len(), 100);
    }
}
