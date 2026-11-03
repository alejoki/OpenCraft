//! Pure hold-to-eat domain for survival food.
//!
//! This module owns *when* an item is eaten and *how much* hunger it restores,
//! but never mutates the player behind the inventory's back. [`EatingState`]
//! accumulates progress while a caller holds the eat input and emits
//! [`EatingTick::Ate`] exactly once per completed cycle; the caller is then
//! responsible for consuming exactly one item from the reported slot and
//! applying the reported hunger restore.
//!
//! Eating consumes real frame time, exactly like [`crate::player::mining`]:
//! callers pass the frame delta scaled by the game clock, and this domain
//! clamps it to 0.1 s so a stalled frame or a debug time-scale cannot skip a
//! meal. It deliberately does not know about `time_scale`.

use crate::player::inventory::{Item, ItemId, Material, HOTBAR_SLOTS};
use crate::player::survival::MAX_HUNGER;

/// Seconds of continuous hold required to eat one item.
///
/// This is real frame time, like mining: callers feed the frame delta already
/// scaled by the game clock, and progress is capped at 0.1 s per tick. It is
/// not accelerated by `time_scale` beyond that per-frame clamp.
pub const EAT_DURATION_SECS: f32 = 1.6;

/// Hunger restored by one apple.
pub const APPLE_HUNGER: u16 = 4;

/// Hunger restored by one bread.
///
/// Bread is the stronger food: it must always restore more than an apple.
pub const BREAD_HUNGER: u16 = 6;

/// Hunger restored by one raw meat item (raw porkchop or raw beef).
///
/// Raw meat sits between an apple and bread: it is a real emergency meal the
/// mob drop path can deliver, but weaker than a crafted staple. The value is
/// pinned by tests so a balance change is a deliberate edit.
pub const RAW_MEAT_HUNGER: u16 = 3;

/// Hunger restored by rotten flesh.
///
/// The floor of the food table: filler that still counts as food so a hostile
/// drop can feed a desperate player without competing with cooked meat. Pinned
/// by tests.
pub const ROTTEN_FLESH_HUNGER: u16 = 1;

/// Hunger restored by eating one `id`, or `None` when `id` is not food.
pub fn food_value(id: ItemId) -> Option<u16> {
    match id {
        ItemId::Material(Material::Apple) => Some(APPLE_HUNGER),
        ItemId::Material(Material::Bread) => Some(BREAD_HUNGER),
        ItemId::Material(Material::RawPorkchop | Material::RawBeef) => Some(RAW_MEAT_HUNGER),
        ItemId::Material(Material::RottenFlesh) => Some(ROTTEN_FLESH_HUNGER),
        _ => None,
    }
}

/// A validated eating target: a food item in a hotbar slot, with the hunger
/// it would restore and the hold duration it requires.
///
/// Full hunger is *structurally* unstartable, not a UI-only check: hunger
/// `>= MAX_HUNGER` cannot produce a target, so a progress bar can never fill
/// for a refused meal. The target carries the item identity rather than the
/// [`Item`] instance because completion is the caller's action; the target
/// only says which slot and identity to consume from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EatingTarget {
    slot: usize,
    item: ItemId,
    restore: u16,
    duration: f32,
}

impl EatingTarget {
    /// Validates a food item held in `slot` for a player at `hunger`.
    ///
    /// Returns `None` when the item is not food, `hunger` is already full (or
    /// out of range), or `slot` is not a hotbar slot.
    pub fn new(slot: usize, item: Item, hunger: u16) -> Option<Self> {
        let id = item.id();
        let restore = food_value(id)?;
        if hunger >= MAX_HUNGER || slot >= HOTBAR_SLOTS {
            return None;
        }
        Some(Self {
            slot,
            item: id,
            restore,
            duration: EAT_DURATION_SECS,
        })
    }

    /// Hotbar slot the meal will be consumed from.
    pub const fn slot(self) -> usize {
        self.slot
    }

