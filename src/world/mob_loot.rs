//! World-local mob drop vocabulary.
//!
//! This is worldgen/content vocabulary, never the player inventory: the module
//! deliberately has no player-layer import (D094, see [`crate::world::loot`]
//! for the established template). The player layer maps each [`MobDropItem`]
//! onto a real item identity (see `From<MobDropItem> for ItemId`).
//!
//! Every table is a fixed list of `(item, count, one_in)` entries. A kill is
//! still deterministic, but rarity is now expressed per entry: [`drop_roll`]
//! derives one value from the world seed, the mob's kind and its quantised
//! home cell, and [`MobDrop::rolls`] says whether that value lands the entry.
//! There is **no wall clock, tick index, or kill order** in the draw, so the
//! same skeleton at the same home in the same world always yields the same
//! rare bow — and a pig/cow/zombie table (all `one_in = 1`) is unaffected.
//!
//! [`MobKind::ALL`] is four kinds today; [`mob_drops`] matches every one so a
//! new kind is a compile error instead of a silent empty drop.

use glam::IVec3;

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
    Bone,
    Arrow,
    Bow,
}

/// What caused a mob's death, as far as drop attribution is concerned.
///
/// Only a player kill grants drops; an environmental death (fall, drowning,
/// fire in a later slice) resolves to the default zero report and mutates
/// nothing. This is deliberately a cause tag rather than a damage source: the
/// drop path only needs to know whether a player earned the table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MobKillCause {
    /// The killing blow was a player's melee hit or a player-fired arrow.
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
/// The count is an exact fixed amount rather than a range: a table entry
/// either lands whole or not at all, so the drop path still needs no
/// count-range RNG. Rarity is the only rolled dimension.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MobDrop {
    /// The yielded item identity.
    pub item: MobDropItem,
    /// Exact stack size yielded by one kill.
    pub count: u16,
    /// One-in-N gate for this entry: `1` means every kill, `16` means one
    /// kill in sixteen. Values `0` and `1` both mean "always" (see
    /// [`MobDrop::rolls`]).
    pub one_in: u32,
}

impl MobDrop {
    /// Builds one always-yielding table entry; `const` so the statics stay
    /// declarative. `one_in = 1` keeps the pre-rarity tables byte-identical.
    pub const fn new(item: MobDropItem, count: u16) -> Self {
        Self {
            item,
            count,
            one_in: 1,
        }
    }

    /// Builds one rare table entry that lands on one [`drop_roll`] in `one_in`.
    pub const fn rare(item: MobDropItem, count: u16, one_in: u32) -> Self {
        Self {
            item,
            count,
            one_in,
        }
    }

    /// Whether this entry lands for the given [`drop_roll`] value.
    ///
    /// `one_in <= 1` short-circuits to `true` **before** the modulo, so a
    /// misconfigured `one_in = 0` can never divide by zero — it degrades to
    /// "always drops" instead of panicking the kill path.
    pub fn rolls(&self, roll: u32) -> bool {
        self.one_in <= 1 || roll.is_multiple_of(self.one_in)
    }
}

/// One-in-N gate for the skeleton's rare bow: one kill in sixteen.
pub const SKELETON_BOW_ONE_IN: u32 = 16;

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

/// Skeleton drops: one bone, two arrows, and a rare bow (one kill in sixteen).
///
/// The bow uses an explicit durability-bearing tool identity in the player
/// layer, so it is deliberately `count = 1`: tools are non-stackable.
static SKELETON_DROPS: [MobDrop; 3] = [
    MobDrop::new(MobDropItem::Bone, 1),
    MobDrop::new(MobDropItem::Arrow, 2),
    MobDrop::rare(MobDropItem::Bow, 1, SKELETON_BOW_ONE_IN),
];

/// The pinned drop table for `kind`.
///
/// Total over [`MobKind`] by construction: adding a mob kind must add an arm
/// here, so a kind can never silently inherit another kind's drops. The table
/// itself carries rarity gates; [`crate::world::session::mob_drop_items`]
/// resolves them against [`drop_roll`].
pub fn mob_drops(kind: MobKind) -> &'static [MobDrop] {
    match kind {
        MobKind::Pig => &PIG_DROPS,
        MobKind::Cow => &COW_DROPS,
        MobKind::Zombie => &ZOMBIE_DROPS,
        MobKind::Skeleton => &SKELETON_DROPS,
    }
}

