//! The session-owned mob entity core and its deterministic fixed-rate
//! runtime (M009/S01-T02, D131/D134/D135).
//!
//! This module is pure lib code: no wgpu, no winit, no wall clock. The
//! renderer (S01-T03/T04) reads [`Mob`] records, and the session
//! ([`crate::world::session::WorldSession`]) owns the flat `Vec<Mob>` plus a
//! [`MobRuntime`]. Everything here is headlessly testable.
//!
//! ## Yaw convention
//!
//! Yaw is measured in radians. **Yaw 0 faces +Z**, and positive yaw rotates
//! toward +X, so the forward vector is `(sin yaw, 0, cos yaw)`. This matches
//! `glam::Quat::from_rotation_y(yaw) * Vec3::Z` and the model-space geometry
//! built in `render/mob.rs` (feet at the origin, yaw 0 => +Z).

use glam::Vec3;

/// Fixed simulation rate of every mob AI step (D134).
pub const MOB_TICK_HZ: u32 = 20;
/// Seconds per fixed mob step.
pub const MOB_TICK_SECS: f32 = 1.0 / MOB_TICK_HZ as f32;
/// A single frame may never run more than this many fixed steps, so a long
/// stall cannot spiral into an unbounded catch-up loop.
pub const MAX_MOB_STEPS_PER_CALL: u32 = 4;

/// Horizontal distance from `home` at which a wandering mob turns back.
const HOME_LEASH: f32 = 4.0;
/// How often (in fixed steps) a wandering mob re-rolls its yaw/state.
const YAW_REROLL_STEPS: u64 = 16;

/// The four S01 mob kinds. S02 persists [`MobKind::tag`] and reuses
/// [`MobKind::from_tag`]; never reorder the tags.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MobKind {
    Pig,
    Cow,
    Zombie,
    Skeleton,
}

impl MobKind {
    /// Every kind in stable tag order.
    pub const ALL: [MobKind; 4] = [
        MobKind::Pig,
        MobKind::Cow,
        MobKind::Zombie,
        MobKind::Skeleton,
    ];

    /// Stable persistence tag (0..=3). S02 writes this byte.
    pub const fn tag(self) -> u8 {
        match self {
            MobKind::Pig => 0,
            MobKind::Cow => 1,
            MobKind::Zombie => 2,
            MobKind::Skeleton => 3,
        }
    }

    /// Inverse of [`MobKind::tag`]; any other byte is an unknown kind.
    pub const fn from_tag(tag: u8) -> Option<MobKind> {
        match tag {
            0 => Some(MobKind::Pig),
            1 => Some(MobKind::Cow),
            2 => Some(MobKind::Zombie),
            3 => Some(MobKind::Skeleton),
            _ => None,
        }
    }

    /// Full health pool. Hostile kinds are tougher than passive ones.
    pub const fn max_health(self) -> u16 {
        match self {
            MobKind::Pig | MobKind::Cow => 10,
            MobKind::Zombie | MobKind::Skeleton => 20,
        }
    }

    /// Full horizontal AABB extent in blocks.
    pub const fn width(self) -> f32 {
        match self {
            MobKind::Pig => 0.7,
            MobKind::Cow => 0.9,
            MobKind::Zombie | MobKind::Skeleton => 0.7,
        }
    }

    /// Full vertical AABB extent in blocks (feet to head).
    pub const fn height(self) -> f32 {
        match self {
            MobKind::Pig => 0.9,
            MobKind::Cow => 1.3,
            MobKind::Zombie | MobKind::Skeleton => 1.8,
        }
    }

    /// Wander speed in blocks per second.
    pub const fn walk_speed(self) -> f32 {
        match self {
            MobKind::Pig => 1.0,
            MobKind::Cow => 0.9,
            MobKind::Zombie => 1.6,
            MobKind::Skeleton => 1.7,
        }
    }
}

/// The only two AI states S01 needs. S05 may add pursuit later without
/// changing [`Mob`]'s layout contract drastically.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MobAiState {
    /// Standing still; `walk_phase` does not advance.
    Idle,
    /// Walking along `yaw` at [`MobKind::walk_speed`].
    Wander,
}

