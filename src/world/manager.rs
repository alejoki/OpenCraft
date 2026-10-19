use crossbeam_channel::{unbounded, Receiver, Sender};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::mesh::greedy::{mesh_chunk, PaddedChunk};
use crate::mesh::quad::ChunkMesh;
use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, world_to_chunk_and_local, Chunk, ChunkPos, CHUNK_SIZE};
use crate::world::persistence::ChunkEditMap;
use crate::world::terrain::{generate_chunk_seeded, DEFAULT_WORLD_SEED};

pub const MIN_CHUNK_Y: i32 = -1;
pub const MAX_CHUNK_Y: i32 = 4;

pub struct WorldManager {
    chunks: FxHashMap<ChunkPos, Chunk>,
    pending_gen: FxHashSet<ChunkPos>,
    pending_mesh: FxHashSet<ChunkPos>,
    meshed_chunks: FxHashSet<ChunkPos>,
    dirty_mesh_queue: FxHashSet<ChunkPos>,
    gen_tx: Sender<(ChunkPos, Chunk)>,
    gen_rx: Receiver<(ChunkPos, Chunk)>,
    mesh_tx: Sender<(ChunkPos, ChunkMesh)>,
    mesh_rx: Receiver<(ChunkPos, ChunkMesh)>,
    render_distance: i32,
    last_center: Option<ChunkPos>,
    spiral_offsets: Vec<(i32, i32)>,
    world_seed: u32,
    chunk_edits: ChunkEditMap,
    unsaved_changes: bool,
}

fn build_spiral_offsets(radius: i32) -> Vec<(i32, i32)> {
    // Load 1 extra ring of chunks beyond render_distance so chunks at the edge have neighbors for meshing
    let gen_radius = radius + 1;
    let mut offsets = Vec::with_capacity(((gen_radius * 2 + 1) * (gen_radius * 2 + 1)) as usize);
    for dz in -gen_radius..=gen_radius {
        for dx in -gen_radius..=gen_radius {
            if dx * dx + dz * dz <= gen_radius * gen_radius + 1 {
                offsets.push((dx, dz));
            }
        }
    }
    offsets.sort_by_key(|&(dx, dz)| dx * dx + dz * dz);
    offsets
}

impl WorldManager {
    pub fn new(render_distance: i32) -> Self {
        Self::with_seed_and_edits(render_distance, DEFAULT_WORLD_SEED, ChunkEditMap::default())
    }

    pub fn with_seed_and_edits(render_distance: i32, seed: u32, chunk_edits: ChunkEditMap) -> Self {
        let rd = render_distance.clamp(4, 36);
        let (gen_tx, gen_rx) = unbounded();
        let (mesh_tx, mesh_rx) = unbounded();
        Self {
            chunks: FxHashMap::default(),
            pending_gen: FxHashSet::default(),
            pending_mesh: FxHashSet::default(),
            meshed_chunks: FxHashSet::default(),
            dirty_mesh_queue: FxHashSet::default(),
            gen_tx,
            gen_rx,
            mesh_tx,
            mesh_rx,
            render_distance: rd,
            last_center: None,
            spiral_offsets: build_spiral_offsets(rd),
            world_seed: seed,
            chunk_edits,
            unsaved_changes: false,
        }
    }

    pub fn insert_chunk_for_test(&mut self, pos: ChunkPos, chunk: Chunk) {
        self.chunks.insert(pos, chunk);
    }

    #[inline]
    pub fn seed(&self) -> u32 {
        self.world_seed
    }

    #[inline]
    pub fn chunk_edits(&self) -> &ChunkEditMap {
        &self.chunk_edits
    }

    #[inline]
    pub fn has_unsaved_changes(&self) -> bool {
        self.unsaved_changes
    }

    #[inline]
    pub fn mark_saved(&mut self) {
        self.unsaved_changes = false;
    }

    pub fn dirty_mesh_queue_len(&self) -> usize {
        self.dirty_mesh_queue.len()
    }

    pub fn queue_all_loaded_for_test(&mut self) {
        let keys: Vec<ChunkPos> = self.chunks.keys().copied().collect();
        for pos in keys {
            if self.has_all_face_neighbors(pos) {
                self.dirty_mesh_queue.insert(pos);
            }
        }
    }

    #[inline]
    pub fn loaded_chunk_count(&self) -> usize {
        self.chunks.len()
    }

    #[inline]
    pub fn render_distance(&self) -> i32 {
        self.render_distance
    }

    pub fn set_render_distance(&mut self, rd: i32) {
        let clamped = rd.clamp(4, 36);
        if clamped != self.render_distance {
            self.render_distance = clamped;
            self.spiral_offsets = build_spiral_offsets(clamped);
            self.last_center = None;
        }
    }

