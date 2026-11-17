//! Pure `(seed, x, z)` surface-height query and its named envelope.
//!
//! The query is device-free and content-free: it returns the same surface
//! height for a column no matter which game adapter asks.

use crate::noise::{smoothstep, value_noise_2d};

/// Water surface height for the whole world. Columns whose surface is below
/// this are ocean basins, and the band just above it is the beach shelf.
/// This is the value that used to be the implicit `h <= 22` Sand threshold in
/// `generate_chunk_seeded`, now named (D071).
///
/// Every column with `h < SEA_LEVEL` is filled with static `BlockId::Water`
/// from just above the seabed skin up to this height (MEM230: worldgen only,
/// no flow, never persisted and never a `ChunkEditMap` entry).
pub const SEA_LEVEL: i32 = 22;

/// Highest surface the height function may ever produce.
///
/// D071 caps it at 150 so the vertical world (`MIN_CHUNK_Y..=MAX_CHUNK_Y` ->
/// wy -32..=159) never has to grow. It is deliberately kept lower (120): the
/// top chunk layer starts at wy 128, so `MAX_TERRAIN_HEIGHT +
/// TREE_CANOPY_HEADROOM` (127) stays strictly below it and that whole layer
/// keeps taking the uniform-Air fast path. Real mountains still reach wy ~110
/// under a fixed cap of 120.
pub const MAX_TERRAIN_HEIGHT: i32 = 120;

/// Lowest surface the height function may produce (deep ocean floor).
/// The raw continental curve bottoms out at 4; the ocean deepening pass below
/// pushes the deepest basins to -2, still 30 blocks above the bedrock floor at
/// wy -32.
pub const MIN_TERRAIN_HEIGHT: i32 = -2;

/// At or above this surface height a land column is mountain stone instead of
/// grass/dirt. Classification and surface skin both key on the same value.
pub const MOUNTAIN_HEIGHT_THRESHOLD: i32 = 64;

/// A land column within `SEA_LEVEL..=SEA_LEVEL + BEACH_BAND` gets a Sand
/// surface and subsurface, which reads as a shoreline beach.
pub const BEACH_BAND: i32 = 3;

/// Piecewise-linear continental elevation curve. Low continental noise digs
/// ocean basins (4..15 before the [`ocean_depth_bonus`] deepening), the mid
/// range builds plains/forest shelves (26..44), and high continental noise
/// climbs into mountain foothills (58..70). Values are the column *mean*;
/// hill/detail deviation and the ridge term are added on top (see
/// `terrain_height_at_seeded`).
const CONTINENTAL_ELEVATION: [(f32, f32); 8] = [
    (0.00, 4.0),
    (0.18, 8.0),
    (0.30, 15.0),
    (0.40, 26.0),
    (0.50, 34.0),
    (0.62, 44.0),
    (0.78, 58.0),
    (1.00, 70.0),
];

/// Below `OCEAN_LANDNESS_LOW` the column has no hill/detail deviation at all,
/// which is what keeps every basin interior strictly below `SEA_LEVEL`.
const OCEAN_LANDNESS_LOW: f32 = 0.30;
const OCEAN_LANDNESS_HIGH: f32 = 0.50;
/// Mountain ridges only grow once continental noise is this high, so mountains
/// exist only in the continental highlands, not as mid-map spikes.
const MOUNTAIN_NOISE_LOW: f32 = 0.62;
const MOUNTAIN_NOISE_HIGH: f32 = 0.82;
const MOUNTAIN_RIDGE_AMPLITUDE: f32 = 30.0;
const HILL_AMPLITUDE: f32 = 14.0;
const DETAIL_AMPLITUDE: f32 = 4.0;

/// Extra depth (blocks) added to submerged basins at full strength.
///
/// The raw continental curve bottoms out at 4, leaving only 18 blocks of water
/// over the deepest seabed. This bonus makes oceans and lakes read as genuinely
/// deep while leaving the waterline and every land column exactly where the
/// raw height function puts them.
const OCEAN_DEEPENING: f32 = 6.0;

/// How many blocks below `SEA_LEVEL` the deepening takes to reach full
/// strength. Ramping it keeps the bonus continuous at the waterline: a
/// one-block-deep puddle gains only a fraction of a block, so no shoreline
/// column moves.
const OCEAN_DEEPENING_RAMP: f32 = 8.0;

