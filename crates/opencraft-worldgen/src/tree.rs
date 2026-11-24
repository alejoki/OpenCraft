//! Pure `(seed, x, z)` oak-tree placement and vertical model.
//!
//! The interior-only placement rule, the tree height band, the per-biome hash
//! gate and the trunk/layout model all live here, so the chunk generator, the
//! spawn search and the village planner cannot disagree about where an oak
//! stands or how tall it is. Nothing here reads a chunk, a clock, an RNG or a
//! season: the module is chunk-free and device-free by construction.
//!
//! The model no longer carries branch geometry (M019/S04): `layout` selects one
//! of the eight authored oak prefabs in
//! [`crate::structure::StructureCatalog`], and the prefab's cells are what the
//! chunk generator emits. Placement (where a tree stands, how tall it is, which
//! layout it uses) stays a pure function of `(position, height, biome, seed)`.
//!
//! The game keeps the emission side (the tree pass in its chunk generator).

use opencraft_core::chunk::CHUNK_SIZE;

use crate::biome::Biome;
use crate::height::{BEACH_BAND, MOUNTAIN_HEIGHT_THRESHOLD, SEA_LEVEL};
use crate::noise::hash2d;

/// Trees only grow on Plains/Forest land in this surface-height band. The
/// lower bound is above the beach band (so never on Sand) and the upper bound
/// stays below the mountain stone threshold (so never on mountain rock). The
/// gate is the pre-season terrain class, not the post-season surface block:
/// Winter keeps its trees even though the surface becomes Snow (D088).
pub const TREE_MIN_HEIGHT: i32 = SEA_LEVEL + BEACH_BAND + 1;
pub const TREE_MAX_HEIGHT: i32 = MOUNTAIN_HEIGHT_THRESHOLD - 1;

/// Tallest trunk any oak can generate, in blocks above its column surface.
/// `oak_tree_at` returns `4..=TREE_MAX_TRUNK_HEIGHT`.
pub const TREE_MAX_TRUNK_HEIGHT: i32 = 5;

/// How far the canopy rises above the highest trunk cell.
pub const TREE_CANOPY_TOP_OVER_TRUNK: i32 = 1;

/// Tallest emitted oak cell above its column surface. Branches sit on the
/// upper trunk, so they are strictly below this bound; the canopy top is the
/// binding cell.
pub const TREE_MAX_HEIGHT_OVER_SURFACE: i32 = TREE_MAX_TRUNK_HEIGHT + TREE_CANOPY_TOP_OVER_TRUNK;

/// Vertical headroom reserved above `MAX_TERRAIN_HEIGHT` for a trunk, its
/// branches and its canopy. One spare block beyond the tallest emitted cell
/// keeps the uniform-Air fast path provably above every possible tree cell, so
/// it can never clip a branch. The derivation is asserted in the tests.
pub const TREE_CANOPY_HEADROOM: i32 = TREE_MAX_HEIGHT_OVER_SURFACE + 1;

/// A deterministic oak's full vertical model: the trunk length plus the branch
/// layout that selects one of the eight authored oak prefabs.
///
/// Produced by [`oak_tree_at`], the single placement rule shared by the chunk
/// generator, the spawn search and the tests. The model is a pure function of
/// `(position, height, biome, seed)` — deliberately season-blind (D088), so a
/// Winter tree keeps its trunk and branches while only its leaves are omitted
/// from the emission pass. The prefab cells themselves (M019/S04) live in
/// [`crate::structure::StructureCatalog::oak`]; this struct only names which
/// one applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OakTree {
    /// Trunk length in blocks above the column surface, `4..=TREE_MAX_TRUNK_HEIGHT`.
    pub trunk_height: i32,
    /// Branch layout in `0..=3`, selecting `oak_t{trunk_height}_l{layout}`.
    pub layout: u8,
}

/// The deterministic oak this column grows, or `None` when the column is
/// ineligible.
///
/// Single source of truth shared by the chunk generator and
/// [`crate::spawn::find_spawn_column`]: the interior-only placement rule, the
/// tree height band, the per-biome hash gate and the model (trunk + branches)
/// all live here, so the spawn search can reject a column the generator would
/// fill with OakLog without duplicating the rule. The gate is the pre-season
/// terrain class (Plains/Forest + the Grass height band), never the post-season
/// surface block, so Winter trees persist (D088).
pub fn oak_tree_at(wx: i32, wz: i32, h: i32, biome: Biome, seed: u32) -> Option<OakTree> {
    // Trees only grow on the chunk interior (3..29) so canopies never cross a
    // chunk border.
    let lx = wx.rem_euclid(CHUNK_SIZE as i32);
    let lz = wz.rem_euclid(CHUNK_SIZE as i32);
    if !(3..29).contains(&lx) || !(3..29).contains(&lz) {
        return None;
    }
    // `TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT` is exactly the pre-season Grass
    // band: above the Sand beach shelf and below Mountain stone. Beaches and
    // seabeds are below it, mountain stone above it, so both stay tree-free
    // without consulting a seasonal surface block.
    if !(TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&h) {
        return None;
    }
    let tree_hash = hash2d(wx, wz, 777 ^ seed);
    let gate = match biome {
        Biome::Forest => (tree_hash & 0x0F) == 0,
        Biome::Plains => (tree_hash & 0xFF) == 0,
        Biome::Mountains | Biome::Ocean => false,
    };
    gate.then(|| OakTree {
        trunk_height: 4 + ((tree_hash >> 8) & 1) as i32,
        layout: ((tree_hash >> 9) & 3) as u8,
    })
}

