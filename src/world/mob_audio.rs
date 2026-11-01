//! Pure classification of the audible mob/combat events in one frame
//! (M009/S09-T04).
//!
//! The nine-slot sound table exists (S09-T03), but something has to decide
//! *which* of the five new slots one frame deserves. That decision is data
//! here — no player type, no session, no renderer, no wall clock, no RNG, and
//! no allocation — exactly like [`crate::world::mob_ai`]. The real Playing
//! frame owns the only `AudioEngine::play` calls (MEM169/MEM171: audio must
//! stay unreachable from free helpers and test-driven methods), so this module
//! only answers *what was audible*.
//!
//! ## Rules
//!
//! * **Melee.** A killing swing is `death`; any other proven hit is `hurt`.
//!   A refused or i-frame-blocked swing (`None`) is silent.
//! * **Arrows.** One arrow tick reports counts, never N sounds: any body or
//!   block hit is one `arrow_impact`, a kill is one `death`, and `hurt` only
//!   when a mob was hit but nothing died (a killing arrow plays `death`
//!   instead of both).
//! * **Idle.** [`MobIdleGate`] is a timer, not a per-frame check, so a player
//!   standing next to a mob hears one idle every [`MOB_IDLE_INTERVAL_SECS`]
//!   rather than a buzz every frame. When nothing is audible it retries on
//!   the cheaper [`MOB_IDLE_RETRY_SECS`] cadence. List order decides, never
//!   RNG, so the same world state always makes the same noise.
//!
//! ## Mutation discipline
//!
//! [`MobAudioFrame`] is a value with no side effects; the frame merges the
//! independent observations with [`MobAudioFrame::merge`] (logical OR), so
//! ordering between the melee arm, the arrow sweep, and the idle gate can
//! never change the result.

use glam::Vec3;

use crate::world::mob::{Mob, MobHit};

/// A mob within this many blocks of the listener can be heard idling.
pub const MOB_IDLE_RANGE: f32 = 12.0;
/// Seconds between audible idle calls while a live mob stays in range.
pub const MOB_IDLE_INTERVAL_SECS: f32 = 6.0;
/// Seconds before retrying when the interval expires with nothing audible.
pub const MOB_IDLE_RETRY_SECS: f32 = 0.5;

/// The audible events one frame produced.
///
/// One flag per new sound slot; `false` means the slot is silent this frame.
/// Plain booleans keep the value `Copy` and allocation-free, and the fixed
/// field order matches [`crate::main`]'s slot mapping.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MobAudioFrame {
    /// A live mob is near the listener and its idle timer expired.
    pub idle: bool,
    /// A mob took damage but survived.
    pub hurt: bool,
    /// A mob died.
    pub death: bool,
    /// The player released a real bow shot (never a tap-cancel).
    pub bow_release: bool,
    /// At least one arrow hit a body or a block this frame.
    pub arrow_impact: bool,
}

impl MobAudioFrame {
    /// Logical OR: the frame accumulates independent observations, so merge
    /// order never changes the result.
    pub fn merge(self, other: MobAudioFrame) -> MobAudioFrame {
        MobAudioFrame {
            idle: self.idle || other.idle,
            hurt: self.hurt || other.hurt,
            death: self.death || other.death,
            bow_release: self.bow_release || other.bow_release,
            arrow_impact: self.arrow_impact || other.arrow_impact,
        }
    }

    /// Classify one proven player melee swing.
    ///
    /// `Some(hit)` is only returned for an applied hit, so a refused or
    /// i-frame-blocked swing (`None`) is silent. A kill is `death`, never
    /// also `hurt`.
    pub fn from_melee_hit(hit: Option<MobHit>) -> MobAudioFrame {
        match hit {
            Some(hit) if hit.killed => MobAudioFrame {
                death: true,
                ..MobAudioFrame::default()
            },
            Some(_) => MobAudioFrame {
                hurt: true,
                ..MobAudioFrame::default()
            },
            None => MobAudioFrame::default(),
        }
    }

    /// Classify one [`crate::world::session::ArrowTick`] by its counts.
    ///
    /// The counts are collapsed, never replayed: any body/block hit is one
    /// `arrow_impact`, a kill is one `death`, and `hurt` fires only when at
    /// least one mob was hit and nothing died (a killing arrow plays `death`
    /// instead of both). Blocks and the player never produce `hurt`.
    pub fn from_arrow_counts(
        mob_hits: u32,
        block_hits: u32,
        player_hits: u32,
        kills: u32,
    ) -> MobAudioFrame {
        MobAudioFrame {
            idle: false,
            hurt: mob_hits > 0 && kills == 0,
            death: kills > 0,
            bow_release: false,
            arrow_impact: mob_hits > 0 || block_hits > 0 || player_hits > 0,
        }
    }
}