/// Depth bonus (blocks) for a raw height: zero at or above sea level, rising
/// smoothly to `OCEAN_DEEPENING` once a basin is `OCEAN_DEEPENING_RAMP` blocks
/// deep. Deliberately a pure function of the raw height so the deepening can
/// never move a land column: the raw height already decides land vs water.
#[inline]
fn ocean_depth_bonus(base: f32) -> f32 {
    let depth = SEA_LEVEL as f32 - base;
    if depth <= 0.0 {
        0.0
    } else {
        OCEAN_DEEPENING * smoothstep(0.0, OCEAN_DEEPENING_RAMP, depth)
    }
}

fn continental_elevation(continental: f32) -> f32 {
    let continental = continental.clamp(0.0, 1.0);
    for pair in CONTINENTAL_ELEVATION.windows(2) {
        let (c0, b0) = pair[0];
        let (c1, b1) = pair[1];
        if continental <= c1 {
            let t = (continental - c0) / (c1 - c0);
            return b0 + t * (b1 - b0);
        }
    }
    CONTINENTAL_ELEVATION[CONTINENTAL_ELEVATION.len() - 1].1
}

#[inline]
pub fn terrain_height_at_seeded(wx: i32, wz: i32, seed: u32) -> i32 {
    let x = wx as f32;
    let z = wz as f32;
    let s = seed.wrapping_mul(0x9E37_79B9);
    let continental = value_noise_2d(x * 0.004, z * 0.004, 101 ^ s);
    let hills = value_noise_2d(x * 0.015, z * 0.015, 202 ^ s);
    let detail = value_noise_2d(x * 0.05, z * 0.05, 303 ^ s);
    let ridge = (value_noise_2d(x * 0.008, z * 0.008, 404 ^ s) * 2.0 - 1.0).abs();

    // Ocean basins keep a flat seabed (landness == 0) so every column below
    // SEA_LEVEL really is open water once T04 fills it; land columns get the
    // full hill/detail deviation.
    let landness = smoothstep(OCEAN_LANDNESS_LOW, OCEAN_LANDNESS_HIGH, continental);
    let hills = (hills - 0.5) * 2.0 * HILL_AMPLITUDE;
    let detail = (detail - 0.5) * 2.0 * DETAIL_AMPLITUDE;
    let mountain = smoothstep(MOUNTAIN_NOISE_LOW, MOUNTAIN_NOISE_HIGH, continental)
        * ridge
        * MOUNTAIN_RIDGE_AMPLITUDE;

    // Worst case: 70 (top of the curve) + 18 (hills + detail) + 30 (ridge)
    // = 118 <= MAX_TERRAIN_HEIGHT; the raw basin floor bottoms out at 4 and
    // the deepening takes it to -2 (MIN_TERRAIN_HEIGHT at full bonus).
    let base = continental_elevation(continental) + landness * (hills + detail) + mountain;
    let base = base - ocean_depth_bonus(base);
    base.round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the game default seed; the crate never owns the game default.
    const DEFAULT_WORLD_SEED: u32 = 1337;

    /// Game-side chunk width, pinned here so the envelope assertion can prove
    /// the top chunk layer stays air without importing a game type.
    const CHUNK_SIZE: i32 = 32;

    /// Mirror of the game's private canopy headroom (trunk 5 + canopy top 1 + one
    /// spare block). The tree model stays game-side, so the envelope test pins
    /// the same number here.
    const TREE_CANOPY_HEADROOM: i32 = 7;

    /// Negative/envelope: over a wide scan the surface never leaves the named
    /// envelope, mountains actually reach the mountain threshold, and ocean
    /// basins below sea level exist.
    #[test]
    fn test_acceptance_height_envelope_scan() {
        const {
            assert!(MAX_TERRAIN_HEIGHT <= 150, "D071 caps the envelope at 150");
            assert!(
                MIN_TERRAIN_HEIGHT > -32,
                "the envelope must stay above the bedrock floor"
            );
            assert!(
                MAX_TERRAIN_HEIGHT + TREE_CANOPY_HEADROOM < 4 * CHUNK_SIZE,
                "the top chunk layer must stay provably air"
            );
        }

        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            let mut min_h = i32::MAX;
            let mut max_h = i32::MIN;
            let mut ocean_columns = 0usize;
            for wz in (-1024..=1024).step_by(8) {
                for wx in (-1024..=1024).step_by(8) {
                    let h = terrain_height_at_seeded(wx, wz, seed);
                    assert!(
                        (MIN_TERRAIN_HEIGHT..=MAX_TERRAIN_HEIGHT).contains(&h),
                        "seed {seed} height {h} at ({wx}, {wz}) outside envelope {MIN_TERRAIN_HEIGHT}..={MAX_TERRAIN_HEIGHT}"
                    );
                    if h < SEA_LEVEL {
                        ocean_columns += 1;
                    }
                    min_h = min_h.min(h);
                    max_h = max_h.max(h);
                }
            }
            assert!(
                max_h >= MOUNTAIN_HEIGHT_THRESHOLD,
                "seed {seed} never reaches mountain height (max {max_h})"
            );
            assert!(
                min_h < SEA_LEVEL,
                "seed {seed} carves no ocean basins (min {min_h})"
            );
            assert!(
                ocean_columns > 0,
                "seed {seed} has no columns below SEA_LEVEL"
            );
            eprintln!(
                "envelope scan seed {seed}: min {min_h}, max {max_h}, ocean columns {ocean_columns}"
            );
        }
    }

    /// The deepening is continuous at the waterline and saturates in deep
    /// water: land columns get no bonus, shallow puddles get only a fraction,
    /// and anything `OCEAN_DEEPENING_RAMP` blocks deep gets all of it. Because
    /// the bonus keys off the raw height, it can never move a land column into
    /// water or a water column onto land.
    #[test]
    fn test_ocean_depth_bonus_is_zero_on_land_and_saturates_in_deep_water() {
        assert_eq!(
            ocean_depth_bonus(SEA_LEVEL as f32),
            0.0,
            "the bonus must be exactly zero at the waterline"
        );
        assert_eq!(
            ocean_depth_bonus(SEA_LEVEL as f32 + 40.0),
            0.0,
            "land columns must never be deepened"
        );

        let shallow = ocean_depth_bonus(SEA_LEVEL as f32 - 1.0);
        assert!(
            shallow > 0.0 && shallow < OCEAN_DEEPENING / 2.0,
            "a one-block-deep puddle should gain only a fraction of the bonus, got {shallow}"
        );

        let saturated = ocean_depth_bonus(SEA_LEVEL as f32 - OCEAN_DEEPENING_RAMP);
        assert!(
            (saturated - OCEAN_DEEPENING).abs() < 1e-5,
            "the bonus must saturate at {OCEAN_DEEPENING}, got {saturated}"
        );

        let mut previous = 0.0;
        for depth in 0..=OCEAN_DEEPENING_RAMP as i32 + 4 {
            let bonus = ocean_depth_bonus(SEA_LEVEL as f32 - depth as f32);
            assert!(
                bonus >= previous,
                "the bonus must grow monotonically with depth: {depth} -> {bonus} < {previous}"
            );
            assert!(bonus <= OCEAN_DEEPENING);
            previous = bonus;
        }
    }

    /// "A bit deeper" acceptance: the deepest sampled basins carry more water
    /// than the raw continental curve allows. The raw curve bottoms out at 4
    /// (18 blocks below sea level); the deepening must exceed 20 blocks, and
    /// every column must stay inside the named envelope.
    #[test]
    fn test_acceptance_ocean_basins_are_deepened() {
        const MIN_DEEP_OCEAN_DEPTH: i32 = 20;
        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            let mut min_h = i32::MAX;
            let mut deep_columns = 0usize;
            for wz in (-1024..=1024).step_by(8) {
                for wx in (-1024..=1024).step_by(8) {
                    let h = terrain_height_at_seeded(wx, wz, seed);
                    assert!(
                        (MIN_TERRAIN_HEIGHT..=MAX_TERRAIN_HEIGHT).contains(&h),
                        "seed {seed} height {h} at ({wx}, {wz}) outside envelope {MIN_TERRAIN_HEIGHT}..={MAX_TERRAIN_HEIGHT}"
                    );
                    min_h = min_h.min(h);
                    if SEA_LEVEL - h >= MIN_DEEP_OCEAN_DEPTH {
                        deep_columns += 1;
                    }
                }
            }
            assert!(
                deep_columns > 0,
                "seed {seed} has no ocean column at least {MIN_DEEP_OCEAN_DEPTH} blocks deep (min h {min_h})"
            );
            assert!(
                min_h >= MIN_TERRAIN_HEIGHT,
                "seed {seed} deepened below MIN_TERRAIN_HEIGHT: min {min_h}"
            );
            eprintln!(
                "ocean depth scan seed {seed}: min h {min_h}, deepest depth {}, \
                 columns >= {MIN_DEEP_OCEAN_DEPTH} deep: {deep_columns}",
                SEA_LEVEL - min_h
            );
        }
    }
}
