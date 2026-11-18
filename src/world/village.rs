//! Game-side shim over the neutral village codec/planner in
//! `opencraft-worldgen`.
//!
//! Definitions, parsing, the fail-soft catalog, the fingerprint, the cell/site
//! gates, [`Rotation`], [`VillagePlan`] and the whole planner live in
//! [`opencraft_worldgen::village`]; this module re-exports them unchanged so
//! every `crate::world::village::...` path keeps working. The one visible
//! difference is [`VillageBlock::block`], a raw `u16` block id: the writer
//! below converts with [`BlockId::from_raw`] at the boundary.
//!
//! The game deliberately keeps the `Chunk` writer
//! ([`place_village_cells`]): chunk storage is a game type, while the crate
//! only knows the neutral `(pos, seed, catalog, spawn)` plan and its
//! chunk-local cell list.

use crate::world::block::BlockId;
use crate::world::chunk::{Chunk, ChunkPos, CHUNK_SIZE};
use opencraft_worldgen::spawn::find_spawn_column;

pub use opencraft_worldgen::village::*;

/// Write this chunk's share of the village plan into `chunk`.
///
/// Called by `terrain::generate_chunk_with_context` as the final generation
/// pass. Pure in `(pos, seed, catalog, spawn_column)`: it never reads a
/// neighbour chunk, keeps no state and ignores the season. All geometry comes
/// from [`village_plan_with_spawn`] and [`VillagePlan::cells_for_chunk`]; this
/// writer never re-derives a single cell.
///
/// The early returns are deliberately conservative and cheap, in this order:
/// 1. the chunk's own cell has no site;
/// 2. the chunk does not intersect the anchor radius box;
/// 3. the whole cell has no plan (gated out, unfit root, or too close to
///    spawn);
/// 4. the plan's exact bounds miss this chunk.
///
/// Only then is the plan materialized for this chunk. Every emitted cell is
/// asserted to belong to `pos` in debug builds.
// The game-side `Chunk` writer intentionally shadows the crate's buffer-based
// emitter until the T03 adapter switch deletes it; the shadow is the point of
// this transitional duplicate, not an accident.
#[allow(hidden_glob_reexports)]
pub(crate) fn place_village_cells(
    chunk: &mut Chunk,
    pos: ChunkPos,
    seed: u32,
    catalog: &VillageCatalog,
    spawn_column: Option<(i32, i32)>,
) {
    let cell = village_cell_at(pos.x * CHUNK_SIZE as i32, pos.z * CHUNK_SIZE as i32);
    let Some(site) = catalog.site(cell, seed) else {
        return;
    };
    if !chunk_intersects_anchor_radius(pos, site.anchor) {
        return;
    }
    let spawn = spawn_column.unwrap_or_else(|| find_spawn_column(seed));
    let Some(plan) = village_plan_with_spawn(site.cell, seed, catalog, spawn) else {
        return;
    };
    if !plan.bounds.intersects_chunk(pos) {
        return;
    }

    let chunk_size = CHUNK_SIZE as i32;
    for block in plan.cells_for_chunk(pos) {
        let [wx, wy, wz] = block.world;
        debug_assert_eq!(
            ChunkPos::new(
                wx.div_euclid(chunk_size),
                wy.div_euclid(chunk_size),
                wz.div_euclid(chunk_size),
            ),
            pos,
            "village plan emitted a cell outside the chunk being generated"
        );
        chunk.set(
            wx.rem_euclid(chunk_size) as usize,
            wy.rem_euclid(chunk_size) as usize,
            wz.rem_euclid(chunk_size) as usize,
            BlockId::from_raw(block.block),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_in_catalog() -> VillageCatalog {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("worldgen");
        let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert!(!catalog.is_empty(), "checked-in village catalog must load");
        catalog
    }

    /// The game keeps only the `Chunk` writer, so the adapter test proves the
    /// writer still inserts every planned cell as the game block id — the crate
    /// side of the same plan is pinned by
    /// `opencraft-worldgen`'s `checked_in_worldgen_tree_pins_fingerprint_and_plan`.
    #[test]
    fn place_village_cells_round_trips_raw_ids_into_the_game_chunk() {
        let catalog = checked_in_catalog();
        let seed = 424_242;
        let cell = VillageCell { x: 6, z: -6 };
        let spawn = find_spawn_column(seed);
        let plan =
            village_plan_with_spawn(cell, seed, &catalog, spawn).expect("pinned cell must plan");

        let cs = CHUNK_SIZE as i32;
        let mut target = None;
        'search: for cy in plan.bounds.min_y.div_euclid(cs)..=plan.bounds.max_y.div_euclid(cs) {
            for cz in plan.bounds.min_z.div_euclid(cs)..=plan.bounds.max_z.div_euclid(cs) {
                for cx in plan.bounds.min_x.div_euclid(cs)..=plan.bounds.max_x.div_euclid(cs) {
                    let pos = ChunkPos::new(cx, cy, cz);
                    if !plan.cells_for_chunk(pos).is_empty() {
                        target = Some(pos);
                        break 'search;
                    }
                }
            }
        }
        let pos = target.expect("the pinned plan must own at least one non-empty chunk");
        let cells = plan.cells_for_chunk(pos);
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        place_village_cells(&mut chunk, pos, seed, &catalog, Some(spawn));
        for cell in &cells {
            let [wx, wy, wz] = cell.world;
            assert_eq!(
                chunk.get(
                    wx.rem_euclid(cs) as usize,
                    wy.rem_euclid(cs) as usize,
                    wz.rem_euclid(cs) as usize,
                ),
                BlockId::from_raw(cell.block),
                "cell {:?} did not round-trip through the writer",
                cell.world
            );
        }
    }
}
