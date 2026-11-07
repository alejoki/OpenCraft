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

/// Tunable player-combat tuning consumed by the `_with_rules` entry points.
///
/// [`CombatRules::default`] is built from the constants above, so the default
/// bundle reproduces [`PlayerCombat::take_hit`], [`PlayerCombat::advance`], and
/// [`PlayerCombat::hurt_intensity`] exactly. The struct is a plain value type:
/// it carries no TOML/config vocabulary, only the numbers the simulation
/// consumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatRules {
    /// Seconds a player ignores further hits after one lands.
    pub iframe_secs: f32,
    /// Seconds the red damage flash stays at full intensity after a hit.
    pub hurt_flash_secs: f32,
    /// Initial horizontal speed of the knockback impulse, blocks/second.
    pub knockback_speed: f32,
    /// Per-fixed-step multiplier applied to the remaining impulse.
    pub knockback_decay: f32,
}

impl Default for CombatRules {
    fn default() -> Self {
        Self {
            iframe_secs: PLAYER_IFRAME_SECS,
            hurt_flash_secs: PLAYER_HURT_FLASH_SECS,
            knockback_speed: PLAYER_KNOCKBACK_SPEED,
            knockback_decay: PLAYER_KNOCKBACK_DECAY,
        }
    }
}

/// Clamp a rules timer/speed to a finite, non-negative value.
///
/// Non-finite rules (impossible through the validated config path) fail
/// closed as zero rather than leaking NaN into a timer or the physics
/// integrator; negative values are inert for the same reason.
fn sanitize_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

/// Squared horizontal length below which a separation vector is treated as
/// degenerate. Mirrors [`crate::world::mob::Mob::take_hit`] so player and mob
/// knockback fall back the same way.
const MIN_DIRECTION_LEN_SQ: f32 = 1e-6;

/// Ambient, never-persisted reason for a player damage event.
///
/// The tag is produced at the single damage site and rendered on the Death
/// screen (T05). It is **not** part of the v7 wire format: a loaded dead save
/// has no cause and renders "Unknown". The declaration order is the order the
/// first four labels shipped in (S06) and that order must never change;
/// [`DamageCause::SkeletonArrow`] was appended by S07-T03 and is the fifth and
/// last label the Death screen can show.
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
    /// A skeleton's arrow, applied through the single player-damage authority.
    ///
    /// Exactly like the other four tags it is ephemeral: it lives in the
    /// session's `last_damage_cause` for the current death and never gains a
    /// save byte.
    SkeletonArrow,
}

impl DamageCause {
    /// Stable human-readable label, also the Death-screen text.
    ///
    /// The first four strings are byte-identical to their S06 values; S07 only
    /// appended the fifth arm.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Zombie => "Zombie",
            Self::Fall => "Fall",
            Self::Drowning => "Drowning",
            Self::Starvation => "Starvation",
            Self::SkeletonArrow => "Skeleton arrow",
        }
    }
}