    #[inline]
    pub fn get_block(&self, wx: i32, wy: i32, wz: i32) -> BlockId {
        if wy < MIN_CHUNK_Y * CHUNK_SIZE as i32 {
            return BlockId::Bedrock;
        }
        if wy >= (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32 {
            return BlockId::Air;
        }
        let (cpos, (lx, ly, lz)) = world_to_chunk_and_local(wx, wy, wz);
        match self.chunks.get(&cpos) {
            Some(chunk) => chunk.get(lx, ly, lz),
            None => BlockId::Air,
        }
    }

    pub fn has_all_face_neighbors(&self, pos: ChunkPos) -> bool {
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let ny = pos.y + dy;
            if ny < MIN_CHUNK_Y || ny > MAX_CHUNK_Y {
                continue;
            }
            let npos = ChunkPos::new(pos.x + dx, ny, pos.z + dz);
            if !self.chunks.contains_key(&npos) {
                return false;
            }
        }
        true
    }

    pub fn build_padded_chunk(&self, pos: ChunkPos) -> Option<PaddedChunk> {
        let center = self.chunks.get(&pos)?;
        if !self.has_all_face_neighbors(pos) {
            return None;
        }

        // Fast path: uniform air chunk produces zero faces
        if center.is_uniform() == Some(BlockId::Air) {
            return Some(PaddedChunk::new_empty());
        }

        let mut padded = PaddedChunk::from_center_only(center);

        // Copy 6 face halos directly from neighbor chunks
        let cs = CHUNK_SIZE;
        // +X (px = 33, lx = 0) and -X (px = 0, lx = 31)
        if let Some(nx_pos) = self.chunks.get(&ChunkPos::new(pos.x + 1, pos.y, pos.z)) {
            if nx_pos.is_uniform() != Some(BlockId::Air) {
                for ly in 0..cs {
                    for lz in 0..cs {
                        padded.set(cs + 1, ly + 1, lz + 1, nx_pos.get(0, ly, lz));
                    }
                }
            }
        }
        if let Some(nx_neg) = self.chunks.get(&ChunkPos::new(pos.x - 1, pos.y, pos.z)) {
            if nx_neg.is_uniform() != Some(BlockId::Air) {
                for ly in 0..cs {
                    for lz in 0..cs {
                        padded.set(0, ly + 1, lz + 1, nx_neg.get(cs - 1, ly, lz));
                    }
                }
            }
        }

        // +Y (py = 33, ly = 0) and -Y (py = 0, ly = 31)
        if pos.y < MAX_CHUNK_Y {
            if let Some(ny_pos) = self.chunks.get(&ChunkPos::new(pos.x, pos.y + 1, pos.z)) {
                if ny_pos.is_uniform() != Some(BlockId::Air) {
                    for lz in 0..cs {
                        for lx in 0..cs {
                            padded.set(lx + 1, cs + 1, lz + 1, ny_pos.get(lx, 0, lz));
                        }
                    }
                }
            }
        }
        if pos.y > MIN_CHUNK_Y {
            if let Some(ny_neg) = self.chunks.get(&ChunkPos::new(pos.x, pos.y - 1, pos.z)) {
                if ny_neg.is_uniform() != Some(BlockId::Air) {
                    for lz in 0..cs {
                        for lx in 0..cs {
                            padded.set(lx + 1, 0, lz + 1, ny_neg.get(lx, cs - 1, lz));
                        }
                    }
                }
            }
        } else {
            // Below MIN_CHUNK_Y is solid bedrock
            for lz in 0..cs {
                for lx in 0..cs {
                    padded.set(lx + 1, 0, lz + 1, BlockId::Bedrock);
                }
            }
        }

        // +Z (pz = 33, lz = 0) and -Z (pz = 0, lz = 31)
        if let Some(nz_pos) = self.chunks.get(&ChunkPos::new(pos.x, pos.y, pos.z + 1)) {
            if nz_pos.is_uniform() != Some(BlockId::Air) {
                for ly in 0..cs {
                    for lx in 0..cs {
                        padded.set(lx + 1, ly + 1, cs + 1, nz_pos.get(lx, ly, 0));
                    }
                }
            }
        }
        if let Some(nz_neg) = self.chunks.get(&ChunkPos::new(pos.x, pos.y, pos.z - 1)) {
            if nz_neg.is_uniform() != Some(BlockId::Air) {
                for ly in 0..cs {
                    for lx in 0..cs {
                        padded.set(lx + 1, ly + 1, 0, nz_neg.get(lx, ly, cs - 1));
                    }
                }
            }
        }

        // Populate 12 edge strips for seamless corner AO across chunk borders
        let base_wx = pos.x * cs as i32;
        let base_wy = pos.y * cs as i32;
        let base_wz = pos.z * cs as i32;
        for i in 0..=cs + 1 {
            for &(px, py) in &[(0, 0), (cs + 1, 0), (0, cs + 1), (cs + 1, cs + 1)] {
                let b = self.get_block(
                    base_wx + px as i32 - 1,
                    base_wy + py as i32 - 1,
                    base_wz + i as i32 - 1,
                );
                padded.set(px, py, i, b);
            }
            for &(px, pz) in &[(0, 0), (cs + 1, 0), (0, cs + 1), (cs + 1, cs + 1)] {
                let b = self.get_block(
                    base_wx + px as i32 - 1,
                    base_wy + i as i32 - 1,
                    base_wz + pz as i32 - 1,
                );
                padded.set(px, i, pz, b);
            }
            for &(py, pz) in &[(0, 0), (cs + 1, 0), (0, cs + 1), (cs + 1, cs + 1)] {
                let b = self.get_block(
                    base_wx + i as i32 - 1,
                    base_wy + py as i32 - 1,
                    base_wz + pz as i32 - 1,
                );
                padded.set(i, py, pz, b);
            }
        }

        Some(padded)
    }

