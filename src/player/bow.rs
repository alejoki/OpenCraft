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

    /// Draw fraction in `0.0..=1.0`, or `None` while no charge is held.
    pub fn progress(&self) -> Option<f32> {
        if self.charge <= 0.0 {
            return None;
        }
        Some((self.charge / BOW_FULL_CHARGE_SECS).clamp(0.0, 1.0))
    }

    /// Advances the draw by `dt` seconds against `target`.
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

        self.charge += dt;
        if self.charge >= BOW_FULL_CHARGE_SECS {
            // Saturate so an arbitrarily long hold cannot grow the value.
            self.charge = BOW_FULL_CHARGE_SECS;
            return BowTick::Full;
        }
        BowTick::Charging {
            progress: self.charge / BOW_FULL_CHARGE_SECS,
        }
    }

    /// Releases the draw, returning the charge fraction when the shot fires.
    ///
    /// Returns `Some(fraction)` with `fraction` in `0.0..=1.0` only when the
    /// charge reached [`BOW_MIN_CHARGE_SECS`]; a shorter tap returns `None`.
    /// The charge is **always** reset, so a quick tap is a cancel and never a
    /// queued shot, and a second release in a row is always `None`.
    pub fn release(&mut self) -> Option<f32> {
        let charge = self.charge;
        self.charge = 0.0;
        if !charge.is_finite() || charge < BOW_MIN_CHARGE_SECS {
            return None;
        }
        Some(charge.clamp(0.0, BOW_FULL_CHARGE_SECS) / BOW_FULL_CHARGE_SECS)
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
}