    /// Food identity the meal will consume.
    pub const fn item(self) -> ItemId {
        self.item
    }

    /// Hunger restored when the meal completes.
    pub const fn restore(self) -> u16 {
        self.restore
    }

    /// Continuous hold required, in seconds.
    pub const fn duration(self) -> f32 {
        self.duration
    }
}

/// Outcome of one eating tick.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EatingTick {
    /// Nothing is being eaten; there is no progress.
    Idle,
    /// A valid target is being held, with progress in `0.0..=1.0`.
    Eating { progress: f32 },
    /// A meal completed exactly once this tick.
    ///
    /// The caller must consume exactly one item from `slot` and restore
    /// `restore` hunger. The state has already reset, so continuing to hold
    /// starts a fresh cycle on the next tick.
    Ate {
        slot: usize,
        item: ItemId,
        restore: u16,
    },
}

/// Continuous hold-to-eat accumulator.
///
/// Mirrors [`crate::player::mining::MiningState`]: the accumulated state is
/// keyed on `(slot, item, duration)`, so switching slot or item restarts
/// progress instead of inheriting it.
#[derive(Copy, Clone, Debug)]
pub struct EatingState {
    slot: Option<usize>,
    item: Option<ItemId>,
    elapsed: f32,
    duration: f32,
}

impl EatingState {
    /// Empty state: nothing held, no progress.
    pub const fn new() -> Self {
        Self {
            slot: None,
            item: None,
            elapsed: 0.0,
            duration: 0.0,
        }
    }

    /// Forgets any partial meal.
    pub fn reset(&mut self) {
        self.slot = None;
        self.item = None;
        self.elapsed = 0.0;
        self.duration = 0.0;
    }

