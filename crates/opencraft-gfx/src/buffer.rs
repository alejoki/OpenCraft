use rustc_hash::FxHashMap;
use std::ops::Range;
use wgpu::util::DeviceExt;

use crate::mesh::quad::{ChunkMesh, PackedQuad};
use opencraft_core::chunk::{ChunkPos, CHUNK_SIZE};

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

/// Crossed-plant vertex-buffer bytes for a mesh; zero when the mesh has no
/// cross/crop geometry.
pub fn cross_buffer_bytes(mesh: &ChunkMesh) -> usize {
    mesh.cross_quad_count() * PACKED_QUAD_BYTES
}

/// Alpha-blended cube-face vertex-buffer bytes (glass); zero when the mesh has
/// no blend geometry.
pub fn blend_buffer_bytes(mesh: &ChunkMesh) -> usize {
    mesh.blend_quad_count() * PACKED_QUAD_BYTES
}

/// Total GPU vertex bytes a chunk's mesh occupies: opaque, water, cross and
/// blend. This is what `GpuChunk::byte_size` (and therefore
/// `total_vram_bytes`) must count so the title-bar VRAM readout stays honest.
pub fn chunk_vram_bytes(mesh: &ChunkMesh) -> usize {
    opaque_buffer_bytes(mesh)
        + water_buffer_bytes(mesh)
        + cross_buffer_bytes(mesh)
        + blend_buffer_bytes(mesh)
}

/// Vertex range covered by the water quads: six vertices per quad. The water
/// buffer is non-indexed, so the translucent draw passes this range as its
/// vertex range. Pure math so headless tests can pin it without a `Device`.
pub fn water_vertex_range(mesh: &ChunkMesh) -> Range<u32> {
    0..(mesh.water_quad_count() as u32) * QUAD_VERTICES
}

/// Vertex range covered by the crossed-plant quads: six vertices per quad.
pub fn cross_vertex_range(mesh: &ChunkMesh) -> Range<u32> {
    0..(mesh.cross_quad_count() as u32) * QUAD_VERTICES
}

/// Vertex range covered by the alpha-blended cube faces: six vertices per quad.
pub fn blend_vertex_range(mesh: &ChunkMesh) -> Range<u32> {
    0..(mesh.blend_quad_count() as u32) * QUAD_VERTICES
}

pub struct GpuChunk {
    pub vertex_buffer: wgpu::Buffer,
    /// Translucent water geometry, intentionally a separate buffer from
    /// `vertex_buffer` so water can never enter the opaque draw path (D070).
    /// `None` when the mesh carries no water.
    pub water_vertex_buffer: Option<wgpu::Buffer>,
    /// Crossed-plant geometry (`face_dir` 6/7), drawn with the voxel pipeline
    /// between the mob batch and the translucent stage. `None` when the mesh
    /// carries no cross/crop quads.
    pub cross_vertex_buffer: Option<wgpu::Buffer>,
    /// Alpha-blended cube faces (glass), drawn with the blend pipeline in the
    /// sorted translucent stage. `None` when the mesh carries no blend quads.
    pub blend_vertex_buffer: Option<wgpu::Buffer>,
    pub bind_group: wgpu::BindGroup,
    pub bucket_ranges: [Range<u32>; 6],
    pub pos: ChunkPos,
    pub quad_count: usize,
    pub water_quad_count: usize,
    pub cross_quad_count: usize,
    pub blend_quad_count: usize,
    pub byte_size: usize,
}

