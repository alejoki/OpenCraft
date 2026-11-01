//! Pure player-side combat carrier: cause tags, i-frames, hurt flash, and a
//! decaying knockback impulse (M009/S06-T03).
//!
//! This module is pure lib code: no wgpu, no winit, no `Instant`, no wall
//! clock, and no [`crate::world::session::WorldSession`]. Every timer is
//! driven by an explicit count of executed fixed simulation steps, so the
//! behaviour is deterministic and headlessly testable.
//!
//! Three deliberate shapes live here:
//!
//! * **The cause is ephemeral.** [`DamageCause`] is a tag the caller passes at
//!   the damage site. [`PlayerCombat`] deliberately does **not** store it: the
//!   session owns the single `last_damage_cause` writer (T04), so there can
//!   never be two cause authorities and a loaded dead save simply has none.
//! * **I-frames are fixed-step.** [`PlayerCombat::advance`] consumes
//!   [`crate::world::mob::MOB_TICK_SECS`] once per executed mob tick, driven by
//!   `MobTick.ai_steps` and never by render `dt`. A frame-rate spike therefore
//!   can never skip (or duplicate) the immune window.
//! * **Knockback is its own field.** The impulse is never written into
//!   `Player::velocity`: `Player::update` reassigns `velocity.x/z` from input
//!   every frame, so the impulse lives here and is integrated *after* that
//!   assignment.

use glam::Vec3;

use crate::player::survival::{GameMode, SurvivalState};
use crate::world::mob::MOB_TICK_SECS;

/// Seconds a player ignores further hits after one lands (i-frames).
///
/// Same tuning as [`crate::world::mob::MOB_IFRAME_SECS`], but the two clocks
/// are separate on purpose: a player hit never delays `Mob.cooldown`, and a
/// mob hit never delays the player window (MEM410).
pub const PLAYER_IFRAME_SECS: f32 = 0.5;
/// Seconds the red damage flash stays at full intensity after a hit.
pub const PLAYER_HURT_FLASH_SECS: f32 = 0.5;
/// Initial horizontal speed of the player knockback impulse, blocks/second.
///
/// Deliberately faster than the mob impulse: the player reads as lighter and
/// the push has to be visible over the input-driven walk velocity.
pub const PLAYER_KNOCKBACK_SPEED: f32 = 6.0;
/// Per-fixed-step multiplier applied to the remaining knockback impulse.
pub const PLAYER_KNOCKBACK_DECAY: f32 = 0.6;
/// Squared speed below which the impulse is dropped to exactly zero, so the
/// body provably comes to rest instead of creeping forever.
pub const PLAYER_KNOCKBACK_EPSILON_SQ: f32 = 1e-4;

/// Squared horizontal length below which a separation vector is treated as
/// degenerate. Mirrors [`crate::world::mob::Mob::take_hit`] so player and mob
/// knockback fall back the same way.
const MIN_DIRECTION_LEN_SQ: f32 = 1e-6;

/// Ambient, never-persisted reason for a player damage event.
///
/// The tag is produced at the single damage site and rendered on the Death
/// screen (T05). It is not part of the v7 wire format: a loaded dead save has
/// no cause and renders "Unknown". S08 appends the skeleton's arrow cause.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DamageCause {
    /// Melee from a hostile zombie.
    Zombie,
    /// Dry-land fall damage.
    Fall,
    /// Air ran out underwater.
    Drowning,
    /// Hunger reached zero.
    Starvation,
}

impl DamageCause {
    /// Stable human-readable label, also the Death-screen text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Zombie => "Zombie",
            Self::Fall => "Fall",
            Self::Drowning => "Drowning",
            Self::Starvation => "Starvation",
        }
    }
}

/// One accepted player hit, as reported back to the caller.
///
/// `damage` is the *applied* damage: a 3-damage hit on a 2 hp player reports
/// `2`, matching [`SurvivalState::damage`]'s saturating rule.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AppliedHit {
    pub damage: u16,
    pub health_after: u16,
    pub killed: bool,
}

