//! World-local mob drop vocabulary.
//!
//! This is worldgen/content vocabulary, never the player inventory: the module
//! deliberately has no player-layer import (D094, see [`crate::world::loot`]
//! for the established template). The player layer maps each [`MobDropItem`]
//! onto a real item identity (see `From<MobDropItem> for ItemId`).
//!
//! Every table is a fixed list of `(item, count)` pairs, so a kill is
//! deterministic: there is no RNG, no ranged roll, and no wall clock in this
//! module. S06 fills the zombie table (one rotten flesh) and S08 still owns
//! the skeleton table, which stays an empty array here rather than being
//! absent.
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
    RottenFlesh,
}

/// What caused a mob's death, as far as drop attribution is concerned.
///
/// Only a player kill grants drops; an environmental death (fall, drowning,
/// fire in a later slice) resolves to the default zero report and mutates
/// nothing. This is deliberately a cause tag rather than a damage source: the
/// drop path only needs to know whether a player earned the table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MobKillCause {
    /// The killing blow was a player's melee hit.
    Player,
    /// Anything else killed the mob; no drops are granted.
    Environment,
}

/// What one [`MobDropReport`] delivery actually did.
///
/// Mirrors the player layer's best-effort batch accounting without importing
/// it: `accepted + lost` is always the total unit count of the resolved table,
/// so a full inventory can only lose the overflow and never duplicate units.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MobDropReport {
    /// Units that landed in the inventory.
    pub accepted: u32,
    /// Units that did not fit and were dropped on the floor.
    pub lost: u32,
}

impl MobDropReport {
    /// Builds a report with explicit accepted/lost totals.
    pub const fn new(accepted: u32, lost: u32) -> Self {
        Self { accepted, lost }
    }

    /// Whether any unit failed to fit.
    pub fn lost_any(&self) -> bool {
        self.lost > 0
    }
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

/// Zombie drops: exactly one rotten flesh.
///
/// One unit, not a range: a kill is deterministic and the count has to fit a
/// material stack, so the drop path needs no RNG and no overflow rule.
static ZOMBIE_DROPS: [MobDrop; 1] = [MobDrop::new(MobDropItem::RottenFlesh, 1)];

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
    fn zombie_drops_exactly_one_rotten_flesh() {
        assert_eq!(
            mob_drops(MobKind::Zombie),
            [expected(MobDropItem::RottenFlesh, 1)]
        );
    }

    #[test]
    fn skeleton_table_stays_empty_until_s08() {
        assert!(mob_drops(MobKind::Skeleton).is_empty());
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
    fn mob_drops_is_total_over_every_kind() {
        for kind in MobKind::ALL {
            // A missing arm would not compile; this pins that every kind is
            // reachable and that only the hostile skeleton is still empty.
            let drops = mob_drops(kind);
            match kind {
                MobKind::Pig | MobKind::Cow | MobKind::Zombie => assert!(!drops.is_empty()),
                MobKind::Skeleton => assert!(drops.is_empty()),
            }
        }
    }

    #[test]
    fn drop_report_accounts_for_losses_and_is_default_zero() {
        assert_eq!(MobDropReport::default(), MobDropReport::new(0, 0));
        assert!(!MobDropReport::default().lost_any());
        assert_eq!(MobDropReport::new(3, 0), MobDropReport::new(3, 0));
        assert!(!MobDropReport::new(3, 0).lost_any());
        assert_eq!(MobDropReport::new(1, 2), MobDropReport::new(1, 2));
        assert!(MobDropReport::new(1, 2).lost_any());
    }

    #[test]
    fn kill_cause_distinguishes_player_from_environment() {
        assert_ne!(MobKillCause::Player, MobKillCause::Environment);
        assert_eq!(MobKillCause::Player, MobKillCause::Player);
        assert_eq!(MobKillCause::Environment, MobKillCause::Environment);
    }
}