    pub fn set_block_immediate(
        &mut self,
        wx: i32,
        wy: i32,
        wz: i32,
        block: BlockId,
    ) -> Vec<(ChunkPos, ChunkMesh)> {
        if wy < MIN_CHUNK_Y * CHUNK_SIZE as i32 || wy >= (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32 {
            return Vec::new();
        }
        let (cpos, (lx, ly, lz)) = world_to_chunk_and_local(wx, wy, wz);
        let Some(chunk) = self.chunks.get_mut(&cpos) else {
            return Vec::new();
        };
        chunk.set(lx, ly, lz, block);
        chunk.try_compress();

        self.chunk_edits
            .entry(cpos)
            .or_default()
            .insert(linear_index(lx, ly, lz) as u16, block);
        self.unsaved_changes = true;

        let mut dirty = vec![cpos];
        let last = CHUNK_SIZE - 1;
        if lx == 0 {
            dirty.push(ChunkPos::new(cpos.x - 1, cpos.y, cpos.z));
        } else if lx == last {
            dirty.push(ChunkPos::new(cpos.x + 1, cpos.y, cpos.z));
        }
        if ly == 0 && cpos.y > MIN_CHUNK_Y {
            dirty.push(ChunkPos::new(cpos.x, cpos.y - 1, cpos.z));
        } else if ly == last && cpos.y < MAX_CHUNK_Y {
            dirty.push(ChunkPos::new(cpos.x, cpos.y + 1, cpos.z));
        }
        if lz == 0 {
            dirty.push(ChunkPos::new(cpos.x, cpos.y, cpos.z - 1));
        } else if lz == last {
            dirty.push(ChunkPos::new(cpos.x, cpos.y, cpos.z + 1));
        }

        let mut out = Vec::with_capacity(dirty.len());
        for dpos in dirty {
            self.pending_mesh.remove(&dpos);
            if let Some(padded) = self.build_padded_chunk(dpos) {
                let mesh = mesh_chunk(&padded);
                self.meshed_chunks.insert(dpos);
                self.dirty_mesh_queue.remove(&dpos);
                out.push((dpos, mesh));
            }
        }
        out
    }

    pub fn update_streaming(
        &mut self,
        center_chunk: ChunkPos,
    ) -> (Vec<(ChunkPos, ChunkMesh)>, Vec<ChunkPos>) {
        let mut completed_meshes = Vec::new();
        let mut unloaded = Vec::new();

        // 1. Drain up to 48 generated chunks per frame
        for _ in 0..48 {
            let Ok((pos, mut chunk)) = self.gen_rx.try_recv() else {
                break;
            };
            self.pending_gen.remove(&pos);
            if let Some(edits) = self.chunk_edits.get(&pos) {
                for (&idx, &block) in edits {
                    chunk.set_by_index(idx, block);
                }
                chunk.try_compress();
            }
            self.chunks.insert(pos, chunk);

            // Check if this chunk and its 6 neighbors can now be queued for meshing
            for (dx, dy, dz) in [
                (0, 0, 0),
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let npos = ChunkPos::new(pos.x + dx, pos.y + dy, pos.z + dz);
                if npos.y >= MIN_CHUNK_Y
                    && npos.y <= MAX_CHUNK_Y
                    && !self.meshed_chunks.contains(&npos)
                    && !self.pending_mesh.contains(&npos)
                    && self.chunks.contains_key(&npos)
                    && self.has_all_face_neighbors(npos)
                {
                    self.dirty_mesh_queue.insert(npos);
                }
            }
        }

        // 2. Drain up to 32 completed meshes per frame
        for _ in 0..32 {
            let Ok((pos, mesh)) = self.mesh_rx.try_recv() else {
                break;
            };
            if self.pending_mesh.remove(&pos) && self.chunks.contains_key(&pos) {
                self.meshed_chunks.insert(pos);
                completed_meshes.push((pos, mesh));
            }
        }

        // 3. Unload chunks far outside render_distance + 2 when player moves to a new chunk
        let center_2d = ChunkPos::new(center_chunk.x, 0, center_chunk.z);
        if self.last_center != Some(center_2d) {
            self.last_center = Some(center_2d);
            let unload_r = self.render_distance + 2;
            let unload_r2 = unload_r * unload_r + 2;

            self.chunks.retain(|&pos, _| {
                let dx = pos.x - center_chunk.x;
                let dz = pos.z - center_chunk.z;
                let keep = dx * dx + dz * dz <= unload_r2;
                if !keep {
                    self.meshed_chunks.remove(&pos);
                    self.dirty_mesh_queue.remove(&pos);
                    unloaded.push(pos);
                }
                keep
            });
        }

        // 4. Dispatch new terrain generation tasks in spiral order around center_chunk
        let mut dispatched_gen = 0;
        let seed = self.world_seed;
        'spiral: for &(dx, dz) in &self.spiral_offsets {
            if self.pending_gen.len() >= 96 || dispatched_gen >= 32 {
                break;
            }
            let cx = center_chunk.x + dx;
            let cz = center_chunk.z + dz;
            for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                let pos = ChunkPos::new(cx, cy, cz);
                if !self.chunks.contains_key(&pos) && !self.pending_gen.contains(&pos) {
                    self.pending_gen.insert(pos);
                    let tx = self.gen_tx.clone();
                    rayon::spawn(move || {
                        let chunk = generate_chunk_seeded(pos, seed);
                        let _ = tx.send((pos, chunk));
                    });
                    dispatched_gen += 1;
                    if dispatched_gen >= 32 {
                        break 'spiral;
                    }
                }
            }
        }

