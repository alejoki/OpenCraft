use rustc_hash::FxHashMap;
use std::ops::Range;
use wgpu::util::DeviceExt;

use crate::mesh::quad::{ChunkMesh, PackedQuad};
use crate::world::chunk::{ChunkPos, CHUNK_SIZE};

/// Bytes in one `PackedQuad` vertex.
const PACKED_QUAD_BYTES: usize = std::mem::size_of::<PackedQuad>();
/// Vertices emitted per quad (two triangles) by the voxel/water vertex shader.
pub const QUAD_VERTICES: u32 = 6;

/// Opaque vertex-buffer bytes for a mesh. Water is excluded by construction
/// (D070): the opaque upload path is sized only from
/// `ChunkMesh::total_quads`, exactly as before the translucent pass existed.
pub fn opaque_buffer_bytes(mesh: &ChunkMesh) -> usize {
    mesh.total_quads() * PACKED_QUAD_BYTES
}

/// Water vertex-buffer bytes for a mesh; zero when the mesh has no water.
pub fn water_buffer_bytes(mesh: &ChunkMesh) -> usize {
    mesh.water_quad_count() * PACKED_QUAD_BYTES
}

/// Total GPU vertex bytes a chunk's mesh occupies: opaque plus water. This is
/// what `GpuChunk::byte_size` (and therefore `total_vram_bytes`) must count so
/// the title-bar VRAM readout stays honest once water is uploaded.
pub fn chunk_vram_bytes(mesh: &ChunkMesh) -> usize {
    opaque_buffer_bytes(mesh) + water_buffer_bytes(mesh)
}

/// Vertex range covered by the water quads: six vertices per quad. The water
/// buffer is non-indexed, so the translucent draw passes this range as its
/// vertex range. Pure math so headless tests can pin it without a `Device`.
pub fn water_vertex_range(mesh: &ChunkMesh) -> Range<u32> {
    0..(mesh.water_quad_count() as u32) * QUAD_VERTICES
}

pub struct GpuChunk {
    pub vertex_buffer: wgpu::Buffer,
    /// Translucent water geometry, intentionally a separate buffer from
    /// `vertex_buffer` so water can never enter the opaque draw path (D070).
    /// `None` when the mesh carries no water.
    pub water_vertex_buffer: Option<wgpu::Buffer>,
    pub bind_group: wgpu::BindGroup,
    pub bucket_ranges: [Range<u32>; 6],
    pub pos: ChunkPos,
    pub quad_count: usize,
    pub water_quad_count: usize,
    pub byte_size: usize,
}

#[derive(Default)]
pub struct ChunkRenderStore {
    pub chunks: FxHashMap<ChunkPos, GpuChunk>,
    pub total_vram_bytes: usize,
    /// Opaque quads only; water is tracked separately in `total_water_quads`.
    pub total_quads: usize,
    pub total_water_quads: usize,
}

