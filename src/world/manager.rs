use std::collections::VecDeque;

use crossbeam_channel::{unbounded, Receiver, Sender};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::mesh::greedy::{mesh_chunk, PaddedChunk};
use crate::mesh::quad::ChunkMesh;
use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, world_to_chunk_and_local, Chunk, ChunkPos, CHUNK_SIZE};
use crate::world::light::{self, LightGrid};
use crate::world::persistence::ChunkEditMap;
use crate::world::season::Season;
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
    /// Season the resident chunk contents were generated under.
    ///
    /// Chunks are persisted only as `chunk_edits`, so every resident chunk was
    /// generated under exactly one season: this single value keys all
    /// generation staleness. A change starts one rollover wave.
    generation_season: Season,
    /// Target season of an in-flight rollover wave (`None` at rest).
    season_rollover_target: Option<Season>,
    /// Resident positions awaiting seasonal regeneration for the in-flight
    /// wave, nearest-to-center first, so the player's surroundings change
    /// before the horizon does.
    season_rollover: VecDeque<ChunkPos>,
    /// Positions whose in-flight generation result is a seasonal replacement
    /// rather than a first load. Exactly the rollover dispatches, so a result
    /// is only ever swapped in place when the wave produced it.
    season_rollover_inflight: FxHashSet<ChunkPos>,
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

/// Cap on generation jobs in flight (player-driven loads and seasonal rollover
/// replacements share it, so a wave can never flood the pool).
const PENDING_GEN_CAP: usize = 96;