/// Deterministic rare-drop draw for one mob kill.
///
/// SplitMix64-style finalizer with its **own** constants so the drop table
/// never shares a stream with [`crate::world::mob::mob_roll`], the spawn
/// table, or the terrain hash (D094/MEM417). Identical inputs always return
/// the identical value.
///
/// The draw is keyed only on `(seed, kind tag, quantised home cell)`: the mob
/// record's home is persisted in the v7 save tail, so a reloaded skeleton
/// still rolls the same bow — and the result cannot drift with tick order,
/// kill order, or the wall clock. The `one_in` modulo reads the low bits, and
/// the finalizer avalanches every input bit into them.
pub fn drop_roll(seed: u32, kind: MobKind, home: IVec3) -> u32 {
    let mut z = (seed as u64).wrapping_mul(0xB5AD_4CE2_49B9_2F73)
        ^ (kind.tag() as u64).wrapping_mul(0x8D58_AC4D_9E37_79B1)
        ^ (home.x as u32 as u64).wrapping_mul(0x2545_F491_4F6C_DD1D)
        ^ (home.y as u32 as u64).wrapping_mul(0xA3C5_9B1D_7E2F_4A69)
        ^ (home.z as u32 as u64).wrapping_mul(0x6A09_E667_F3BC_C908);
    z = z.wrapping_add(0xED5A_D551_2F47_2B19);
    z = (z ^ (z >> 30)).wrapping_mul(0x9E37_79B1_85EB_CA87);
    z = (z ^ (z >> 27)).wrapping_mul(0xC2B2_AE35_27D4_EB2F);
    z ^= z >> 31;
    (z ^ (z >> 32)) as u32
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
    fn existing_tables_keep_one_in_one() {
        for kind in [MobKind::Pig, MobKind::Cow, MobKind::Zombie] {
            for drop in mob_drops(kind) {
                assert_eq!(
                    drop.one_in, 1,
                    "{kind:?} {:?} must stay unconditional",
                    drop.item
                );
            }
        }
    }

    #[test]
    fn skeleton_drops_are_bone_arrows_and_a_rare_bow() {
        assert_eq!(
            mob_drops(MobKind::Skeleton),
            [
                expected(MobDropItem::Bone, 1),
                expected(MobDropItem::Arrow, 2),
                MobDrop::rare(MobDropItem::Bow, 1, SKELETON_BOW_ONE_IN),
            ]
        );
        assert_eq!(SKELETON_BOW_ONE_IN, 16);
        assert_eq!(mob_drops(MobKind::Skeleton)[2].one_in, 16);
    }

    #[test]
    fn always_entries_roll_and_rare_zero_short_circuits() {
        assert!(MobDrop::new(MobDropItem::Bone, 1).rolls(0));
        assert!(MobDrop::new(MobDropItem::Bone, 1).rolls(u32::MAX));

        let misconfigured = MobDrop::rare(MobDropItem::Bow, 1, 0);
        assert!(
            misconfigured.rolls(0) && misconfigured.rolls(u32::MAX),
            "one_in = 0 must degrade to always, never divide by zero"
        );

        let rare = MobDrop::rare(MobDropItem::Bow, 1, 16);
        assert!(rare.rolls(0));
        assert!(rare.rolls(32));
        assert!(!rare.rolls(1));
        assert!(!rare.rolls(31));
    }

    #[test]
    fn drop_roll_is_deterministic_and_does_not_reuse_mob_roll() {
        let home = IVec3::new(8, 5, 8);
        assert_eq!(
            drop_roll(7, MobKind::Skeleton, home),
            drop_roll(7, MobKind::Skeleton, home)
        );
        assert_ne!(
            drop_roll(7, MobKind::Skeleton, home),
            drop_roll(8, MobKind::Skeleton, home),
            "a different world seed must draw differently"
        );
        assert_ne!(
            drop_roll(7, MobKind::Skeleton, home),
            drop_roll(7, MobKind::Zombie, home),
            "different kinds must be independent"
        );
        assert_ne!(
            drop_roll(7, MobKind::Skeleton, home),
            drop_roll(7, MobKind::Skeleton, IVec3::new(9, 5, 8))
        );
        assert_ne!(
            drop_roll(7, MobKind::Skeleton, home),
            drop_roll(7, MobKind::Skeleton, IVec3::new(8, 6, 8))
        );
        assert_ne!(
            drop_roll(7, MobKind::Skeleton, home),
            drop_roll(7, MobKind::Skeleton, IVec3::new(8, 5, 9))
        );
        // The drop table must not ride on the mob-AI stream (MEM417/D094).
        assert_ne!(
            drop_roll(7, MobKind::Skeleton, home),
            crate::world::mob::mob_roll(7, 0, 3)
        );
    }

    #[test]
    fn drop_roll_hits_and_misses_the_rare_gate() {
        let rare = MobDrop::rare(MobDropItem::Bow, 1, SKELETON_BOW_ONE_IN);
        let mut hits = 0;
        let mut misses = 0;
        for x in 0..64 {
            let hit = rare.rolls(drop_roll(9, MobKind::Skeleton, IVec3::new(x, 5, 8)));
            if hit {
                hits += 1;
            } else {
                misses += 1;
            }
        }
        assert!(hits > 0, "64 homes must contain at least one bow roll");
        assert!(misses > 0, "64 homes must contain at least one miss");
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
            // reachable and that every table now yields at least one entry.
            assert!(
                !mob_drops(kind).is_empty(),
                "{kind:?} must have a populated drop table"
            );
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
