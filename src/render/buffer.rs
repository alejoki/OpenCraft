use rustc_hash::FxHashMap;
use std::ops::Range;
use wgpu::util::DeviceExt;

use crate::mesh::quad::{ChunkMesh, PackedQuad};
use crate::world::chunk::{ChunkPos, CHUNK_SIZE};

pub struct GpuChunk {
    pub vertex_buffer: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
    pub bucket_ranges: [Range<u32>; 6],
    pub pos: ChunkPos,
    pub quad_count: usize,
    pub byte_size: usize,
}

#[derive(Default)]
pub struct ChunkRenderStore {
    pub chunks: FxHashMap<ChunkPos, GpuChunk>,
    pub total_vram_bytes: usize,
    pub total_quads: usize,
}

impl ChunkRenderStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        if let Some(old) = self.chunks.remove(&pos) {
            self.total_vram_bytes = self.total_vram_bytes.saturating_sub(old.byte_size);
            self.total_quads = self.total_quads.saturating_sub(old.quad_count);
        }
    }

    pub fn upload_mesh(
        &mut self,
        device: &wgpu::Device,
        chunk_bgl: &wgpu::BindGroupLayout,
        pos: ChunkPos,
        mesh: &ChunkMesh,
    ) {
        self.remove_chunk(pos);
        let total = mesh.total_quads();
        if total == 0 {
            return;
        }

        let mut all_quads: Vec<PackedQuad> = Vec::with_capacity(total);
        let mut bucket_ranges: [Range<u32>; 6] = [0..0, 0..0, 0..0, 0..0, 0..0, 0..0];

        for (i, bucket) in mesh.buckets.iter().enumerate() {
            let start = all_quads.len() as u32;
            all_quads.extend_from_slice(bucket);
            let end = all_quads.len() as u32;
            bucket_ranges[i] = start..end;
        }

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chunk_quad_buffer"),
            contents: bytemuck::cast_slice(&all_quads),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let origin = [
            (pos.x * CHUNK_SIZE as i32) as f32,
            (pos.y * CHUNK_SIZE as i32) as f32,
            (pos.z * CHUNK_SIZE as i32) as f32,
            0.0f32,
        ];
        let origin_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chunk_origin_buffer"),
            contents: bytemuck::cast_slice(&origin),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chunk_origin_bg"),
            layout: chunk_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: origin_buffer.as_entire_binding(),
            }],
        });

        let byte_size = all_quads.len() * std::mem::size_of::<PackedQuad>();
        self.total_vram_bytes += byte_size;
        self.total_quads += total;

        self.chunks.insert(
            pos,
            GpuChunk {
                vertex_buffer,
                bind_group,
                bucket_ranges,
                pos,
                quad_count: total,
                byte_size,
            },
        );
    }
}
