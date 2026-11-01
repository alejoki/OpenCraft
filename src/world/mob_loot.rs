//! World-local mob drop vocabulary.
//!
//! This is worldgen/content vocabulary, never the player inventory: the module
//! deliberately has no player-layer import (D094, see [`crate::world::loot`]
//! for the established template). The player layer maps each [`MobDropItem`]
//! onto a real item identity (see `From<MobDropItem> for ItemId`).
//!
//! Every table is a fixed list of `(item, count)` pairs, so a kill is
//! deterministic: there is no RNG, no ranged roll, and no wall clock in this
//! module. S06 adds the zombie table and S08 the skeleton table to this same
//! module, which is why both hostile tables exist here already as empty
//! arrays rather than being absent.
//!
//! [`MobKind::ALL`] is four kinds today; [`mob_drops`] matches every one so a
//! new kind is a compile error instead of a silent empty drop.

use crate::world::mob::MobKind;

/// One item identity a mob can yield.
///
/// Declaration order is table readability, not a persisted tag: drop tables
/// are never written to saves, so this enum has no wire contract to protect.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MobDropItem {
    RawPorkchop,
    RawBeef,
    Leather,
}

/// One pinned entry in a mob drop table.
///
/// The count is an exact fixed amount rather than a range: every kill of a
/// given kind yields exactly these counts, so the drop path needs no RNG.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MobDrop {
    /// The yielded item identity.
    pub item: MobDropItem,
    /// Exact stack size yielded by one kill.
    pub count: u16,
}

impl MobDrop {
    /// Builds one fixed table entry; `const` so the statics stay declarative.
    pub const fn new(item: MobDropItem, count: u16) -> Self {
        Self { item, count }
    }
}

/// Pig drops: one raw porkchop, then one leather (meat before leather).
static PIG_DROPS: [MobDrop; 2] = [
    MobDrop::new(MobDropItem::RawPorkchop, 1),
    MobDrop::new(MobDropItem::Leather, 1),
];

/// Cow drops: one raw beef, then two leather (meat before leather).
static COW_DROPS: [MobDrop; 2] = [
    MobDrop::new(MobDropItem::RawBeef, 1),
    MobDrop::new(MobDropItem::Leather, 2),
];

/// Zombie drops stay empty until S06 fills them in.
static ZOMBIE_DROPS: [MobDrop; 0] = [];

/// Skeleton drops stay empty until S08 fills them in.
static SKELETON_DROPS: [MobDrop; 0] = [];

/// The pinned drop table for `kind`.
///
/// Total over [`MobKind`] by construction: adding a mob kind must add an arm
/// here, so a kind can never silently inherit another kind's drops.
pub fn mob_drops(kind: MobKind) -> &'static [MobDrop] {
    match kind {
        MobKind::Pig => &PIG_DROPS,
        MobKind::Cow => &COW_DROPS,
        MobKind::Zombie => &ZOMBIE_DROPS,
        MobKind::Skeleton => &SKELETON_DROPS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The player layer's material stack limit, duplicated here so this module
    /// stays free of player imports. The mapping test in the player layer pins
    /// the real limit against this bound.
    const MATERIAL_STACK_LIMIT: u16 = 64;

    fn expected(item: MobDropItem, count: u16) -> MobDrop {
        MobDrop::new(item, count)
    }

    #[test]
    fn pig_drops_are_exactly_pinned() {
        assert_eq!(
            mob_drops(MobKind::Pig),
            [
                expected(MobDropItem::RawPorkchop, 1),
                expected(MobDropItem::Leather, 1),
            ]
        );
    }

    #[test]
    fn cow_drops_are_exactly_pinned() {
        assert_eq!(
            mob_drops(MobKind::Cow),
            [
                expected(MobDropItem::RawBeef, 1),
                expected(MobDropItem::Leather, 2),
            ]
        );
    }

    #[test]
    fn passive_tables_are_meat_before_leather() {
        for (kind, meat) in [
            (MobKind::Pig, MobDropItem::RawPorkchop),
            (MobKind::Cow, MobDropItem::RawBeef),
        ] {
            let drops = mob_drops(kind);
            assert_eq!(drops.len(), 2, "{kind:?} must drop meat plus leather");
            assert_eq!(drops[0].item, meat, "{kind:?} must drop meat first");
            assert_eq!(
                drops[1].item,
                MobDropItem::Leather,
                "{kind:?} must drop leather second"
            );
        }
    }

    #[test]
    fn every_pinned_count_is_a_valid_material_stack() {
        for kind in MobKind::ALL {
            for drop in mob_drops(kind) {
                assert!(
                    (1..=MATERIAL_STACK_LIMIT).contains(&drop.count),
                    "{kind:?} drops {} x{:?}, which is not a valid material stack",
                    drop.count,
                    drop.item
                );
            }
        }
    }

    #[test]
    fn hostile_tables_are_empty_until_their_slices() {
        assert!(mob_drops(MobKind::Zombie).is_empty());
        assert!(mob_drops(MobKind::Skeleton).is_empty());
    }

    #[test]
    fn mob_drops_is_total_over_every_kind() {
        for kind in MobKind::ALL {
            // A missing arm would not compile; this pins that every kind is
            // reachable and that only the passive kinds drop anything yet.
            let drops = mob_drops(kind);
            match kind {
                MobKind::Pig | MobKind::Cow => assert!(!drops.is_empty()),
                MobKind::Zombie | MobKind::Skeleton => assert!(drops.is_empty()),
            }
        }
    }
}
