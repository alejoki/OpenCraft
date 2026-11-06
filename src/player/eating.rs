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

use crate::content::ContentRegistry;
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

/// Tunable eating tuning consumed by [`EatingTarget::new_with_rules`] and the
/// rules-aware completion path.
///
/// [`EatingRules::default`] is built from the constants above, so the default
/// bundle reproduces the frozen built-in table exactly. Pack-authored
/// [`FoodDef`](crate::content::FoodDef) values always win over these rules:
/// a pack's explicit nutrition and duration are content, not tuning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EatingRules {
    /// Seconds of continuous hold required to eat a built-in food item.
    pub eat_duration_secs: f32,
    /// Hunger restored by one apple.
    pub apple_hunger: u16,
    /// Hunger restored by one bread.
    pub bread_hunger: u16,
    /// Hunger restored by one raw meat item.
    pub raw_meat_hunger: u16,
    /// Hunger restored by rotten flesh.
    pub rotten_flesh_hunger: u16,
}

impl Default for EatingRules {
    fn default() -> Self {
        Self {
            eat_duration_secs: EAT_DURATION_SECS,
            apple_hunger: APPLE_HUNGER,
            bread_hunger: BREAD_HUNGER,
            raw_meat_hunger: RAW_MEAT_HUNGER,
            rotten_flesh_hunger: ROTTEN_FLESH_HUNGER,
        }
    }
}

/// Hunger restored by one `id`, or `None` when `id` is not food.
///
/// This is the frozen built-in answer. Pack-authored food resolves through
/// [`food_value_with`]; keeping this function registry-free preserves every
/// built-in call site byte-for-byte.
pub fn food_value(id: ItemId) -> Option<u16> {
    food_value_with_rules(id, &EatingRules::default())
}

/// Built-in hunger restored by `id` under `rules`, or `None` when `id` is not
/// a built-in food identity.
///
/// This resolves only the frozen material table; pack items go through
/// [`food_value_with_registry_rules`] so authored nutrition always wins.
pub fn food_value_with_rules(id: ItemId, rules: &EatingRules) -> Option<u16> {
    match id {
        ItemId::Material(Material::Apple) => Some(rules.apple_hunger),
        ItemId::Material(Material::Bread) => Some(rules.bread_hunger),
        ItemId::Material(Material::RawPorkchop | Material::RawBeef) => Some(rules.raw_meat_hunger),
        ItemId::Material(Material::RottenFlesh) => Some(rules.rotten_flesh_hunger),
        _ => None,
    }
}

/// Hunger restored by eating `id`, resolving pack items through `registry`.
///
/// Built-in materials and blocks answer exactly like [`food_value`]; a
/// registry-allocated generic item reads its authored [`FoodDef::nutrition`](crate::content::FoodDef).
/// An unknown or placeholder item index is not food, so malformed content
/// fails closed instead of eating for free. Pack tools are never food.
pub fn food_value_with(registry: &ContentRegistry, id: ItemId) -> Option<u16> {
    food_value_with_registry_rules(registry, id, &EatingRules::default())
}

/// Registry- and rules-aware hunger resolution.
///
/// Pack-authored [`FoodDef::nutrition`](crate::content::FoodDef) wins over
/// `rules` for every registry item; built-in identities read `rules`.
pub fn food_value_with_registry_rules(
    registry: &ContentRegistry,
    id: ItemId,
    rules: &EatingRules,
) -> Option<u16> {
    match id {
        ItemId::Item(index) => registry
            .item(index)
            .filter(|def| !def.placeholder)
            .and_then(|def| def.food)
            .map(|food| food.nutrition),
        other => food_value_with_rules(other, rules),
    }
}

/// Authored hold duration for `id`, or the built-in constant when unset.
fn food_duration_with(registry: &ContentRegistry, id: ItemId) -> Option<f32> {
    food_duration_with_rules(registry, id, &EatingRules::default())
}