    /// Current progress in `0.0..=1.0`, or `None` while idle.
    pub fn progress(&self) -> Option<f32> {
        match (self.slot, self.item) {
            (Some(_), Some(_)) if self.duration > 0.0 => {
                Some((self.elapsed / self.duration).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    /// Advances the meal by `dt` seconds against `candidate`.
    ///
    /// Contract:
    /// - non-finite or non-positive `dt` adds nothing; positive `dt` is
    ///   clamped to 0.1 s so a stalled frame cannot skip progress;
    /// - `None` resets and returns [`EatingTick::Idle`];
    /// - a defensively invalid target (non-food identity, invalid slot, or
    ///   non-finite/non-positive duration) resets and returns `Idle`;
    /// - changing `(slot, item, duration)` restarts progress;
    /// - completion emits [`EatingTick::Ate`] exactly once and resets before
    ///   returning, so a still-held target begins a fresh cycle next tick.
    pub fn tick(&mut self, dt: f32, candidate: Option<EatingTarget>) -> EatingTick {
        let dt = if !dt.is_finite() || dt <= 0.0 {
            0.0
        } else {
            dt.min(0.1)
        };

        let Some(target) = candidate else {
            self.reset();
            return EatingTick::Idle;
        };
        // Fail closed even though `EatingTarget::new` already validated: a
        // degenerate target must never accumulate progress.
        if target.slot >= HOTBAR_SLOTS
            || food_value(target.item).is_none()
            || !target.duration.is_finite()
            || target.duration <= 0.0
        {
            self.reset();
            return EatingTick::Idle;
        }

        // Slot, identity, and duration are all part of the reset key:
        // switching food or slot mid-meal must not inherit progress.
        if self.slot != Some(target.slot)
            || self.item != Some(target.item)
            || self.duration != target.duration
        {
            self.slot = Some(target.slot);
            self.item = Some(target.item);
            self.elapsed = 0.0;
            self.duration = target.duration;
        }

        self.elapsed += dt;
        if self.elapsed >= self.duration {
            let ate = EatingTick::Ate {
                slot: target.slot,
                item: target.item,
                restore: target.restore,
            };
            self.reset();
            return ate;
        }

        EatingTick::Eating {
            progress: self.progress().unwrap_or(0.0),
        }
    }
}

impl Default for EatingState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{ToolKind, ToolTier};
    use crate::world::block::BlockId;

    fn apple() -> Item {
        Item::new(ItemId::Material(Material::Apple), 1).expect("valid apple")
    }

    fn bread() -> Item {
        Item::new(ItemId::Material(Material::Bread), 1).expect("valid bread")
    }

    fn apple_target(slot: usize, hunger: u16) -> EatingTarget {
        EatingTarget::new(slot, apple(), hunger).expect("apple in range is a valid target")
    }

    fn tick_progress(state: &mut EatingState, dt: f32, target: EatingTarget) -> f32 {
        match state.tick(dt, Some(target)) {
            EatingTick::Eating { progress } => progress,
            other => panic!("expected Eating, got {other:?}"),
        }
    }

    fn hold(state: &mut EatingState, target: EatingTarget, ticks: usize) {
        for _ in 0..ticks {
            match state.tick(0.1, Some(target)) {
                EatingTick::Eating { .. } => {}
                other => panic!("expected Eating, got {other:?}"),
            }
        }
    }

    #[test]
    fn food_values_are_apple_four_and_bread_six() {
        assert_eq!(
            food_value(ItemId::Material(Material::Apple)),
            Some(APPLE_HUNGER)
        );
        assert_eq!(
            food_value(ItemId::Material(Material::Bread)),
            Some(BREAD_HUNGER)
        );
        assert_eq!(APPLE_HUNGER, 4);
        assert_eq!(BREAD_HUNGER, 6);
        const { assert!(BREAD_HUNGER > APPLE_HUNGER, "bread must beat apple") };
    }

    #[test]
    fn non_food_identities_have_no_value() {
        let non_food = [
            ItemId::Material(Material::Stick),
            ItemId::Material(Material::Wheat),
            ItemId::Material(Material::WheatSeeds),
            ItemId::Material(Material::Leather),
            ItemId::Block(BlockId::Stone),
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron,
            },
            ItemId::Item(0),
        ];
        for id in non_food {
            assert_eq!(food_value(id), None, "{id:?} must not be food");
        }
    }

    #[test]
    fn raw_meat_and_rotten_flesh_values_are_pinned() {
        assert_eq!(RAW_MEAT_HUNGER, 3);
        assert_eq!(ROTTEN_FLESH_HUNGER, 1);
        assert_eq!(APPLE_HUNGER, 4, "apple must stay unchanged");
        assert_eq!(BREAD_HUNGER, 6, "bread must stay unchanged");
        const {
            assert!(
                RAW_MEAT_HUNGER > ROTTEN_FLESH_HUNGER,
                "raw meat must beat rotten flesh"
            )
        };
        for (material, value) in [
            (Material::RawPorkchop, RAW_MEAT_HUNGER),
            (Material::RawBeef, RAW_MEAT_HUNGER),
            (Material::RottenFlesh, ROTTEN_FLESH_HUNGER),
        ] {
            assert_eq!(food_value(ItemId::Material(material)), Some(value));
        }
    }

    #[test]
    fn raw_porkchop_becomes_a_target_below_full_hunger_and_not_at_full() {
        let porkchop =
            Item::new(ItemId::Material(Material::RawPorkchop), 1).expect("valid porkchop");
        let target = EatingTarget::new(4, porkchop, MAX_HUNGER - 1)
            .expect("raw meat is edible below full hunger");
        assert_eq!(target.slot(), 4);
        assert_eq!(target.item(), ItemId::Material(Material::RawPorkchop));
        assert_eq!(target.restore(), RAW_MEAT_HUNGER);

        assert!(
            EatingTarget::new(4, porkchop, MAX_HUNGER).is_none(),
            "full hunger must still refuse raw meat"
        );
    }

    #[test]
    fn target_accepts_nineteen_hunger_and_rejects_full_non_food_and_bad_slot() {
        let target = apple_target(0, MAX_HUNGER - 1);
        assert_eq!(target.slot(), 0);
        assert_eq!(target.item(), ItemId::Material(Material::Apple));
        assert_eq!(target.restore(), APPLE_HUNGER);
        assert_eq!(target.duration(), EAT_DURATION_SECS);
        assert_eq!(EAT_DURATION_SECS, 1.6);

        for hunger in [MAX_HUNGER, MAX_HUNGER + 1, u16::MAX] {
            assert!(
                EatingTarget::new(0, apple(), hunger).is_none(),
                "hunger {hunger} must make eating structurally unstartable"
            );
        }
        for item in [
            Item::new(ItemId::Material(Material::Stick), 1).expect("valid stick"),
            Item::new(ItemId::Material(Material::Wheat), 1).expect("valid wheat"),
            Item::new(ItemId::Block(BlockId::Stone), 1).expect("valid stone"),
            Item::new(
                ItemId::Tool {
                    kind: ToolKind::Pickaxe,
                    tier: ToolTier::Iron,
                },
                1,
            )
            .expect("valid pickaxe"),
        ] {
            assert!(
                EatingTarget::new(0, item, 10).is_none(),
                "{:?} must not become an eating target",
                item.id()
            );
        }
        assert!(EatingTarget::new(HOTBAR_SLOTS, apple(), 10).is_none());
        assert!(EatingTarget::new(usize::MAX, apple(), 10).is_none());
        // Last valid slot and a nearly-full belly are still allowed.
        assert!(EatingTarget::new(HOTBAR_SLOTS - 1, apple(), MAX_HUNGER - 1).is_some());
    }

    #[test]
    fn no_candidate_is_idle_with_no_progress() {
        let mut eating = EatingState::new();
        for _ in 0..100 {
            assert_eq!(eating.tick(0.1, None), EatingTick::Idle);
            assert!(eating.progress().is_none());
        }
    }

    #[test]
    fn eight_ticks_halfway_and_completion_exactly_once() {
        let mut eating = EatingState::new();
        let target = apple_target(0, MAX_HUNGER - 1);

        hold(&mut eating, target, 8);
        let progress = eating.progress().expect("a held meal has progress");
        assert!(
            (progress - 0.5).abs() < 0.01,
            "8 ticks of 0.1 s should be about half of 1.6 s, got {progress}"
        );

        let mut ate_tick = None;
        for tick in 9..=20 {
            match eating.tick(0.1, Some(target)) {
                EatingTick::Ate {
                    slot,
                    item,
                    restore,
                } => {
                    assert_eq!(slot, 0);
                    assert_eq!(item, ItemId::Material(Material::Apple));
                    assert_eq!(restore, APPLE_HUNGER);
                    ate_tick = Some(tick);
                    break;
                }
                EatingTick::Eating { .. } => {}
                EatingTick::Idle => panic!("a held, valid target must not go Idle"),
            }
        }
        let ate_tick = ate_tick.expect("a held apple must complete");
        assert!(
            (15..=17).contains(&ate_tick),
            "completion should take 16 +/- 1 ticks, took {ate_tick}"
        );
        assert!(eating.progress().is_none(), "completion must reset state");

        // Exactly once: immediately after completion, holding restarts from
        // scratch and must not emit a second Ate within half a cycle.
        for _ in 0..8 {
            assert!(matches!(
                eating.tick(0.1, Some(target)),
                EatingTick::Eating { .. }
            ));
        }
    }

    #[test]
    fn huge_dt_is_clamped_to_one_tenth() {
        let mut eating = EatingState::new();
        let target = apple_target(0, MAX_HUNGER - 1);
        let progress = tick_progress(&mut eating, 5.0, target);
        assert!(
            (progress - 0.1 / EAT_DURATION_SECS).abs() < 1e-4,
            "5.0 s must clamp to 0.1 s, got progress {progress}"
        );
        assert!(progress < 0.1, "a single huge dt must not eat the item");
    }

    #[test]
    fn non_finite_and_non_positive_dt_add_nothing() {
        let mut eating = EatingState::new();
        let target = apple_target(0, MAX_HUNGER - 1);
        hold(&mut eating, target, 4);
        let before = eating.progress().expect("partial progress");

        for dt in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.1, -5.0] {
            match eating.tick(dt, Some(target)) {
                EatingTick::Eating { progress } => {
                    assert_eq!(progress, before, "dt {dt} must add no progress");
                }
                other => panic!("dt {dt} must stay Eating, got {other:?}"),
            }
        }
        assert_eq!(eating.progress(), Some(before));
    }

    #[test]
    fn release_slot_change_and_item_change_reset_partial_progress() {
        let mut eating = EatingState::new();

        // Release resets completely.
        hold(&mut eating, apple_target(0, 19), 5);
        assert!(eating.progress().expect("partial") > 0.2);
        assert_eq!(eating.tick(0.1, None), EatingTick::Idle);
        assert!(eating.progress().is_none());

        // Moving to another slot restarts at one tick of progress.
        hold(&mut eating, apple_target(0, 19), 5);
        let before = eating.progress().expect("partial");
        let moved = tick_progress(&mut eating, 0.1, apple_target(1, 19));
        assert!(moved < before, "slot change must not inherit progress");
        assert!((moved - 0.1 / EAT_DURATION_SECS).abs() < 1e-4);

        // Switching food restarts too, even in the same slot.
        hold(&mut eating, apple_target(1, 19), 3);
        let before = eating.progress().expect("partial");
        let switched = tick_progress(&mut eating, 0.1, EatingTarget::new(1, bread(), 19).unwrap());
        assert!(switched < before, "item change must not inherit progress");
        assert!((switched - 0.1 / EAT_DURATION_SECS).abs() < 1e-4);
    }

    #[test]
    fn completion_resets_so_continued_holding_restarts() {
        let mut eating = EatingState::new();
        let target = apple_target(0, MAX_HUNGER - 1);
        let mut meals = 0;
        for _ in 0..40 {
            if matches!(eating.tick(0.1, Some(target)), EatingTick::Ate { .. }) {
                meals += 1;
            }
        }
        assert_eq!(meals, 2, "40 ticks of 0.1 s covers two full 1.6 s meals");
    }

    #[test]
    fn defensive_invalid_targets_go_idle_without_progress() {
        let mut eating = EatingState::new();
        let non_food = EatingTarget {
            slot: 0,
            item: ItemId::Material(Material::Stick),
            restore: 0,
            duration: EAT_DURATION_SECS,
        };
        assert_eq!(eating.tick(0.1, Some(non_food)), EatingTick::Idle);
        assert!(eating.progress().is_none());

        let bad_slot = EatingTarget {
            slot: HOTBAR_SLOTS,
            item: ItemId::Material(Material::Apple),
            restore: APPLE_HUNGER,
            duration: EAT_DURATION_SECS,
        };
        assert_eq!(eating.tick(0.1, Some(bad_slot)), EatingTick::Idle);
        assert!(eating.progress().is_none());

        for duration in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let degenerate = EatingTarget {
                slot: 0,
                item: ItemId::Material(Material::Apple),
                restore: APPLE_HUNGER,
                duration,
            };
            assert_eq!(
                eating.tick(0.1, Some(degenerate)),
                EatingTick::Idle,
                "duration {duration} must fail closed"
            );
            assert!(eating.progress().is_none());
        }
    }

    #[test]
    fn default_is_empty() {
        let eating = EatingState::default();
        assert!(eating.progress().is_none());
    }
}