#[derive(Default)]
pub struct ChunkRenderStore {
    pub chunks: FxHashMap<ChunkPos, GpuChunk>,
    pub total_vram_bytes: usize,
    /// Opaque quads only; water/cross/blend are tracked separately.
    pub total_quads: usize,
    pub total_water_quads: usize,
    pub total_cross_quads: usize,
    pub total_blend_quads: usize,
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
            self.total_cross_quads = self.total_cross_quads.saturating_sub(old.cross_quad_count);
            self.total_blend_quads = self.total_blend_quads.saturating_sub(old.blend_quad_count);
        }
    }

    /// Remove every chunk (dropping its wgpu buffers) and zero all
    /// totals. The game's session-swap boundary calls this so a freshly
    /// loaded world can never render stale GPU meshes from the outgoing
    /// world. The populated-clear proof rides on the windowed launch smoke
    /// test and human UAT: constructing a real `GpuChunk` needs a wgpu
    /// `Device`, which headless lib tests do not provide (honest limitation
    /// — the tests below cover the empty-store and missing-pos contracts).
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.total_vram_bytes = 0;
        self.total_quads = 0;
        self.total_water_quads = 0;
        self.total_cross_quads = 0;
        self.total_blend_quads = 0;
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
        let cross_count = mesh.cross_quad_count();
        let blend_count = mesh.blend_quad_count();
        if total == 0 && water_count == 0 && cross_count == 0 && blend_count == 0 {
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

        let cross_vertex_buffer = if cross_count == 0 {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("chunk_cross_buffer"),
                    contents: bytemuck::cast_slice(&mesh.cross_quads),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            )
        };

        let blend_vertex_buffer = if blend_count == 0 {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("chunk_blend_buffer"),
                    contents: bytemuck::cast_slice(&mesh.blend_quads),
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
        self.total_cross_quads += cross_count;
        self.total_blend_quads += blend_count;

        self.chunks.insert(
            pos,
            GpuChunk {
                vertex_buffer,
                water_vertex_buffer,
                cross_vertex_buffer,
                blend_vertex_buffer,
                bind_group,
                bucket_ranges,
                pos,
                quad_count: total,
                water_quad_count: water_count,
                cross_quad_count: cross_count,
                blend_quad_count: blend_count,
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
        // GpuChunks dropping to zero totals) rides on the windowed launch
        // smoke test and human UAT — constructing a real GpuChunk needs a
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

    /// Synthetic mesh carrying every quad class, so one fixture can pin all
    /// four buffers independently. Cross plants emit `face_dir` 6/7 and blend
    /// faces are plain cube quads, matching what the mesher produces.
    fn synthetic_full_mesh(
        opaque_per_bucket: usize,
        water: usize,
        cross: usize,
        blend: usize,
    ) -> ChunkMesh {
        let mut mesh = synthetic_mesh(opaque_per_bucket, water);
        for n in 0..cross {
            mesh.cross_quads.push(PackedQuad::new(
                [(n % 32) as u32, 5, 6],
                [1, 1],
                6 + (n as u32 % 2),
                19,
                0xFF,
                0xFFFF,
            ));
        }
        for n in 0..blend {
            mesh.blend_quads.push(PackedQuad::new(
                [(n % 32) as u32, 7, 8],
                [1, 1],
                (n as u32) % 6,
                25,
                0xFF,
                0xFFFF,
            ));
        }
        mesh
    }

    #[test]
    fn cross_and_blend_buffer_sizing_and_vertex_ranges() {
        // Cross-only chunk: the opaque upload stays empty while the cross
        // buffer gets its own size and vertex range.
        let cross_only = synthetic_full_mesh(0, 0, 2, 0);
        assert_eq!(cross_only.total_quads(), 0);
        assert_eq!(cross_only.cross_quad_count(), 2);
        assert_eq!(cross_buffer_bytes(&cross_only), 16);
        assert_eq!(cross_vertex_range(&cross_only), 0..12);
        assert_eq!(blend_buffer_bytes(&cross_only), 0);
        assert_eq!(blend_vertex_range(&cross_only), 0..0);
        assert_eq!(chunk_vram_bytes(&cross_only), 16);

        // Blend-only chunk (glass): same rule, separate buffer.
        let blend_only = synthetic_full_mesh(0, 0, 0, 3);
        assert_eq!(blend_only.total_quads(), 0);
        assert_eq!(blend_only.blend_quad_count(), 3);
        assert_eq!(blend_buffer_bytes(&blend_only), 24);
        assert_eq!(blend_vertex_range(&blend_only), 0..18);
        assert_eq!(cross_buffer_bytes(&blend_only), 0);
        assert_eq!(chunk_vram_bytes(&blend_only), 24);

        // Empty mesh: every buffer is zero-sized and no range is non-empty.
        let empty = synthetic_full_mesh(0, 0, 0, 0);
        for bytes in [
            opaque_buffer_bytes(&empty),
            water_buffer_bytes(&empty),
            cross_buffer_bytes(&empty),
            blend_buffer_bytes(&empty),
            chunk_vram_bytes(&empty),
        ] {
            assert_eq!(bytes, 0);
        }
        assert_eq!(cross_vertex_range(&empty), 0..0);
        assert_eq!(blend_vertex_range(&empty), 0..0);
    }

    #[test]
    fn mixed_mesh_counts_every_class_and_sums_its_bytes_into_vram() {
        let mixed = synthetic_full_mesh(1, 2, 3, 4);
        assert_eq!(mixed.total_quads(), 6, "six buckets x one opaque quad");
        assert_eq!(mixed.water_quad_count(), 2);
        assert_eq!(mixed.cross_quad_count(), 3);
        assert_eq!(mixed.blend_quad_count(), 4);
        assert_eq!(mixed.total_quads_including_water(), 15);

        assert_eq!(opaque_buffer_bytes(&mixed), 48);
        assert_eq!(water_buffer_bytes(&mixed), 16);
        assert_eq!(cross_buffer_bytes(&mixed), 24);
        assert_eq!(blend_buffer_bytes(&mixed), 32);
        assert_eq!(chunk_vram_bytes(&mixed), 48 + 16 + 24 + 32);
        assert_eq!(cross_vertex_range(&mixed), 0..18);
        assert_eq!(blend_vertex_range(&mixed), 0..24);
    }

    #[test]
    fn water_buffer_sizing_and_vertex_range_for_water_only_opaque_only_and_mixed_meshes() {
        assert_eq!(
            std::mem::size_of::<PackedQuad>(),
            8,
            "PackedQuad stays two u32 words"
        );

        // Water-only chunk contract: the opaque buffer reserves zero bytes
        // while the filled water buffer gets its own size/range.
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
    fn clear_zeroes_every_quad_counter() {
        let mut store = ChunkRenderStore::new();
        store.total_quads = 2;
        store.total_water_quads = 4;
        store.total_cross_quads = 6;
        store.total_blend_quads = 8;
        store.total_vram_bytes = 160;

        store.clear();

        assert!(store.chunks.is_empty());
        assert_eq!(store.total_vram_bytes, 0);
        assert_eq!(store.total_quads, 0);
        assert_eq!(store.total_water_quads, 0);
        assert_eq!(store.total_cross_quads, 0);
        assert_eq!(store.total_blend_quads, 0);
    }

    #[test]
    fn remove_chunk_of_a_missing_pos_leaves_every_counter_untouched() {
        let mut store = ChunkRenderStore::new();
        store.total_quads = 2;
        store.total_water_quads = 4;
        store.total_cross_quads = 6;
        store.total_blend_quads = 8;
        store.total_vram_bytes = 160;

        store.remove_chunk(ChunkPos { x: -9, y: 1, z: 5 });

        assert!(store.chunks.is_empty());
        assert_eq!(store.total_vram_bytes, 160);
        assert_eq!(store.total_quads, 2);
        assert_eq!(store.total_water_quads, 4);
        assert_eq!(store.total_cross_quads, 6);
        assert_eq!(store.total_blend_quads, 8);
    }
}
