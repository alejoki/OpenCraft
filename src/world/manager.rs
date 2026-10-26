use std::collections::VecDeque;

use crossbeam_channel::{unbounded, Receiver, Sender};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::mesh::greedy::{mesh_chunk, PaddedChunk};
use crate::mesh::quad::ChunkMesh;
use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, world_to_chunk_and_local, Chunk, ChunkPos, CHUNK_SIZE};
use crate::world::light::{self, LightGrid};
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
    /// Per-chunk sky-light grids (D038/D044). Derived data only: never
    /// persisted, recomputed deterministically from chunks + edits.
    light_grids: FxHashMap<ChunkPos, LightGrid>,
    gen_tx: Sender<(ChunkPos, Chunk)>,
    gen_rx: Receiver<(ChunkPos, Chunk)>,
    mesh_tx: Sender<(ChunkPos, ChunkMesh)>,
    mesh_rx: Receiver<(ChunkPos, ChunkMesh)>,
    /// Completed columns waiting for a worker slot. `queued_light` is the
    /// de-dupe companion; the deque is FIFO so a column triggered later can
    /// never be dispatched (and therefore snapshotted) before an earlier one.
    pending_light: VecDeque<(i32, i32, u64)>,
    queued_light: FxHashSet<(i32, i32)>,
    /// Columns whose light region is currently being computed on rayon.
    light_inflight: FxHashSet<(i32, i32)>,
    light_tx: Sender<LightJobResult>,
    light_rx: Receiver<LightJobResult>,
    /// Monotonic dispatch sequence per column. A later result must win over
    /// an earlier one even when workers finish out of order (an older result
    /// may have read a still-loading neighbour column as air and be over-lit).
    next_light_seq: u64,
    light_applied_seq: FxHashMap<(i32, i32), u64>,
    /// Bumped on every block edit. In-flight results from before the edit may
    /// carry pre-edit blocks, so their grids are discarded rather than allowed
    /// to overwrite the edit path's synchronous light recompute.
    light_epoch: u64,
    render_distance: i32,
    last_center: Option<ChunkPos>,
    spiral_offsets: Vec<(i32, i32)>,
    world_seed: u32,
    chunk_edits: ChunkEditMap,
    unsaved_changes: bool,
}

/// Result of one rayon sky-light job. Produced entirely off the frame thread;
/// `update_streaming` only inserts the finished grids.
struct LightJobResult {
    /// The complete column whose completion queued the job.
    trigger: (i32, i32),
    seq: u64,
    epoch: u64,
    grids: FxHashMap<ChunkPos, LightGrid>,
    /// Columns that were complete in the worker's block snapshot. Only these
    /// may receive a grid: a column that completed after the snapshot would
    /// otherwise be lit from a view that still read its missing chunks as air
    /// and stored an over-lit grid.
    complete_columns: FxHashSet<(i32, i32)>,
}

