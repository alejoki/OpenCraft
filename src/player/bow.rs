//! Pure bow-charge domain: the draw curve, the release threshold, and the
//! structural cancel rules.
//!
//! This module is pure lib code — no session, no renderer, no wgpu, no
//! [`std::time::Instant`], no wall clock — so the whole draw curve is
//! assertable headlessly. It mirrors the proven [`crate::player::melee`] and
//! [`crate::player::eating`] shape: a tiny interface, one stateful float, and a
//! caller that owns the mutation. [`BowState`] never fires an arrow and never
//! touches an inventory; it only reports how much charge has accumulated and
//! what fraction a release should hand to the projectile domain.
//!
//! ## Draw curve
//!
//! A draw is real frame time, exactly like a melee cooldown: callers pass the
//! raw frame delta, never `dt * time_scale`. Non-finite or non-positive `dt`
//! adds nothing and a positive `dt` is clamped to [`MAX_DT`] (0.1 s), so a
//! stalled frame cannot skip a draw.
//!
//! - `0.0 .. BOW_MIN_CHARGE_SECS` (0.2 s) is a **tap**: [`BowState::release`]
//!   returns `None` and the shot is cancelled, never queued.
//! - `BOW_MIN_CHARGE_SECS .. BOW_FULL_CHARGE_SECS` (1.0 s) scales the released
//!   fraction linearly, which the projectile domain turns into speed and damage.
//! - `>= BOW_FULL_CHARGE_SECS` is [`BowTick::Full`]; the stored charge saturates
//!   at the full value so holding forever cannot grow it without bound.
//!
//! ## What cancels a draw
//!
//! Only a released hold, a non-target selection (nothing selected, or a
//! selected item that is not a wood bow), a panel/ungrabbed frame, death, and a
//! session swap. **Movement deliberately does not cancel a draw**: unlike
//! eating, a bow that stops working when the player walks is worse feel and
//! nothing in the roadmap asks for it. The session enforces the non-movement
//! cancels by passing `None` to [`BowState::tick`], which resets the charge to
//! exactly `0.0` — the same structural cancellation [`crate::player::eating`]
//! uses for a refused meal.

use crate::player::inventory::{Item, ItemId, ToolKind, ToolTier, HOTBAR_SLOTS};

/// Shortest hold that produces a shot at all, in seconds.
///
/// Anything shorter is a tap and cancels: releasing early must never fire a
/// queued arrow and must never consume ammo.
pub const BOW_MIN_CHARGE_SECS: f32 = 0.2;

/// Hold duration that reaches full draw, in seconds.
pub const BOW_FULL_CHARGE_SECS: f32 = 1.0;

/// Largest frame delta one tick may integrate, so a stall cannot skip a draw.
pub const MAX_DT: f32 = 0.1;

/// Tunable bow tuning consumed by the `_with_rules` entry points.
///
/// [`BowRules::default`] is built from the constants above, so the default
/// bundle reproduces [`BowState::tick`], [`BowState::release`], and
/// [`BowState::progress`] exactly. The struct is a plain value type: it
/// carries no TOML/config vocabulary, only the numbers the simulation
/// consumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BowRules {
    /// Shortest hold that produces a shot at all, in seconds.
    pub min_charge_secs: f32,
    /// Hold duration that reaches full draw, in seconds.
    pub full_charge_secs: f32,
}

impl Default for BowRules {
    fn default() -> Self {
        Self {
            min_charge_secs: BOW_MIN_CHARGE_SECS,
            full_charge_secs: BOW_FULL_CHARGE_SECS,
        }
    }
}

impl BowRules {
    /// Min charge actually applied.
    ///
    /// A non-finite value falls back to the pinned default; a zero or
    /// negative configured minimum means any accumulated charge can fire.
    fn effective_min_charge_secs(self) -> f32 {
        if self.min_charge_secs.is_finite() {
            self.min_charge_secs.max(0.0)
        } else {
            BOW_MIN_CHARGE_SECS
        }
    }