/// Authored hold duration for `id`, or `rules.eat_duration_secs` when unset.
///
/// Pack-authored [`FoodDef::eat_seconds`](crate::content::FoodDef) always wins
/// over the tuning rule.
fn food_duration_with_rules(
    registry: &ContentRegistry,
    id: ItemId,
    rules: &EatingRules,
) -> Option<f32> {
    match id {
        ItemId::Item(index) => registry
            .item(index)
            .filter(|def| !def.placeholder)
            .and_then(|def| def.food)
            .map(|food| food.eat_seconds),
        _ => Some(rules.eat_duration_secs),
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
        Self::validated(slot, id, restore, EAT_DURATION_SECS, hunger)
    }

    /// Validates `item` in `slot` against `registry` for a player at `hunger`.
    ///
    /// Pack-authored food restores its authored nutrition over its authored
    /// `eat_seconds`; built-in food keeps [`EAT_DURATION_SECS`]. Full hunger
    /// stays structurally unstartable exactly like [`EatingTarget::new`], and
    /// a non-food, unknown or placeholder identity is refused.
    pub fn new_with(
        registry: &ContentRegistry,
        slot: usize,
        item: Item,
        hunger: u16,
    ) -> Option<Self> {
        let id = item.id();
        let restore = food_value_with(registry, id)?;
        let duration = food_duration_with(registry, id)?;
        Self::validated(slot, id, restore, duration, hunger)
    }

    /// Rules-aware [`EatingTarget::new_with`].
    ///
    /// Built-in food reads `rules` for both hunger and duration; pack-authored
    /// [`FoodDef`](crate::content::FoodDef) values still win. The resolved
    /// `restore` and `duration` are carried on the target, so the completion
    /// caller can restore exactly the tuned amount it displayed.
    pub fn new_with_rules(
        registry: &ContentRegistry,
        rules: &EatingRules,
        slot: usize,
        item: Item,
        hunger: u16,
    ) -> Option<Self> {
        let id = item.id();
        let restore = food_value_with_registry_rules(registry, id, rules)?;
        let duration = food_duration_with_rules(registry, id, rules)?;
        Self::validated(slot, id, restore, duration, hunger)
    }

    /// Shared post-resolution validation.
    fn validated(
        slot: usize,
        id: ItemId,
        restore: u16,
        duration: f32,
        hunger: u16,
    ) -> Option<Self> {
        if hunger >= MAX_HUNGER
            || slot >= HOTBAR_SLOTS
            || restore == 0
            || !duration.is_finite()
            || duration <= 0.0
        {
            return None;
        }
        Some(Self {
            slot,
            item: id,
            restore,
            duration,
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
        // `restore == 0` marks a forged non-food target: every constructor
        // rejects zero nutrition, and authored nutrition is always 1..=20.
        // This replaces the built-in-only `food_value` check so pack food can
        // accumulate progress without a registry parameter on `tick`.
        if target.slot >= HOTBAR_SLOTS
            || target.restore == 0
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
    use crate::content::spec::{FoodSpec, ItemEntry, PackAsset};
    use crate::content::{build_registry, ContentRegistry, PackContentSpec, PackIdentity};
    use crate::player::inventory::{ToolKind, ToolTier};
    use crate::world::block::BlockId;

    /// A pack registry with `mymod:ruby_stew` (nutrition 7, 0.8 s) and a
    /// non-food `mymod:ruby`.
    fn pack_food_registry() -> ContentRegistry {
        let mut spec = PackContentSpec::default();
        spec.items.push(pack_item(
            "mymod:ruby_stew",
            "stew.png",
            Some(FoodSpec {
                nutrition: 7,
                eat_seconds: 0.8,
            }),
        ));
        spec.items.push(pack_item("mymod:ruby", "ruby.png", None));
        for (name, byte) in [("stew.png", 0x11), ("ruby.png", 0x22)] {
            spec.assets.insert(
                name.to_string(),
                PackAsset {
                    rgba: vec![byte; 1024],
                    source_side: 16,
                },
            );
        }
        let pack = (
            PackIdentity {
                folder: "mymod".to_string(),
                version: None,
            },
            spec,
        );
        let (registry, report) = build_registry(ContentRegistry::builtin(), &[pack]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(
            registry
                .item_by_name("mymod:ruby_stew")
                .map(|def| def.index),
            Some(pack_stew_index(&registry))
        );
        assert!(registry.item_by_name("mymod:ruby").is_some());
        registry
    }

    fn pack_stew_index(registry: &ContentRegistry) -> u16 {
        registry
            .item_by_name("mymod:ruby_stew")
            .expect("registered pack stew")
            .index
    }

    fn pack_gem_index(registry: &ContentRegistry) -> u16 {
        registry
            .item_by_name("mymod:ruby")
            .expect("registered pack gem")
            .index
    }

    fn pack_item(id: &str, icon: &str, food: Option<FoodSpec>) -> ItemEntry {
        ItemEntry {
            id: id.to_string(),
            display_name: None,
            icon: icon.to_string(),
            stack_limit: 64,
            food,
        }
    }

    fn pack_stew(registry: &ContentRegistry) -> Item {
        Item::new_with(registry, ItemId::Item(pack_stew_index(registry)), 3)
            .expect("registered pack stew")
    }

    fn pack_stew_target(registry: &ContentRegistry, hunger: u16) -> EatingTarget {
        EatingTarget::new_with(registry, 2, pack_stew(registry), hunger)
            .expect("pack stew is edible below full hunger")
    }

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
    fn food_value_with_resolves_authored_nutrition_and_fails_closed() {
        let registry = pack_food_registry();

        assert_eq!(
            food_value_with(&registry, ItemId::Item(pack_stew_index(&registry))),
            Some(7)
        );
        assert_eq!(
            food_value_with(&registry, ItemId::Item(pack_gem_index(&registry))),
            None,
            "a non-food pack item is never edible"
        );
        assert_eq!(
            food_value_with(&registry, ItemId::Item(99)),
            None,
            "an unknown index must not eat for free"
        );

        // Built-in answers pass through the registry path unchanged.
        for (id, value) in [
            (ItemId::Material(Material::Apple), APPLE_HUNGER),
            (ItemId::Material(Material::Bread), BREAD_HUNGER),
            (ItemId::Material(Material::RawBeef), RAW_MEAT_HUNGER),
            (ItemId::Material(Material::RottenFlesh), ROTTEN_FLESH_HUNGER),
        ] {
            assert_eq!(food_value_with(&registry, id), Some(value));
            assert_eq!(food_value_with(&registry, id), food_value(id));
        }
        for id in [
            ItemId::Material(Material::Stick),
            ItemId::Block(BlockId::Stone),
            ItemId::Tool {
                kind: ToolKind::Sword,
                tier: ToolTier::Iron,
            },
            ItemId::ModTool(0),
        ] {
            assert_eq!(food_value_with(&registry, id), None, "{id:?}");
        }
    }

    #[test]
    fn placeholder_pack_item_has_no_food_value_or_target() {
        let mut registry = pack_food_registry();
        let index = registry.append_placeholder_item("mymod:missing_stew");
        assert!(
            registry
                .item(index)
                .expect("placeholder registered")
                .placeholder
        );
        assert_eq!(food_value_with(&registry, ItemId::Item(index)), None);

        let placeholder =
            Item::new_with(&registry, ItemId::Item(index), 1).expect("placeholder item constructs");
        assert!(EatingTarget::new_with(&registry, 0, placeholder, 10).is_none());
    }

    #[test]
    fn pack_food_target_uses_authored_nutrition_and_duration() {
        let registry = pack_food_registry();
        let target = pack_stew_target(&registry, MAX_HUNGER - 1);
        assert_eq!(target.slot(), 2);
        assert_eq!(target.item(), ItemId::Item(pack_stew_index(&registry)));
        assert_eq!(target.restore(), 7);
        assert!((target.duration() - 0.8).abs() < 1e-6);

        assert!(
            EatingTarget::new_with(&registry, 2, pack_stew(&registry), MAX_HUNGER).is_none(),
            "full hunger must refuse pack food"
        );
        assert!(
            EatingTarget::new_with(&registry, HOTBAR_SLOTS, pack_stew(&registry), 10).is_none(),
            "a non-hotbar slot must refuse"
        );
        let gem = Item::new_with(&registry, ItemId::Item(pack_gem_index(&registry)), 1)
            .expect("registered pack gem");
        assert!(EatingTarget::new_with(&registry, 0, gem, 10).is_none());
    }

    #[test]
    fn builtin_foods_keep_their_exact_values_and_durations_through_the_registry() {
        let registry = ContentRegistry::builtin();
        for (id, value) in [
            (ItemId::Material(Material::Apple), APPLE_HUNGER),
            (ItemId::Material(Material::Bread), BREAD_HUNGER),
            (ItemId::Material(Material::RawPorkchop), RAW_MEAT_HUNGER),
            (ItemId::Material(Material::RottenFlesh), ROTTEN_FLESH_HUNGER),
        ] {
            let item = Item::new(id, 1).expect("built-in food constructs");
            let target = EatingTarget::new_with(registry, 0, item, 10).expect("built-in target");
            assert_eq!(target.restore(), value);
            assert_eq!(target.duration(), EAT_DURATION_SECS);
        }
    }

    #[test]
    fn pack_food_completes_at_its_authored_duration_and_restarts() {
        let registry = pack_food_registry();
        let target = pack_stew_target(&registry, MAX_HUNGER - 1);
        let mut eating = EatingState::new();

        // 0.79 s of real frame time must not finish a 0.8 s meal.
        for _ in 0..7 {
            assert!(matches!(
                eating.tick(0.1, Some(target)),
                EatingTick::Eating { .. }
            ));
        }
        assert!(matches!(
            eating.tick(0.09, Some(target)),
            EatingTick::Eating { .. }
        ));

        // The 0.8 s tick completes exactly once with authored nutrition.
        match eating.tick(0.01, Some(target)) {
            EatingTick::Ate {
                slot,
                item,
                restore,
            } => {
                assert_eq!(slot, 2);
                assert_eq!(item, ItemId::Item(pack_stew_index(&registry)));
                assert_eq!(restore, 7);
            }
            other => panic!("expected exactly one Ate, got {other:?}"),
        }
        assert!(eating.progress().is_none(), "completion must reset state");

        // A changed slot or identity restarts instead of inheriting progress.
        let _ = eating.tick(0.2, Some(target));
        let moved =
            EatingTarget::new_with(&registry, 3, pack_stew(&registry), 10).expect("moved target");
        match eating.tick(0.1, Some(moved)) {
            EatingTick::Eating { progress } => {
                assert!(progress < 0.2, "slot change must restart progress");
            }
            other => panic!("expected a fresh Eating, got {other:?}"),
        }
    }

    #[test]
    fn forged_pack_target_without_nutrition_goes_idle() {
        let mut eating = EatingState::new();
        let forged = EatingTarget {
            slot: 0,
            item: ItemId::Item(0),
            restore: 0,
            duration: 0.8,
        };
        assert_eq!(eating.tick(0.1, Some(forged)), EatingTick::Idle);
        assert!(eating.progress().is_none());
    }

    #[test]
    fn default_is_empty() {
        let eating = EatingState::default();
        assert!(eating.progress().is_none());
    }

    #[test]
    fn eating_rules_default_matches_the_module_constants() {
        let rules = EatingRules::default();
        assert_eq!(rules.eat_duration_secs, EAT_DURATION_SECS);
        assert_eq!(rules.apple_hunger, APPLE_HUNGER);
        assert_eq!(rules.bread_hunger, BREAD_HUNGER);
        assert_eq!(rules.raw_meat_hunger, RAW_MEAT_HUNGER);
        assert_eq!(rules.rotten_flesh_hunger, ROTTEN_FLESH_HUNGER);
    }

    #[test]
    fn tuned_rules_change_builtin_restore_and_duration() {
        let rules = EatingRules {
            eat_duration_secs: 0.5,
            apple_hunger: 7,
            bread_hunger: 9,
            raw_meat_hunger: 5,
            rotten_flesh_hunger: 2,
        };
        assert_eq!(
            food_value_with_rules(ItemId::Material(Material::Apple), &rules),
            Some(7)
        );
        assert_eq!(
            food_value_with_rules(ItemId::Material(Material::Bread), &rules),
            Some(9)
        );
        assert_eq!(
            food_value_with_rules(ItemId::Material(Material::RawBeef), &rules),
            Some(5)
        );
        assert_eq!(
            food_value_with_rules(ItemId::Material(Material::RottenFlesh), &rules),
            Some(2)
        );
        assert_eq!(
            food_value_with_rules(ItemId::Material(Material::Stick), &rules),
            None
        );

        let registry = ContentRegistry::builtin();
        let target = EatingTarget::new_with_rules(registry, &rules, 0, apple(), 10)
            .expect("tuned apple target");
        assert_eq!(target.restore(), 7);
        assert!((target.duration() - 0.5).abs() < 1e-6);

        // The default wrapper keeps the frozen table and 1.6 s duration.
        let default_target =
            EatingTarget::new_with(registry, 0, apple(), 10).expect("default apple target");
        assert_eq!(default_target.restore(), APPLE_HUNGER);
        assert_eq!(default_target.duration(), EAT_DURATION_SECS);
    }

    #[test]
    fn pack_authored_food_still_wins_over_tuned_rules() {
        let registry = pack_food_registry();
        let rules = EatingRules {
            eat_duration_secs: 0.2,
            apple_hunger: 19,
            bread_hunger: 19,
            raw_meat_hunger: 19,
            rotten_flesh_hunger: 19,
        };
        let target = EatingTarget::new_with_rules(&registry, &rules, 2, pack_stew(&registry), 10)
            .expect("pack stew target");
        assert_eq!(target.restore(), 7, "pack nutrition is content, not tuning");
        assert!(
            (target.duration() - 0.8).abs() < 1e-6,
            "pack eat_seconds is content, not tuning"
        );
        assert_eq!(
            food_value_with_registry_rules(
                &registry,
                ItemId::Item(pack_stew_index(&registry)),
                &rules
            ),
            Some(7)
        );
    }

    #[test]
    fn tuned_target_completes_at_the_tuned_duration() {
        let rules = EatingRules {
            eat_duration_secs: 0.3,
            apple_hunger: 7,
            ..EatingRules::default()
        };
        let registry = ContentRegistry::builtin();
        let target =
            EatingTarget::new_with_rules(registry, &rules, 0, apple(), 10).expect("tuned target");
        let mut eating = EatingState::new();

        assert!(matches!(
            eating.tick(0.1, Some(target)),
            EatingTick::Eating { .. }
        ));
        assert!(matches!(
            eating.tick(0.1, Some(target)),
            EatingTick::Eating { .. }
        ));
        match eating.tick(0.1, Some(target)) {
            EatingTick::Ate { restore, .. } => assert_eq!(restore, 7),
            other => panic!("expected the tuned 0.3 s meal, got {other:?}"),
        }
    }

    #[test]
    fn invalid_tuned_duration_is_refused_structurally() {
        let registry = ContentRegistry::builtin();
        for duration in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let rules = EatingRules {
                eat_duration_secs: duration,
                ..EatingRules::default()
            };
            assert!(
                EatingTarget::new_with_rules(registry, &rules, 0, apple(), 10).is_none(),
                "duration {duration} must not become a target"
            );
        }
    }
}
