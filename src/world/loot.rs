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