    /// Full-charge duration actually applied.
    ///
    /// A non-finite or non-positive duration cannot be divided by, so it
    /// falls back to the pinned default rather than producing an infinite or
    /// NaN progress fraction.
    fn effective_full_charge_secs(self) -> f32 {
        if self.full_charge_secs.is_finite() && self.full_charge_secs > 0.0 {
            self.full_charge_secs
        } else {
            BOW_FULL_CHARGE_SECS
        }
    }
}

/// A validated draw target: a wood bow sitting in a hotbar slot.
///
/// Construction is the only gate, so a caller that holds a `BowTarget` knows
/// the shot is structurally firable. The target carries the slot rather than
/// the [`Item`] because consuming the arrow and wearing the bow are the
/// session's actions, not this domain's.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BowTarget {
    slot: usize,
}

impl BowTarget {
    /// Validates a wood bow held in `slot`.
    ///
    /// Returns `None` when `slot` is not a hotbar slot or `item` is anything
    /// other than `Tool { kind: Bow, tier: Wood }` — a sword, a pickaxe, an
    /// arrow material, and an empty hand all fail here.
    ///
    /// [`Item::new`] already enforces the wood-only tier rule (there is no
    /// stone or iron bow), so this is the second lane of defence at the input
    /// boundary: even if a future code path ever handed this a forged item, the
    /// draw still could not start.
    pub fn new(slot: usize, item: Item) -> Option<Self> {
        if slot >= HOTBAR_SLOTS {
            return None;
        }
        if item.id()
            != (ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            })
        {
            return None;
        }
        Some(Self { slot })
    }

    /// Hotbar slot holding the bow.
    pub const fn slot(self) -> usize {
        self.slot
    }
}

/// Outcome of one bow tick.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum BowTick {
    /// Nothing is being drawn; there is no charge.
    Idle,
    /// A bow is held, with draw progress in `0.0..1.0`.
    Charging { progress: f32 },
    /// The draw has reached [`BOW_FULL_CHARGE_SECS`] and cannot grow further.
    Full,
}

/// Draw accumulator for the selected bow.
///
/// The stored value is seconds of accumulated hold, clamped to
/// [`BOW_FULL_CHARGE_SECS`]. There is no per-slot keying: unlike eating, a draw
/// is bound to "the selected wood bow", and switching selection passes `None`
/// to [`BowState::tick`], which resets to `0.0`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BowState {
    charge: f32,
}

impl BowState {
    /// Idle state: no charge accumulated.
    pub const fn new() -> Self {
        Self { charge: 0.0 }
    }

    /// Forgets any partial or full draw.
    pub fn reset(&mut self) {
        self.charge = 0.0;
    }

    /// Accumulated hold in seconds, in `0.0..=BOW_FULL_CHARGE_SECS`.
    pub const fn charge_secs(&self) -> f32 {
        self.charge
    }

    /// Draw fraction in `0.0..=1.0` using default rules, or `None` while no
    /// charge is held.
    pub fn progress(&self) -> Option<f32> {
        self.progress_with(&BowRules::default())
    }

    /// Rules-aware [`BowState::progress`].
    ///
    /// The fraction divides by the tuned full-charge duration, so a HUD fed
    /// from this method matches the draw the tuned [`BowState::tick_with`]
    /// produces rather than the pinned constant.
    pub fn progress_with(&self, rules: &BowRules) -> Option<f32> {
        if !self.charge.is_finite() || self.charge <= 0.0 {
            return None;
        }
        Some((self.charge / rules.effective_full_charge_secs()).clamp(0.0, 1.0))
    }