/// Timer that turns "a live mob is near" into a bounded idle cadence.
///
/// `Default` starts one full [`MOB_IDLE_INTERVAL_SECS`] away, so a fresh
/// session (or a world swap that resets it) never fires an idle on its very
/// first frame.
pub struct MobIdleGate {
    /// Seconds until the next idle check. Public to no one: only [`Self::tick`]
    /// reads or writes it.
    timer: f32,
}

impl Default for MobIdleGate {
    fn default() -> Self {
        MobIdleGate {
            timer: MOB_IDLE_INTERVAL_SECS,
        }
    }
}

impl MobIdleGate {
    /// Advance the gate by one raw frame delta and report whether an idle
    /// call is due this frame.
    ///
    /// `dt` is sanitized: a non-finite or non-positive delta advances
    /// nothing, and a genuine delta is clamped to 0.25 s so one long stall
    /// (a paused debugger, a slow frame) cannot skip several intervals and
    /// fire repeatedly. While the timer is positive the call stays silent and
    /// counts down. On expiry it re-arms to [`MOB_IDLE_INTERVAL_SECS`] and
    /// returns `true` when at least one live mob is within
    /// [`MOB_IDLE_RANGE`]; otherwise it re-arms to [`MOB_IDLE_RETRY_SECS`] and
    /// returns `false`. Iteration order decides, never RNG.
    pub fn tick<'a>(
        &mut self,
        dt: f32,
        listener: Vec3,
        mobs: impl Iterator<Item = &'a Mob>,
    ) -> bool {
        if !dt.is_finite() || dt <= 0.0 {
            return false;
        }
        let dt = dt.min(0.25);
        if self.timer > 0.0 {
            self.timer -= dt;
            if self.timer > 0.0 {
                return false;
            }
        }
        let range_squared = MOB_IDLE_RANGE * MOB_IDLE_RANGE;
        let audible = mobs
            .into_iter()
            .any(|mob| mob.health > 0 && mob.position.distance_squared(listener) <= range_squared);
        if audible {
            self.timer = MOB_IDLE_INTERVAL_SECS;
            true
        } else {
            self.timer = MOB_IDLE_RETRY_SECS;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::mob::{Mob, MobKind};

    fn hit(killed: bool) -> MobHit {
        MobHit {
            damage: 4,
            health_after: if killed { 0 } else { 6 },
            killed,
        }
    }

    fn mob(at: Vec3, health: u16) -> Mob {
        let mut mob = Mob::new(MobKind::Pig, at, 0.0);
        mob.health = health;
        mob
    }

    #[test]
    fn melee_maps_kill_to_death_and_survival_to_hurt() {
        assert_eq!(
            MobAudioFrame::from_melee_hit(Some(hit(true))),
            MobAudioFrame {
                death: true,
                ..MobAudioFrame::default()
            }
        );
        assert_eq!(
            MobAudioFrame::from_melee_hit(Some(hit(false))),
            MobAudioFrame {
                hurt: true,
                ..MobAudioFrame::default()
            }
        );
    }

    #[test]
    fn refused_melee_is_silent() {
        assert_eq!(
            MobAudioFrame::from_melee_hit(None),
            MobAudioFrame::default()
        );
    }

    #[test]
    fn arrow_hitting_a_mob_hurts_and_impacts() {
        assert_eq!(
            MobAudioFrame::from_arrow_counts(1, 0, 0, 0),
            MobAudioFrame {
                hurt: true,
                arrow_impact: true,
                ..MobAudioFrame::default()
            }
        );
    }

    #[test]
    fn arrow_into_a_block_only_impacts() {
        assert_eq!(
            MobAudioFrame::from_arrow_counts(0, 1, 0, 0),
            MobAudioFrame {
                arrow_impact: true,
                ..MobAudioFrame::default()
            }
        );
    }

    #[test]
    fn arrow_into_the_player_only_impacts() {
        assert_eq!(
            MobAudioFrame::from_arrow_counts(0, 0, 1, 0),
            MobAudioFrame {
                arrow_impact: true,
                ..MobAudioFrame::default()
            }
        );
    }

    #[test]
    fn killing_arrow_plays_death_instead_of_hurt() {
        assert_eq!(
            MobAudioFrame::from_arrow_counts(1, 0, 0, 1),
            MobAudioFrame {
                death: true,
                arrow_impact: true,
                ..MobAudioFrame::default()
            }
        );
    }

    #[test]
    fn one_arrow_tick_collapses_every_count_to_at_most_three_slots() {
        let frame = MobAudioFrame::from_arrow_counts(3, 2, 1, 2);
        assert!(frame.arrow_impact);
        assert!(frame.death);
        assert!(!frame.hurt, "a kill suppresses hurt");
        assert!(!frame.idle && !frame.bow_release);
    }

    #[test]
    fn quiet_arrow_tick_is_silent() {
        assert_eq!(
            MobAudioFrame::from_arrow_counts(0, 0, 0, 0),
            MobAudioFrame::default()
        );
    }

    #[test]
    fn merge_is_logical_or_and_order_independent() {
        let a = MobAudioFrame {
            idle: true,
            death: true,
            ..MobAudioFrame::default()
        };
        let b = MobAudioFrame {
            hurt: true,
            bow_release: true,
            arrow_impact: true,
            ..MobAudioFrame::default()
        };
        let merged = a.merge(b);
        assert!(
            merged.idle && merged.hurt && merged.death && merged.bow_release && merged.arrow_impact
        );
        assert_eq!(merged, b.merge(a));
        assert_eq!(MobAudioFrame::default().merge(a), a);
    }

    #[test]
    fn idle_fires_after_the_interval_with_a_live_mob_in_range() {
        let mut gate = MobIdleGate::default();
        let listener = Vec3::ZERO;
        let near = mob(Vec3::new(3.0, 0.0, 0.0), 10);
        // 24 x 0.25 s = the full 6 s interval.
        for _ in 0..23 {
            assert!(!gate.tick(0.25, listener, std::iter::once(&near)));
        }
        assert!(gate.tick(0.25, listener, std::iter::once(&near)));
    }

    /// Advance a gate through one full default interval (24 x 0.25 s = 6 s)
    /// and return the final tick's answer.
    fn run_to_expiry(gate: &mut MobIdleGate, listener: Vec3, mobs: &[Mob]) -> bool {
        let mut fired = false;
        for _ in 0..24 {
            fired = gate.tick(0.25, listener, mobs.iter());
        }
        fired
    }

    #[test]
    fn idle_is_silent_with_no_mobs_out_of_range_or_dead() {
        let listener = Vec3::ZERO;
        let mut gate = MobIdleGate::default();
        assert!(!run_to_expiry(&mut gate, listener, &[]));
        assert_eq!(gate.timer, MOB_IDLE_RETRY_SECS);

        let far = mob(Vec3::new(MOB_IDLE_RANGE + 1.0, 0.0, 0.0), 10);
        let mut gate = MobIdleGate::default();
        assert!(!run_to_expiry(&mut gate, listener, &[far]));

        let dead = mob(Vec3::ZERO, 0);
        let mut gate = MobIdleGate::default();
        assert!(!run_to_expiry(&mut gate, listener, &[dead]));
    }

    #[test]
    fn idle_retry_cadence_waits_the_short_interval_after_a_quiet_check() {
        let mut gate = MobIdleGate::default();
        let listener = Vec3::ZERO;
        assert!(!run_to_expiry(&mut gate, listener, &[]));
        assert_eq!(gate.timer, MOB_IDLE_RETRY_SECS);
        assert!(!gate.tick(0.25, listener, std::iter::empty()));
        let near = mob(Vec3::ZERO, 10);
        assert!(gate.tick(0.25, listener, std::iter::once(&near)));
    }

    #[test]
    fn non_finite_or_non_positive_dt_advances_nothing() {
        let listener = Vec3::ZERO;
        let near = mob(Vec3::ZERO, 10);
        let mut gate = MobIdleGate::default();
        for bad in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert!(!gate.tick(bad, listener, std::iter::once(&near)));
            assert_eq!(gate.timer, MOB_IDLE_INTERVAL_SECS);
        }
    }

    #[test]
    fn dt_is_clamped_so_one_long_stall_cannot_fire_repeatedly() {
        let listener = Vec3::ZERO;
        let near = mob(Vec3::ZERO, 10);
        let mut gate = MobIdleGate::default();
        // A 5 s stall only advances the timer by the 0.25 s clamp.
        assert!(!gate.tick(5.0, listener, std::iter::once(&near)));
        assert_eq!(gate.timer, MOB_IDLE_INTERVAL_SECS - 0.25);
        // And it cannot fire again on the very next frame either.
        assert!(!gate.tick(0.25, listener, std::iter::once(&near)));
    }
}