/// One live mob record. `home` is the spawn anchor used for the leash and
/// reused by S05's spawn/bed rules.
#[derive(Clone, Debug)]
pub struct Mob {
    pub kind: MobKind,
    /// Spawn anchor (feet position at spawn).
    pub home: Vec3,
    /// Feet position.
    pub position: Vec3,
    /// Last applied horizontal velocity in blocks/second.
    pub velocity: Vec3,
    /// Facing angle in radians; 0 faces +Z (see module docs).
    pub yaw: f32,
    pub health: u16,
    pub state: MobAiState,
    /// Remaining hurt-flash time in seconds; decays only while resident.
    pub hurt: f32,
    /// Generic action cooldown in seconds; S02+ uses it.
    pub cooldown: f32,
    /// Walk cycle phase (radians), advanced while wandering.
    pub walk_phase: f32,
}

impl Mob {
    /// A freshly spawned, full-health wandering mob anchored at `position`.
    pub fn new(kind: MobKind, position: Vec3, yaw: f32) -> Mob {
        Mob {
            kind,
            home: position,
            position,
            velocity: Vec3::ZERO,
            yaw,
            health: kind.max_health(),
            state: MobAiState::Wander,
            hurt: 0.0,
            cooldown: 0.0,
            walk_phase: 0.0,
        }
    }

    /// World-space AABB as `(min, max)`. Feet are at `position.y`, so the
    /// box spans `position.y ..= position.y + height` vertically and
    /// `position ± width / 2` on x/z.
    pub fn aabb(&self) -> (Vec3, Vec3) {
        let half = self.kind.width() * 0.5;
        (
            Vec3::new(
                self.position.x - half,
                self.position.y,
                self.position.z - half,
            ),
            Vec3::new(
                self.position.x + half,
                self.position.y + self.kind.height(),
                self.position.z + half,
            ),
        )
    }
}

/// Deterministic pseudo-random draw for one mob slot at one fixed tick.
///
/// SplitMix64-style finalizer with its own constants so mob decisions never
/// consume the terrain hash, a global thread RNG, or the wall clock
/// (D135/D013). Identical inputs always return the identical value, and
/// different slots at the same tick are independent draws.
pub fn mob_roll(seed: u32, tick_index: u64, slot: u32) -> u32 {
    let mut z = tick_index
        ^ (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (slot as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    z = z.wrapping_add(0xA076_1D64_78BD_642F);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z ^ (z >> 32)) as u32
}

/// What one [`MobRuntime::tick`] call actually executed.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MobTick {
    /// Number of fixed 20 Hz steps executed this call.
    pub ai_steps: u32,
}

/// Fixed-rate accumulator driving every mob at exactly [`MOB_TICK_HZ`],
/// independent of the render frame rate (D134).
pub struct MobRuntime {
    /// Fractional seconds carried between calls. `f64` keeps long runs of
    /// small frame deltas from drifting into a missed step.
    accum: f64,
    /// Number of fixed steps executed since construction.
    tick_index: u64,
    /// Deterministic decision seed; never the wall clock.
    seed: u32,
}

impl MobRuntime {
    pub fn new(seed: u32) -> MobRuntime {
        MobRuntime {
            accum: 0.0,
            tick_index: 0,
            seed,
        }
    }

    /// Fixed steps executed so far. Diagnostics and tests read this.
    #[inline]
    pub fn tick_index(&self) -> u64 {
        self.tick_index
    }

    /// Advance by `game_dt` seconds of already-clamped, already-scaled game
    /// time, running whole fixed steps.
    ///
    /// `is_resident` is consulted per mob per step. A non-resident mob keeps
    /// its record and its list slot but does not move, decay, or animate for
    /// that step (D131).
    pub fn tick(
        &mut self,
        game_dt: f32,
        mobs: &mut [Mob],
        is_resident: impl Fn(Vec3) -> bool,
    ) -> MobTick {
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return MobTick::default();
        }