    /// Advances the draw by `dt` seconds against `target` using default rules.
    ///
    /// Contract:
    /// - non-finite or non-positive `dt` adds nothing; positive `dt` is clamped
    ///   to [`MAX_DT`] so a stalled frame cannot skip a draw;
    /// - `None` resets the charge to exactly `0.0` and returns [`BowTick::Idle`]
    ///   (structural cancellation: released hold, non-bow selection, panel,
    ///   death, session swap);
    /// - with a target the charge grows and saturates at
    ///   [`BOW_FULL_CHARGE_SECS`], returning [`BowTick::Full`] from then on.
    pub fn tick(&mut self, dt: f32, target: Option<BowTarget>) -> BowTick {
        self.tick_with(dt, target, &BowRules::default())
    }

    /// Rules-aware [`BowState::tick`].
    ///
    /// Identical contract, but the charge saturates at
    /// `rules.full_charge_secs` and the reported progress divides by that
    /// tuned duration.
    pub fn tick_with(&mut self, dt: f32, target: Option<BowTarget>, rules: &BowRules) -> BowTick {
        let dt = if !dt.is_finite() || dt <= 0.0 {
            0.0
        } else {
            dt.min(MAX_DT)
        };

        let Some(target) = target else {
            self.reset();
            return BowTick::Idle;
        };
        // Fail closed even though `BowTarget::new` already validated: a
        // degenerate target must never accumulate charge.
        if target.slot >= HOTBAR_SLOTS {
            self.reset();
            return BowTick::Idle;
        }

        let full_charge = rules.effective_full_charge_secs();
        self.charge += dt;
        if self.charge >= full_charge {
            // Saturate so an arbitrarily long hold cannot grow the value.
            self.charge = full_charge;
            return BowTick::Full;
        }
        BowTick::Charging {
            progress: self.charge / full_charge,
        }
    }

    /// Releases the draw using default rules, returning the charge fraction
    /// when the shot fires.
    ///
    /// Returns `Some(fraction)` with `fraction` in `0.0..=1.0` only when the
    /// charge reached [`BOW_MIN_CHARGE_SECS`]; a shorter tap returns `None`.
    /// The charge is **always** reset, so a quick tap is a cancel and never a
    /// queued shot, and a second release in a row is always `None`.
    pub fn release(&mut self) -> Option<f32> {
        self.release_with(&BowRules::default())
    }

    /// Rules-aware [`BowState::release`].
    ///
    /// The firable threshold is `rules.min_charge_secs` and the returned
    /// fraction divides by `rules.full_charge_secs`, so a tuned draw releases
    /// the same fraction the tuned tick reported.
    pub fn release_with(&mut self, rules: &BowRules) -> Option<f32> {
        let charge = self.charge;
        self.charge = 0.0;
        if !charge.is_finite() || charge < rules.effective_min_charge_secs() {
            return None;
        }
        let full_charge = rules.effective_full_charge_secs();
        Some(charge.clamp(0.0, full_charge) / full_charge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{ItemError, Material};

    fn bow(slot: usize) -> BowTarget {
        let item = Item::new(
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            },
            1,
        )
        .expect("a wood bow is constructible");
        BowTarget::new(slot, item).expect("wood bow in a hotbar slot")
    }

    fn other(id: ItemId) -> Item {
        Item::new(id, 1).expect("test identity is constructible")
    }

    #[test]
    fn bow_target_accepts_only_a_wood_bow_in_a_hotbar_slot() {
        assert_eq!(bow(0).slot(), 0);
        assert_eq!(bow(HOTBAR_SLOTS - 1).slot(), HOTBAR_SLOTS - 1);

        let wood_bow = other(ItemId::Tool {
            kind: ToolKind::Bow,
            tier: ToolTier::Wood,
        });
        assert!(
            BowTarget::new(HOTBAR_SLOTS, wood_bow).is_none(),
            "a slot outside the hotbar is not a draw target"
        );
        assert!(
            BowTarget::new(usize::MAX, wood_bow).is_none(),
            "no slot index may overflow into a target"
        );

        for rejected in [
            ItemId::Tool {
                kind: ToolKind::Sword,
                tier: ToolTier::Wood,
            },
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            },
            ItemId::Material(Material::Arrow),
            ItemId::Material(Material::Stick),
            ItemId::Block(crate::world::block::BlockId::OakPlanks),
        ] {
            assert!(
                BowTarget::new(0, other(rejected)).is_none(),
                "{rejected:?} must never start a draw"
            );
        }
    }

