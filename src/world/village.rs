//! Game-side shim over the neutral village codec/planner in
//! `opencraft-worldgen`.
//!
//! Definitions, parsing, the fail-soft catalog, the fingerprint, the cell/site
//! gates, [`Rotation`], [`VillagePlan`], the whole planner and the chunk-local
//! emitter all live in [`opencraft_worldgen::village`]; this module re-exports
//! them unchanged so every `crate::world::village::...` path keeps working.
//!
//! The crate owns the only village writer
//! ([`opencraft_worldgen::village::place_village_cells`]), which writes raw
//! `u16` ids into a neutral [`opencraft_worldgen::buffer::ChunkBuffer`]. The
//! game converts that buffer to a [`crate::world::chunk::Chunk`] once at the
//! boundary ([`crate::world::terrain::chunk_from_buffer`]); there is no second
//! writer and no duplicate plan-to-cell conversion on the game side.

pub use opencraft_worldgen::village::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;
    use crate::world::chunk::{ChunkPos, CHUNK_SIZE};
    use crate::world::terrain::chunk_from_buffer;
    use opencraft_worldgen::block::BLOCK_AIR;
    use opencraft_worldgen::buffer::ChunkBuffer;
    use opencraft_worldgen::spawn::find_spawn_column;

    fn checked_in_catalog() -> VillageCatalog {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("worldgen");
        let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert!(!catalog.is_empty(), "checked-in village catalog must load");
        catalog
    }

    /// The game keeps no writer of its own: the crate emitter writes raw ids
    /// into a neutral buffer and the adapter must move every one of them into
    /// the game chunk unchanged. The crate side of the same plan is pinned by
    /// `opencraft-worldgen`'s checked-in worldgen tests.
    #[test]
    fn crate_emitter_cells_round_trip_through_the_game_adapter() {
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

        let mut buffer = ChunkBuffer::uniform(BLOCK_AIR);
        opencraft_worldgen::village::place_village_cells(
            &mut buffer,
            pos,
            seed,
            &catalog,
            Some(spawn),
        );
        let chunk = chunk_from_buffer(buffer);
        for cell in &cells {
            let [wx, wy, wz] = cell.world;
            assert_eq!(
                chunk.get(
                    wx.rem_euclid(cs) as usize,
                    wy.rem_euclid(cs) as usize,
                    wz.rem_euclid(cs) as usize,
                ),
                BlockId::from_raw(cell.block),
                "cell {:?} did not round-trip through the crate emitter and adapter",
                cell.world
            );
        }
    }
}