/// Trunk height of the oak at this column, or `None` when it grows no tree.
/// Convenience wrapper used by the spawn search and tests that only need the
/// trunk; the generator uses [`oak_tree_at`] for the layout too.
pub fn tree_trunk_height(wx: i32, wz: i32, h: i32, biome: Biome, seed: u32) -> Option<i32> {
    oak_tree_at(wx, wz, h, biome, seed).map(|tree| tree.trunk_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::biome_at_seeded;
    use crate::height::{terrain_height_at_seeded, MAX_TERRAIN_HEIGHT};
    use crate::structure::StructureCatalog;

    /// Mirror of the game default seed; the crate never owns the game default.
    const DEFAULT_WORLD_SEED: u32 = 1337;

    /// The model bound used by `TREE_CANOPY_HEADROOM` must cover every emitted
    /// prefab cell — trunk, branch and canopy — so the uniform-Air fast path
    /// can never clip a cell. Pins the constants together with a sampled scan
    /// and, per tree, the resolved authored oak prefab.
    #[test]
    fn test_tree_model_bounds_fit_the_canopy_headroom() {
        const {
            assert!(
                TREE_MAX_HEIGHT_OVER_SURFACE >= TREE_MAX_TRUNK_HEIGHT + TREE_CANOPY_TOP_OVER_TRUNK,
                "the max tree height must cover the tallest trunk plus its canopy top"
            );
            assert!(
                TREE_CANOPY_HEADROOM > TREE_MAX_HEIGHT_OVER_SURFACE,
                "headroom must leave at least one spare block above the tallest tree cell"
            );
            assert!(
                MAX_TERRAIN_HEIGHT + TREE_CANOPY_HEADROOM < 4 * CHUNK_SIZE as i32,
                "the top chunk layer must stay provably air with tree headroom"
            );
        }

        let catalog = StructureCatalog::builtin();
        let seed = DEFAULT_WORLD_SEED;
        let mut sampled = 0usize;
        for wz in -256..=256 {
            for wx in -256..=256 {
                let h = terrain_height_at_seeded(wx, wz, seed);
                let biome = biome_at_seeded(wx, wz, seed);
                let Some(tree) = oak_tree_at(wx, wz, h, biome, seed) else {
                    continue;
                };
                sampled += 1;
                assert!(
                    (4..=TREE_MAX_TRUNK_HEIGHT).contains(&tree.trunk_height),
                    "trunk height {} outside the headroom derivation at ({wx}, {wz})",
                    tree.trunk_height
                );
                assert!(
                    tree.layout <= 3,
                    "layout {} is not a 2-bit value",
                    tree.layout
                );
                let prefab = catalog
                    .oak(tree.trunk_height, tree.layout)
                    .unwrap_or_else(|| {
                        panic!(
                            "builtin catalog lacks oak_t{}_l{} for ({wx}, {wz})",
                            tree.trunk_height, tree.layout
                        )
                    });
                for block in &prefab.blocks {
                    // Grid y is relative to origin.y = 0; world y is h + 1 + dy.
                    let dy = block.position[1] as i32 - prefab.origin[1] as i32;
                    let dx = block.position[0] as i32 - prefab.origin[0] as i32;
                    let dz = block.position[2] as i32 - prefab.origin[2] as i32;
                    assert!(
                        (1..=TREE_MAX_HEIGHT_OVER_SURFACE).contains(&(1 + dy)),
                        "prefab cell dy {dy} at ({wx}, {wz}) is outside the headroom derivation"
                    );
                    let cell_x = (wx + dx).rem_euclid(CHUNK_SIZE as i32);
                    let cell_z = (wz + dz).rem_euclid(CHUNK_SIZE as i32);
                    assert!(
                        (1..=CHUNK_SIZE as i32 - 2).contains(&cell_x),
                        "prefab cell x offset {dx} left the chunk interior (lx {cell_x})"
                    );
                    assert!(
                        (1..=CHUNK_SIZE as i32 - 2).contains(&cell_z),
                        "prefab cell z offset {dz} left the chunk interior (lz {cell_z})"
                    );
                }
            }
        }
        assert!(
            sampled > 0,
            "no oak sampled in the model-bound region (test is vacuous)"
        );
    }
}