    #[test]
    fn a_stone_or_iron_bow_cannot_even_be_constructed() {
        // The tier rule lives in `Item`, so there is no way to forge the item
        // this domain would otherwise have to reject.
        for tier in [ToolTier::Stone, ToolTier::Iron] {
            assert_eq!(
                Item::new(
                    ItemId::Tool {
                        kind: ToolKind::Bow,
                        tier,
                    },
                    1
                ),
                Err(ItemError::TierNotAllowed)
            );
            assert_eq!(
                Item::tool_with_durability(ToolKind::Bow, tier, 10),
                Err(ItemError::TierNotAllowed)
            );
        }
    }

    #[test]
    fn none_target_resets_a_mid_charge_draw() {
        let mut state = BowState::new();
        // Five 0.1 s frames, because one frame may integrate at most MAX_DT.
        for _ in 0..5 {
            state.tick(0.1, Some(bow(2)));
        }
        match state.tick(0.0, Some(bow(2))) {
            BowTick::Charging { progress } => assert!((progress - 0.5).abs() < 1e-6),
            other => panic!("expected half draw, got {other:?}"),
        }
        let progress = state.progress().expect("still charging");
        assert!((progress - 0.5).abs() < 1e-6);

        assert_eq!(state.tick(0.1, None), BowTick::Idle);
        assert_eq!(state.charge_secs(), 0.0);
        assert_eq!(state.progress(), None);
    }

