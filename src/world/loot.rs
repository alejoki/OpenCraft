//! World-local loot vocabulary.
//!
//! Worldgen may decide *what a ruin can yield* without knowing anything about
//! the player inventory: this module deliberately has no `player::inventory`
//! import. [`LootEntry`] names a reward in this closed vocabulary plus an
//! inclusive count range; the player layer maps each [`LootItem`] onto a real
//! item identity (see its `From<LootItem> for ItemId` adapter).
//!
//! Keeping the vocabulary world-local is what stops a worldgen data change
//! from pulling UI, stacking, or save-tag concerns into terrain generation.
//! The enum is intentionally small and closed: an unknown identity is not
//! representable, so a typo cannot reach a loot table.
//!
//! The vocabulary, the deterministic ruin table and the wheat-and-seed
//! harvest rules live in the neutral `opencraft-worldgen` crate; this module is
//! the game-side adapter that keeps every existing `world::loot` path and
//! signature. [`wheat_harvest`] is the one wrapper: the crate matches raw
//! `u16` ids, and the game passes its `BlockId` through `to_raw` so calls stay
//! typed. The `From<LootItem> for ItemId` mapping stays in the player layer,
//! exactly as before.

pub use opencraft_worldgen::loot::*;

use crate::world::block::BlockId;

/// Deterministic break-site yield for `block`, or `None` when breaking it
/// yields no crop loot.
///
/// Game adapter over the neutral `opencraft_worldgen::loot::wheat_harvest`:
/// mature `BlockId::WheatStage3` yields one wheat and two seeds; immature
/// stages 0-2 return one seed; every other block has no harvest table. The
/// crate matches raw ids, so this wrapper forwards `block.to_raw()` and keeps
/// every call site on the `BlockId` signature. The session maps entries
/// through its fail-closed `loot_items` helper, so the degenerate counts are
/// consumed exactly as written.
pub fn wheat_harvest(block: BlockId) -> Option<&'static [LootEntry]> {
    opencraft_worldgen::loot::wheat_harvest(block.to_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{Item, ItemId, Material};

    #[test]
    fn wheat_harvest_entries_map_through_item_construction() {
        let mature = wheat_harvest(BlockId::WheatStage3).expect("mature wheat must yield");
        let items: Vec<Item> = mature
            .iter()
            .map(|entry| {
                Item::new(ItemId::from(entry.item), entry.min_count)
                    .expect("a degenerate non-zero entry must build a real item")
            })
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id(), ItemId::Material(Material::Wheat));
        assert_eq!(items[0].count(), 1);
        assert_eq!(items[1].id(), ItemId::Material(Material::WheatSeeds));
        assert_eq!(items[1].count(), 2);

        let immature = wheat_harvest(BlockId::WheatStage1).expect("immature wheat must yield");
        let seed = Item::new(ItemId::from(immature[0].item), immature[0].min_count)
            .expect("the immature entry must build a real item");
        assert_eq!(seed.id(), ItemId::Material(Material::WheatSeeds));
        assert_eq!(seed.count(), 1);
    }
}