/// Player combat timers and the one decaying hostile impulse.
///
/// All fields are private and monotonic-by-construction: `hurt`/`iframe` only
/// come from [`PlayerCombat::take_hit`] and only shrink in
/// [`PlayerCombat::advance`]; `knockback` is the only carrier of a hostile
/// push and is never persisted.
#[derive(Clone, Debug, Default)]
pub struct PlayerCombat {
    /// Remaining hurt-flash time in seconds; also the render intensity.
    hurt: f32,
    /// Remaining i-frame time in seconds; `> 0.0` refuses hits.
    iframe: f32,
    /// Remaining horizontal knockback impulse in blocks/second.
    knockback: Vec3,
}

impl PlayerCombat {
    /// Red flash intensity in `0.0..=1.0`, exactly `0.0` when quiescent.
    ///
    /// Non-finite and non-positive timers both read as "no flash", so a
    /// corrupted value can never paint a garbage colour.
    pub fn hurt_intensity(&self) -> f32 {
        if !self.hurt.is_finite() || self.hurt <= 0.0 {
            return 0.0;
        }
        (self.hurt / PLAYER_HURT_FLASH_SECS).clamp(0.0, 1.0)
    }

    /// Remaining i-frame seconds. `> 0.0` means the next hit is refused.
    pub fn iframe(&self) -> f32 {
        self.iframe
    }

    /// The current horizontal knockback impulse, blocks/second.
    pub fn knockback(&self) -> Vec3 {
        self.knockback
    }

    /// Clears every timer and the impulse (mode change, respawn).
    pub fn reset(&mut self) {
        self.hurt = 0.0;
        self.iframe = 0.0;
        self.knockback = Vec3::ZERO;
    }

    /// Applies one player hit, or refuses it without mutating anything.
    ///
    /// Refuses when the mode is not Survival, the player is already dead
    /// (`health == 0`), the i-frame window is open, or `damage == 0`. On
    /// acceptance it arms the hurt flash and i-frames, sets the knockback
    /// impulse pointing horizontally away from `from`, and saturates vitals
    /// damage through [`SurvivalState::damage`] — no negative or wrapped
    /// health can be stored.
    ///
    /// `cause` is accepted so the caller passes one authoritative tag at the
    /// damage site, but it is deliberately **not** stored here: the session
    /// owns the single cause writer, so there is exactly one authority.
    ///
    /// The knockback direction is the horizontal vector from `from` to
    /// `player_feet`. A degenerate direction (attacker inside the player) or a
    /// non-finite one falls back to `Vec3::Z`, so the impulse is always finite
    /// and non-zero. Vertical knockback is deliberately absent: the player is
    /// never pushed into the floor or the ceiling.
    pub fn take_hit(
        &mut self,
        vitals: &mut SurvivalState,
        damage: u16,
        mode: GameMode,
        cause: DamageCause,
        player_feet: Vec3,
        from: Vec3,
    ) -> Option<AppliedHit> {
        if mode != GameMode::Survival || vitals.health() == 0 || self.iframe > 0.0 || damage == 0 {
            return None;
        }

        // The cause is intentionally not persisted or stored on this type; the
        // session is the only writer of `last_damage_cause` (T04).
        let _ = cause;

        self.hurt = PLAYER_HURT_FLASH_SECS;
        self.iframe = PLAYER_IFRAME_SECS;

        let delta = player_feet - from;
        let horizontal = Vec3::new(delta.x, 0.0, delta.z);
        let mut dir = horizontal.normalize_or_zero();
        if dir.length_squared() < MIN_DIRECTION_LEN_SQ {
            // Deterministic degenerate fallback instead of a zero impulse.
            dir = Vec3::Z;
        }
        if !dir.is_finite() || dir.length_squared() < MIN_DIRECTION_LEN_SQ {
            // A corrupted position must still not produce a zero or NaN push.
            dir = Vec3::Z;
        }
        self.knockback = dir * PLAYER_KNOCKBACK_SPEED;

        let applied = vitals.damage(damage);
        let health_after = vitals.health();
        Some(AppliedHit {
            damage: applied,
            health_after,
            killed: health_after == 0,
        })
    }

