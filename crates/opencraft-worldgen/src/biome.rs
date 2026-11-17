//! Pure `(seed, x, z)` biome classification and its neutral tags.
//!
//! Classification reads the same height function the game uses, so a column
//! can never be labeled `Ocean` above sea level or `Mountains` below the
//! mountain threshold.

use crate::height::terrain_height_at_seeded;
use crate::noise::value_noise_2d;

pub use crate::height::{BEACH_BAND, MOUNTAIN_HEIGHT_THRESHOLD, SEA_LEVEL};
pub use opencraft_content::catalog::BiomeTag;
pub use opencraft_content::vocabulary::BIOME_NAMES;

/// Coarse worldgen biome for a column.
///
/// Classification reads the same continental/ridge sample as the height
/// function (it is computed *from* the height), so classification and height
/// cannot disagree: `Ocean` is exactly `height < SEA_LEVEL` and `Mountains` is
/// exactly `height >= MOUNTAIN_HEIGHT_THRESHOLD`. Plains and forest split the
/// remaining band on a third low-frequency noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    Plains,
    Forest,
    Mountains,
    Ocean,
}

impl Biome {
    /// Neutral content tag for this biome, index-aligned to
    /// [`BIOME_NAMES`].
    pub const fn tag(self) -> BiomeTag {
        match self {
            Biome::Plains => BiomeTag::Plains,
            Biome::Forest => BiomeTag::Forest,
            Biome::Mountains => BiomeTag::Mountains,
            Biome::Ocean => BiomeTag::Ocean,
        }
    }

    /// The biome for a neutral content tag.
    pub const fn from_tag(tag: BiomeTag) -> Biome {
        match tag {
            BiomeTag::Plains => Biome::Plains,
            BiomeTag::Forest => Biome::Forest,
            BiomeTag::Mountains => Biome::Mountains,
            BiomeTag::Ocean => Biome::Ocean,
        }
    }
}

/// Canonical vocabulary label for `biome` (`plains`, `forest`, `mountains`,
/// `ocean`).
pub fn biome_label(biome: Biome) -> &'static str {
    BIOME_NAMES[biome.tag().index() as usize]
}

/// Low-frequency forest mask. Deliberately a separate noise channel from the
/// continental/ridge pair that shapes height: the split between plains and
/// forest must not move the terrain surface.
#[inline]
pub fn forest_noise(wx: i32, wz: i32, seed: u32) -> f32 {
    let s = seed.wrapping_mul(0x9E37_79B9);
    value_noise_2d(wx as f32 * 0.006, wz as f32 * 0.006, 505 ^ s)
}

/// Above this forest-noise value a non-ocean, non-mountain column is forest.
const FOREST_NOISE_THRESHOLD: f32 = 0.55;

/// Pure height -> biome rule. Split out so the agreement pin has a small,
/// directly testable surface.
#[inline]
pub fn biome_for_height(height: i32, forest: f32) -> Biome {
    if height < SEA_LEVEL {
        Biome::Ocean
    } else if height >= MOUNTAIN_HEIGHT_THRESHOLD {
        Biome::Mountains
    } else if forest > FOREST_NOISE_THRESHOLD {
        Biome::Forest
    } else {
        Biome::Plains
    }
}

/// Pure biome classification for a world column.
///
/// Reads the same continental/ridge noise as `terrain_height_at_seeded` (it
/// classifies the height that function returns), so a column can never be
/// classified `Ocean` above sea level or `Mountains` below the mountain
/// threshold.
pub fn biome_at_seeded(wx: i32, wz: i32, seed: u32) -> Biome {
    biome_for_height(
        terrain_height_at_seeded(wx, wz, seed),
        forest_noise(wx, wz, seed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the game default seed; the crate never owns the game default.
    const DEFAULT_WORLD_SEED: u32 = 1337;

    /// Deterministic index for per-biome bookkeeping.
    fn biome_index(biome: Biome) -> usize {
        match biome {
            Biome::Plains => 0,
            Biome::Forest => 1,
            Biome::Mountains => 2,
            Biome::Ocean => 3,
        }
    }

    #[test]
    fn biome_labels_match_the_frozen_vocabulary() {
        let labels = [
            biome_label(Biome::Plains),
            biome_label(Biome::Forest),
            biome_label(Biome::Mountains),
            biome_label(Biome::Ocean),
        ];
        assert_eq!(labels, BIOME_NAMES);
    }

    #[test]
    fn biome_tags_round_trip() {
        for biome in [Biome::Plains, Biome::Forest, Biome::Mountains, Biome::Ocean] {
            assert_eq!(Biome::from_tag(biome.tag()), biome);
            assert_eq!(
                biome_label(biome),
                BIOME_NAMES[biome.tag().index() as usize]
            );
        }
        for tag in [
            BiomeTag::Plains,
            BiomeTag::Forest,
            BiomeTag::Mountains,
            BiomeTag::Ocean,
        ] {
            assert_eq!(Biome::from_tag(tag).tag(), tag);
        }
    }

    /// Acceptance: one pinned seed's reachable region contains all four
    /// biomes — plains, forest, mountains and ocean basins.
    #[test]
    fn test_acceptance_four_biomes_present_in_one_seed() {
        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            let mut counts = [0usize; 4];
            for wz in (-512..=512).step_by(4) {
                for wx in (-512..=512).step_by(4) {
                    counts[biome_index(biome_at_seeded(wx, wz, seed))] += 1;
                }
            }
            assert!(counts[0] > 0, "seed {seed} has no plains columns");
            assert!(counts[1] > 0, "seed {seed} has no forest columns");
            assert!(counts[2] > 0, "seed {seed} has no mountain columns");
            assert!(counts[3] > 0, "seed {seed} has no ocean columns");
            eprintln!(
                "four-biome scan seed {seed}: plains {}, forest {}, mountains {}, ocean {}",
                counts[0], counts[1], counts[2], counts[3]
            );
        }
    }

    /// Negative: classification can never disagree with the height it was
    /// derived from. Ocean is exactly below sea level, Mountains exactly at or
    /// above the mountain threshold, and land in between.
    #[test]
    fn test_biome_height_agreement_pin() {
        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            for wz in (-512..=512).step_by(4) {
                for wx in (-512..=512).step_by(4) {
                    let h = terrain_height_at_seeded(wx, wz, seed);
                    let biome = biome_at_seeded(wx, wz, seed);
                    assert_eq!(
                        biome,
                        biome_at_seeded(wx, wz, seed),
                        "biome classification is not pure at ({wx}, {wz})"
                    );
                    match biome {
                        Biome::Ocean => assert!(
                            h < SEA_LEVEL,
                            "Ocean column ({wx}, {wz}) has h {h} >= SEA_LEVEL"
                        ),
                        Biome::Mountains => assert!(
                            h >= MOUNTAIN_HEIGHT_THRESHOLD,
                            "Mountains column ({wx}, {wz}) has h {h} < threshold"
                        ),
                        Biome::Plains | Biome::Forest => assert!(
                            (SEA_LEVEL..MOUNTAIN_HEIGHT_THRESHOLD).contains(&h),
                            "land column ({wx}, {wz}) has h {h} outside sea..mountain band"
                        ),
                    }
                }
            }
        }
    }
}
