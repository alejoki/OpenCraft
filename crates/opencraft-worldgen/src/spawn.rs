//! Deterministic, bounded player-spawn search over the neutral height/ruin/tree
//! queries.
//!
//! The search is pure in `seed`: the same seed always yields the same column,
//! with no clock, RNG, chunk or shared state (R001). It composes the crate's
//! own terrain, biome, ruin and tree rules, so it can never disagree with the
//! generator about what a safe column is.

use crate::biome::{biome_for_height, forest_noise, Biome};
use crate::height::terrain_height_at_seeded;
use crate::ruin::ruin_covers;
use crate::tree::tree_trunk_height;

/// Minimum surface height of a spawnable column: two blocks above the named sea
/// level, so the player always starts on dry land above the water line rather
/// than on the seabed. The Sand beach band can still reach `SEA_LEVEL +
/// BEACH_BAND`, so a spawn on the shoreline shelf is possible (and safe);
/// `SPAWN_MIN_HEIGHT` is the D071 threshold from the plan.
pub const SPAWN_MIN_HEIGHT: i32 = crate::height::SEA_LEVEL + 2;

/// Horizontal Chebyshev radius of the tree-free clearing demanded around a
/// spawn column.
///
/// A generated canopy reaches two blocks out from its trunk, so rejecting every
/// tree within this radius guarantees no trunk or neighbour leaf can occupy the
/// player's feet/head blocks (surface + 1..=surface + 4).
pub const SPAWN_TREE_CLEARANCE: i32 = 2;

/// Bounded Chebyshev radius (blocks) of the deterministic spawn search.
///
/// Sized from the terrain distribution, not guessed: a scan of 10_000 hashed
/// seeds found the first safe land column within 365 blocks of (0, 0) in the
/// worst case, so 512 leaves headroom while keeping the pathological
/// no-land-found path to ~1M height samples.
pub const SPAWN_SEARCH_RADIUS: i32 = 512;

/// Whether a player can safely spawn on this column: land above the water line,
/// with no generated oak trunk or neighbour canopy overhead.
pub fn is_spawn_safe(wx: i32, wz: i32, seed: u32) -> bool {
    let h = terrain_height_at_seeded(wx, wz, seed);
    if h < SPAWN_MIN_HEIGHT {
        return false;
    }
    // Land-biome policy stated explicitly rather than inferred: Ocean is
    // exactly `h < SEA_LEVEL`, so this can only fire if that coupling ever
    // breaks, and then the search keeps moving instead of spawning in water.
    if biome_for_height(h, forest_noise(wx, wz, seed)) == Biome::Ocean {
        return false;
    }
    for dz in -SPAWN_TREE_CLEARANCE..=SPAWN_TREE_CLEARANCE {
        for dx in -SPAWN_TREE_CLEARANCE..=SPAWN_TREE_CLEARANCE {
            let nx = wx + dx;
            let nz = wz + dz;
            let nh = terrain_height_at_seeded(nx, nz, seed);
            let nb = biome_for_height(nh, forest_noise(nx, nz, seed));
            if tree_trunk_height(nx, nz, nh, nb, seed).is_some() {
                return false;
            }
        }
    }
    // Ruin clearance (S07): a footprint column is unsafe because the tallest
    // ruin cell is `surface + 4` — the top of the spawn body — so a spawn here
    // would be buried inside a wall or corner pillar. `ruin_covers` shares
    // the placement rule with the generator, so the footprint shape and gate
    // are never duplicated here.
    if ruin_covers(wx, wz, seed) {
        return false;
    }
    true
}

/// Deterministic, bounded land search for the player spawn column.
///
/// Scans fixed-order square rings outward from (0, 0) — every ring clockwise
/// from its north edge — and returns the first column that is land
/// (`SPAWN_MIN_HEIGHT` or higher, never `Biome::Ocean`) with a tree-free
/// clearing (see `SPAWN_TREE_CLEARANCE`). Pure in `seed`: the same seed always
/// yields the same column, with no clock, RNG, or shared state (R001).
///
/// D071: after the four-biome rewrite column (0, 0) can be open ocean for a
/// given seed, and spawning there would drop the player on the seabed (and a
/// tree column would swallow the surface + 4 spawn in OakLog). The search is
/// bounded by `SPAWN_SEARCH_RADIUS`; if it finds nothing it panics loudly
/// instead of silently returning an unsafe column.
pub fn find_spawn_column(seed: u32) -> (i32, i32) {
    find_spawn_column_within(seed, SPAWN_SEARCH_RADIUS)
}