    /// Advances the timers by `ai_steps` executed fixed mob steps.
    ///
    /// Driven by `MobTick.ai_steps`, never by render `dt`, so i-frames and the
    /// flash are frame-rate independent (MEM410/MEM415). A non-finite impulse
    /// is dropped rather than propagated into the physics integration.
    pub fn advance(&mut self, ai_steps: u32) {
        for _ in 0..ai_steps {
            self.hurt = (self.hurt - MOB_TICK_SECS).max(0.0);
            self.iframe = (self.iframe - MOB_TICK_SECS).max(0.0);
            self.knockback *= PLAYER_KNOCKBACK_DECAY;
            if self.knockback.length_squared() < PLAYER_KNOCKBACK_EPSILON_SQ {
                self.knockback = Vec3::ZERO;
            }
            if !self.knockback.is_finite() {
                self.knockback = Vec3::ZERO;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::physics::{InputState, Player, PLAYER_HALF_WIDTH};
    use crate::world::block::BlockId;

    fn survival_full() -> SurvivalState {
        SurvivalState::full()
    }

    fn survival_player(position: Vec3) -> Player {
        Player::new_with_mode(position, GameMode::Survival)
    }

    /// Flat stone floor whose top surface is `y = 5`, air above.
    fn flat_floor(_x: i32, y: i32, _z: i32) -> BlockId {
        if y < 5 {
            BlockId::Stone
        } else {
            BlockId::Air
        }
    }

    #[test]
    fn take_hit_applies_damage_and_opens_the_iframe() {
        let mut combat = PlayerCombat::default();
        let mut vitals = survival_full();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        // Attacker one block to +Z.
        let from = player_feet + Vec3::Z;

        let hit = combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            from,
        );

        assert_eq!(
            hit,
            Some(AppliedHit {
                damage: 3,
                health_after: 17,
                killed: false,
            })
        );
        assert_eq!(vitals.health(), 17);
        assert_eq!(combat.hurt_intensity(), 1.0);
        assert_eq!(combat.iframe(), PLAYER_IFRAME_SECS);
    }

    #[test]
    fn second_hit_inside_the_window_mutates_nothing() {
        let mut combat = PlayerCombat::default();
        let mut vitals = survival_full();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        let first = combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            from,
        );
        assert!(first.is_some());
        // Let one fixed step pass so the timers are mid-window, then snapshot
        // every observable field: the refused hit must not reset any of them.
        combat.advance(1);
        let hurt_before = combat.hurt_intensity();
        let iframe_before = combat.iframe();
        let knockback_before = combat.knockback();

        // Different attacker, same window: still refused, nothing changes.
        let second = combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            player_feet - Vec3::X,
        );

        assert_eq!(second, None);
        assert_eq!(vitals.health(), 17, "the refused hit costs nothing");
        assert_eq!(combat.hurt_intensity(), hurt_before, "flash keeps decaying");
        assert_eq!(combat.iframe(), iframe_before, "i-frame keeps decaying");
        assert_eq!(combat.knockback(), knockback_before);
    }

    #[test]
    fn iframe_expires_after_exactly_ten_fixed_steps() {
        let mut combat = PlayerCombat::default();
        let mut vitals = survival_full();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        assert!(combat
            .take_hit(
                &mut vitals,
                3,
                GameMode::Survival,
                DamageCause::Zombie,
                player_feet,
                from,
            )
            .is_some());

        combat.advance(9);
        assert!(
            combat.iframe() > 0.0,
            "9 of 10 steps still inside the window, got {}",
            combat.iframe()
        );
        let blocked = combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            from,
        );
        assert_eq!(blocked, None);
        assert_eq!(vitals.health(), 17);