        // 5. Fast-clear any Uniform(Air) chunks from dirty_mesh_queue before counting the 24-chunk frame budget
        let mut empty_air_chunks = Vec::new();
        for &pos in &self.dirty_mesh_queue {
            if self.chunks.get(&pos).and_then(|c| c.is_uniform()) == Some(BlockId::Air) {
                empty_air_chunks.push(pos);
            }
        }
        for pos in empty_air_chunks {
            self.dirty_mesh_queue.remove(&pos);
            self.meshed_chunks.insert(pos);
        }

        // Dispatch meshing tasks for chunks inside render_distance whose 6 neighbors are ready
        let rd2 = self.render_distance * self.render_distance;
        let mut ready_to_mesh: Vec<ChunkPos> = self
            .dirty_mesh_queue
            .iter()
            .copied()
            .filter(|pos| {
                let dx = pos.x - center_chunk.x;
                let dz = pos.z - center_chunk.z;
                dx * dx + dz * dz <= rd2
            })
            .collect();

        // Sort closest to player first so nearby terrain appears immediately
        ready_to_mesh.sort_by_key(|pos| {
            let dx = pos.x - center_chunk.x;
            let dz = pos.z - center_chunk.z;
            dx * dx + dz * dz
        });

        for pos in ready_to_mesh.into_iter().take(24) {
            if self.pending_mesh.len() >= 64 {
                break;
            }
            self.dirty_mesh_queue.remove(&pos);
            let Some(padded) = self.build_padded_chunk(pos) else {
                continue;
            };
            // Fast path on main thread: if padded chunk has no solid voxels at all, skip worker dispatch
            if !padded.has_any_solid {
                self.meshed_chunks.insert(pos);
                continue;
            }
            self.pending_mesh.insert(pos);
            let tx = self.mesh_tx.clone();
            rayon::spawn(move || {
                let mesh = mesh_chunk(&padded);
                let _ = tx.send((pos, mesh));
            });
        }

