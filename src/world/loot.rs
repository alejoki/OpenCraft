//! World-local loot vocabulary.
//!
//! Worldgen may decide *what a ruin can yield* without knowing anything about
//! the player inventory: this module deliberately has no `player::inventory`
//! import. [`LootEntry`] names a reward in this closed vocabulary plus an
//! inclusive count range; the player layer maps each [`LootItem`] onto a real
//! [`crate::player::inventory::ItemId`] (see `From<LootItem> for ItemId`).
//!
//! Keeping the vocabulary world-local is what stops a worldgen data change
//! from pulling UI, stacking, or save-tag concerns into terrain generation.
//! The enum is intentionally small and closed: an unknown identity is not
//! representable, so a typo cannot reach a loot table.

use glam::IVec3;

/// Probability denominator for the oak-leaf apple bonus: one in eight.
///
/// Denominator form keeps the roll comparable and testable instead of hiding
/// a floating-point threshold in the break path.
pub const OAK_LEAVES_APPLE_DENOMINATOR: u32 = 8;

/// Whether breaking oak leaves at `pos` in a world with `seed` also drops an
/// apple.
///
/// Deliberately world-local and independent of `terrain::hash3d`: that hash is
/// private, tuned to noise generation, and a terrain retune must never move
/// the world's drop table. The mixer below owns its constants (xor-shift plus
/// wrapping-multiply finalizer), so the result is stable for one
/// `(seed, pos)` and changes with either input.
pub fn oak_leaves_apple(seed: u32, pos: IVec3) -> bool {
    let mut hash = seed ^ 0x9E37_79B9;
    hash ^= (pos.x as u32).wrapping_mul(0x85EB_CA6B);
    hash ^= (pos.y as u32).wrapping_mul(0xC2B2_AE35);
    hash ^= (pos.z as u32).wrapping_mul(0x27D4_EB2F);
    // Murmur3-style fmix32 finalizer: avalanche every input bit into the low
    // bits the modulo reads, so nearby coordinates do not cluster.
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85EB_CA6B);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xC2B2_AE35);
    hash ^= hash >> 16;
    hash.is_multiple_of(OAK_LEAVES_APPLE_DENOMINATOR)
}

/// One item identity a ruin can yield.
///
/// This is worldgen's vocabulary, not the player's: it deliberately has fewer
/// variants than [`crate::player::inventory::Material`] and no block ids. The
/// survival layer resolves ores to their block item identity when the drop is
/// collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LootItem {
    Apple,
    Bread,
    Wheat,
    WheatSeeds,
    Stick,
    CoalOre,
    IronOre,
}