        combat.advance(1);
        assert_eq!(combat.iframe(), 0.0, "the tenth step closes the window");
        let landed = combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            from,
        );
        assert_eq!(
            landed,
            Some(AppliedHit {
                damage: 3,
                health_after: 14,
                killed: false,
            })
        );
    }

    #[test]
    fn saturating_lethal_hit_reports_killed_and_never_wraps() {
        let mut combat = PlayerCombat::default();
        let mut vitals = SurvivalState::new(2, 20, 300).expect("2 hp is legal");
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        let hit = combat
            .take_hit(
                &mut vitals,
                3,
                GameMode::Survival,
                DamageCause::Zombie,
                player_feet,
                from,
            )
            .expect("a lethal hit still lands");
        assert_eq!(vitals.health(), 0, "health must saturate, not wrap");
        assert!(hit.killed);
        assert_eq!(hit.damage, 2, "only the remaining health is applied");
        assert_eq!(hit.health_after, 0);

        // Even with the window closed by the hit and then advanced away, a
        // corpse refuses everything.
        combat.advance(20);
        let corpse = combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            from,
        );
        assert_eq!(corpse, None);
        assert_eq!(vitals.health(), 0);
    }

    #[test]
    fn creative_zero_damage_and_dead_players_are_refused() {
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        // Creative refuses even at full health with an expired window.
        let mut creative_combat = PlayerCombat::default();
        let mut creative_vitals = survival_full();
        assert_eq!(
            creative_combat.take_hit(
                &mut creative_vitals,
                3,
                GameMode::Creative,
                DamageCause::Zombie,
                player_feet,
                from,
            ),
            None
        );
        assert_eq!(creative_vitals, survival_full(), "vitals untouched");
        assert_eq!(creative_combat.hurt_intensity(), 0.0);
        assert_eq!(creative_combat.iframe(), 0.0);
        assert_eq!(creative_combat.knockback(), Vec3::ZERO);

        // Zero damage is refused, as a legal wire value must not arm a window.
        let mut zero_combat = PlayerCombat::default();
        let mut zero_vitals = survival_full();
        assert_eq!(
            zero_combat.take_hit(
                &mut zero_vitals,
                0,
                GameMode::Survival,
                DamageCause::Zombie,
                player_feet,
                from,
            ),
            None
        );
        assert_eq!(zero_vitals, survival_full());
        assert_eq!(zero_combat.iframe(), 0.0);
        assert_eq!(zero_combat.knockback(), Vec3::ZERO);

        // A dead player refuses without touching the cause or timers.
        let mut dead_combat = PlayerCombat::default();
        let mut dead_vitals = SurvivalState::new(0, 20, 300).expect("0 hp is legal");
        assert_eq!(
            dead_combat.take_hit(
                &mut dead_vitals,
                3,
                GameMode::Survival,
                DamageCause::Zombie,
                player_feet,
                from,
            ),
            None
        );
        assert_eq!(dead_vitals.health(), 0);
        assert_eq!(dead_combat.hurt_intensity(), 0.0);
        assert_eq!(dead_combat.iframe(), 0.0);
    }

    #[test]
    fn knockback_points_away_and_never_collapses() {
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let mut vitals = survival_full();

        // Attacker at +Z pushes the player toward -Z.
        let mut away = PlayerCombat::default();
        away.take_hit(
            &mut vitals,
            1,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            player_feet + Vec3::Z,
        );
        assert!((away.knockback().z + PLAYER_KNOCKBACK_SPEED).abs() < 1e-5);
        assert_eq!(away.knockback().x, 0.0);
        assert_eq!(away.knockback().y, 0.0, "no vertical knockback");

        // Attacker exactly at the player's feet: finite non-zero fallback.
        let mut degenerate = PlayerCombat::default();
        degenerate.take_hit(
            &mut vitals,
            1,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            player_feet,
        );
        let fallback = degenerate.knockback();
        assert!(fallback.is_finite());
        assert!((fallback.length() - PLAYER_KNOCKBACK_SPEED).abs() < 1e-5);
        assert_eq!(fallback, Vec3::Z * PLAYER_KNOCKBACK_SPEED);

        // NaN attacker: still finite and non-zero.
        let mut corrupt = PlayerCombat::default();
        corrupt.take_hit(
            &mut vitals,
            1,
            GameMode::Survival,
            DamageCause::Zombie,
            player_feet,
            Vec3::new(f32::NAN, f32::NAN, f32::NAN),
        );
        let recovered = corrupt.knockback();
        assert!(
            recovered.is_finite(),
            "a NaN position cannot leak a NaN push"
        );
        assert!((recovered.length() - PLAYER_KNOCKBACK_SPEED).abs() < 1e-5);

        // Decay provably terminates.
        corrupt.advance(60);
        assert_eq!(corrupt.knockback(), Vec3::ZERO);
    }

    #[test]
    fn knockback_is_integrated_by_update_without_persisting_in_velocity() {
        let mut player = survival_player(Vec3::new(0.5, 5.0, 0.5));
        player.combat.knockback = Vec3::new(PLAYER_KNOCKBACK_SPEED, 0.0, 0.0);

        let start_x = player.position.x;
        player.update(0.05, &InputState::default(), flat_floor);
        let pushed_x = player.position.x;
        assert!(
            pushed_x > start_x + 0.1,
            "the impulse must visibly move the body, {} -> {}",
            start_x,
            pushed_x
        );
        // The impulse lives only in `combat`; velocity is re-derived from input
        // next frame, so it can never become a second, persisted owner.
        assert!(player.combat.knockback().x > 0.0);

        player.combat.advance(60);
        assert_eq!(player.combat.knockback(), Vec3::ZERO);
        let settled_x = player.position.x;
        for _ in 0..10 {
            player.update(0.05, &InputState::default(), flat_floor);
        }
        assert!(
            (player.position.x - settled_x).abs() < 1e-4,
            "a decayed impulse must stop adding displacement, {} -> {}",
            settled_x,
            player.position.x
        );
    }

    #[test]
    fn knockback_never_penetrates_a_wall() {
        let mut player = survival_player(Vec3::new(0.5, 5.0, 0.5));
        player.combat.knockback = Vec3::new(PLAYER_KNOCKBACK_SPEED, 0.0, 0.0);
        // Stone wall occupying block column x = 1 above the floor.
        let wall = |x: i32, y: i32, z: i32| {
            if y < 5 || (x == 1 && (5..8).contains(&y) && z.abs() < 4) {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        };

        for _ in 0..4 {
            player.update(0.05, &InputState::default(), wall);
        }

        assert!(
            player.position.x + PLAYER_HALF_WIDTH < 1.0 + 1e-4,
            "the body must stop at the wall, got x = {}",
            player.position.x
        );
    }

    #[test]
    fn mode_change_clears_combat_state() {
        let mut player = survival_player(Vec3::new(0.5, 5.0, 0.5));
        let mut vitals = player.survival;
        let hit = player.combat.take_hit(
            &mut vitals,
            3,
            GameMode::Survival,
            DamageCause::Zombie,
            player.position,
            player.position + Vec3::Z,
        );
        assert!(hit.is_some());
        player.survival = vitals;
        assert_eq!(player.combat.hurt_intensity(), 1.0);
        assert!(player.combat.knockback().length_squared() > 0.0);

        player.set_mode(GameMode::Creative);

        assert_eq!(player.combat.hurt_intensity(), 0.0);
        assert_eq!(player.combat.iframe(), 0.0);
        assert_eq!(player.combat.knockback(), Vec3::ZERO);
    }

    #[test]
    fn hurt_intensity_is_clamped_and_literal() {
        let mut combat = PlayerCombat::default();
        assert_eq!(combat.hurt_intensity(), 0.0, "quiescent is exactly zero");

        combat.hurt = PLAYER_HURT_FLASH_SECS;
        assert_eq!(combat.hurt_intensity(), 1.0);

        combat.hurt = PLAYER_HURT_FLASH_SECS * 0.5;
        assert!((combat.hurt_intensity() - 0.5).abs() < 1e-6);

        combat.hurt = PLAYER_HURT_FLASH_SECS * 4.0;
        assert_eq!(combat.hurt_intensity(), 1.0, "clamped above");

        combat.hurt = -1.0;
        assert_eq!(combat.hurt_intensity(), 0.0);

        combat.hurt = f32::NAN;
        assert_eq!(combat.hurt_intensity(), 0.0, "non-finite reads as zero");

        combat.hurt = f32::INFINITY;
        assert_eq!(combat.hurt_intensity(), 0.0, "non-finite reads as zero");
    }

    #[test]
    fn damage_cause_labels_are_stable() {
        // T05 renders these strings and S08 extends the enum; the existing
        // four must never move.
        assert_eq!(DamageCause::Zombie.label(), "Zombie");
        assert_eq!(DamageCause::Fall.label(), "Fall");
        assert_eq!(DamageCause::Drowning.label(), "Drowning");
        assert_eq!(DamageCause::Starvation.label(), "Starvation");
    }
}