/// `find_spawn_column` with an explicit bound, so the loud failure path is
/// directly testable without needing a pathological seed.
fn find_spawn_column_within(seed: u32, max_radius: i32) -> (i32, i32) {
    assert!(max_radius >= 0, "spawn search radius must be non-negative");
    if is_spawn_safe(0, 0, seed) {
        return (0, 0);
    }
    for radius in 1..=max_radius {
        // North edge: west -> east.
        for wx in -radius..=radius {
            if is_spawn_safe(wx, -radius, seed) {
                return (wx, -radius);
            }
        }
        // East edge: north -> south (the north-east corner was visited above).
        for wz in (-radius + 1)..=radius {
            if is_spawn_safe(radius, wz, seed) {
                return (radius, wz);
            }
        }
        // South edge: east -> west.
        for wx in (-radius..radius).rev() {
            if is_spawn_safe(wx, radius, seed) {
                return (wx, radius);
            }
        }
        // West edge: south -> north (both corners were visited above).
        for wz in (-radius + 1..radius).rev() {
            if is_spawn_safe(-radius, wz, seed) {
                return (-radius, wz);
            }
        }
    }
    panic!(
        "terrain: no land spawn column within {max_radius} blocks of (0, 0) for seed {seed} \
         (need h >= SPAWN_MIN_HEIGHT {SPAWN_MIN_HEIGHT} and no oak within \
         {SPAWN_TREE_CLEARANCE} blocks) — raise SPAWN_SEARCH_RADIUS or fix the height envelope"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::biome_at_seeded;
    use crate::height::{terrain_height_at_seeded, MAX_TERRAIN_HEIGHT, SEA_LEVEL};

    /// Mirror of the game default seed; the crate never owns the game default.
    const DEFAULT_WORLD_SEED: u32 = 1337;

    /// Positive + determinism: the spawn column is always land above the water
    /// line, is identical across repeat calls (R001 purity), and has no tree
    /// trunk or neighbour canopy inside the spawn body.
    #[test]
    fn test_find_spawn_column_is_land_above_sea_level_and_deterministic() {
        // Includes seeds whose (0, 0) column is open ocean after the T03
        // rewrite (12345 -> h 8, 4 -> h 2, VISTA_SEED -> h -2) beside seeds
        // whose origin is already safe, plus the seed with the widest observed
        // search ring (2400638911 -> 365 blocks).
        for seed in [DEFAULT_WORLD_SEED, 12345, 9, 4, 0x5EED_4A11, 2_400_638_911] {
            let (wx, wz) = find_spawn_column(seed);
            let h = terrain_height_at_seeded(wx, wz, seed);
            assert!(
                h >= SPAWN_MIN_HEIGHT,
                "seed {seed} spawn ({wx}, {wz}) has h {h} below SPAWN_MIN_HEIGHT {SPAWN_MIN_HEIGHT}"
            );
            assert!(
                h <= MAX_TERRAIN_HEIGHT,
                "seed {seed} spawn ({wx}, {wz}) leaves the height envelope"
            );
            assert_ne!(
                biome_at_seeded(wx, wz, seed),
                Biome::Ocean,
                "seed {seed} spawned in an ocean column"
            );
            assert_eq!(
                find_spawn_column(seed),
                (wx, wz),
                "seed {seed} spawn search must be byte-identical across repeats"
            );
            // Tree clearance: no oak within the canopy radius, so the surface+4
            // spawn can never be inside a trunk or a neighbour's leaves.
            for dz in -SPAWN_TREE_CLEARANCE..=SPAWN_TREE_CLEARANCE {
                for dx in -SPAWN_TREE_CLEARANCE..=SPAWN_TREE_CLEARANCE {
                    let nx = wx + dx;
                    let nz = wz + dz;
                    let nh = terrain_height_at_seeded(nx, nz, seed);
                    let nb = biome_for_height(nh, forest_noise(nx, nz, seed));
                    assert_eq!(
                        tree_trunk_height(nx, nz, nh, nb, seed),
                        None,
                        "seed {seed} spawn ({wx}, {wz}) is within {SPAWN_TREE_CLEARANCE} blocks of a tree at ({nx}, {nz})"
                    );
                }
            }
        }
    }

    /// The common case stays put: a safe origin column keeps the spawn at
    /// (0, 0), so the search changes nothing for the default world.
    #[test]
    fn test_find_spawn_column_keeps_a_safe_origin() {
        let seed = DEFAULT_WORLD_SEED;
        assert!(is_spawn_safe(0, 0, seed));
        assert_eq!(find_spawn_column(seed), (0, 0));
    }

    /// Negative/envelope: when (0, 0) is open ocean the search must move to
    /// land instead of pinning the old fixed column.
    #[test]
    fn test_find_spawn_column_moves_off_an_ocean_origin() {
        let seed = 12345u32;
        assert!(
            terrain_height_at_seeded(0, 0, seed) < SEA_LEVEL,
            "test premise: seed {seed} must have an ocean at (0, 0)"
        );
        assert_eq!(biome_at_seeded(0, 0, seed), Biome::Ocean);
        let (wx, wz) = find_spawn_column(seed);
        assert_ne!((wx, wz), (0, 0));
        assert!(terrain_height_at_seeded(wx, wz, seed) >= SPAWN_MIN_HEIGHT);
    }

    /// Failure mode: a bound too small for the seed panics loudly with the
    /// search parameters instead of silently returning an unsafe column.
    #[test]
    #[should_panic(expected = "no land spawn column within 8 blocks")]
    fn test_find_spawn_column_fails_loudly_when_bounded_out() {
        let seed = 12345u32;
        // Premise: the first safe column for this seed is far outside 8 blocks.
        for wx in -8..=8 {
            for wz in -8..=8 {
                assert!(
                    !is_spawn_safe(wx, wz, seed),
                    "test premise: ({wx}, {wz}) is safe, widen the bound"
                );
            }
        }
        let _ = find_spawn_column_within(seed, 8);
    }
}