/// One incoming player hit: how much, what caused it, and the geometry the
/// knockback direction derives from.
///
/// `cause` is carried so the caller passes one authoritative tag at the damage
/// site, but [`PlayerCombat`] never stores it: the session owns the single
/// cause writer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HitRequest {
    /// Requested damage before the vitals' saturating clamp.
    pub damage: u16,
    /// Ambient cause tag for the session's death screen.
    pub cause: DamageCause,
    /// Player feet position at hit time.
    pub player_feet: Vec3,
    /// Attacker/projectile position the knockback direction points away from.
    pub from: Vec3,
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
    /// Red flash intensity in `0.0..=1.0` using default rules.
    ///
    /// Non-finite and non-positive timers both read as "no flash", so a
    /// corrupted value can never paint a garbage colour.
    pub fn hurt_intensity(&self) -> f32 {
        self.hurt_intensity_with_rules(&CombatRules::default())
    }

    /// Rules-aware [`PlayerCombat::hurt_intensity`].
    ///
    /// The flash fraction divides by `rules.hurt_flash_secs`, so tuning the
    /// flash duration changes the fade speed without moving the stored timer.
    /// A non-finite or non-positive duration reads as "no flash".
    pub fn hurt_intensity_with_rules(&self, rules: &CombatRules) -> f32 {
        if !self.hurt.is_finite() || self.hurt <= 0.0 {
            return 0.0;
        }
        let duration = sanitize_non_negative(rules.hurt_flash_secs);
        if duration <= 0.0 {
            return 0.0;
        }
        (self.hurt / duration).clamp(0.0, 1.0)
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

    /// Applies one player hit using default rules, or refuses it without
    /// mutating anything.
    ///
    /// Refuses when the mode is not Survival, the player is already dead
    /// (`health == 0`), the i-frame window is open, or `hit.damage == 0`. On
    /// acceptance it arms the hurt flash and i-frames, sets the knockback
    /// impulse pointing horizontally away from `hit.from`, and saturates vitals
    /// damage through [`SurvivalState::damage`] — no negative or wrapped
    /// health can be stored.
    ///
    /// `hit.cause` is accepted so the caller passes one authoritative tag at
    /// the damage site, but it is deliberately **not** stored here: the session
    /// owns the single cause writer, so there is exactly one authority.
    ///
    /// The knockback direction is the horizontal vector from `hit.from` to
    /// `hit.player_feet`. A degenerate direction (attacker inside the player)
    /// or a non-finite one falls back to `Vec3::Z`, so the impulse is always
    /// finite and non-zero. Vertical knockback is deliberately absent: the
    /// player is never pushed into the floor or the ceiling.
    pub fn take_hit(
        &mut self,
        vitals: &mut SurvivalState,
        hit: HitRequest,
        mode: GameMode,
    ) -> Option<AppliedHit> {
        self.take_hit_with_rules(vitals, hit, mode, &CombatRules::default())
    }

    /// Rules-aware [`PlayerCombat::take_hit`].
    ///
    /// The same acceptance gate and saturating damage apply; the armed flash,
    /// i-frame window, and initial knockback speed come from `rules`.
    /// `knockback_decay` is not used here — [`PlayerCombat::advance_with_rules`]
    /// owns the per-fixed-step decay exactly like the pinned constant did.
    pub fn take_hit_with_rules(
        &mut self,
        vitals: &mut SurvivalState,
        hit: HitRequest,
        mode: GameMode,
        rules: &CombatRules,
    ) -> Option<AppliedHit> {
        let HitRequest {
            damage,
            cause,
            player_feet,
            from,
        } = hit;
        if mode != GameMode::Survival || vitals.health() == 0 || self.iframe > 0.0 || damage == 0 {
            return None;
        }

        // The cause is intentionally not persisted or stored on this type; the
        // session is the only writer of `last_damage_cause` (T04).
        let _ = cause;

        self.hurt = sanitize_non_negative(rules.hurt_flash_secs);
        self.iframe = sanitize_non_negative(rules.iframe_secs);

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
        self.knockback = dir * sanitize_non_negative(rules.knockback_speed);

        let applied = vitals.damage(damage);
        let health_after = vitals.health();
        Some(AppliedHit {
            damage: applied,
            health_after,
            killed: health_after == 0,
        })
    }

    /// Advances the timers by `ai_steps` executed fixed mob steps using
    /// default rules.
    ///
    /// Driven by `MobTick.ai_steps`, never by render `dt`, so i-frames and the
    /// flash are frame-rate independent (MEM410/MEM415). A non-finite impulse
    /// is dropped rather than propagated into the physics integration.
    pub fn advance(&mut self, ai_steps: u32) {
        self.advance_with_rules(ai_steps, &CombatRules::default())
    }

    /// Rules-aware [`PlayerCombat::advance`].
    ///
    /// `rules.knockback_decay` is applied **once per executed fixed step**,
    /// preserving the existing multiplier semantics; it is not reinterpreted
    /// as a per-second rate even though the registry documents the field that
    /// way. A zero decay stops the impulse immediately, and a non-finite one
    /// fails closed to zero.
    pub fn advance_with_rules(&mut self, ai_steps: u32, rules: &CombatRules) {
        let decay = sanitize_non_negative(rules.knockback_decay);
        for _ in 0..ai_steps {
            self.hurt = (self.hurt - MOB_TICK_SECS).max(0.0);
            self.iframe = (self.iframe - MOB_TICK_SECS).max(0.0);
            self.knockback *= decay;
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
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet,
                from,
            },
            GameMode::Survival,
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
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet,
                from,
            },
            GameMode::Survival,
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
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet,
                from: player_feet - Vec3::X,
            },
            GameMode::Survival,
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
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival
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
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet,
                from,
            },
            GameMode::Survival,
        );
        assert_eq!(blocked, None);
        assert_eq!(vitals.health(), 17);

        combat.advance(1);
        assert_eq!(combat.iframe(), 0.0, "the tenth step closes the window");
        let landed = combat.take_hit(
            &mut vitals,
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet,
                from,
            },
            GameMode::Survival,
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
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from,
                },
                GameMode::Survival,
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
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet,
                from,
            },
            GameMode::Survival,
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
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Creative
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
                HitRequest {
                    damage: 0,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival
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
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival
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
            HitRequest {
                damage: 1,
                cause: DamageCause::Zombie,
                player_feet,
                from: player_feet + Vec3::Z,
            },
            GameMode::Survival,
        );
        assert!((away.knockback().z + PLAYER_KNOCKBACK_SPEED).abs() < 1e-5);
        assert_eq!(away.knockback().x, 0.0);
        assert_eq!(away.knockback().y, 0.0, "no vertical knockback");

        // Attacker exactly at the player's feet: finite non-zero fallback.
        let mut degenerate = PlayerCombat::default();
        degenerate.take_hit(
            &mut vitals,
            HitRequest {
                damage: 1,
                cause: DamageCause::Zombie,
                player_feet,
                from: player_feet,
            },
            GameMode::Survival,
        );
        let fallback = degenerate.knockback();
        assert!(fallback.is_finite());
        assert!((fallback.length() - PLAYER_KNOCKBACK_SPEED).abs() < 1e-5);
        assert_eq!(fallback, Vec3::Z * PLAYER_KNOCKBACK_SPEED);

        // NaN attacker: still finite and non-zero.
        let mut corrupt = PlayerCombat::default();
        corrupt.take_hit(
            &mut vitals,
            HitRequest {
                damage: 1,
                cause: DamageCause::Zombie,
                player_feet,
                from: Vec3::new(f32::NAN, f32::NAN, f32::NAN),
            },
            GameMode::Survival,
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
            HitRequest {
                damage: 3,
                cause: DamageCause::Zombie,
                player_feet: player.position,
                from: player.position + Vec3::Z,
            },
            GameMode::Survival,
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
        // T05 renders these strings. The first four are byte-identical to
        // their S06 values and must never move; S07-T03 appended the fifth.
        assert_eq!(DamageCause::Zombie.label(), "Zombie");
        assert_eq!(DamageCause::Fall.label(), "Fall");
        assert_eq!(DamageCause::Drowning.label(), "Drowning");
        assert_eq!(DamageCause::Starvation.label(), "Starvation");
        assert_eq!(DamageCause::SkeletonArrow.label(), "Skeleton arrow");
    }

    // ---- CombatRules ---------------------------------------------------

    fn tuned_rules() -> CombatRules {
        CombatRules {
            iframe_secs: 1.0,
            hurt_flash_secs: 1.0,
            knockback_speed: 2.0,
            knockback_decay: 0.5,
        }
    }

    #[test]
    fn default_combat_rules_are_the_pinned_constants() {
        assert_eq!(
            CombatRules::default(),
            CombatRules {
                iframe_secs: PLAYER_IFRAME_SECS,
                hurt_flash_secs: PLAYER_HURT_FLASH_SECS,
                knockback_speed: PLAYER_KNOCKBACK_SPEED,
                knockback_decay: PLAYER_KNOCKBACK_DECAY,
            }
        );
    }

    #[test]
    fn tuned_combat_rules_change_iframe_flash_and_knockback() {
        let rules = tuned_rules();
        let mut combat = PlayerCombat::default();
        let mut vitals = survival_full();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        let hit = combat
            .take_hit_with_rules(
                &mut vitals,
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from,
                },
                GameMode::Survival,
                &rules,
            )
            .expect("a legal hit lands");
        assert_eq!(hit.health_after, 17);
        assert_eq!(combat.iframe(), 1.0, "the tuned i-frame is armed");
        assert_eq!(combat.hurt_intensity_with_rules(&rules), 1.0);
        assert!(
            (combat.knockback() - Vec3::Z * -2.0).length() < 1e-6,
            "the tuned 2 b/s impulse points away from the attacker, got {:?}",
            combat.knockback()
        );

        // One fixed step at the tuned 0.5 decay halves the impulse; the
        // pinned default would leave 2 * 0.6 = 1.2.
        combat.advance_with_rules(1, &rules);
        assert!(
            (combat.knockback() - Vec3::Z * -1.0).length() < 1e-6,
            "tuned decay must be applied once per fixed step, got {:?}",
            combat.knockback()
        );
        // One fixed 0.05 s step of the tuned 1.0 s flash leaves 0.95; the
        // default wrapper still divides by the pinned 0.5 s duration and
        // clamps, so it reads 1.0 for the same timer.
        let intensity = combat.hurt_intensity_with_rules(&rules);
        assert!(
            (intensity - 0.95).abs() < 1e-5,
            "tuned flash fade was {intensity}"
        );
        assert_eq!(
            combat.hurt_intensity(),
            1.0,
            "the default wrapper still divides by the pinned flash duration"
        );

        // The tuned 1.0 s window is still open after 0.5 s but accepts the
        // hit after a full second.
        combat.advance_with_rules(9, &rules);
        assert!(combat.iframe() > 0.0);
        // Ten 0.05 s steps consume half of the tuned flash: 0.50 remains.
        let half_intensity = combat.hurt_intensity_with_rules(&rules);
        assert!(
            (half_intensity - 0.5).abs() < 1e-5,
            "tuned half-flash fade was {half_intensity}"
        );
        assert_eq!(
            combat.take_hit_with_rules(
                &mut vitals,
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival,
                &rules
            ),
            None,
            "the tuned window must refuse a hit at 0.5 s"
        );
        combat.advance_with_rules(10, &rules);
        assert_eq!(combat.iframe(), 0.0);
        assert!(
            combat
                .take_hit_with_rules(
                    &mut vitals,
                    HitRequest {
                        damage: 3,
                        cause: DamageCause::Zombie,
                        player_feet,
                        from
                    },
                    GameMode::Survival,
                    &rules
                )
                .is_some(),
            "the tuned window must open after 1.0 s"
        );
    }

    #[test]
    fn zero_tuned_iframe_allows_back_to_back_hits() {
        let rules = CombatRules {
            iframe_secs: 0.0,
            ..CombatRules::default()
        };
        let mut combat = PlayerCombat::default();
        let mut vitals = survival_full();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        assert!(combat
            .take_hit_with_rules(
                &mut vitals,
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival,
                &rules
            )
            .is_some());
        assert_eq!(combat.iframe(), 0.0);
        let second = combat
            .take_hit_with_rules(
                &mut vitals,
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from,
                },
                GameMode::Survival,
                &rules,
            )
            .expect("a zero i-frame window never blocks");
        assert_eq!(second.health_after, 14);
    }

    #[test]
    fn zero_and_malformed_combat_rules_fail_closed() {
        let zero = CombatRules {
            iframe_secs: 0.0,
            hurt_flash_secs: 0.0,
            knockback_speed: 0.0,
            knockback_decay: 0.0,
        };
        let mut combat = PlayerCombat::default();
        let mut vitals = survival_full();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        assert!(combat
            .take_hit_with_rules(
                &mut vitals,
                HitRequest {
                    damage: 1,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from: player_feet + Vec3::Z
                },
                GameMode::Survival,
                &zero
            )
            .is_some());
        assert_eq!(combat.hurt_intensity_with_rules(&zero), 0.0);
        assert_eq!(combat.iframe(), 0.0);
        assert_eq!(combat.knockback(), Vec3::ZERO);
        combat.advance_with_rules(30, &zero);
        assert_eq!(combat.knockback(), Vec3::ZERO);

        let malformed = CombatRules {
            iframe_secs: f32::NAN,
            hurt_flash_secs: f32::NEG_INFINITY,
            knockback_speed: f32::INFINITY,
            knockback_decay: f32::NAN,
        };
        let mut corrupt = PlayerCombat::default();
        let mut corrupt_vitals = survival_full();
        assert!(corrupt
            .take_hit_with_rules(
                &mut corrupt_vitals,
                HitRequest {
                    damage: 1,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from: player_feet + Vec3::Z
                },
                GameMode::Survival,
                &malformed
            )
            .is_some());
        assert_eq!(corrupt.hurt_intensity_with_rules(&malformed), 0.0);
        assert_eq!(corrupt.iframe(), 0.0);
        assert_eq!(corrupt.knockback(), Vec3::ZERO);
        corrupt.advance_with_rules(30, &malformed);
        assert_eq!(corrupt.knockback(), Vec3::ZERO);
    }

    #[test]
    fn default_combat_rules_reproduce_the_existing_wrappers() {
        let rules = CombatRules::default();
        let player_feet = Vec3::new(0.5, 5.0, 0.5);
        let from = player_feet + Vec3::Z;

        let mut legacy = PlayerCombat::default();
        let mut legacy_vitals = survival_full();
        let mut tuned = PlayerCombat::default();
        let mut tuned_vitals = survival_full();

        assert_eq!(
            legacy.take_hit(
                &mut legacy_vitals,
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival
            ),
            tuned.take_hit_with_rules(
                &mut tuned_vitals,
                HitRequest {
                    damage: 3,
                    cause: DamageCause::Zombie,
                    player_feet,
                    from
                },
                GameMode::Survival,
                &rules
            )
        );
        assert_eq!(
            legacy.hurt_intensity(),
            tuned.hurt_intensity_with_rules(&rules)
        );
        assert_eq!(legacy.iframe(), tuned.iframe());
        assert_eq!(legacy.knockback(), tuned.knockback());

        for steps in [1, 4, 10, 30] {
            legacy.advance(steps);
            tuned.advance_with_rules(steps, &rules);
            assert_eq!(legacy.iframe(), tuned.iframe(), "iframe diverged");
            assert_eq!(
                legacy.knockback(),
                tuned.knockback(),
                "knockback diverged after {steps} steps"
            );
            assert_eq!(
                legacy.hurt_intensity(),
                tuned.hurt_intensity_with_rules(&rules),
                "flash diverged after {steps} steps"
            );
        }
    }
}