        (completed_meshes, unloaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    #[test]
    fn test_boundary_block_edit_marks_neighbor_dirty() {
        let mut mgr = WorldManager::new(2);
        mgr.insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Stone));
        mgr.insert_chunk_for_test(ChunkPos::new(-1, 0, 0), Chunk::new_uniform(BlockId::Stone));
        for dir in [
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(0, -1, 0),
            ChunkPos::new(0, 0, 1),
            ChunkPos::new(0, 0, -1),
            ChunkPos::new(-2, 0, 0),
            ChunkPos::new(-1, 1, 0),
            ChunkPos::new(-1, -1, 0),
            ChunkPos::new(-1, 0, 1),
            ChunkPos::new(-1, 0, -1),
        ] {
            mgr.insert_chunk_for_test(dir, Chunk::new_uniform(BlockId::Air));
        }

        // Breaking block at wx = 0 (local x = 0 of chunk 0,0,0) borders chunk (-1,0,0) at local x = 31
        let remeshed = mgr.set_block_immediate(0, 5, 5, BlockId::Air);
        let positions: Vec<ChunkPos> = remeshed.iter().map(|(p, _)| *p).collect();
        assert!(positions.contains(&ChunkPos::new(0, 0, 0)));
        assert!(positions.contains(&ChunkPos::new(-1, 0, 0)));
    }

    #[test]
    fn test_set_block_immediate_cancels_inflight_pending_mesh() {
        let mut mgr = WorldManager::new(4);
        let cpos = ChunkPos::new(0, 0, 0);
        mgr.insert_chunk_for_test(cpos, Chunk::new_uniform(BlockId::Stone));
        for dir in [
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(-1, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(0, -1, 0),
            ChunkPos::new(0, 0, 1),
            ChunkPos::new(0, 0, -1),
        ] {
            mgr.insert_chunk_for_test(dir, Chunk::new_uniform(BlockId::Air));
        }

        // Simulate an older in-flight background mesh that completes after a synchronous block edit
        mgr.pending_mesh.insert(cpos);
        let _ = mgr.mesh_tx.send((cpos, ChunkMesh::new()));

        let remeshed = mgr.set_block_immediate(5, 5, 5, BlockId::Air);
        assert_eq!(remeshed.len(), 1);

        // Subsequent streaming update must discard the stale in-flight mesh result
        let (completed, _) = mgr.update_streaming(cpos);
        assert!(
            !completed.iter().any(|(p, _)| *p == cpos),
            "stale in-flight mesh must be ignored after set_block_immediate"
        );
    }

    #[test]
    fn test_chunk_edits_persist_across_unload_and_regeneration() {
        let mut mgr = WorldManager::with_seed_and_edits(4, 1337, ChunkEditMap::default());
        let c0 = ChunkPos::new(0, 1, 0);
        let c_neg_x = ChunkPos::new(-1, 1, 0);
        for dx in -2..=1 {
            for dy in 0..=2 {
                for dz in -1..=1 {
                    let p = ChunkPos::new(dx, dy, dz);
                    mgr.insert_chunk_for_test(p, generate_chunk_seeded(p, 1337));
                }
            }
        }

        // Place a Brick block at wx = 0, wy = 40, wz = 5 (local x = 0 of chunk (0, 1, 0))
        mgr.set_block_immediate(0, 40, 5, BlockId::Brick);
        assert!(mgr.has_unsaved_changes());
        assert_eq!(mgr.get_block(0, 40, 5), BlockId::Brick);

        // Move player far away (chunk 100, 1, 100) to force unloading chunk (0, 1, 0)
        let (_, unloaded) = mgr.update_streaming(ChunkPos::new(100, 1, 100));
        assert!(unloaded.contains(&c0));
        assert_eq!(mgr.get_block(0, 40, 5), BlockId::Air);

        // Move back to (0, 1, 0) and pump streaming until (0, 1, 0) regenerates
        for _ in 0..40 {
            mgr.update_streaming(c0);
            if mgr.get_block(0, 40, 5) == BlockId::Brick {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(mgr.get_block(0, 40, 5), BlockId::Brick);
        let _ = c_neg_x;
    }

    #[test]
    fn test_uniform_air_chunks_do_not_consume_mesh_budget() {
        let mut mgr = WorldManager::new(4);
        for dx in -2..=2 {
            for dz in -2..=2 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(dx, cy, dz);
                    let block = if cy == 0 && dx == 0 && dz == 0 {
                        BlockId::Stone
                    } else {
                        BlockId::Air
                    };
                    mgr.insert_chunk_for_test(pos, Chunk::new_uniform(block));
                }
            }
        }
        mgr.queue_all_loaded_for_test();
        assert!(mgr.dirty_mesh_queue_len() > 25);

        mgr.update_streaming(ChunkPos::new(0, 0, 0));
        assert_eq!(mgr.dirty_mesh_queue_len(), 0);
    }
}