    #[test]
    fn one_tenth_second_tick_is_a_tenth_progress() {
        let mut state = BowState::new();
        match state.tick(0.1, Some(bow(0))) {
            BowTick::Charging { progress } => assert!((progress - 0.1).abs() < 1e-6),
            other => panic!("expected Charging, got {other:?}"),
        }
        assert!((state.charge_secs() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn ten_tenth_second_ticks_reach_full() {
        let mut state = BowState::new();
        for tick in 0..9 {
            assert!(
                matches!(state.tick(0.1, Some(bow(0))), BowTick::Charging { .. }),
                "tick {tick} must still be charging"
            );
        }
        assert_eq!(state.tick(0.1, Some(bow(0))), BowTick::Full);
        assert_eq!(state.charge_secs(), BOW_FULL_CHARGE_SECS);
    }

    #[test]
    fn a_single_long_tick_is_clamped_and_cannot_reach_full() {
        // MEM413: a stalled frame integrates at most MAX_DT, so a 1 s frame is
        // one 0.1 s tick and never a whole draw.
        let mut state = BowState::new();
        assert_eq!(
            state.tick(1.0, Some(bow(0))),
            BowTick::Charging { progress: 0.1 }
        );
        assert!((state.charge_secs() - MAX_DT).abs() < 1e-6);
        assert_ne!(state.tick(0.0, Some(bow(0))), BowTick::Full);
    }

    #[test]
    fn non_finite_and_negative_dt_never_advance_the_charge() {
        let mut state = BowState::new();
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.5, -0.0] {
            let tick = state.tick(bad, Some(bow(0)));
            assert!(
                matches!(tick, BowTick::Charging { progress } if progress == 0.0),
                "{bad} must not advance the draw, got {tick:?}"
            );
            assert_eq!(state.charge_secs(), 0.0);
        }
    }

    #[test]
    fn a_tap_below_the_threshold_cancels_without_a_shot() {
        let mut state = BowState::new();
        state.tick(0.1, Some(bow(0)));
        assert_eq!(state.release(), None);
        assert_eq!(state.progress(), None);
        assert_eq!(state.release(), None, "a cancel cannot queue a shot");
    }

    #[test]
    fn release_at_the_threshold_reports_a_fifth() {
        let mut state = BowState::new();
        // 0.2 s of hold is two 0.1 s frames: a single 0.2 s frame is clamped
        // to MAX_DT and must not reach the threshold on its own.
        state.tick(BOW_MIN_CHARGE_SECS, Some(bow(0)));
        assert_eq!(state.release(), None, "one clamped frame is still a tap");

        state.tick(0.1, Some(bow(0)));
        state.tick(0.1, Some(bow(0)));
        let charge = state.release().expect("0.2 s of hold is a real shot");
        assert!((charge - 0.2).abs() < 1e-6);
        assert_eq!(state.charge_secs(), 0.0);
    }

    #[test]
    fn release_at_full_reports_one() {
        let mut state = BowState::new();
        for _ in 0..10 {
            state.tick(0.1, Some(bow(0)));
        }
        assert_eq!(state.release(), Some(1.0));
        assert_eq!(state.progress(), None);
        assert_eq!(state.release(), None, "release always resets");
    }

    #[test]
    fn a_full_hold_does_not_grow_the_stored_charge() {
        let mut state = BowState::new();
        for _ in 0..50 {
            state.tick(0.1, Some(bow(0)));
        }
        assert_eq!(state.charge_secs(), BOW_FULL_CHARGE_SECS);
        assert_eq!(state.progress(), Some(1.0));
        assert_eq!(state.release(), Some(1.0));
    }

    #[test]
    fn reset_clears_a_full_charge() {
        let mut state = BowState::new();
        for _ in 0..10 {
            state.tick(0.1, Some(bow(0)));
        }
        assert_eq!(state.progress(), Some(1.0));
        state.reset();
        assert_eq!(state.charge_secs(), 0.0);
        assert_eq!(state.progress(), None);
        assert_eq!(
            state.tick(0.1, Some(bow(0))),
            BowTick::Charging { progress: 0.1 }
        );
    }

    #[test]
    fn a_fresh_state_is_default_and_idle() {
        let fresh = BowState::new();
        assert_eq!(fresh, BowState::default());
        assert_eq!(fresh.progress(), None);
        assert_eq!(fresh.charge_secs(), 0.0);
        let mut releasing = fresh;
        assert_eq!(releasing.release(), None);
    }

    // ---- BowRules ------------------------------------------------------

    fn tuned_rules() -> BowRules {
        BowRules {
            min_charge_secs: 0.1,
            full_charge_secs: 0.5,
        }
    }

    #[test]
    fn default_bow_rules_are_the_pinned_constants() {
        assert_eq!(
            BowRules::default(),
            BowRules {
                min_charge_secs: BOW_MIN_CHARGE_SECS,
                full_charge_secs: BOW_FULL_CHARGE_SECS,
            }
        );
    }

    #[test]
    fn tuned_bow_rules_change_tick_progress_and_saturation() {
        let rules = tuned_rules();
        let mut state = BowState::new();
        for _ in 0..2 {
            assert!(matches!(
                state.tick_with(0.1, Some(bow(0)), &rules),
                BowTick::Charging { .. }
            ));
        }
        // 0.2 s of a 0.5 s tuned draw is 40%; the default full-charge time
        // would report 20% for the same stored charge.
        let tuned = state.progress_with(&rules).expect("still charging");
        assert!((tuned - 0.4).abs() < 1e-6, "tuned progress was {tuned}");
        let default = state.progress().expect("still charging");
        assert!(
            (default - 0.2).abs() < 1e-6,
            "default progress was {default}"
        );

        // Only the third and fourth 0.1 s ticks (0.3 s and 0.4 s) are still
        // charging; the fifth reaches the tuned 0.5 s saturation point.
        for tick in 2..4 {
            assert!(
                matches!(
                    state.tick_with(0.1, Some(bow(0)), &rules),
                    BowTick::Charging { .. }
                ),
                "tick {tick} must still be charging"
            );
        }
        assert_eq!(
            state.tick_with(0.1, Some(bow(0)), &rules),
            BowTick::Full,
            "the tuned 0.5 s draw must be full on the fifth 0.1 s tick"
        );
        assert_eq!(state.charge_secs(), rules.full_charge_secs);
        assert_eq!(state.progress_with(&rules), Some(1.0));
        // Holding longer cannot grow the tuned saturation value.
        state.tick_with(0.1, Some(bow(0)), &rules);
        assert_eq!(state.charge_secs(), rules.full_charge_secs);
    }

    #[test]
    fn tuned_minimum_charge_and_release_fraction() {
        let rules = tuned_rules();
        let mut state = BowState::new();
        state.tick_with(0.1, Some(bow(0)), &rules);

        // One 0.1 s frame meets the tuned 0.1 s minimum, so it fires 20% of
        // the tuned draw; the pinned 0.2 s threshold would cancel the tap.
        let released = state
            .release_with(&rules)
            .expect("the tuned minimum accepts a 0.1 s draw");
        assert!((released - 0.2).abs() < 1e-6, "fraction was {released}");
        assert_eq!(state.charge_secs(), 0.0, "release always resets");
        assert_eq!(state.release_with(&rules), None);

        let mut legacy = BowState::new();
        legacy.tick(0.1, Some(bow(0)));
        assert_eq!(
            legacy.release(),
            None,
            "the default wrapper keeps the 0.2 s tap threshold"
        );
    }

    #[test]
    fn zero_and_malformed_bow_rules_fail_closed() {
        // A zero/NaN full-charge time cannot be divided by, so it falls back
        // to the pinned duration and keeps the draw usable.
        for full in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let rules = BowRules {
                min_charge_secs: 0.2,
                full_charge_secs: full,
            };
            let mut state = BowState::new();
            state.tick_with(0.1, Some(bow(0)), &rules);
            let progress = state
                .progress_with(&rules)
                .expect("a corrupt duration still reports a finite fraction");
            assert!(
                progress.is_finite() && (progress - 0.1).abs() < 1e-6,
                "full charge {full} produced progress {progress}"
            );
            assert_eq!(
                state.charge_secs(),
                0.1,
                "no corruption may alter the stored charge"
            );
        }

        // A zero minimum means any accumulated charge fires; a non-finite one
        // falls back to the pinned threshold.
        let zero_min = BowRules {
            min_charge_secs: 0.0,
            ..BowRules::default()
        };
        let mut state = BowState::new();
        state.tick_with(0.1, Some(bow(0)), &zero_min);
        assert!(state.release_with(&zero_min).is_some());

        let nan_min = BowRules {
            min_charge_secs: f32::NAN,
            ..BowRules::default()
        };
        let mut state = BowState::new();
        state.tick_with(0.1, Some(bow(0)), &nan_min);
        assert_eq!(
            state.release_with(&nan_min),
            None,
            "a corrupt minimum falls back to the pinned 0.2 s threshold"
        );
    }

    #[test]
    fn default_bow_rules_reproduce_the_existing_wrappers() {
        let rules = BowRules::default();
        let mut legacy = BowState::new();
        let mut tuned = BowState::new();
        for step in 0..12 {
            assert_eq!(
                legacy.tick(0.1, Some(bow(0))),
                tuned.tick_with(0.1, Some(bow(0)), &rules),
                "tick diverged on step {step}"
            );
            assert_eq!(legacy.charge_secs(), tuned.charge_secs());
            assert_eq!(legacy.progress(), tuned.progress_with(&rules));
        }
        assert_eq!(legacy.release(), tuned.release_with(&rules));
        assert_eq!(legacy.charge_secs(), 0.0);

        // Cancellation stays structural under tuned rules too.
        let mut cancelled = BowState::new();
        cancelled.tick_with(0.1, Some(bow(0)), &rules);
        assert_eq!(cancelled.tick_with(0.1, None, &rules), BowTick::Idle);
        assert_eq!(cancelled.charge_secs(), 0.0);
        assert_eq!(cancelled.progress_with(&rules), None);
    }
}
