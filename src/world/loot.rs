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
//!
//! [`ruin_loot`] is the deterministic ruin table: a pure function of
//! `(seed, anchor_x, anchor_z)` that always includes `Wheat` and `WheatSeeds`
//! so the first farm is reachable, rolls 3..=5 further extras, and never
//! produces more entries than one chest has slots. It owns a private mixer
//! with its own constants: terrain hashing is tuned for noise and must never
//! move a drop table when it is retuned.

use glam::IVec3;

/// Slot capacity of one chest, duplicated world-locally so worldgen can bound
/// its own table without importing `player::inventory` (D094).
///
/// Keep in step with `crate::player::inventory::CHEST_SLOTS`; the session layer
/// fails closed if a table ever exceeds the real container.
pub const RUIN_LOOT_MAX_SLOTS: usize = 27;

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
/// [`ruin_loot`] always emits degenerate ranges (`min_count == max_count`)
/// because it rolls the count for each generated ruin instead of asking the
/// session to roll it later; the range type is kept so the table can widen
/// without changing the D084 seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootEntry {
    /// The yielded item identity.
    pub item: LootItem,
    /// Inclusive lower bound on the stack size of one roll.
    pub min_count: u16,
    /// Inclusive upper bound on the stack size of one roll.
    pub max_count: u16,
}

/// Extras a ruin may also contain, with their inclusive count ranges.
///
/// `Wheat` and `WheatSeeds` are deliberately absent: they are unconditional
/// head entries, not part of the shuffled pool.
const RUIN_LOOT_EXTRAS: [(LootItem, u16, u16); 5] = [
    (LootItem::Apple, 1, 2),
    (LootItem::Bread, 1, 1),
    (LootItem::Stick, 2, 4),
    (LootItem::CoalOre, 1, 4),
    (LootItem::IronOre, 1, 2),
];

// Salts are spaced so XOR-ing a small index into one never collides with
// another purpose's salt. They are private: callers ask for loot, not hashes.
const RUIN_LOOT_SALT_EXTRA_COUNT: u32 = 0x11A1_0000;
const RUIN_LOOT_SALT_SHUFFLE: u32 = 0x22B2_0000;
const RUIN_LOOT_SALT_ROLL: u32 = 0x33C3_0000;
const RUIN_LOOT_SALT_WHEAT: u32 = 0x44D4_0000;
const RUIN_LOOT_SALT_SEEDS: u32 = 0x55E5_0000;

/// 32-bit mixer owned by the ruin drop table.
///
/// Deliberately not `terrain::hash3d` and deliberately not the
/// `oak_leaves_apple` constants: those belong to differently tuned systems, so
/// a terrain-noise retune or a leaf-rate tweak must not silently reroll ruin
/// loot. The xor-shift + wrapping-multiply finalizer avalanches every input bit
/// into the low bits the modulo reads, so neighbouring anchors do not cluster.
fn ruin_loot_hash(seed: u32, anchor_x: i32, anchor_z: i32, salt: u32) -> u32 {
    let mut hash = seed.wrapping_mul(0x7A2D_8C4B)
        ^ (anchor_x as u32).wrapping_mul(0xA24B_AED5)
        ^ (anchor_z as u32).wrapping_mul(0x9FB2_1C65)
        ^ salt.wrapping_mul(0x5C3F_91ED);
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x6E5E_1A27);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0x3B7A_4D19);
    hash ^= hash >> 16;
    hash
}

/// One concrete entry: roll an inclusive count from `min..=max` with `hash`.
fn rolled_entry(item: LootItem, min_count: u16, max_count: u16, hash: u32) -> LootEntry {
    let span = u32::from(max_count - min_count) + 1;
    let count = min_count + (hash % span) as u16;
    LootEntry {
        item,
        min_count: count,
        max_count: count,
    }
}