/// Per-frame ceiling for seasonal rollover replacements. Named to mirror
/// `LIGHT_DISPATCH_BUDGET`; 32 matches the generation dispatch cap, so a
/// season change is spread over at least ceil(resident / 32) frames instead of
/// remeshing the whole world in one frame.
const SEASON_ROLLOVER_BUDGET: usize = 32;

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
            // Blanket-absent baseline, matching terrain.rs's unseeded
            // `generate_chunk` default: a fresh world starts in Summer and its
            // first season rollover is planned against that.
            generation_season: Season::Summer,
            season_rollover_target: None,
            season_rollover: VecDeque::new(),
            season_rollover_inflight: FxHashSet::default(),
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

    /// Season the resident chunk contents were generated under. Advances only
    /// when a rollover wave drains; diagnostics and tests read it to prove a
    /// wave actually completed.
    #[inline]
    pub fn generation_season(&self) -> Season {
        self.generation_season
    }

    /// True while `pos` has resident block data. The rollover contract is that
    /// a resident position is never removed first, so this stays true across a
    /// seasonal swap (no Air gap for physics or edits).
    #[inline]
    pub fn is_resident(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
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

    /// The season newly generated chunks must use: the target of an in-flight
    /// rollover, or the settled generation season at rest.
    ///
    /// A chunk that loads while a wave is in flight must land already in the
    /// target season. Otherwise it would be silently stale: the wave snapshot
    /// predates it (so it is never queued) and `generation_season` advances
    /// when the queue drains.
    fn current_gen_season(&self) -> Season {
        self.season_rollover_target
            .unwrap_or(self.generation_season)
    }

    /// Start one rollover wave when the requested season differs from the one
    /// the resident content was generated under.
    ///
    /// Nothing is dropped from `self.chunks`: the wave replaces chunks in place
    /// through the normal generation queue. While a wave is in flight a second
    /// season change is ignored; once it drains, the next call sees the new
    /// mismatch and starts another wave (so a fast calendar costs bounded
    /// waves, never a one-frame full-world remesh).
    fn begin_season_rollover_if_needed(&mut self, center_chunk: ChunkPos, season: Season) {
        if season == self.generation_season || self.season_rollover_target.is_some() {
            return;
        }
        let mut positions: Vec<ChunkPos> = self.chunks.keys().copied().collect();
        // `dirty_mesh_queue` is an unordered set, so the wave cannot inherit an
        // order from it: sort explicitly, nearest-first by the same dist² key
        // the mesh dispatch uses (x/z only, so a whole column travels
        // together), with a total-order tie-break for reproducibility.
        positions.sort_by_key(|pos| {
            let dx = pos.x - center_chunk.x;
            let dz = pos.z - center_chunk.z;
            (dx * dx + dz * dz, pos.x, pos.z, pos.y)
        });
        self.season_rollover = positions.into();
        self.season_rollover_target = Some(season);
    }

    /// Dispatch up to [`SEASON_ROLLOVER_BUDGET`] seasonal replacements per
    /// frame through the existing `gen_tx`/`pending_gen` machinery, so their
    /// results ride the ordinary generation drain and never a separate path.
    fn dispatch_season_rollover(&mut self) {
        let Some(target) = self.season_rollover_target else {
            return;
        };
        let seed = self.world_seed;
        let mut dispatched = 0usize;
        while dispatched < SEASON_ROLLOVER_BUDGET && self.pending_gen.len() < PENDING_GEN_CAP {
            let Some(pos) = self.season_rollover.pop_front() else {
                break;
            };
            // Unloaded since the snapshot, or already covered by a player-
            // driven generation dispatch: never double-track a position. A
            // dropped position is harmless either way -- if it reloads it is
            // generated in the target season via `current_gen_season`.
            if !self.chunks.contains_key(&pos) || self.pending_gen.contains(&pos) {
                continue;
            }
            self.pending_gen.insert(pos);
            self.season_rollover_inflight.insert(pos);
            let tx = self.gen_tx.clone();
            rayon::spawn(move || {
                let chunk = generate_chunk_seeded(pos, seed, target);
                let _ = tx.send((pos, chunk));
            });
            dispatched += 1;
        }
        if self.season_rollover.is_empty() {
            // The queue has drained: resident content is now (or is arriving
            // as) the target season, and the settle point is where staleness
            // keying moves. Steady-state streaming then resumes unchanged: a
            // stable season never re-enters this path.
            self.generation_season = target;
            self.season_rollover_target = None;
        }
    }

    /// Replace a resident chunk with its seasonal regeneration, in place.
    ///
    /// The old chunk is overwritten rather than removed first, so a resident
    /// position never reads as Air: `get_block` cannot drop the player through
    /// the floor, `set_block_immediate` cannot silently no-op on a missing
    /// chunk, and the GPU keeps drawing the old mesh until the new one lands.
    /// The caller has already re-applied `chunk_edits`, so edits always win:
    /// mined blocks stay mined while the surroundings turn white.
    fn swap_resident_chunk(&mut self, pos: ChunkPos, chunk: Chunk) {
        self.chunks.insert(pos, chunk);

        // Sky light is derived from these blocks. Drop the whole (x, z)
        // column BEFORE asking for light: `propagate_light_for_completed_column`
        // early-returns while `has_column_light` is true, so dropping afterwards
        // would leave the wave permanently unlit.
        for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
            self.light_grids.remove(&ChunkPos::new(pos.x, cy, pos.z));
        }
        if self.column_is_complete(pos.x, pos.z) {
            self.force_enqueue_light_column(pos.x, pos.z);
        }
        // An incomplete column has no grids to drop and re-lights itself when
        // it completes via the column-ready trigger.

        // The old mesh is stale; drop it and let the wave re-mesh once the
        // column's fresh light lands (the dispatch gate checks the 3x3 light
        // neighborhood). The global `light_epoch` is deliberately NOT bumped:
        // that would discard the whole in-flight light pipeline, while the
        // per-column sequence guard is already sufficient.
        self.pending_mesh.remove(&pos);
        self.meshed_chunks.remove(&pos);
        self.dirty_mesh_queue.insert(pos);
    }

    /// Enqueue a fresh light job for one column even when an older job is still
    /// in flight, and floor `light_applied_seq` at the fresh sequence so the
    /// older snapshot can never win on arrival.
    ///
    /// The edit path can afford the global `light_epoch` bump because it
    /// recomputes synchronously. The rollover must stay inside the per-frame
    /// light budgets, so it relies on the per-column sequence instead -- the
    /// same guard that already resolves out-of-order worker completion. The
    /// floor is the fresh sequence itself, so the fresh result (equal, not
    /// older) is still accepted.
    fn force_enqueue_light_column(&mut self, cx: i32, cz: i32) {
        self.next_light_seq += 1;
        let seq = self.next_light_seq;
        self.light_applied_seq.insert((cx, cz), seq);
        self.pending_light.push_back((cx, cz, seq));
        self.queued_light.insert((cx, cz));
    }

    pub fn update_streaming(
        &mut self,
        center_chunk: ChunkPos,
        season: Season,
    ) -> (Vec<(ChunkPos, ChunkMesh)>, Vec<ChunkPos>) {
        let mut completed_meshes = Vec::new();
        let mut unloaded = Vec::new();

        // 0. Season staleness: a requested season that differs from the one the
        //    resident content was generated under starts one bounded rollover
        //    wave. Nothing is dropped; replacements ride the normal gen queue.
        self.begin_season_rollover_if_needed(center_chunk, season);

        // 1. Drain up to 48 generated chunks per frame
        for _ in 0..48 {
            let Ok((pos, mut chunk)) = self.gen_rx.try_recv() else {
                break;
            };
            self.pending_gen.remove(&pos);
            // Only a rollover dispatch targets an already-resident position;
            // track its own in-flight set so a first load can never be
            // mistaken for a replacement (and vice versa).
            let replacement =
                self.season_rollover_inflight.remove(&pos) && self.chunks.contains_key(&pos);
            if let Some(edits) = self.chunk_edits.get(&pos) {
                // Edits are applied to every generation result: they always win
                // over the season.
                for (&idx, &block) in edits {
                    chunk.set_by_index(idx, block);
                }
                chunk.try_compress();
            }
            if replacement {
                self.swap_resident_chunk(pos, chunk);
            } else {
                self.chunks.insert(pos, chunk);

                // Column-ready trigger: this chunk may be the last piece of a
                // full 192-block column. Only enqueue here; the light BFS runs
                // on a rayon worker and lands on a later frame.
                self.propagate_light_for_completed_column(pos.x, pos.z);
            }

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
            // Positions that left the world during a wave are dropped from the
            // queue: if they ever reload they are generated in the target
            // season through `current_gen_season`.
            self.season_rollover.retain(|&pos| {
                let dx = pos.x - center_chunk.x;
                let dz = pos.z - center_chunk.z;
                dx * dx + dz * dz <= unload_r2
            });
        }

        // 5. Seasonal rollover wave: replace at most SEASON_ROLLOVER_BUDGET
        //    resident chunks per frame through the same generation queue, ahead
        //    of new chunk loads so a wave is never starved by a crossing.
        self.dispatch_season_rollover();

        // 6. Dispatch new terrain generation tasks in spiral order around center_chunk
        let mut dispatched_gen = 0;
        let seed = self.world_seed;
        // Chunks generated while a wave is in flight must land in the target
        // season, or they would be stale the moment the wave drains.
        let gen_season = self.current_gen_season();
        'spiral: for &(dx, dz) in &self.spiral_offsets {
            if self.pending_gen.len() >= PENDING_GEN_CAP || dispatched_gen >= 32 {
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
                        let chunk = generate_chunk_seeded(pos, seed, gen_season);
                        let _ = tx.send((pos, chunk));
                    });
                    dispatched_gen += 1;
                    if dispatched_gen >= 32 {
                        break 'spiral;
                    }
                }
            }
        }

        // 7. Fast-clear any Uniform(Air) chunks from dirty_mesh_queue before counting the 24-chunk frame budget
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
            // Fast path on main thread: if padded chunk has no solid and no
            // liquid voxels, skip worker dispatch. Water must be included so a
            // water-bearing (or water-only) chunk is still meshed.
            if !padded.has_any_solid && !padded.has_any_liquid {
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

        // 8. Hand queued columns to rayon last, so snapshot cloning (up to 54
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

    /// Serialises the tests that depend on the shared global rayon pool. The
    /// ocean-crossing acceptance test deliberately saturates that pool (a full
    /// rd=4 ring of debug-mode light jobs), and the streaming tests below prove
    /// worker results land inside bounded retry windows. Holding this lock keeps
    /// those windows meaningful instead of racing a load-generating sibling.
    static HEAVY_STREAMING_POOL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Acquire the heavy-streaming lock, ignoring poisoning: a panic in a
    /// sibling test is reported by that test, not by every later tenant.
    fn lock_heavy_streaming() -> std::sync::MutexGuard<'static, ()> {
        HEAVY_STREAMING_POOL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
        let (completed, _) = mgr.update_streaming(cpos, Season::Summer);
        assert!(
            !completed.iter().any(|(p, _)| *p == cpos),
            "stale in-flight mesh must be ignored after set_block_immediate"
        );
    }

    #[test]
    fn test_chunk_edits_persist_across_unload_and_regeneration() {
        let _heavy = lock_heavy_streaming();
        let mut mgr = WorldManager::with_seed_and_edits(4, 1337, ChunkEditMap::default());
        let c0 = ChunkPos::new(0, 1, 0);
        let c_neg_x = ChunkPos::new(-1, 1, 0);
        for dx in -2..=1 {
            for dy in 0..=2 {
                for dz in -1..=1 {
                    let p = ChunkPos::new(dx, dy, dz);
                    mgr.insert_chunk_for_test(p, generate_chunk_seeded(p, 1337, Season::Summer));
                }
            }
        }

        // Place a Brick block at wx = 0, wy = 40, wz = 5 (local x = 0 of chunk (0, 1, 0))
        mgr.set_block_immediate(0, 40, 5, BlockId::Brick);
        assert!(mgr.has_unsaved_changes());
        assert_eq!(mgr.get_block(0, 40, 5), BlockId::Brick);

        // Move player far away (chunk 100, 1, 100) to force unloading chunk (0, 1, 0)
        let (_, unloaded) = mgr.update_streaming(ChunkPos::new(100, 1, 100), Season::Summer);
        assert!(unloaded.contains(&c0));
        assert_eq!(mgr.get_block(0, 40, 5), BlockId::Air);

        // Move back to (0, 1, 0) and pump streaming until (0, 1, 0) regenerates.
        // The pump is deadline-based because the debug lib suite runs ~600 tests
        // in parallel and the shared rayon pool can take far longer than the
        // isolated round-trip; the iteration cap keeps it finite if a real
        // regression ever stops the chunk from arriving.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut pump = 0usize;
        while mgr.get_block(0, 40, 5) != BlockId::Brick
            && pump < 10_000
            && std::time::Instant::now() < deadline
        {
            mgr.update_streaming(c0, Season::Summer);
            pump += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            mgr.get_block(0, 40, 5),
            BlockId::Brick,
            "the edited chunk must regenerate with its edit applied inside the bounded pump \
             window ({pump} polls)"
        );
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

        mgr.update_streaming(ChunkPos::new(0, 0, 0), Season::Summer);
        assert_eq!(mgr.dirty_mesh_queue_len(), 0);
    }

    // -----------------------------------------------------------------------
    // M007/S05-T02: season-keyed generation staleness and the bounded
    // swap-on-arrival rollover wave.
    // -----------------------------------------------------------------------

    /// 42 resident chunks (14 columns x 3 vertical levels), all inside an rd=4
    /// manager's unload radius, used as the rollover snapshot.
    fn insert_rollover_resident(mgr: &mut WorldManager) -> Vec<ChunkPos> {
        let mut positions = Vec::new();
        for dx in -3..=3 {
            for dz in 0..=1 {
                for cy in 0..=2 {
                    let pos = ChunkPos::new(dx, cy, dz);
                    mgr.insert_chunk_for_test(pos, Chunk::new_uniform(BlockId::Stone));
                    positions.push(pos);
                }
            }
        }
        positions
    }

    #[test]
    fn test_season_rollover_is_bounded_per_frame_and_keeps_every_chunk_resident() {
        let mut mgr = WorldManager::new(4);
        let resident = insert_rollover_resident(&mut mgr);
        assert_eq!(resident.len(), 42);
        assert_eq!(mgr.generation_season(), Season::Summer);
        let center = ChunkPos::new(0, 0, 0);

        // A season mismatch enqueues the whole resident set but dispatches only
        // one bounded slice: never a one-frame full-world remesh.
        mgr.update_streaming(center, Season::Winter);
        assert_eq!(mgr.season_rollover_target, Some(Season::Winter));
        assert_eq!(
            mgr.season_rollover.len(),
            resident.len() - SEASON_ROLLOVER_BUDGET,
            "the first call must dispatch exactly one bounded slice, not the whole world"
        );
        assert_eq!(
            mgr.generation_season(),
            Season::Summer,
            "the generation season settles only once the queue has drained"
        );
        for pos in &resident {
            assert!(mgr.is_resident(*pos), "resident chunk {pos:?} went absent");
        }

        // The wave drains over subsequent calls. No position may ever go absent:
        // a swap replaces contents in place, so physics and edits never see Air.
        let mut calls = 1usize;
        while mgr.generation_season() != Season::Winter && calls < 256 {
            mgr.update_streaming(center, Season::Winter);
            calls += 1;
            for pos in &resident {
                assert!(mgr.is_resident(*pos), "resident chunk {pos:?} went absent");
            }
        }
        assert_eq!(
            mgr.generation_season(),
            Season::Winter,
            "the rollover must settle on the requested season ({calls} calls)"
        );
        assert!(mgr.season_rollover_target.is_none());
        assert!(mgr.season_rollover.is_empty());

        // Steady state: once the wave has fully arrived, a stable season
        // re-dispatches nothing for resident chunks.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while resident.iter().any(|pos| mgr.pending_gen.contains(pos))
            && std::time::Instant::now() < deadline
        {
            mgr.update_streaming(center, Season::Winter);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        mgr.update_streaming(center, Season::Winter);
        assert!(mgr.season_rollover_target.is_none());
        assert!(mgr.season_rollover.is_empty());
        assert!(
            mgr.pending_gen.iter().all(|pos| !mgr.is_resident(*pos)),
            "a stable season must never re-dispatch a resident chunk for generation"
        );
    }

    /// The first two block positions where Summer and Winter disagree inside one
    /// chunk, searched over T01's known-land witness region for seed 1.
    /// Returns `(chunk, witness, second)` with both positions and their Summer
    /// and Winter block ids.
    #[allow(clippy::type_complexity)]
    fn find_seasonal_witness(
        seed: u32,
    ) -> (
        ChunkPos,
        ((i32, i32, i32), BlockId, BlockId),
        ((i32, i32, i32), BlockId, BlockId),
    ) {
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in 0..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let summer = generate_chunk_seeded(pos, seed, Season::Summer);
                    let winter = generate_chunk_seeded(pos, seed, Season::Winter);
                    let mut found = Vec::new();
                    for ly in 0..CHUNK_SIZE {
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let s = summer.get(lx, ly, lz);
                                let w = winter.get(lx, ly, lz);
                                if s != w {
                                    let world = (
                                        cx * CHUNK_SIZE as i32 + lx as i32,
                                        cy * CHUNK_SIZE as i32 + ly as i32,
                                        cz * CHUNK_SIZE as i32 + lz as i32,
                                    );
                                    found.push((world, s, w));
                                    if found.len() == 2 {
                                        return (pos, found[0], found[1]);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        panic!("seed {seed} has no seasonal surface change in the witness region");
    }

    #[test]
    fn test_seasonal_swap_reapplies_edits_and_replaces_content_in_place() {
        let _heavy = lock_heavy_streaming();
        let seed = 1;
        let (
            witness_chunk,
            (edit_pos, summer_at_edit, winter_at_edit),
            (probe, summer_probe, winter_probe),
        ) = find_seasonal_witness(seed);
        assert_ne!(summer_at_edit, winter_at_edit);
        assert_ne!(summer_probe, winter_probe);

        let mut mgr = WorldManager::with_seed_and_edits(4, seed, ChunkEditMap::default());
        let mut column = Vec::new();
        for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
            let pos = ChunkPos::new(witness_chunk.x, cy, witness_chunk.z);
            mgr.insert_chunk_for_test(pos, generate_chunk_seeded(pos, seed, Season::Summer));
            column.push(pos);
        }

        // Mine the witness surface block. The edit is recorded in `chunk_edits`,
        // which is the only persisted per-chunk state, so the seasonal swap must
        // re-apply it: edits always win over the season.
        mgr.set_block_immediate(edit_pos.0, edit_pos.1, edit_pos.2, BlockId::Brick);
        assert_eq!(
            mgr.get_block(edit_pos.0, edit_pos.1, edit_pos.2),
            BlockId::Brick
        );
        assert_eq!(
            mgr.get_block(probe.0, probe.1, probe.2),
            summer_probe,
            "the untagged probe still carries Summer content before the wave"
        );

        let center = ChunkPos::new(witness_chunk.x, 0, witness_chunk.z);
        mgr.update_streaming(center, Season::Winter);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while mgr.get_block(probe.0, probe.1, probe.2) == summer_probe
            && std::time::Instant::now() < deadline
        {
            mgr.update_streaming(center, Season::Winter);
            std::thread::sleep(std::time::Duration::from_millis(2));
            for pos in &column {
                assert!(
                    mgr.is_resident(*pos),
                    "the wave removed {pos:?} instead of swapping it in place"
                );
            }
        }

        assert_eq!(
            mgr.get_block(probe.0, probe.1, probe.2),
            winter_probe,
            "the swapped chunk did not carry Winter content at ({}, {}, {})",
            probe.0,
            probe.1,
            probe.2
        );
        assert_eq!(
            mgr.get_block(edit_pos.0, edit_pos.1, edit_pos.2),
            BlockId::Brick,
            "a seasonal swap must never discard a mined block"
        );

        // Byte-level pin: the swapped chunk is exactly the Winter generation
        // with the single persisted edit layered back on top.
        let winter_chunk = generate_chunk_seeded(witness_chunk, seed, Season::Winter);
        let (_, edit_local) = world_to_chunk_and_local(edit_pos.0, edit_pos.1, edit_pos.2);
        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let (wx, wy, wz) = (
                        witness_chunk.x * CHUNK_SIZE as i32 + lx as i32,
                        witness_chunk.y * CHUNK_SIZE as i32 + ly as i32,
                        witness_chunk.z * CHUNK_SIZE as i32 + lz as i32,
                    );
                    let expected = if (lx, ly, lz) == edit_local {
                        BlockId::Brick
                    } else {
                        winter_chunk.get(lx, ly, lz)
                    };
                    assert_eq!(
                        mgr.get_block(wx, wy, wz),
                        expected,
                        "seasonal swap content at ({wx}, {wy}, {wz})"
                    );
                }
            }
        }

        // Light must be rebuilt for the swapped column. Grids are dropped first
        // because the column-ready trigger early-returns while they exist; the
        // per-column sequence guard makes the post-swap job win over any
        // pre-swap snapshot that is still in flight.
        let key = (witness_chunk.x, witness_chunk.z);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !(mgr.has_column_light(key.0, key.1)
            && !mgr.light_inflight.contains(&key)
            && !mgr.queued_light.contains(&key))
            && std::time::Instant::now() < deadline
        {
            mgr.update_streaming(center, Season::Winter);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            mgr.has_column_light(key.0, key.1),
            "the swapped column never relit: its grids were not dropped before the trigger"
        );
    }

    #[test]
    fn test_forced_column_light_outranks_an_older_in_flight_snapshot() {
        let _heavy = lock_heavy_streaming();
        let mut mgr = WorldManager::new(4);
        insert_3x3_columns(&mut mgr, false, None);

        // An older light job for the column is already in flight, carrying a
        // grid computed from pre-swap blocks.
        let old_seq = 7u64;
        mgr.next_light_seq = old_seq;
        mgr.light_inflight.insert((0, 0));
        let stale = LightJobResult {
            trigger: (0, 0),
            seq: old_seq,
            epoch: mgr.light_epoch,
            grids: grids_for_column(0, 0, &LightGrid::new()),
            complete_columns: [(0, 0)].into_iter().collect(),
        };

        // The seasonal swap drops the column's grids and forces a fresh job.
        mgr.force_enqueue_light_column(0, 0);
        assert!(!mgr.has_column_light(0, 0));
        assert_eq!(
            mgr.pending_light.back().map(|&(_, _, seq)| seq),
            Some(old_seq + 1),
            "a forced column job must bypass the in-flight de-dupe"
        );

        // The stale snapshot arrives: it must be rejected, not stored.
        mgr.apply_light_result(stale);
        assert!(
            !mgr.has_column_light(0, 0),
            "a pre-swap light snapshot must never be stored"
        );
        assert_eq!(mgr.light_at_world(5, 5, 5), light::MAX_SKY_LIGHT);
        assert_eq!(
            mgr.light_applied_seq.get(&(0, 0)).copied(),
            Some(old_seq + 1),
            "the applied-sequence floor must survive the stale arrival"
        );

        // The forced job then lands on a worker and relights the column.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !mgr.has_column_light(0, 0) && std::time::Instant::now() < deadline {
            mgr.dispatch_light_jobs();
            mgr.drain_light_results();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            mgr.has_column_light(0, 0),
            "the forced column job must relight the swapped column"
        );
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
        mgr.update_streaming(ChunkPos::new(0, 0, 0), Season::Summer);
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
        mgr.update_streaming(ChunkPos::new(0, 0, 0), Season::Summer);
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
        let _heavy = lock_heavy_streaming();
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
        // subsequent update once the worker finishes. Deadline-based because a
        // single debug-mode 3x3 light region takes about a second in isolation
        // and the debug lib suite shares the rayon pool across ~600 parallel
        // tests, so a fixed 3 s iteration window is too tight; the iteration cap
        // still bounds the wait.
        let center = ChunkPos::new(0, 0, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut pump = 0usize;
        while !mgr.has_column_light(0, 0) && pump < 20_000 && std::time::Instant::now() < deadline {
            mgr.update_streaming(center, Season::Summer);
            pump += 1;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            mgr.has_column_light(0, 0),
            "the worker light result must be applied eventually ({pump} bounded polls)"
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
                    mgr.insert_chunk_for_test(
                        pos,
                        generate_chunk_seeded(pos, SEED, Season::Summer),
                    );
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

    /// A chunk whose only content is water must still be dispatched through
    /// the mesher: the `!has_any_solid` fast skip used to drop it silently.
    #[test]
    fn test_water_only_chunk_is_dispatched_and_meshes_water_quads() {
        let _heavy = lock_heavy_streaming();
        let center = ChunkPos::new(0, 0, 0);
        let mut mgr = WorldManager::new(4);
        for dx in -1..=1 {
            for dz in -1..=1 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(dx, cy, dz);
                    let chunk = if pos == center {
                        Chunk::new_uniform(BlockId::Water)
                    } else {
                        Chunk::new_uniform(BlockId::Air)
                    };
                    mgr.insert_chunk_for_test(pos, chunk);
                }
            }
        }
        mgr.recompute_light_for_test();

        let padded = mgr
            .build_padded_chunk(center)
            .expect("the center keeps its full face-neighbour ring");
        assert!(padded.has_any_liquid, "water reaches the padded snapshot");
        assert!(!padded.has_any_solid, "water is not solid");
        let direct = mesh_chunk(&padded);
        assert!(!direct.is_empty(), "a water-only mesh is not empty");
        assert_eq!(
            direct.total_quads(),
            0,
            "no water quad enters an opaque bucket"
        );
        assert!(direct.water_quad_count() > 0, "water faces are emitted");

        mgr.queue_all_loaded_for_test();
        mgr.update_streaming(center, Season::Summer);
        assert!(
            mgr.pending_mesh.contains(&center),
            "the water-only chunk must not be dropped by the no-solid fast skip"
        );

        // Deadline-based for the same reason as the persistence pump: the debug
        // suite's parallel load can delay a shared-pool worker well past the
        // isolated cost. The iteration cap keeps the wait finite.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut result = None;
        let mut pump = 0usize;
        while result.is_none() && pump < 10_000 && std::time::Instant::now() < deadline {
            let (completed, _) = mgr.update_streaming(center, Season::Summer);
            if let Some((_, mesh)) = completed.iter().find(|(pos, _)| *pos == center) {
                result = Some(mesh.clone());
            }
            pump += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mesh = result.unwrap_or_else(|| {
            panic!("the water-only chunk must publish a mesh inside the bounded pump window ({pump} polls)")
        });
        assert!(!mesh.is_empty(), "the published water mesh is not empty");
        assert_eq!(mesh.total_quads(), 0, "the opaque path stays untouched");
        assert!(
            mesh.water_quad_count() > 0,
            "the published mesh carries water"
        );
    }

    /// The uniform-Air fast clear must keep firing only for `is_uniform() ==
    /// Air`; a water-bearing chunk is not Air and must never be cleared as if
    /// it had nothing to draw.
    #[test]
    fn test_uniform_air_fast_clear_ignores_water_chunks() {
        let center = ChunkPos::new(0, 0, 0);
        let mut mgr = WorldManager::new(4);
        for dx in -1..=1 {
            for dz in -1..=1 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(dx, cy, dz);
                    mgr.insert_chunk_for_test(pos, Chunk::new_uniform(BlockId::Air));
                }
            }
        }
        // Air twin: the fast clear marks it meshed without a worker dispatch.
        mgr.queue_all_loaded_for_test();
        mgr.update_streaming(center, Season::Summer);
        assert!(
            mgr.meshed_chunks.contains(&center),
            "a uniform-Air chunk is still fast-cleared"
        );
        assert!(
            !mgr.pending_mesh.contains(&center),
            "a uniform-Air chunk is not dispatched"
        );

        // Water twin: same position, uniform Water instead of Air. It must not
        // be swallowed by the Air fast clear.
        let mut water_mgr = WorldManager::new(4);
        for dx in -1..=1 {
            for dz in -1..=1 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(dx, cy, dz);
                    let chunk = if pos == center {
                        Chunk::new_uniform(BlockId::Water)
                    } else {
                        Chunk::new_uniform(BlockId::Air)
                    };
                    water_mgr.insert_chunk_for_test(pos, chunk);
                }
            }
        }
        water_mgr.recompute_light_for_test();
        water_mgr.queue_all_loaded_for_test();
        water_mgr.update_streaming(center, Season::Summer);
        assert!(
            water_mgr.pending_mesh.contains(&center),
            "a uniform-Water chunk is dispatched, not fast-cleared"
        );
    }

    /// Worldgen water never becomes a `ChunkEditMap` entry: generating a real
    /// ocean region leaves the edit map empty, and T02 already rejects Water on
    /// every persistence path.
    #[test]
    fn test_generated_water_never_enters_chunk_edits() {
        let mut mgr = WorldManager::with_seed_and_edits(4, 1337, ChunkEditMap::default());
        let mut water_cells = 0usize;
        for cx in -3..=3 {
            for cz in -3..=3 {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, 1337, Season::Summer);
                    for ly in 0..CHUNK_SIZE {
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                if chunk.get(lx, ly, lz) == BlockId::Water {
                                    water_cells += 1;
                                }
                            }
                        }
                    }
                    mgr.insert_chunk_for_test(pos, chunk);
                }
            }
        }
        assert!(water_cells > 0, "the generated region must contain water");
        assert!(
            mgr.chunk_edits().is_empty(),
            "worldgen water must never create an edit entry"
        );
    }

    /// Headless ocean-crossing acceptance test (M007/S03-T02).
    ///
    /// Drives the exact production physics seam and per-frame order from the
    /// playing frame in `src/main.rs`: `Player::update` first, then
    /// `WorldManager::update_streaming` around the player's new chunk. The
    /// player falls into a generated ocean, floats near the surface, rises with
    /// `jump_or_up`, and swims across to the far shore, where it finishes
    /// standing on land above `SEA_LEVEL`.
    ///
    /// Before the run, the crossing is bounded by a pure, chunk-free scan: seed
    /// 12345 has an Ocean column at (0, 0) with h = 14 (S01), and the first
    /// column at or above `SEA_LEVEL` along the +Z axis is the far shore. The
    /// whole route is then known to be Ocean, so no assertion chases a moving
    /// target and no chunk has to exist to compute the bounds.
    ///
    /// This is the slice's highest-risk proof because it couples three things:
    /// the T01 swim constants, worldgen Water residency, and streaming ahead of
    /// the player. `WorldManager::get_block` returns Air for a non-resident
    /// chunk, so a lagging pipeline silently removes buoyancy; every frame runs
    /// behind a bounded residency gate, and the float/rise assertions fail
    /// loudly if the body approaches the seabed instead of the surface.
    ///
    /// Determinism and bounds: fixed `dt = 1/60`, an explicit frame budget per
    /// phase, and bounded polling for both the warm-up and the per-frame gate,
    /// so the test can never hang. Every panic names the stage (bounds,
    /// warm-up, entry, float, rise, crossing, streaming-gate), the seed, the
    /// axis, the crossing distance and the relevant frame/poll budget, so a
    /// regression pinpoints which stage stalled.
    ///
    /// Honesty note: no fall-damage or health claim is made here — no
    /// health/fall/damage system exists in `src/`; fall damage and drowning are
    /// deferred to M008 per MEM227. The assertions are mechanics only:
    /// positions, buoyancy and `on_ground`.
    #[test]
    fn test_acceptance_swim_ocean_crossing() {
        let _heavy = lock_heavy_streaming();
        use crate::player::physics::{InputState, Player};
        use crate::world::terrain::{biome_at_seeded, terrain_height_at_seeded, Biome, SEA_LEVEL};
        use std::time::Duration;

        const SEED: u32 = 12345;
        /// Forward axis for the crossing: +Z (`CROSSING_AXIS`).
        const AXIS_STEP: i32 = 1;
        const CROSSING_AXIS: &str = "+Z";
        const RENDER_DISTANCE: i32 = 4;
        const DT: f32 = 1.0 / 60.0;
        const WARMUP_POLL_CAP: usize = 20_000;
        const GATE_POLL_CAP: usize = 20_000;
        const ENTRY_FRAME_CAP: usize = 600;
        const FLOAT_FRAMES: usize = 180;
        const RISE_FRAME_CAP: usize = 120;
        const CROSSING_FRAME_CAP: usize = 9_000;
        const SEABED_MARGIN: f32 = 2.0;

        // ---- 1. Bound the crossing before any chunk exists (pure scan) ----
        let ocean_start = (0..512)
            .find(|&d| {
                let (wx, wz) = (0, AXIS_STEP * d);
                biome_at_seeded(wx, wz, SEED) == Biome::Ocean
                    && terrain_height_at_seeded(wx, wz, SEED) < SEA_LEVEL
            })
            .unwrap_or_else(|| {
                panic!(
                    "test_acceptance_swim_ocean_crossing stage 'bounds': seed {SEED} axis \
                     {CROSSING_AXIS}: no Ocean column (h < SEA_LEVEL {SEA_LEVEL}) within 512 \
                     blocks of (0, 0)"
                )
            });
        let far_shore = (ocean_start + 1..4096)
            .find(|&d| {
                let (wx, wz) = (0, AXIS_STEP * d);
                terrain_height_at_seeded(wx, wz, SEED) >= SEA_LEVEL
            })
            .unwrap_or_else(|| {
                panic!(
                    "test_acceptance_swim_ocean_crossing stage 'bounds': seed {SEED} axis \
                     {CROSSING_AXIS}: no far shore (h >= SEA_LEVEL {SEA_LEVEL}) within 4096 \
                     blocks after ocean column {ocean_start}"
                )
            });
        assert!(
            far_shore > ocean_start + 1,
            "test_acceptance_swim_ocean_crossing stage 'bounds': seed {SEED} axis \
             {CROSSING_AXIS} needs a multi-column ocean, got ocean_start={ocean_start} \
             far_shore={far_shore} ({} columns)",
            far_shore - ocean_start
        );
        let crossing_distance = far_shore - ocean_start;
        let spawn_column_x = 0;
        let spawn_column_z = AXIS_STEP * ocean_start;
        let far_shore_axis = (AXIS_STEP * far_shore) as f32;
        let seabed_height = terrain_height_at_seeded(spawn_column_x, spawn_column_z, SEED);
        assert!(
            seabed_height < SEA_LEVEL,
            "test_acceptance_swim_ocean_crossing stage 'bounds': seed {SEED} axis \
             {CROSSING_AXIS}: spawn column ({spawn_column_x}, {spawn_column_z}) has h={seabed_height}, \
             not below SEA_LEVEL {SEA_LEVEL}"
        );

        // ---- 2. Warm the spawn column through the real streaming seam ----
        let mut mgr =
            WorldManager::with_seed_and_edits(RENDER_DISTANCE, SEED, ChunkEditMap::default());
        let start_chunk = ChunkPos::new(0, 0, 0);
        let mut warmup_polls = 0usize;
        while mgr.get_block(spawn_column_x, SEA_LEVEL, spawn_column_z) != BlockId::Water {
            assert!(
                warmup_polls < WARMUP_POLL_CAP,
                "test_acceptance_swim_ocean_crossing stage 'warm-up': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, budget \
                 {WARMUP_POLL_CAP} polls: spawn-column Water at ({spawn_column_x}, {SEA_LEVEL}, \
                 {spawn_column_z}) was still non-resident after {warmup_polls} bounded \
                 update_streaming polls ({} chunks loaded)",
                mgr.loaded_chunk_count()
            );
            mgr.update_streaming(start_chunk, Season::Summer);
            warmup_polls += 1;
            std::thread::sleep(Duration::from_millis(1));
        }

        /// Production-seam harness: `step` is one playing frame (physics first,
        /// then streaming), preceded by a bounded stream-ahead residency gate.
        struct Harness {
            player: Player,
            mgr: WorldManager,
            frames: usize,
            gate_polls: usize,
            gate_worst_polls: usize,
        }

        impl Harness {
            fn feet_block(&self) -> BlockId {
                self.mgr.get_block(
                    self.player.position.x.floor() as i32,
                    (self.player.position.y + 1e-4).floor() as i32,
                    self.player.position.z.floor() as i32,
                )
            }

            fn axis_position(&self) -> f32 {
                self.player.position.z * AXIS_STEP as f32
            }

            /// Bounded streaming-ahead gate. The real game gives the pipeline
            /// about 16 ms of wall time per frame while the player swims at
            /// roughly 2.5 m/s, and rd=4 keeps the chunk under the player
            /// resident well before arrival. A headless loop advances sim-time
            /// far faster than wall time, so the test waits (1 ms polls, bounded
            /// cap) until the water column the player occupies is actually
            /// resident instead of silently reading Air for a non-resident
            /// chunk. This never changes the per-frame order: it is a
            /// precondition wait before the frame's physics step.
            fn ensure_water_resident(&mut self, cap: usize, crossing_distance: i32) {
                let col_x = self.player.position.x.floor() as i32;
                let col_z = self.player.position.z.floor() as i32;
                if terrain_height_at_seeded(col_x, col_z, SEED) >= SEA_LEVEL {
                    // Dry land: buoyancy is not at stake, so nothing to gate.
                    return;
                }
                let (center, _) = world_to_chunk_and_local(col_x, SEA_LEVEL, col_z);
                let mut polls = 0usize;
                while self.mgr.get_block(col_x, SEA_LEVEL, col_z) != BlockId::Water {
                    assert!(
                        polls < cap,
                        "test_acceptance_swim_ocean_crossing stage 'streaming-gate': seed \
                         {SEED} axis {CROSSING_AXIS} distance {crossing_distance} blocks, frame \
                         {}, position {:?}: Water at column ({col_x}, {col_z}) was still \
                         non-resident after {polls} bounded update_streaming polls (cap {cap}); a \
                         non-resident chunk reads as Air, which silently removes buoyancy",
                        self.frames,
                        self.player.position
                    );
                    self.mgr.update_streaming(center, Season::Summer);
                    polls += 1;
                    self.gate_polls += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
                self.gate_worst_polls = self.gate_worst_polls.max(polls);
            }

            fn step(&mut self, input: &InputState, crossing_distance: i32) {
                self.ensure_water_resident(GATE_POLL_CAP, crossing_distance);
                // Production order (src/main.rs playing frame): physics first,
                // then streaming around the player's new chunk.
                self.player
                    .update(DT, input, |x, y, z| self.mgr.get_block(x, y, z));
                let (center, _) = world_to_chunk_and_local(
                    self.player.position.x.floor() as i32,
                    self.player.position.y.floor() as i32,
                    self.player.position.z.floor() as i32,
                );
                self.mgr.update_streaming(center, Season::Summer);
                self.frames += 1;
            }
        }

        let mut harness = Harness {
            player: Player::new(glam::Vec3::new(
                spawn_column_x as f32 + 0.5,
                SEA_LEVEL as f32 + 3.0,
                spawn_column_z as f32 + 0.5,
            )),
            mgr,
            frames: 0,
            gate_polls: 0,
            gate_worst_polls: 0,
        };
        harness.player.fly_mode = false;
        harness.player.yaw = if AXIS_STEP > 0 {
            std::f32::consts::FRAC_PI_2
        } else {
            -std::f32::consts::FRAC_PI_2
        };
        let (sin_yaw, cos_yaw) = harness.player.yaw.sin_cos();
        assert!(
            (sin_yaw - AXIS_STEP as f32).abs() < 1e-3 && cos_yaw.abs() < 1e-3,
            "test_acceptance_swim_ocean_crossing stage 'setup': yaw fixture must put forward \
             along {CROSSING_AXIS}, got sin(yaw)={sin_yaw} cos(yaw)={cos_yaw}"
        );

        // ---- 3. Phase 1: entry — fall from above into the water column ----
        let idle_input = InputState::default();
        let mut entry_frame = None;
        for frame in 0..ENTRY_FRAME_CAP {
            harness.step(&idle_input, crossing_distance);
            if harness.feet_block() == BlockId::Water {
                entry_frame = Some(frame);
                break;
            }
            assert!(
                harness.player.position.y > seabed_height as f32 + SEABED_MARGIN,
                "test_acceptance_swim_ocean_crossing stage 'entry': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, frame budget \
                 {ENTRY_FRAME_CAP}: fell to y={} at frame {frame} before the feet block became \
                 Water (seabed h={seabed_height}, margin {SEABED_MARGIN})",
                harness.player.position.y
            );
        }
        let entry_frame = entry_frame.unwrap_or_else(|| {
            panic!(
                "test_acceptance_swim_ocean_crossing stage 'entry': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, frame budget \
                 {ENTRY_FRAME_CAP}: the feet block never became Water (final position {:?})",
                harness.player.position
            )
        });
        assert!(
            harness.player.position.y > SEA_LEVEL as f32 - 1.0,
            "test_acceptance_swim_ocean_crossing stage 'entry': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: entered water at y={} on frame \
             {entry_frame}, far below the surface (SEA_LEVEL {SEA_LEVEL}); the body fell through \
             the surface instead of stopping in it",
            harness.player.position.y
        );

        // ---- 4. Phase 2: float — settle in a band near the sea surface ----
        let mut float_min_y = f32::MAX;
        let mut float_max_y = f32::MIN;
        let mut float_water_frames = 0usize;
        for frame in 0..FLOAT_FRAMES {
            harness.step(&idle_input, crossing_distance);
            let y = harness.player.position.y;
            float_min_y = float_min_y.min(y);
            float_max_y = float_max_y.max(y);
            if harness.feet_block() == BlockId::Water {
                float_water_frames += 1;
            }
            assert!(
                !harness.player.on_ground,
                "test_acceptance_swim_ocean_crossing stage 'float': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, frame {frame}/{FLOAT_FRAMES} \
                 (global frame {}): a floating body touched ground at y={y} (seabed \
                 h={seabed_height}); the pre-swim bug walks the seabed",
                harness.frames
            );
        }
        assert!(
            float_min_y > seabed_height as f32 + SEABED_MARGIN,
            "test_acceptance_swim_ocean_crossing stage 'float': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks, {FLOAT_FRAMES}-frame budget: \
             the body sank to y={float_min_y} (seabed h={seabed_height}, margin {SEABED_MARGIN}) \
             instead of floating near the surface"
        );
        assert!(
            float_min_y >= SEA_LEVEL as f32 - 1.5 && float_max_y <= SEA_LEVEL as f32 + 2.0,
            "test_acceptance_swim_ocean_crossing stage 'float': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: band {float_min_y}..{float_max_y} \
             is not near SEA_LEVEL {SEA_LEVEL} (expected within {}..{})",
            SEA_LEVEL as f32 - 1.5,
            SEA_LEVEL as f32 + 2.0
        );
        assert!(
            float_max_y - float_min_y < 1.5,
            "test_acceptance_swim_ocean_crossing stage 'float': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: band width {} is not a gentle \
             float (oscillation or teleport)",
            float_max_y - float_min_y
        );
        assert!(
            float_water_frames > FLOAT_FRAMES / 4,
            "test_acceptance_swim_ocean_crossing stage 'float': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: only {float_water_frames}/\
             {FLOAT_FRAMES} frames had Water at the feet, so buoyancy was never engaged and the \
             water column was not resident through the streaming seam"
        );

        // ---- 5. Phase 3: rise — jump_or_up climbs and clears the surface ----
        let rise_input = InputState {
            jump_or_up: true,
            ..InputState::default()
        };
        let rise_start_y = harness.player.position.y;
        let mut rise_max_y = rise_start_y;
        let mut rise_submerged_frames = 0usize;
        for frame in 0..RISE_FRAME_CAP {
            harness.step(&rise_input, crossing_distance);
            rise_max_y = rise_max_y.max(harness.player.position.y);
            if harness.feet_block() == BlockId::Water {
                rise_submerged_frames += 1;
            }
            assert!(
                !harness.player.on_ground,
                "test_acceptance_swim_ocean_crossing stage 'rise': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, frame {frame}/\
                 {RISE_FRAME_CAP}: a swimming body touched ground at y={}",
                harness.player.position.y
            );
        }
        assert!(
            rise_submerged_frames > 0,
            "test_acceptance_swim_ocean_crossing stage 'rise': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks, {RISE_FRAME_CAP}-frame budget: \
             jump_or_up never acted on a submerged body ({rise_submerged_frames} submerged frames)"
        );
        let water_top = SEA_LEVEL as f32 + 1.0;
        assert!(
            rise_max_y > water_top,
            "test_acceptance_swim_ocean_crossing stage 'rise': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks, frame budget \
             {RISE_FRAME_CAP}: holding jump_or_up rose from y={rise_start_y} to only \
             y={rise_max_y}, never clearing the top of the water column (y > {water_top})"
        );
        assert!(
            rise_max_y > rise_start_y + 0.1,
            "test_acceptance_swim_ocean_crossing stage 'rise': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: holding jump_or_up did not raise \
             the body (start {rise_start_y}, max {rise_max_y})"
        );

        // ---- 6. Phase 4: crossing — forward to the far shore ----
        let crossing_input = InputState {
            forward: true,
            ..InputState::default()
        };
        let gate_polls_before_crossing = harness.gate_polls;
        let crossing_start_axis = harness.axis_position();
        let mut crossing_min_y = f32::MAX;
        let mut crossing_water_frames = 0usize;
        let mut landed = None;
        for frame in 0..CROSSING_FRAME_CAP {
            harness.step(&crossing_input, crossing_distance);
            crossing_min_y = crossing_min_y.min(harness.player.position.y);
            if harness.feet_block() == BlockId::Water {
                crossing_water_frames += 1;
            }
            assert!(
                harness.player.position.y > seabed_height as f32 + SEABED_MARGIN,
                "test_acceptance_swim_ocean_crossing stage 'crossing': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, frame {frame}/\
                 {CROSSING_FRAME_CAP}: sank to y={} at axis position {:.1} (seabed \
                 h={seabed_height}); the body walked the seabed instead of swimming",
                harness.player.position.y,
                harness.axis_position()
            );
            if harness.axis_position() >= far_shore_axis
                && harness.player.on_ground
                && harness.player.position.y > SEA_LEVEL as f32
            {
                landed = Some(frame);
                break;
            }
        }
        let landed = landed.unwrap_or_else(|| {
            panic!(
                "test_acceptance_swim_ocean_crossing stage 'crossing': seed {SEED} axis \
                 {CROSSING_AXIS} distance {crossing_distance} blocks, frame budget \
                 {CROSSING_FRAME_CAP}: never reached the far shore at axis position \
                 {far_shore_axis} standing on land above SEA_LEVEL {SEA_LEVEL} (final position \
                 {:?}, on_ground {}, started at axis position {crossing_start_axis})",
                harness.player.position, harness.player.on_ground
            )
        });
        assert!(
            crossing_water_frames > 500,
            "test_acceptance_swim_ocean_crossing stage 'crossing': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: only {crossing_water_frames}/\
             {CROSSING_FRAME_CAP} frames had Water at the feet, so the body did not swim the ocean"
        );
        assert!(
            crossing_min_y > seabed_height as f32 + SEABED_MARGIN,
            "test_acceptance_swim_ocean_crossing stage 'crossing': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: the body reached y={crossing_min_y} \
             at its lowest (seabed h={seabed_height}, margin {SEABED_MARGIN})"
        );
        let support = harness.mgr.get_block(
            harness.player.position.x.floor() as i32,
            (harness.player.position.y - 1.0).floor() as i32,
            harness.player.position.z.floor() as i32,
        );
        assert!(
            support.is_solid(),
            "test_acceptance_swim_ocean_crossing stage 'crossing': seed {SEED} axis \
             {CROSSING_AXIS} distance {crossing_distance} blocks: finished on_ground at {:?} but \
             the supporting block is {support:?}, not solid land",
            harness.player.position
        );
        let crossing_gate_polls = harness.gate_polls - gate_polls_before_crossing;
        assert!(
            crossing_gate_polls > 0,
            "test_acceptance_swim_ocean_crossing stage 'crossing': seed {SEED} axis \
             {CROSSING_AXIS}: the streaming gate never waited during the {crossing_distance}-block \
             crossing, so streaming-ahead-of-the-player was not exercised"
        );

        eprintln!(
            "test_acceptance_swim_ocean_crossing: seed={SEED} axis={CROSSING_AXIS} \
             distance={crossing_distance} blocks frames={} (entry={entry_frame}, landed={landed}) \
             float_band={float_min_y:.3}..{float_max_y:.3} float_water_frames={float_water_frames} \
             rise={rise_start_y:.3}->{rise_max_y:.3} rise_submerged_frames={rise_submerged_frames} \
             crossing_water_frames={crossing_water_frames} gate_polls={} (worst {} polls)",
            harness.frames, harness.gate_polls, harness.gate_worst_polls
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
            mgr.update_streaming(start, Season::Summer);
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
            let (meshes, _unloaded) = mgr.update_streaming(center, Season::Summer);
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
            let (meshes, _) = mgr.update_streaming(
                ChunkPos::new(start.x + STEPS as i32, 0, start.z),
                Season::Summer,
            );
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