/// One prospective reward a ruin can yield.
///
/// A stable [item](LootItem) plus an inclusive `min_count..=max_count` range.
/// Nothing in M008 constructs a non-empty table yet; the type exists so the
/// survival layer can fill the M007 seam without changing its signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootEntry {
    /// The yielded item identity.
    pub item: LootItem,
    /// Inclusive lower bound on the stack size of one roll.
    pub min_count: u16,
    /// Inclusive upper bound on the stack size of one roll.
    pub max_count: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The complete M008 loot vocabulary. Growing the enum must grow this
    /// list; the exhaustive match below stops compiling when a variant is
    /// forgotten.
    const M008_LOOT: [LootItem; 7] = [
        LootItem::Apple,
        LootItem::Bread,
        LootItem::Wheat,
        LootItem::WheatSeeds,
        LootItem::Stick,
        LootItem::CoalOre,
        LootItem::IronOre,
    ];

    fn oak_leaves_apple_scan(seed: u32) -> Vec<(IVec3, bool)> {
        let mut results = Vec::new();
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    let pos = IVec3::new(x, y, z);
                    results.push((pos, oak_leaves_apple(seed, pos)));
                }
            }
        }
        results
    }

    #[test]
    fn oak_leaves_apple_denominator_is_nonzero() {
        assert_ne!(
            OAK_LEAVES_APPLE_DENOMINATOR, 0,
            "a zero denominator would panic on modulo"
        );
    }

    #[test]
    fn oak_leaves_apple_is_deterministic_for_the_same_seed_and_position() {
        for pos in [
            IVec3::new(0, 0, 0),
            IVec3::new(3, 12, -7),
            IVec3::new(-15, 31, 8),
        ] {
            assert_eq!(
                oak_leaves_apple(42, pos),
                oak_leaves_apple(42, pos),
                "the roll must not depend on call order"
            );
        }
    }

    #[test]
    fn oak_leaves_apple_scan_produces_both_outcomes_inside_a_loose_rate_band() {
        let results = oak_leaves_apple_scan(1);
        let hits = results.iter().filter(|(_, hit)| *hit).count();
        let total = results.len();

        assert!(
            hits > 0 && hits < total,
            "a 16x16x16 scan must contain both outcomes, got {hits}/{total}"
        );
        let rate_percent = hits * 100 / total;
        assert!(
            (5..=25).contains(&rate_percent),
            "hit rate {rate_percent}% is outside the 5-25% band"
        );
    }

    #[test]
    fn oak_leaves_apple_changes_with_the_seed_across_a_fixed_scan() {
        let small_scan = |seed: u32| {
            let mut outcomes = Vec::new();
            for x in 0..4 {
                for y in 0..4 {
                    for z in 0..4 {
                        outcomes.push(oak_leaves_apple(seed, IVec3::new(x, y, z)));
                    }
                }
            }
            outcomes
        };

        let seed_one = small_scan(1);
        let seed_two = small_scan(2);
        assert!(
            seed_one
                .iter()
                .zip(&seed_two)
                .any(|(left, right)| left != right),
            "changing the seed must change at least one result"
        );
    }

    #[test]
    fn oak_leaves_apple_changes_with_coordinates_across_a_fixed_scan() {
        let results: Vec<bool> = (0..4)
            .flat_map(|x| (0..4).flat_map(move |y| (0..4).map(move |z| (x, y, z))))
            .map(|(x, y, z)| oak_leaves_apple(1, IVec3::new(x, y, z)))
            .collect();

        assert!(
            results.iter().any(|hit| *hit) && results.iter().any(|hit| !*hit),
            "coordinate changes must be observable within a small scan"
        );
    }

    #[test]
    fn loot_entry_is_constructible_with_an_inclusive_range() {
        let entry = LootEntry {
            item: LootItem::Apple,
            min_count: 1,
            max_count: 3,
        };
        assert_eq!(entry.item, LootItem::Apple);
        assert_eq!(entry.min_count, 1);
        assert_eq!(entry.max_count, 3);
        assert!(entry.min_count <= entry.max_count);

        let single = LootEntry {
            item: LootItem::CoalOre,
            min_count: 1,
            max_count: 1,
        };
        assert_eq!(single.item, LootItem::CoalOre);
        assert_eq!((single.min_count, single.max_count), (1, 1));
    }

    #[test]
    fn vocabulary_is_exhaustive_for_the_m008_set() {
        let mut names = Vec::new();
        for item in M008_LOOT {
            // Exhaustive match: adding a variant without updating this test
            // fails to compile rather than silently missing coverage.
            let name = match item {
                LootItem::Apple => "Apple",
                LootItem::Bread => "Bread",
                LootItem::Wheat => "Wheat",
                LootItem::WheatSeeds => "WheatSeeds",
                LootItem::Stick => "Stick",
                LootItem::CoalOre => "CoalOre",
                LootItem::IronOre => "IronOre",
            };
            names.push(name);
        }

        assert_eq!(names.len(), 7, "the M008 vocabulary is seven identities");
        let mut deduped = names.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(deduped.len(), names.len(), "no two variants share a name");
        assert_eq!(M008_LOOT.len(), 7);
    }
}