        let tick_secs = MOB_TICK_SECS as f64;
        self.accum += game_dt as f64;

        // A tiny tolerance makes 50 x 0.01 s and 10 x 0.05 s both produce
        // exactly ten steps despite f32 rounding.
        let whole = ((self.accum / tick_secs) + 1e-6).floor();
        let requested = if whole.is_finite() && whole > 0.0 {
            whole as u64
        } else {
            0
        };
        let steps = requested.min(u64::from(MAX_MOB_STEPS_PER_CALL)) as u32;

        if steps == 0 {
            return MobTick::default();
        }

        for _ in 0..steps {
            self.tick_index += 1;
            for (index, mob) in mobs.iter_mut().enumerate() {
                if !is_resident(mob.position) {
                    continue;
                }
                self.step_mob(mob, index as u32);
            }
        }

        // Only the executed steps are consumed; a frame that wanted more is
        // clamped to at most one pending step so the backlog cannot spiral.
        self.accum = (self.accum - f64::from(steps) * tick_secs).max(0.0);
        self.accum = self.accum.min(tick_secs);

        MobTick { ai_steps: steps }
    }

    fn step_mob(&self, mob: &mut Mob, slot: u32) {
        mob.hurt = (mob.hurt - MOB_TICK_SECS).max(0.0);
        mob.cooldown = (mob.cooldown - MOB_TICK_SECS).max(0.0);

        if !mob.position.length_squared().is_finite() {
            // A corrupted record self-heals instead of poisoning every
            // downstream render/AABB consumer.
            mob.position = mob.home;
            mob.velocity = Vec3::ZERO;
        }

        if self.tick_index % YAW_REROLL_STEPS == 0 {
            let roll = mob_roll(self.seed, self.tick_index, slot);
            let unit = (roll & 0xFFFF) as f32 / 65_535.0;
            mob.yaw = unit * std::f32::consts::TAU;
            // One in eight rerolls rests; every other reroll (re)enters the
            // wander state.
            mob.state = if (roll >> 16) & 0x7 == 0 {
                MobAiState::Idle
            } else {
                MobAiState::Wander
            };
        }

        match mob.state {
            MobAiState::Idle => {
                mob.velocity = Vec3::ZERO;
            }
            MobAiState::Wander => {
                let offset = Vec3::new(
                    mob.position.x - mob.home.x,
                    0.0,
                    mob.position.z - mob.home.z,
                );
                let leash = offset.x * offset.x + offset.z * offset.z;
                if leash > HOME_LEASH * HOME_LEASH {
                    // Steer back toward the anchor instead of escaping.
                    mob.yaw = offset.x.atan2(offset.z) + std::f32::consts::PI;
                }

                let speed = mob.kind.walk_speed();
                mob.velocity = Vec3::new(mob.yaw.sin(), 0.0, mob.yaw.cos()) * speed;
                mob.position += mob.velocity * MOB_TICK_SECS;
                mob.walk_phase += speed * MOB_TICK_SECS * 4.0;

                if !mob.position.is_finite() {
                    mob.position = mob.home;
                    mob.velocity = Vec3::ZERO;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resident(_: Vec3) -> bool {
        true
    }

    #[test]
    fn kinds_have_stable_tags_and_consts() {
        for (i, kind) in MobKind::ALL.iter().copied().enumerate() {
            assert_eq!(kind.tag() as usize, i);
            assert_eq!(MobKind::from_tag(kind.tag()), Some(kind));
        }
        assert_eq!(MobKind::from_tag(4), None);
        assert_eq!(MobKind::Pig.max_health(), 10);
        assert_eq!(MobKind::Cow.max_health(), 10);
        assert_eq!(MobKind::Zombie.max_health(), 20);
        assert_eq!(MobKind::Skeleton.max_health(), 20);
        assert!((MobKind::Pig.width() - 0.7).abs() < 1e-6);
        assert!((MobKind::Cow.width() - 0.9).abs() < 1e-6);
        assert!((MobKind::Zombie.width() - 0.7).abs() < 1e-6);
        assert!((MobKind::Skeleton.width() - 0.7).abs() < 1e-6);
        assert!((MobKind::Pig.height() - 0.9).abs() < 1e-6);
        assert!((MobKind::Cow.height() - 1.3).abs() < 1e-6);
        assert!((MobKind::Zombie.height() - 1.8).abs() < 1e-6);
        assert!((MobKind::Skeleton.height() - 1.8).abs() < 1e-6);
        assert!((MobKind::Pig.walk_speed() - 1.0).abs() < 1e-6);
        assert!((MobKind::Cow.walk_speed() - 0.9).abs() < 1e-6);
        assert!((MobKind::Zombie.walk_speed() - 1.6).abs() < 1e-6);
        assert!((MobKind::Skeleton.walk_speed() - 1.7).abs() < 1e-6);
    }

    #[test]
    fn new_mob_anchors_health_and_state() {
        let pos = Vec3::new(3.5, 7.0, -2.5);
        let mob = Mob::new(MobKind::Cow, pos, 1.25);
        assert_eq!(mob.kind, MobKind::Cow);
        assert_eq!(mob.home, pos);
        assert_eq!(mob.position, pos);
        assert_eq!(mob.velocity, Vec3::ZERO);
        assert_eq!(mob.health, MobKind::Cow.max_health());
        assert_eq!(mob.state, MobAiState::Wander);
        assert_eq!(mob.hurt, 0.0);
        assert_eq!(mob.cooldown, 0.0);
        assert_eq!(mob.walk_phase, 0.0);
    }

    #[test]
    fn aabb_uses_width_and_height_around_the_feet() {
        let mob = Mob::new(MobKind::Cow, Vec3::new(1.0, 2.0, -3.0), 0.0);
        let (min, max) = mob.aabb();
        assert!((min.x - 0.55).abs() < 1e-6);
        assert!((min.z + 3.45).abs() < 1e-6);
        assert!((min.y - 2.0).abs() < 1e-6);
        assert!((max.x - 1.45).abs() < 1e-6);
        assert!((max.z + 2.55).abs() < 1e-6);
        assert!((max.y - 3.3).abs() < 1e-6);
    }

    #[test]
    fn mob_roll_is_deterministic_and_slot_independent() {
        assert_eq!(mob_roll(7, 42, 1), mob_roll(7, 42, 1));
        assert_ne!(mob_roll(7, 42, 1), mob_roll(7, 42, 2));
        assert_ne!(mob_roll(7, 42, 1), mob_roll(8, 42, 1));
        assert_ne!(mob_roll(7, 42, 1), mob_roll(7, 43, 1));
    }

    #[test]
    fn runtime_is_frame_rate_independent_at_20hz() {
        let mut a = MobRuntime::new(1);
        let mut b = MobRuntime::new(1);
        let mut mobs_a = vec![Mob::new(MobKind::Pig, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        let mut mobs_b = mobs_a.clone();

        for _ in 0..10 {
            assert_eq!(a.tick(0.05, &mut mobs_a, resident).ai_steps, 1);
        }
        let mut b_steps = 0;
        for _ in 0..50 {
            b_steps += b.tick(0.01, &mut mobs_b, resident).ai_steps;
        }
        assert_eq!(b_steps, 10, "50 small frames still run ten fixed steps");

        assert_eq!(a.tick_index(), 10);
        assert_eq!(b.tick_index(), 10);
        assert!((mobs_a[0].position - mobs_b[0].position).length() < 1e-3);
        assert!((mobs_a[0].walk_phase - mobs_b[0].walk_phase).abs() < 1e-3);
    }

    #[test]
    fn runtime_caps_steps_per_call_and_clamps_leftover() {
        let mut runtime = MobRuntime::new(3);
        let mut mobs = vec![Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        let report = runtime.tick(1.0, &mut mobs, resident);
        assert_eq!(report.ai_steps, MAX_MOB_STEPS_PER_CALL);
        assert_eq!(runtime.tick_index(), u64::from(MAX_MOB_STEPS_PER_CALL));
        // The backlog is clamped to one pending step, not 16.
        assert_eq!(runtime.tick(0.0, &mut mobs, resident).ai_steps, 0);
        assert_eq!(
            runtime.tick(0.05, &mut mobs, resident).ai_steps,
            2,
            "leftover accum is clamped to one step, plus the new 0.05 s"
        );
    }

    #[test]
    fn runtime_ignores_non_positive_and_non_finite_dt() {
        let mut runtime = MobRuntime::new(3);
        let mut mobs = vec![Mob::new(MobKind::Pig, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        assert_eq!(runtime.tick(0.0, &mut mobs, resident).ai_steps, 0);
        assert_eq!(runtime.tick(-1.0, &mut mobs, resident).ai_steps, 0);
        assert_eq!(runtime.tick(f32::NAN, &mut mobs, resident).ai_steps, 0);
        assert_eq!(runtime.tick_index(), 0);
    }

    #[test]
    fn non_resident_mob_keeps_slot_and_position() {
        let mut runtime = MobRuntime::new(9);
        let anchor = Vec3::new(0.5, 5.0, 0.5);
        let mut mobs = vec![
            Mob::new(MobKind::Pig, anchor, 0.0),
            Mob::new(MobKind::Cow, Vec3::new(100.5, 5.0, 0.5), 0.0),
        ];
        runtime.tick(0.05, &mut mobs, |pos| pos.x < 50.0);
        assert_eq!(mobs.len(), 2);
        assert_eq!(mobs[1].position, Vec3::new(100.5, 5.0, 0.5));
        assert_eq!(mobs[1].walk_phase, 0.0);
        assert_ne!(mobs[0].position, anchor);
        assert_eq!(mobs[0].home, anchor);
    }

    #[test]
    fn hurt_and_cooldown_decay_only_while_resident() {
        let mut runtime = MobRuntime::new(5);
        let mut mobs = vec![Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        mobs[0].hurt = 0.2;
        mobs[0].cooldown = 0.2;

        runtime.tick(0.05, &mut mobs, |_| false);
        assert!((mobs[0].hurt - 0.2).abs() < 1e-6);
        assert!((mobs[0].cooldown - 0.2).abs() < 1e-6);

        for _ in 0..4 {
            runtime.tick(0.05, &mut mobs, resident);
        }
        assert!(mobs[0].hurt <= 1e-5);
        assert!(mobs[0].cooldown <= 1e-5);
    }

    #[test]
    fn mob_returns_home_when_it_wanders_past_the_leash() {
        let mut runtime = MobRuntime::new(11);
        let home = Vec3::new(0.5, 5.0, 0.5);
        let mut mobs = vec![Mob::new(MobKind::Skeleton, home, 0.0)];
        // Straight +Z for many steps should trip the 4-block leash.
        for _ in 0..200 {
            runtime.tick(0.05, &mut mobs, resident);
            mobs[0].state = MobAiState::Wander;
        }
        let dx = mobs[0].position.x - home.x;
        let dz = mobs[0].position.z - home.z;
        assert!(
            (dx * dx + dz * dz).sqrt() < 8.0,
            "mob must be steered back toward home, got {:?}",
            mobs[0].position
        );
        assert!(mobs[0].position.is_finite());
    }

    #[test]
    fn positions_stay_finite_over_many_steps() {
        let mut runtime = MobRuntime::new(0xDEAD_BEEF);
        let mut mobs = vec![
            Mob::new(MobKind::Pig, Vec3::ZERO, 0.0),
            Mob::new(MobKind::Cow, Vec3::new(1.0, 0.0, 1.0), 1.0),
            Mob::new(MobKind::Zombie, Vec3::new(-1.0, 0.0, -1.0), 2.0),
            Mob::new(MobKind::Skeleton, Vec3::new(2.0, 0.0, -2.0), 3.0),
        ];
        for _ in 0..100 {
            runtime.tick(0.05, &mut mobs, resident);
        }
        for mob in &mobs {
            assert!(mob.position.is_finite(), "position became non-finite");
            assert!(mob.velocity.is_finite());
            assert!(mob.walk_phase.is_finite());
        }
    }
}