/// Deterministic, capacity-bounded loot for the ruin anchored at
/// `(anchor_x, anchor_z)` in a world with `seed`.
///
/// Shape and guarantees:
///
/// - `Wheat` (1..=2) and `WheatSeeds` (2..=4) always come first, so S07's
///   first farm is reachable from any ruin.
/// - 3..=5 distinct extras are drawn from [`RUIN_LOOT_EXTRAS`] by a
///   deterministic partial Fisher-Yates shuffle, then each count is rolled in
///   its own range. The returned order is the shuffle order.
/// - 5..=7 entries total: always within [`RUIN_LOOT_MAX_SLOTS`], with no
///   duplicate item and every count in `1..=64`.
/// - Each range is degenerate (`min_count == max_count`): the table is already
///   rolled, so opening the chest never needs an RNG and never re-rolls.
///
/// Pure by construction — a function of `(seed, anchor_x, anchor_z)` only. It
/// reads no clock, RNG state, season/day index or chunk, so loot can never
/// make worldgen season-dependent (the cross-slice season-purity invariant)
/// and every caller asking about the same ruin always gets the same answer.
pub fn ruin_loot(seed: u32, anchor_x: i32, anchor_z: i32) -> Vec<LootEntry> {
    let mut entries = Vec::with_capacity(7);
    entries.push(rolled_entry(
        LootItem::Wheat,
        1,
        2,
        ruin_loot_hash(seed, anchor_x, anchor_z, RUIN_LOOT_SALT_WHEAT),
    ));
    entries.push(rolled_entry(
        LootItem::WheatSeeds,
        2,
        4,
        ruin_loot_hash(seed, anchor_x, anchor_z, RUIN_LOOT_SALT_SEEDS),
    ));

    let extra_count =
        3 + (ruin_loot_hash(seed, anchor_x, anchor_z, RUIN_LOOT_SALT_EXTRA_COUNT) % 3) as usize;

    // Partial Fisher-Yates: only the prefix this ruin uses is shuffled, and
    // each swap draws from the untouched suffix, so the chosen extras are
    // distinct by construction.
    let mut extras = RUIN_LOOT_EXTRAS;
    for index in 0..extra_count {
        let remaining = extras.len() - index;
        let offset = (ruin_loot_hash(
            seed,
            anchor_x,
            anchor_z,
            RUIN_LOOT_SALT_SHUFFLE ^ index as u32,
        ) % remaining as u32) as usize;
        extras.swap(index, index + offset);
    }

    for (index, &(item, min_count, max_count)) in extras.iter().take(extra_count).enumerate() {
        entries.push(rolled_entry(
            item,
            min_count,
            max_count,
            ruin_loot_hash(seed, anchor_x, anchor_z, RUIN_LOOT_SALT_ROLL ^ index as u32),
        ));
    }

    debug_assert!(entries.len() <= RUIN_LOOT_MAX_SLOTS);
    entries
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
    fn ruin_loot_is_deterministic_for_the_same_seed_and_anchor() {
        for (x, z) in [(0, 0), (123, -456), (-2048, 9876)] {
            assert_eq!(
                ruin_loot(99, x, z),
                ruin_loot(99, x, z),
                "the table must not depend on call order or hidden state"
            );
        }
    }

    #[test]
    fn ruin_loot_changes_with_the_anchor_and_the_seed() {
        let base = ruin_loot(7, 0, 0);
        assert_ne!(base, ruin_loot(7, 1, 0), "a +x neighbour must reroll loot");
        assert_ne!(base, ruin_loot(7, 0, 1), "a +z neighbour must reroll loot");
        assert_ne!(base, ruin_loot(8, 0, 0), "a new seed must reroll loot");
    }

    #[test]
    fn ruin_loot_always_contains_wheat_and_wheat_seeds() {
        for seed in [0, 1, 7, 0x0BAD_F00D, u32::MAX] {
            for x in -4..=4 {
                for z in -4..=4 {
                    let loot = ruin_loot(seed, x, z);
                    let items: Vec<LootItem> = loot.iter().map(|entry| entry.item).collect();
                    assert!(
                        items.contains(&LootItem::Wheat),
                        "seed {seed} ruin ({x}, {z}) has no Wheat: {items:?}"
                    );
                    assert!(
                        items.contains(&LootItem::WheatSeeds),
                        "seed {seed} ruin ({x}, {z}) has no WheatSeeds: {items:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ruin_loot_is_capacity_bounded_with_degenerate_ranges_and_no_duplicates() {
        for seed in 0..=15 {
            for x in -8..=8 {
                for z in -8..=8 {
                    let loot = ruin_loot(seed, x, z);
                    assert!(
                        (5..=7).contains(&loot.len()),
                        "seed {seed} ruin ({x}, {z}) returned {} entries",
                        loot.len()
                    );
                    assert!(
                        loot.len() <= RUIN_LOOT_MAX_SLOTS,
                        "seed {seed} ruin ({x}, {z}) overflows a chest"
                    );
                    assert_eq!(loot[0].item, LootItem::Wheat, "Wheat must stay first");
                    assert_eq!(
                        loot[1].item,
                        LootItem::WheatSeeds,
                        "WheatSeeds must stay second"
                    );
                    for entry in &loot {
                        assert_eq!(
                            entry.min_count, entry.max_count,
                            "seed {seed} ruin ({x}, {z}) left a range unrolled: {entry:?}"
                        );
                        assert!(
                            (1..=64).contains(&entry.min_count),
                            "seed {seed} ruin ({x}, {z}) count {} outside 1..=64",
                            entry.min_count
                        );
                    }
                    for (i, entry) in loot.iter().enumerate() {
                        for other in &loot[i + 1..] {
                            assert_ne!(
                                entry.item, other.item,
                                "seed {seed} ruin ({x}, {z}) repeats {:?}",
                                entry.item
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ruin_loot_table_is_pinned_for_one_documented_anchor() {
        // Pinned on purpose: changing the mixer constants, salts, extras pool,
        // ranges, shuffle direction or roll order must fail here so a drop-table
        // change is a deliberate edit rather than an accidental drift. The
        // anchor is one documented pair, not a generated ruin, so this test
        // stays independent of terrain tuning.
        let loot = ruin_loot(0x0BAD_F00D, 123, -456);
        let expected = vec![
            LootEntry {
                item: LootItem::Wheat,
                min_count: 1,
                max_count: 1,
            },
            LootEntry {
                item: LootItem::WheatSeeds,
                min_count: 3,
                max_count: 3,
            },
            LootEntry {
                item: LootItem::CoalOre,
                min_count: 4,
                max_count: 4,
            },
            LootEntry {
                item: LootItem::Stick,
                min_count: 3,
                max_count: 3,
            },
            LootEntry {
                item: LootItem::Apple,
                min_count: 2,
                max_count: 2,
            },
            LootEntry {
                item: LootItem::Bread,
                min_count: 1,
                max_count: 1,
            },
            LootEntry {
                item: LootItem::IronOre,
                min_count: 2,
                max_count: 2,
            },
        ];
        assert_eq!(loot, expected);
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