impl ChunkRenderStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        if let Some(old) = self.chunks.remove(&pos) {
            self.total_vram_bytes = self.total_vram_bytes.saturating_sub(old.byte_size);
            self.total_quads = self.total_quads.saturating_sub(old.quad_count);
            self.total_water_quads = self.total_water_quads.saturating_sub(old.water_quad_count);
        }
    }

    /// Remove every chunk (dropping its wgpu buffers) and zero all
    /// totals. The session-swap boundary (`GameApp::start_world_session`,
    /// S03-T02) calls this so a freshly loaded world can never render
    /// stale GPU meshes from the outgoing world. The populated-clear
    /// proof rides on the launch smoke script and S04's human UAT:
    /// constructing a real `GpuChunk` needs a wgpu `Device`, which
    /// headless lib tests do not provide (honest limitation — the tests
    /// below cover the empty-store and missing-pos contracts).
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.total_vram_bytes = 0;
        self.total_quads = 0;
        self.total_water_quads = 0;
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
        let water_count = mesh.water_quad_count();
        if total == 0 && water_count == 0 {
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

        // Opaque buffer construction is byte-identical to the pre-water path:
        // sized from `total_quads()` and filled only from the six buckets. A
        // water-only chunk still gets a zero-sized opaque buffer so `GpuChunk`
        // keeps its non-optional opaque handle (the opaque draw skips every
        // empty bucket range).
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chunk_quad_buffer"),
            contents: bytemuck::cast_slice(&all_quads),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let water_vertex_buffer = if water_count == 0 {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("chunk_water_buffer"),
                    contents: bytemuck::cast_slice(&mesh.water_quads),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            )
        };

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

        let byte_size = chunk_vram_bytes(mesh);
        self.total_vram_bytes += byte_size;
        self.total_quads += total;
        self.total_water_quads += water_count;

        self.chunks.insert(
            pos,
            GpuChunk {
                vertex_buffer,
                water_vertex_buffer,
                bind_group,
                bucket_ranges,
                pos,
                quad_count: total,
                water_quad_count: water_count,
                byte_size,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_on_an_empty_store_is_a_no_op_keeping_both_totals_zero() {
        // Headless contract: clear() must leave an empty store empty with
        // both totals zero. The populated-clear proof (a store with real
        // GpuChunks dropping to zero totals) rides on the launch smoke
        // script and S04's human UAT — constructing a real GpuChunk needs a
        // wgpu Device, which headless lib tests do not provide (honest
        // limitation, documented on `clear`).
        let mut store = ChunkRenderStore::new();
        store.clear();
        assert!(store.chunks.is_empty());
        assert_eq!(store.total_vram_bytes, 0);
        assert_eq!(store.total_quads, 0);
    }

    #[test]
    fn remove_chunk_of_a_missing_pos_stays_a_no_op() {
        let mut store = ChunkRenderStore::new();
        store.remove_chunk(ChunkPos { x: 3, y: -1, z: 7 });
        assert!(store.chunks.is_empty());
        assert_eq!(store.total_vram_bytes, 0);
        assert_eq!(store.total_quads, 0);
    }

    fn quad(x: u32, y: u32, z: u32) -> PackedQuad {
        PackedQuad::new([x % 32, y % 32, z % 32], [1, 1], 2, 0, 0, 0)
    }

    /// Synthetic mesh with `opaque_per_bucket` quads in every opaque bucket and
    /// `water` quads in the separate water list. No device required.
    fn synthetic_mesh(opaque_per_bucket: usize, water: usize) -> ChunkMesh {
        let mut mesh = ChunkMesh::new();
        for (i, bucket) in mesh.buckets.iter_mut().enumerate() {
            for n in 0..opaque_per_bucket {
                bucket.push(quad((i + n) as u32, 1, 2));
            }
        }
        for n in 0..water {
            // Water quads carry placeholder layer 0 and never enter a bucket.
            mesh.water_quads.push(PackedQuad::new(
                [(n % 32) as u32, 3, 4],
                [32, 1],
                2,
                0,
                0,
                0,
            ));
        }
        mesh
    }

    #[test]
    fn water_buffer_sizing_and_vertex_range_for_water_only_opaque_only_and_mixed_meshes() {
        assert_eq!(
            std::mem::size_of::<PackedQuad>(),
            8,
            "PackedQuad stays two u32 words"
        );

        // Water-only chunk (S01 contract): the opaque buffer reserves zero
        // bytes while the filled water buffer gets its own size/range.
        let water_only = synthetic_mesh(0, 3);
        assert_eq!(water_only.total_quads(), 0);
        assert_eq!(water_only.water_quad_count(), 3);
        assert_eq!(opaque_buffer_bytes(&water_only), 0);
        assert_eq!(water_buffer_bytes(&water_only), 24);
        assert_eq!(water_vertex_range(&water_only), 0..18);
        assert_eq!(chunk_vram_bytes(&water_only), 24);

        // Opaque-only chunk: no water buffer, byte-identical opaque sizing.
        let opaque_only = synthetic_mesh(1, 0);
        assert_eq!(opaque_only.total_quads(), 6);
        assert_eq!(opaque_buffer_bytes(&opaque_only), 48);
        assert_eq!(water_buffer_bytes(&opaque_only), 0);
        assert_eq!(water_vertex_range(&opaque_only), 0..0);
        assert_eq!(chunk_vram_bytes(&opaque_only), 48);

        // Mixed chunk: both buffers are accounted independently.
        let mixed = synthetic_mesh(1, 4);
        assert_eq!(mixed.total_quads(), 6);
        assert_eq!(mixed.water_quad_count(), 4);
        assert_eq!(opaque_buffer_bytes(&mixed), 48);
        assert_eq!(water_buffer_bytes(&mixed), 32);
        assert_eq!(water_vertex_range(&mixed), 0..24);
        assert_eq!(chunk_vram_bytes(&mixed), 80);
    }

    #[test]
    fn vram_math_counts_water_bytes_while_opaque_totals_stay_water_free() {
        let mixed = synthetic_mesh(2, 5);
        let opaque = mixed.total_quads();
        assert_eq!(opaque, 12, "six buckets x two opaque quads");
        assert_eq!(mixed.water_quad_count(), 5);
        assert_eq!(mixed.total_quads_including_water(), 17);

        // Opaque byte/quad totals exclude water (D070), while VRAM includes it.
        assert_eq!(opaque_buffer_bytes(&mixed), opaque * 8);
        assert_eq!(
            chunk_vram_bytes(&mixed),
            opaque_buffer_bytes(&mixed) + water_buffer_bytes(&mixed)
        );
        assert!(chunk_vram_bytes(&mixed) > opaque_buffer_bytes(&mixed));

        let water_only = synthetic_mesh(0, 2);
        assert_eq!(water_only.total_quads(), 0);
        assert_eq!(opaque_buffer_bytes(&water_only), 0);
        assert_eq!(chunk_vram_bytes(&water_only), 16);
    }

    #[test]
    fn clear_zeroes_total_water_quads() {
        let mut store = ChunkRenderStore::new();
        store.total_quads = 2;
        store.total_water_quads = 4;
        store.total_vram_bytes = 48;

        store.clear();

        assert!(store.chunks.is_empty());
        assert_eq!(store.total_vram_bytes, 0);
        assert_eq!(store.total_quads, 0);
        assert_eq!(store.total_water_quads, 0);
    }

    #[test]
    fn remove_chunk_of_a_missing_pos_leaves_every_counter_untouched() {
        let mut store = ChunkRenderStore::new();
        store.total_quads = 2;
        store.total_water_quads = 4;
        store.total_vram_bytes = 48;

        store.remove_chunk(ChunkPos { x: -9, y: 1, z: 5 });

        assert!(store.chunks.is_empty());
        assert_eq!(store.total_vram_bytes, 48);
        assert_eq!(store.total_quads, 2);
        assert_eq!(store.total_water_quads, 4);
    }
}