/// Recompute sky light for a 3x3 column region from an owned chunk snapshot.
///
/// This mirrors `WorldManager::compute_light_region`'s out-of-range semantics
/// exactly; the only difference is that the block source is the detached map
/// the frame thread handed to rayon, so the worker never touches manager state.
fn compute_light_region_snapshot(
    chunks: &FxHashMap<ChunkPos, Chunk>,
    cx0: i32,
    cx1: i32,
    cz0: i32,
    cz1: i32,
) -> FxHashMap<ChunkPos, LightGrid> {
    light::compute_region_light(
        |wx, wy, wz| {
            if wy < MIN_CHUNK_Y * CHUNK_SIZE as i32 {
                return BlockId::Bedrock;
            }
            if wy >= (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32 {
                return BlockId::Air;
            }
            let (cpos, (lx, ly, lz)) = world_to_chunk_and_local(wx, wy, wz);
            chunks
                .get(&cpos)
                .map_or(BlockId::Air, |chunk| chunk.get(lx, ly, lz))
        },
        cx0,
        cx1,
        cz0,
        cz1,
    )
}

/// Per-frame ceilings for the light pipeline. Grid application is cheap (map
/// inserts plus mesh-queue dirt) but result payloads are large; dispatch is
/// limited because snapshotting clones up to 54 chunk block arrays on the frame
/// thread. Both are well below the MEM229 8 ms p95 budget.
const LIGHT_RESULT_DRAIN_BUDGET: usize = 8;
const LIGHT_DISPATCH_BUDGET: usize = 4;

fn build_spiral_offsets(radius: i32) -> Vec<(i32, i32)> {
    // One extra ring beyond render_distance supplied the 6 face neighbours for
    // meshing. Meshing now also needs sky light in the 3x3 column neighborhood
    // (face halos plus the 12 edge strips), so load a second extra ring: that
    // keeps the diagonal neighbour columns of chunks on the render-distance
    // circle generated (and therefore light-ready) instead of leaving a
    // permanently unmeshed ring at the 45-degree edge.
    let gen_radius = radius + 2;
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
        let (light_tx, light_rx) = unbounded();
        Self {
            chunks: FxHashMap::default(),
            pending_gen: FxHashSet::default(),
            pending_mesh: FxHashSet::default(),
            meshed_chunks: FxHashSet::default(),
            dirty_mesh_queue: FxHashSet::default(),
            light_grids: FxHashMap::default(),
            gen_tx,
            gen_rx,
            mesh_tx,
            mesh_rx,
            pending_light: VecDeque::new(),
            queued_light: FxHashSet::default(),
            light_inflight: FxHashSet::default(),
            light_tx,
            light_rx,
            next_light_seq: 0,
            light_applied_seq: FxHashMap::default(),
            light_epoch: 0,
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
            if !(MIN_CHUNK_Y..=MAX_CHUNK_Y).contains(&ny) {
                continue;
            }
            let npos = ChunkPos::new(pos.x + dx, ny, pos.z + dz);
            if !self.chunks.contains_key(&npos) {
                return false;
            }
        }
        true
    }

    /// True when every chunk of `(cx, cz)` across the full loaded Y range is
    /// resident. Sky-light propagation is column-granular: a partial column
    /// cannot produce a trustworthy grid.
    fn column_is_complete(&self, cx: i32, cz: i32) -> bool {
        (MIN_CHUNK_Y..=MAX_CHUNK_Y).all(|cy| self.chunks.contains_key(&ChunkPos::new(cx, cy, cz)))
    }

    /// True when every chunk of `(cx, cz)` has a sky-light grid. Grids are only
    /// stored for complete columns, so this is also the mesh-readiness signal.
    pub fn has_column_light(&self, cx: i32, cz: i32) -> bool {
        (MIN_CHUNK_Y..=MAX_CHUNK_Y)
            .all(|cy| self.light_grids.contains_key(&ChunkPos::new(cx, cy, cz)))
    }

    /// Meshing reads light from the 3x3 column neighborhood around a chunk
    /// (6 face halos plus the 12 edge strips), so all nine columns must be lit
    /// before a mesh may be baked.
    fn has_light_neighborhood(&self, cx: i32, cz: i32) -> bool {
        (-1..=1).all(|dx| (-1..=1).all(|dz| self.has_column_light(cx + dx, cz + dz)))
    }

    /// Sky light at a world position, used for the padded snapshot's 12 edge
    /// strips.
    ///
    /// Missing grids fall back to full sky light. That fallback is only
    /// reachable for direct `build_padded_chunk` callers (headless edit tests)
    /// before light is seeded: the streaming path refuses to mesh until the 3x3
    /// light neighborhood is ready.
    pub fn light_at_world(&self, wx: i32, wy: i32, wz: i32) -> u8 {
        if wy < MIN_CHUNK_Y * CHUNK_SIZE as i32 {
            return 0;
        }
        if wy >= (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32 {
            return light::MAX_SKY_LIGHT;
        }
        let (cpos, (lx, ly, lz)) = world_to_chunk_and_local(wx, wy, wz);
        self.light_grids
            .get(&cpos)
            .map_or(light::MAX_SKY_LIGHT, |grid| grid.get(lx, ly, lz))
    }

    /// Compute sky light for the inclusive chunk-column region with the same
    /// out-of-range semantics as `get_block`.
    fn compute_light_region(
        &self,
        cx0: i32,
        cx1: i32,
        cz0: i32,
        cz1: i32,
    ) -> FxHashMap<ChunkPos, LightGrid> {
        let chunks = &self.chunks;
        light::compute_region_light(
            |wx, wy, wz| {
                if wy < MIN_CHUNK_Y * CHUNK_SIZE as i32 {
                    return BlockId::Bedrock;
                }
                if wy >= (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32 {
                    return BlockId::Air;
                }
                let (cpos, (lx, ly, lz)) = world_to_chunk_and_local(wx, wy, wz);
                chunks
                    .get(&cpos)
                    .map_or(BlockId::Air, |chunk| chunk.get(lx, ly, lz))
            },
            cx0,
            cx1,
            cz0,
            cz1,
        )
    }

    /// Store freshly computed grids, dirtying already-meshed chunks whose light
    /// actually changed, then re-evaluate mesh readiness for the neighborhood.
    ///
    /// Returns every chunk whose packed grid changed (whether or not it was
    /// already meshed) so the edit path can merge them into its synchronous
    /// remesh set — the dirty-on-light-diff contract.
    fn apply_light_region(
        &mut self,
        grids: FxHashMap<ChunkPos, LightGrid>,
        cx0: i32,
        cx1: i32,
        cz0: i32,
        cz1: i32,
    ) -> Vec<ChunkPos> {
        let mut changed_any = false;
        let mut changed_positions = Vec::new();
        for (pos, grid) in grids {
            // A 3x3 region around a completing column also contains columns
            // that are still loading; their absent chunks read as air and
            // would produce an over-lit grid. Do not store a grid until the
            // column itself is complete -- its own completion recomputes it.
            if !self.column_is_complete(pos.x, pos.z) {
                continue;
            }
            let changed = match self.light_grids.get(&pos) {
                Some(existing) => light::grids_differ(existing, &grid),
                None => true,
            };
            if !changed {
                continue;
            }
            self.light_grids.insert(pos, grid);
            changed_any = true;
            changed_positions.push(pos);
            if self.meshed_chunks.contains(&pos) || self.pending_mesh.contains(&pos) {
                // Dirty-on-light-diff: any mesh baked from the old grid (or an
                // in-flight job carrying one) is stale. Cancelling the pending
                // job makes the drain discard its late result, exactly like the
                // synchronous edit path does.
                self.pending_mesh.remove(&pos);
                self.dirty_mesh_queue.insert(pos);
            }
        }
        if changed_any {
            // Light-readiness can flip for chunks in columns adjacent to the
            // freshly lit region.
            self.queue_light_ready_chunks(cx0 - 1, cx1 + 1, cz0 - 1, cz1 + 1);
        }
        changed_positions
    }

    /// Edit-path light recompute: the 3x3 column ring around the edited chunk.
    ///
    /// Sufficiency proof: a one-block edit's light delta propagates at most 15
    /// blocks laterally (one level lost per BFS step), and 15 < 32 = one chunk
    /// column, so it cannot escape the edited chunk-column +/-1 ring. The whole
    /// 192-block column is recomputed, so there is no vertical escape either.
    ///
    /// Returns the chunks whose packed grid actually changed; callers remesh
    /// them even when they are not voxel-adjacent to the edit.
    fn recompute_light_region_for_edit(&mut self, cx: i32, cz: i32) -> Vec<ChunkPos> {
        let (cx0, cx1, cz0, cz1) = (cx - 1, cx + 1, cz - 1, cz + 1);
        let grids = self.compute_light_region(cx0, cx1, cz0, cz1);
        self.apply_light_region(grids, cx0, cx1, cz0, cz1)
    }

    /// Queue resident, unmeshed chunks whose face and light neighborhoods are
    /// both ready. The 3x3 gate is the mesh-readiness contract from D045.
    fn queue_light_ready_chunks(&mut self, cx0: i32, cx1: i32, cz0: i32, cz1: i32) {
        for cx in cx0..=cx1 {
            for cz in cz0..=cz1 {
                if !self.has_light_neighborhood(cx, cz) {
                    continue;
                }
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(cx, cy, cz);
                    if self.chunks.contains_key(&pos)
                        && !self.meshed_chunks.contains(&pos)
                        && !self.pending_mesh.contains(&pos)
                        && self.has_all_face_neighbors(pos)
                    {
                        self.dirty_mesh_queue.insert(pos);
                    }
                }
            }
        }
    }

    /// Column-ready trigger: once a column is complete it gains sky light, and
    /// the surrounding 3x3 columns are recomputed so lateral bleed into already
    /// loaded neighbours is captured.
    ///
    /// The frame thread only enqueues here. The actual propagation runs on a
    /// rayon worker and is applied on a later `update_streaming` call, so a
    /// column completing mid-crossing cannot block the frame (MEM229 / D072,
    /// decision decouple-light).
    fn propagate_light_for_completed_column(&mut self, cx: i32, cz: i32) {
        if self.has_column_light(cx, cz) || !self.column_is_complete(cx, cz) {
            return;
        }
        self.enqueue_light_column(cx, cz);
    }

    /// Queue one complete column for off-thread light computation. Deduped
    /// against both the pending queue and in-flight jobs.
    fn enqueue_light_column(&mut self, cx: i32, cz: i32) {
        let key = (cx, cz);
        if self.queued_light.contains(&key) || self.light_inflight.contains(&key) {
            return;
        }
        self.next_light_seq += 1;
        self.pending_light.push_back((cx, cz, self.next_light_seq));
        self.queued_light.insert(key);
    }

    /// Snapshot the 3x3 column neighborhood of up to `LIGHT_DISPATCH_BUDGET`
    /// queued columns and hand each snapshot to rayon. Runs late in
    /// `update_streaming`, so the frame pays only the snapshot clone, never the
    /// light BFS.
    fn dispatch_light_jobs(&mut self) {
        for _ in 0..LIGHT_DISPATCH_BUDGET {
            let Some((cx, cz, seq)) = self.pending_light.pop_front() else {
                break;
            };
            self.queued_light.remove(&(cx, cz));

            // State may have changed while queued: an edit's synchronous
            // recompute can light the column, or an unload can remove it.
            // An incomplete column re-enqueues itself when it completes.
            if self.has_column_light(cx, cz) || !self.column_is_complete(cx, cz) {
                continue;
            }

            let mut chunks: FxHashMap<ChunkPos, Chunk> = FxHashMap::default();
            let mut complete_columns: FxHashSet<(i32, i32)> = FxHashSet::default();
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let ccx = cx + dx;
                    let ccz = cz + dz;
                    if self.column_is_complete(ccx, ccz) {
                        complete_columns.insert((ccx, ccz));
                    }
                    for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                        let pos = ChunkPos::new(ccx, cy, ccz);
                        if let Some(chunk) = self.chunks.get(&pos) {
                            chunks.insert(pos, chunk.clone());
                        }
                    }
                }
            }

            let epoch = self.light_epoch;
            let tx = self.light_tx.clone();
            self.light_inflight.insert((cx, cz));
            rayon::spawn(move || {
                let grids = compute_light_region_snapshot(&chunks, cx - 1, cx + 1, cz - 1, cz + 1);
                let _ = tx.send(LightJobResult {
                    trigger: (cx, cz),
                    seq,
                    epoch,
                    grids,
                    complete_columns,
                });
            });
        }
    }

    /// Apply finished light jobs from the frame thread. Bounded per call; a
    /// result that raced an edit (epoch mismatch) or that read a still-loading
    /// neighbourhood (older seq than already applied) is discarded, and any
    /// column left unlit is re-queued so the pipeline cannot strand it.
    fn drain_light_results(&mut self) {
        for _ in 0..LIGHT_RESULT_DRAIN_BUDGET {
            let Ok(result) = self.light_rx.try_recv() else {
                break;
            };
            self.light_inflight.remove(&result.trigger);
            self.apply_light_result(result);
        }
    }

    fn apply_light_result(&mut self, result: LightJobResult) {
        let stale_epoch = result.epoch != self.light_epoch;
        let LightJobResult {
            trigger,
            seq,
            grids,
            complete_columns,
            ..
        } = result;

        if stale_epoch {
            // The edit path already recomputed the columns the edit can reach.
            // Anything else in this job still lacks light and must be redone
            // from a post-edit snapshot.
            for &(cx, cz) in &complete_columns {
                if !self.has_column_light(cx, cz) && self.column_is_complete(cx, cz) {
                    self.enqueue_light_column(cx, cz);
                }
            }
            return;
        }

        let mut accepted: FxHashMap<ChunkPos, LightGrid> = FxHashMap::default();
        for (pos, grid) in grids {
            let column = (pos.x, pos.z);
            // Grids are only trustworthy for columns the worker snapshot saw
            // complete (see `LightJobResult::complete_columns`).
            if !complete_columns.contains(&column) {
                continue;
            }
            // Out-of-order worker completion: a newer request already applied
            // a grid for this column, so an older snapshot must not overwrite
            // it (older snapshots can read missing neighbours as air).
            if let Some(applied) = self.light_applied_seq.get(&column) {
                if seq < *applied {
                    continue;
                }
            }
            accepted.insert(pos, grid);
        }
        for &column in &complete_columns {
            let entry = self.light_applied_seq.entry(column).or_insert(seq);
            if seq > *entry {
                *entry = seq;
            }
        }
        // `apply_light_region` re-checks current completeness and the
        // dirty-on-light-diff contract, exactly like the synchronous path.
        self.apply_light_region(
            accepted,
            trigger.0 - 1,
            trigger.0 + 1,
            trigger.1 - 1,
            trigger.1 + 1,
        );
    }

    /// Test seam: recompute sky light for every resident complete column without
    /// going through rayon streaming. Computes one region spanning all complete
    /// columns plus a one-chunk margin, so every column's light is independent
    /// of the region boundary (max lateral attenuation 15 < 32).
    pub fn recompute_light_for_test(&mut self) {
        let mut complete: Vec<(i32, i32)> = self
            .chunks
            .keys()
            .map(|pos| (pos.x, pos.z))
            .collect::<FxHashSet<_>>()
            .into_iter()
            .filter(|&(cx, cz)| self.column_is_complete(cx, cz))
            .collect();
        if complete.is_empty() {
            return;
        }
        complete.sort_unstable();
        let cx0 = complete.iter().map(|c| c.0).min().unwrap() - 1;
        let cx1 = complete.iter().map(|c| c.0).max().unwrap() + 1;
        let cz0 = complete.iter().map(|c| c.1).min().unwrap() - 1;
        let cz1 = complete.iter().map(|c| c.1).max().unwrap() + 1;
        let grids = self.compute_light_region(cx0, cx1, cz0, cz1);
        self.apply_light_region(grids, cx0, cx1, cz0, cz1);
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

        // Sky light mirrors the voxel halo geometry exactly: the center grid
        // fills the inner 32^3, the 6 face halos come from the neighbour chunk
        // grids, and the 12 edge strips use the world-space light accessor.
        if let Some(grid) = self.light_grids.get(&pos) {
            for ly in 0..cs {
                for lz in 0..cs {
                    for lx in 0..cs {
                        padded.set_light(lx + 1, ly + 1, lz + 1, grid.get(lx, ly, lz));
                    }
                }
            }
        }

        // +X (px = 33, lx = 0) and -X (px = 0, lx = 31)
        if let Some(grid) = self
            .light_grids
            .get(&ChunkPos::new(pos.x + 1, pos.y, pos.z))
        {
            for ly in 0..cs {
                for lz in 0..cs {
                    padded.set_light(cs + 1, ly + 1, lz + 1, grid.get(0, ly, lz));
                }
            }
        }
        if let Some(grid) = self
            .light_grids
            .get(&ChunkPos::new(pos.x - 1, pos.y, pos.z))
        {
            for ly in 0..cs {
                for lz in 0..cs {
                    padded.set_light(0, ly + 1, lz + 1, grid.get(cs - 1, ly, lz));
                }
            }
        }

        // +Y (py = 33, ly = 0) and -Y (py = 0, ly = 31)
        if let Some(grid) = self
            .light_grids
            .get(&ChunkPos::new(pos.x, pos.y + 1, pos.z))
        {
            for lz in 0..cs {
                for lx in 0..cs {
                    padded.set_light(lx + 1, cs + 1, lz + 1, grid.get(lx, 0, lz));
                }
            }
        }
        if pos.y > MIN_CHUNK_Y {
            if let Some(grid) = self
                .light_grids
                .get(&ChunkPos::new(pos.x, pos.y - 1, pos.z))
            {
                for lz in 0..cs {
                    for lx in 0..cs {
                        padded.set_light(lx + 1, 0, lz + 1, grid.get(lx, cs - 1, lz));
                    }
                }
            }
        } else {
            // Below MIN_CHUNK_Y is opaque bedrock, so no sky light reaches it.
            for lz in 0..cs {
                for lx in 0..cs {
                    padded.set_light(lx + 1, 0, lz + 1, 0);
                }
            }
        }

        // +Z (pz = 33, lz = 0) and -Z (pz = 0, lz = 31)
        if let Some(grid) = self
            .light_grids
            .get(&ChunkPos::new(pos.x, pos.y, pos.z + 1))
        {
            for ly in 0..cs {
                for lx in 0..cs {
                    padded.set_light(lx + 1, ly + 1, cs + 1, grid.get(lx, ly, 0));
                }
            }
        }
        if let Some(grid) = self
            .light_grids
            .get(&ChunkPos::new(pos.x, pos.y, pos.z - 1))
        {
            for ly in 0..cs {
                for lx in 0..cs {
                    padded.set_light(lx + 1, ly + 1, 0, grid.get(lx, ly, cs - 1));
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

        // ... and the matching light edge strips, through the grid map. Missing
        // grids fall back to full light (see `light_at_world`).
        for i in 0..=cs + 1 {
            for &(px, py) in &[(0, 0), (cs + 1, 0), (0, cs + 1), (cs + 1, cs + 1)] {
                let l = self.light_at_world(
                    base_wx + px as i32 - 1,
                    base_wy + py as i32 - 1,
                    base_wz + i as i32 - 1,
                );
                padded.set_light(px, py, i, l);
            }
            for &(px, pz) in &[(0, 0), (cs + 1, 0), (0, cs + 1), (cs + 1, cs + 1)] {
                let l = self.light_at_world(
                    base_wx + px as i32 - 1,
                    base_wy + i as i32 - 1,
                    base_wz + pz as i32 - 1,
                );
                padded.set_light(px, i, pz, l);
            }
            for &(py, pz) in &[(0, 0), (cs + 1, 0), (0, cs + 1), (cs + 1, cs + 1)] {
                let l = self.light_at_world(
                    base_wx + i as i32 - 1,
                    base_wy + py as i32 - 1,
                    base_wz + pz as i32 - 1,
                );
                padded.set_light(i, py, pz, l);
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

        // Any in-flight light job may have snapshotted the pre-edit block. Bump
        // the epoch so its result is discarded instead of overwriting the
        // synchronous recompute below.
        self.light_epoch = self.light_epoch.wrapping_add(1);

        self.chunk_edits
            .entry(cpos)
            .or_default()
            .insert(linear_index(lx, ly, lz) as u16, block);
        self.unsaved_changes = true;

        // A block edit changes sky light. Recompute the edit's chunk-column
        // +/-1 ring (the provably sufficient region: max lateral attenuation
        // 15 < one 32-block chunk, full 192-block height). Chunks whose light
        // grid changed must be remeshed even when the edit is not at their
        // voxel boundary. Skip while the edited column is still loading: the
        // column-ready trigger will recompute it once it is complete, and a
        // region computed with absent chunks would be over-lit.
        let light_dirty = if self.column_is_complete(cpos.x, cpos.z) {
            self.recompute_light_region_for_edit(cpos.x, cpos.z)
        } else {
            Vec::new()
        };

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
        for pos in light_dirty {
            if !dirty.contains(&pos) {
                dirty.push(pos);
            }
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

            // Column-ready trigger: this chunk may be the last piece of a full
            // 192-block column. Only enqueue here; the light BFS runs on a
            // rayon worker and lands on a later frame.
            self.propagate_light_for_completed_column(pos.x, pos.z);

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
                    && self.has_light_neighborhood(npos.x, npos.z)
                {
                    self.dirty_mesh_queue.insert(npos);
                }
            }
        }

        // 2. Apply up to 8 finished light jobs. Grids were computed on rayon;
        //    this step only inserts them and re-evaluates mesh readiness.
        self.drain_light_results();

        // 3. Drain up to 32 completed meshes per frame
        for _ in 0..32 {
            let Ok((pos, mesh)) = self.mesh_rx.try_recv() else {
                break;
            };
            if self.pending_mesh.remove(&pos) && self.chunks.contains_key(&pos) {
                self.meshed_chunks.insert(pos);
                completed_meshes.push((pos, mesh));
            }
        }

        // 4. Unload chunks far outside render_distance + 2 when player moves to a new chunk
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

            // Light is derived data: drop it with the chunks and let the
            // column-ready trigger recompute on reload. Residual staleness in a
            // still-loaded neighbour (its light was fed by an unloaded column)
            // is invisible: unloads happen beyond render_distance + 2 while
            // meshes only exist within render_distance, so every meshed chunk
            // keeps its full halo ring resident.
            self.light_grids.retain(|&pos, _| {
                let dx = pos.x - center_chunk.x;
                let dz = pos.z - center_chunk.z;
                dx * dx + dz * dz <= unload_r2
            });
            // Drop queued/applied light bookkeeping for unloaded columns. Jobs
            // already running finish and are skipped on arrival because their
            // columns are no longer complete.
            self.pending_light.retain(|&(cx, cz, _)| {
                let dx = cx - center_chunk.x;
                let dz = cz - center_chunk.z;
                dx * dx + dz * dz <= unload_r2
            });
            self.queued_light.retain(|&(cx, cz)| {
                let dx = cx - center_chunk.x;
                let dz = cz - center_chunk.z;
                dx * dx + dz * dz <= unload_r2
            });
            self.light_applied_seq.retain(|&(cx, cz), _| {
                let dx = cx - center_chunk.x;
                let dz = cz - center_chunk.z;
                dx * dx + dz * dz <= unload_r2
            });
        }

        // 5. Dispatch new terrain generation tasks in spiral order around center_chunk
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

        // 6. Fast-clear any Uniform(Air) chunks from dirty_mesh_queue before counting the 24-chunk frame budget
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
                if !self.has_light_neighborhood(pos.x, pos.z) {
                    return false;
                }
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

        // 7. Hand queued columns to rayon last, so snapshot cloning (up to 54
        //    chunks of block data) never delays this frame's mesh dispatch. The
        //    worker results are applied by step 2 of a later frame.
        self.dispatch_light_jobs();

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
        mgr.recompute_light_for_test();
        mgr.queue_all_loaded_for_test();
        assert!(mgr.dirty_mesh_queue_len() > 25);

        mgr.update_streaming(ChunkPos::new(0, 0, 0));
        assert_eq!(mgr.dirty_mesh_queue_len(), 0);
    }

    /// Insert every chunk of the 3x3 column neighborhood around (0,0), all as
    /// `block` except the optional stone chunk at the origin column's y = 0.
    fn insert_3x3_columns(mgr: &mut WorldManager, stone_center: bool, skip: Option<ChunkPos>) {
        for dx in -1..=1 {
            for dz in -1..=1 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(dx, cy, dz);
                    if skip == Some(pos) {
                        continue;
                    }
                    let block = if stone_center && dx == 0 && dz == 0 && cy == 0 {
                        BlockId::Stone
                    } else {
                        BlockId::Air
                    };
                    mgr.insert_chunk_for_test(pos, Chunk::new_uniform(block));
                }
            }
        }
    }

    #[test]
    fn test_mesh_dispatch_waits_for_full_light_neighborhood() {
        let mut mgr = WorldManager::new(4);
        // Full 3x3 column ring around the stone chunk, but column (1,0) is
        // missing its top chunk, so it can never become light-ready yet.
        insert_3x3_columns(&mut mgr, true, Some(ChunkPos::new(1, MAX_CHUNK_Y, 0)));
        mgr.recompute_light_for_test();
        assert!(!mgr.has_column_light(1, 0));

        mgr.queue_all_loaded_for_test();
        let center = ChunkPos::new(0, 0, 0);
        assert!(mgr.dirty_mesh_queue.contains(&center));

        // Not dispatched while one halo column has no light.
        mgr.update_streaming(ChunkPos::new(0, 0, 0));
        assert!(
            mgr.dirty_mesh_queue.contains(&center),
            "a chunk with an unlit halo column must stay queued"
        );
        assert!(!mgr.pending_mesh.contains(&center));

        // Completing the missing column lights it; dispatch may proceed.
        mgr.insert_chunk_for_test(
            ChunkPos::new(1, MAX_CHUNK_Y, 0),
            Chunk::new_uniform(BlockId::Air),
        );
        mgr.recompute_light_for_test();
        assert!(mgr.has_column_light(1, 0));
        mgr.update_streaming(ChunkPos::new(0, 0, 0));
        assert!(!mgr.dirty_mesh_queue.contains(&center));
        assert!(mgr.pending_mesh.contains(&center));
    }

    /// Fill every column the default spiral can ask for with uniform air so a
    /// pumped `update_streaming` loop does not spawn real terrain generation
    /// work; tests then overwrite the columns they care about.
    fn prefill_spiral_with_air(mgr: &mut WorldManager) {
        let offsets = mgr.spiral_offsets.clone();
        for (dx, dz) in offsets {
            for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                mgr.insert_chunk_for_test(
                    ChunkPos::new(dx, cy, dz),
                    Chunk::new_uniform(BlockId::Air),
                );
            }
        }
    }

    fn grids_for_column(cx: i32, cz: i32, grid: &LightGrid) -> FxHashMap<ChunkPos, LightGrid> {
        (MIN_CHUNK_Y..=MAX_CHUNK_Y)
            .map(|cy| (ChunkPos::new(cx, cy, cz), grid.clone()))
            .collect()
    }

    #[test]
    fn test_completed_column_light_is_deferred_to_worker() {
        let mut mgr = WorldManager::new(4);
        prefill_spiral_with_air(&mut mgr);
        // Make the centre column opaque enough that its light is not simply
        // the trivial all-sky answer.
        mgr.insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Stone));

        // The completion trigger must only enqueue: the frame thread never
        // runs the 3x3 light region inline.
        mgr.propagate_light_for_completed_column(0, 0);
        assert!(
            !mgr.has_column_light(0, 0),
            "light must not be computed inline"
        );
        assert_eq!(mgr.pending_light.len(), 1, "the column must be queued");

        // A later update dispatches the job to rayon; the grids land on a
        // subsequent update once the worker finishes.
        let center = ChunkPos::new(0, 0, 0);
        for _ in 0..3000 {
            mgr.update_streaming(center);
            if mgr.has_column_light(0, 0) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            mgr.has_column_light(0, 0),
            "the worker light result must be applied eventually"
        );
        assert!(mgr.has_light_neighborhood(0, 0));
        assert!(mgr.pending_light.is_empty());
        assert!(mgr.light_inflight.is_empty());
    }

    #[test]
    fn test_out_of_order_light_result_cannot_overwrite_newer_grid() {
        let mut mgr = WorldManager::new(4);
        insert_3x3_columns(&mut mgr, false, None);
        let complete: FxHashSet<(i32, i32)> = [(0, 0)].into_iter().collect();

        // A newer, correct result (fully lit) applies first.
        let newer = LightJobResult {
            trigger: (0, 0),
            seq: 2,
            epoch: mgr.light_epoch,
            grids: grids_for_column(0, 0, &LightGrid::full()),
            complete_columns: complete.clone(),
        };
        mgr.apply_light_result(newer);
        assert_eq!(mgr.light_at_world(5, 5, 5), light::MAX_SKY_LIGHT);

        // An older result whose snapshot still read a loading neighbour as air
        // would be over-lit; it must not overwrite the newer grids. It also
        // smuggles grids for a column its snapshot saw incomplete, which must
        // be dropped.
        let mut older_grids = grids_for_column(0, 0, &LightGrid::new());
        older_grids.extend(grids_for_column(1, 0, &LightGrid::full()));
        let older = LightJobResult {
            trigger: (0, 0),
            seq: 1,
            epoch: mgr.light_epoch,
            grids: older_grids,
            complete_columns: complete,
        };
        mgr.apply_light_result(older);

        assert_eq!(
            mgr.light_at_world(5, 5, 5),
            light::MAX_SKY_LIGHT,
            "an older sequence must not overwrite newer light"
        );
        assert!(
            !mgr.has_column_light(1, 0),
            "grids for columns the worker snapshot saw incomplete must be dropped"
        );
    }

    #[test]
    fn test_stale_epoch_light_result_is_discarded_and_requeued() {
        let mut mgr = WorldManager::new(4);
        insert_3x3_columns(&mut mgr, false, None);
        let stale = LightJobResult {
            trigger: (0, 0),
            seq: 1,
            epoch: mgr.light_epoch.wrapping_sub(1),
            grids: grids_for_column(0, 0, &LightGrid::full()),
            complete_columns: [(0, 0)].into_iter().collect(),
        };
        mgr.apply_light_result(stale);

        assert!(
            !mgr.has_column_light(0, 0),
            "a result that raced an edit must be discarded"
        );
        assert!(
            mgr.pending_light
                .iter()
                .any(|&(cx, cz, _)| (cx, cz) == (0, 0)),
            "the unlit column must be re-queued from a post-edit snapshot"
        );
    }

    #[test]
    fn test_light_diff_requeues_already_meshed_chunk() {
        let mut mgr = WorldManager::new(4);
        insert_3x3_columns(&mut mgr, false, None);
        mgr.recompute_light_for_test();

        let center = ChunkPos::new(0, 0, 0);
        assert_eq!(mgr.light_at_world(5, 5, 5), light::MAX_SKY_LIGHT);
        mgr.meshed_chunks.insert(center);
        mgr.dirty_mesh_queue.remove(&center);

        // A solid roof on the meshed chunk's own column shuts its sky shaft;
        // light now only bleeds in laterally, so the meshed chunk's grid changes
        // and its baked mesh is stale.
        mgr.insert_chunk_for_test(
            ChunkPos::new(0, MAX_CHUNK_Y, 0),
            Chunk::new_uniform(BlockId::Stone),
        );
        mgr.recompute_light_for_test();

        assert!(mgr.light_at_world(5, 5, 5) < light::MAX_SKY_LIGHT);
        assert!(
            mgr.dirty_mesh_queue.contains(&center),
            "a light change in a meshed chunk must re-queue it"
        );
    }

    #[test]
    fn test_loaded_edge_padded_halo_light_matches_grids() {
        // Unloads happen beyond render_distance + 2 while meshes exist only
        // within render_distance, so a rendered chunk always keeps its full
        // 3x3 halo ring resident; residual staleness fed by an unloaded column
        // therefore cannot reach a visible mesh.
        let mut mgr = WorldManager::new(4);
        insert_3x3_columns(&mut mgr, true, None);
        mgr.recompute_light_for_test();

        let center = ChunkPos::new(0, 0, 0);
        let padded = mgr.build_padded_chunk(center).expect("halo is resident");

        // Inner volume comes from the center chunk's own grid (stone chunk =
        // dark). The +X face halo comes from the air column at (1,0).
        assert_eq!(padded.light_at(1, 1, 1), 0);
        assert_eq!(padded.light_at(33, 1, 1), light::MAX_SKY_LIGHT);
        // +Y halo is open sky above the loaded world.
        assert_eq!(padded.light_at(17, 33, 17), light::MAX_SKY_LIGHT);

        // The 12 edge strips read the same world-space values as the manager
        // accessor, including the chunk-border seam cells.
        let cs = CHUNK_SIZE as i32;
        for &(px, py, pz) in &[
            (0usize, 0usize, 17usize),
            (cs as usize + 1, 0, 17),
            (17, 0, 0),
            (17, cs as usize + 1, 0),
            (0, 17, cs as usize + 1),
            (cs as usize + 1, 17, cs as usize + 1),
        ] {
            let wx = center.x * cs + px as i32 - 1;
            let wy = center.y * cs + py as i32 - 1;
            let wz = center.z * cs + pz as i32 - 1;
            assert_eq!(
                padded.light_at(px, py, pz),
                mgr.light_at_world(wx, wy, wz),
                "edge strip light at padded ({px},{py},{pz})"
            );
        }

        // Missing grids default to full sky light; outside the world the
        // physically meaningful values apply.
        assert_eq!(mgr.light_at_world(10_000, 10, 10_000), light::MAX_SKY_LIGHT);
        assert_eq!(mgr.light_at_world(0, (MAX_CHUNK_Y + 1) * cs, 0), 15);
        assert_eq!(mgr.light_at_world(0, MIN_CHUNK_Y * cs - 1, 0), 0);
    }

    /// Insert full columns for the 3x3 ring around (0,0) plus one extra column
    /// at x = 2. The extra column supplies the outer face neighbour of column
    /// (1,0), so a light-diff chunk there can be meshed synchronously inside
    /// `set_block_immediate`. Every chunk is handed to `build`, which lets a
    /// test hand-place overhangs and shafts before light is seeded.
    fn insert_light_edit_columns<F>(mgr: &mut WorldManager, mut build: F)
    where
        F: FnMut(ChunkPos) -> Chunk,
    {
        for dx in -1..=2 {
            for dz in -1..=1 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(dx, cy, dz);
                    mgr.insert_chunk_for_test(pos, build(pos));
                }
            }
        }
    }

    /// Build the shared overhang fixture: a stone roof at world y = 4 spanning
    /// world x 16..=47 (chunk columns 0 and 1) and all z of chunk columns 0..1.
    /// `hole` optionally opens one roof cell, letting sky light down a shaft.
    fn overhang_chunk(pos: ChunkPos, hole: Option<(i32, i32)>) -> Chunk {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        if pos.y != 0 || pos.z != 0 {
            return chunk;
        }
        let base_x = pos.x * CHUNK_SIZE as i32;
        for world_x in 16..=47 {
            let lx = world_x - base_x;
            if !(0..CHUNK_SIZE as i32).contains(&lx) {
                continue;
            }
            for lz in 0..CHUNK_SIZE {
                let is_hole = hole == Some((world_x, lz as i32));
                if !is_hole {
                    chunk.set(lx as usize, 4, lz, BlockId::Stone);
                }
            }
        }
        chunk
    }

    #[test]
    fn test_break_under_overhang_remeshes_non_adjacent_light_diff_chunk() {
        let mut mgr = WorldManager::new(4);
        insert_light_edit_columns(&mut mgr, |pos| overhang_chunk(pos, None));
        mgr.recompute_light_for_test();

        // The edit sits at local x = 24 of chunk (0,0,0), nowhere near a chunk
        // boundary, so the voxel-adjacency set is exactly {(0,0,0)}.
        let edit_chunk = world_to_chunk_and_local(24, 4, 15).0;
        assert_eq!(edit_chunk, ChunkPos::new(0, 0, 0));
        assert_eq!(mgr.light_at_world(24, 4, 15), 0, "roof cell is opaque");
        let before_neighbor = mgr.light_at_world(32, 3, 15);
        assert_eq!(
            before_neighbor, 0,
            "under-roof cells of the neighbor chunk start dark (17+ steps from the open edge)"
        );

        let remeshed = mgr.set_block_immediate(24, 4, 15, BlockId::Air);

        assert_eq!(
            mgr.light_at_world(24, 4, 15),
            light::MAX_SKY_LIGHT,
            "the freed cell opens a sky shaft"
        );
        assert!(
            mgr.light_at_world(32, 3, 15) > before_neighbor,
            "light bleeds into the neighbor chunk under the roof"
        );

        let positions: Vec<ChunkPos> = remeshed.iter().map(|(p, _)| *p).collect();
        assert!(positions.contains(&edit_chunk), "the edited chunk remeshes");
        assert!(
            positions.contains(&ChunkPos::new(1, 0, 0)),
            "the light-diff chunk must remesh despite not being voxel-adjacent: {positions:?}"
        );
    }

    #[test]
    fn test_place_into_open_shaft_darkens_cells_below_and_remeshes() {
        let mut mgr = WorldManager::new(4);
        insert_light_edit_columns(&mut mgr, |pos| overhang_chunk(pos, Some((30, 15))));
        mgr.recompute_light_for_test();

        // The hole at (30,4,15) feeds a shaft: cells below it near the hole are
        // lit, including cells in the neighboring chunk.
        assert_eq!(
            mgr.light_at_world(30, 3, 15),
            light::MAX_SKY_LIGHT,
            "the open shaft is fully sky-lit"
        );
        let before = mgr.light_at_world(32, 3, 15);
        assert!(before > 0, "lateral bleed reaches the neighbor chunk");

        let remeshed = mgr.set_block_immediate(30, 4, 15, BlockId::Stone);

        assert!(
            mgr.light_at_world(32, 3, 15) < before,
            "sealing the shaft darkens cells below it"
        );
        let positions: Vec<ChunkPos> = remeshed.iter().map(|(p, _)| *p).collect();
        assert!(positions.contains(&ChunkPos::new(0, 0, 0)));
        assert!(
            positions.contains(&ChunkPos::new(1, 0, 0)),
            "the affected light-diff chunk remeshes: {positions:?}"
        );
    }

    #[test]
    fn test_light_neutral_edit_returns_only_voxel_adjacency_remesh_set() {
        let mut mgr = WorldManager::new(4);
        insert_light_edit_columns(&mut mgr, |_| Chunk::new_uniform(BlockId::Stone));
        mgr.recompute_light_for_test();

        // A fully sealed world is zero-light everywhere. Swapping one opaque
        // block type for another cannot change light, so the remesh set must
        // stay exactly the pre-light (voxel-adjacency) set: the interior chunk.
        assert_eq!(mgr.light_at_world(5, 5, 5), 0);
        let remeshed = mgr.set_block_immediate(5, 5, 5, BlockId::Cobblestone);
        let positions: Vec<ChunkPos> = remeshed.iter().map(|(p, _)| *p).collect();
        assert_eq!(positions, vec![ChunkPos::new(0, 0, 0)]);
        assert_eq!(mgr.light_at_world(5, 5, 5), 0);
    }

    /// Recorded greedy-mesh cost of real sky light. The same bounded
    /// fixed-seed terrain region is meshed twice: once with propagated light
    /// and once with every grid overwritten to uniform 15. The delta measures
    /// how many extra quads per-corner light variation costs (and the strict
    /// `real > uniform` assertion catches light that accidentally went uniform
    /// or never reached the mesher at all).
    #[test]
    fn quad_count_real_vs_uniform_light_measurement() {
        const SEED: u32 = 1337;
        // Measured region: 5x5 chunk columns x 6 chunk layers = 150 chunks,
        // i.e. 160x160 blocks of terrain. One resident halo column ring
        // supplies the face neighbours `build_padded_chunk` needs; the outer
        // halo is not measured.
        const RADIUS: i32 = 2;
        const HALO: i32 = 1;

        let mut mgr = WorldManager::with_seed_and_edits(6, SEED, ChunkEditMap::default());
        for cx in -RADIUS - HALO..=RADIUS + HALO {
            for cz in -RADIUS - HALO..=RADIUS + HALO {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(cx, cy, cz);
                    mgr.insert_chunk_for_test(pos, generate_chunk_seeded(pos, SEED));
                }
            }
        }
        mgr.recompute_light_for_test();

        let measured: Vec<ChunkPos> = (-RADIUS..=RADIUS)
            .flat_map(|cx| {
                (-RADIUS..=RADIUS).flat_map(move |cz| {
                    (MIN_CHUNK_Y..=MAX_CHUNK_Y).map(move |cy| ChunkPos::new(cx, cy, cz))
                })
            })
            .collect();
        let mesh_total = |mgr: &WorldManager| -> usize {
            measured
                .iter()
                .map(|&pos| {
                    let padded = mgr
                        .build_padded_chunk(pos)
                        .expect("measured chunks keep their face-neighbour ring resident");
                    mesh_chunk(&padded).total_quads()
                })
                .sum()
        };

        let real = mesh_total(&mgr);

        // Uniform-light baseline: same geometry, every corner lit 15.
        for grid in mgr.light_grids.values_mut() {
            *grid = light::full_light_grid();
        }
        let uniform = mesh_total(&mgr);

        let ratio = real as f64 / uniform as f64;
        eprintln!(
            "sky-light quad measurement: {}x{} columns x {} layers = {} chunks; \
             real={} uniform={} ratio={:.3}",
            RADIUS * 2 + 1,
            RADIUS * 2 + 1,
            MAX_CHUNK_Y - MIN_CHUNK_Y + 1,
            measured.len(),
            real,
            uniform,
            ratio,
        );

        assert!(uniform > 0, "measurement region must contain visible faces");
        assert!(
            real > uniform,
            "per-corner light variation must split greedy merges: real {real} vs uniform {uniform}"
        );
        // Just above the measured ratio (113536 / 112424 = 1.010): real light
        // barely fragments this terrain, but the bound guards against a
        // runaway light-aware merge key while still catching uniform light.
        assert!(
            real * 4 <= uniform * 5,
            "light fragmentation must stay bounded (measured ratio 1.010): \
             real {real} vs uniform {uniform}"
        );
    }

    /// Frame-time harness for `WorldManager::update_streaming` (M007/S01).
    ///
    /// Ignored by default and release-only: the repo's tuned release profile
    /// (`opt-level = 3`, `lto = "thin"`, `codegen-units = 1`) is what ships, and
    /// debug-mode timings are not meaningful. Run it with:
    ///
    /// ```text
    /// cargo test --locked --release --lib -- --ignored streaming_budget
    /// ```
    ///
    /// It builds a real manager at `DEFAULT_RENDER_DISTANCE`, warms the initial
    /// ring up so the first measured step is not just cold-cache dispatch, then
    /// steps the streaming center `STEPS` chunks in +X and times each
    /// `update_streaming` call. Asserts the MEM229 dispatch targets (p95 ≤ 8 ms,
    /// max ≤ 16 ms) and prints sample count plus p50/p95/max/mean so the numbers
    /// can be recorded for the pre/post-rewrite comparison. Append `--nocapture`
    /// when the test passes: libtest hides the measurement line on success (a
    /// failing run always prints it with the captured output).
    ///
    /// Only the main-thread call is timed: terrain generation and greedy meshing
    /// stay on rayon workers, exactly as they do in the frame loop.
    #[test]
    #[ignore = "release-only frame-time harness: cargo test --locked --release --lib -- --ignored streaming_budget"]
    fn streaming_budget() {
        use crate::settings::DEFAULT_RENDER_DISTANCE;
        use std::time::{Duration, Instant};

        // More than the 32-chunk minimum crossing in the slice plan.
        const STEPS: usize = 40;
        const P95_BUDGET: Duration = Duration::from_millis(8);
        const MAX_BUDGET: Duration = Duration::from_millis(16);

        if cfg!(debug_assertions) {
            panic!(
                "streaming_budget measures release timings: rerun with \
                 `cargo test --locked --release --lib -- --ignored streaming_budget`"
            );
        }

        let mut mgr = WorldManager::new(DEFAULT_RENDER_DISTANCE);
        let start = ChunkPos::new(0, 0, 0);

        // Warm up so every measured step pays the steady-state dispatch cost
        // instead of the one-time cold fill of the initial ring. Bounded by both
        // the idle-poll count and the iteration cap so a slow machine cannot
        // hang the test.
        let warmup_start = Instant::now();
        let mut warmup_calls = 0usize;
        let mut idle_polls = 0usize;
        for _ in 0..2000 {
            mgr.update_streaming(start);
            warmup_calls += 1;
            if mgr.pending_gen.is_empty()
                && mgr.pending_mesh.is_empty()
                && mgr.pending_light.is_empty()
                && mgr.light_inflight.is_empty()
                && mgr.light_rx.is_empty()
            {
                idle_polls += 1;
                if idle_polls >= 4 {
                    break;
                }
            } else {
                idle_polls = 0;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let warmup_elapsed = warmup_start.elapsed();

        let mut samples: Vec<Duration> = Vec::with_capacity(STEPS);
        let mut meshes_completed = 0usize;
        for step in 1..=STEPS {
            let center = ChunkPos::new(start.x + step as i32, start.y, start.z);
            let t0 = Instant::now();
            let (meshes, _unloaded) = mgr.update_streaming(center);
            samples.push(t0.elapsed());
            meshes_completed += meshes.len();
            // Roughly one frame of slack so background workers keep feeding the
            // channels; the harness times only the main-thread call itself.
            std::thread::sleep(Duration::from_millis(2));
        }
        // Let the decoupled light/mesh pipeline settle after the measured
        // crossing. These calls are not timed; they only make the "pipeline
        // actually ran" negative control count the work the crossing started.
        for _ in 0..400 {
            let (meshes, _) =
                mgr.update_streaming(ChunkPos::new(start.x + STEPS as i32, 0, start.z));
            meshes_completed += meshes.len();
            if mgr.pending_light.is_empty()
                && mgr.light_inflight.is_empty()
                && mgr.light_rx.is_empty()
                && mgr.pending_mesh.is_empty()
                && mgr.dirty_mesh_queue.is_empty()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let loaded_chunks = mgr.loaded_chunk_count();

        let mut sorted = samples.clone();
        sorted.sort();
        let pct = |p: f64| -> Duration {
            let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
            sorted[idx]
        };
        let p50 = pct(0.50);
        let p95 = pct(0.95);
        let max = *sorted.last().expect("at least one measured step");
        let mean_ms =
            samples.iter().map(|d| d.as_secs_f64()).sum::<f64>() / samples.len() as f64 * 1000.0;

        eprintln!(
            "streaming_budget: samples={} warmup_calls={} warmup={:.2}s \
             p50={:.3}ms p95={:.3}ms max={:.3}ms mean={:.3}ms \
             meshes={} loaded={} targets: p95<={:.0}ms max<={:.0}ms (release, rd={})",
            samples.len(),
            warmup_calls,
            warmup_elapsed.as_secs_f64(),
            p50.as_secs_f64() * 1000.0,
            p95.as_secs_f64() * 1000.0,
            max.as_secs_f64() * 1000.0,
            mean_ms,
            meshes_completed,
            loaded_chunks,
            P95_BUDGET.as_secs_f64() * 1000.0,
            MAX_BUDGET.as_secs_f64() * 1000.0,
            DEFAULT_RENDER_DISTANCE,
        );

        assert_eq!(samples.len(), STEPS, "every step must be timed");
        // Negative control: the crossing must actually stream. A stalled rayon
        // pipeline (or an empty drain) would make every call trivially cheap, so
        // a green harness without this guard would be lying about the load it
        // measured.
        assert!(
            meshes_completed >= STEPS,
            "only {meshes_completed} completed meshes over {STEPS} steps: the streaming pipeline did not run"
        );
        // The 32-chunk/frame dispatch budget means a 40-chunk crossing only
        // streams a fraction of the rd=16 ring before the settling pump runs;
        // the pump then fills it. A floor of 1024 chunks proves the crossing
        // streamed a real ring, not just the warmup leftovers.
        assert!(
            loaded_chunks >= 1024,
            "only {loaded_chunks} chunks resident after settling a {STEPS}-chunk crossing at rd={DEFAULT_RENDER_DISTANCE}: \
             the harness did not exercise real streaming"
        );
        assert!(
            p95 <= P95_BUDGET,
            "p95 update_streaming cost {:.3}ms exceeds the {:.0}ms MEM229 budget: {sorted:?}",
            p95.as_secs_f64() * 1000.0,
            P95_BUDGET.as_secs_f64() * 1000.0,
        );
        assert!(
            max <= MAX_BUDGET,
            "max update_streaming cost {:.3}ms exceeds the {:.0}ms MEM229 budget: {sorted:?}",
            max.as_secs_f64() * 1000.0,
            MAX_BUDGET.as_secs_f64() * 1000.0,
        );
    }
}
