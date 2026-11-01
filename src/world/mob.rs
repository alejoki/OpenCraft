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

use crate::world::mob_ai::{MobAction, MobAttack, MobTarget, MobWorld};

/// Fixed simulation rate of every mob AI step (D134).
pub const MOB_TICK_HZ: u32 = 20;
/// Seconds per fixed mob step.
pub const MOB_TICK_SECS: f32 = 1.0 / MOB_TICK_HZ as f32;
/// A single frame may never run more than this many fixed steps, so a long
/// stall cannot spiral into an unbounded catch-up loop.
pub const MAX_MOB_STEPS_PER_CALL: u32 = 4;

/// Seconds the hurt flash stays at full intensity after a hit.
///
/// [`Mob::hurt`] is both the countdown and the render intensity, so there is
/// no second flash field to keep in sync.
pub const MOB_HURT_FLASH_SECS: f32 = 0.5;
/// Seconds a mob ignores further hits after one lands (i-frames).
pub const MOB_IFRAME_SECS: f32 = 0.5;
/// Initial horizontal speed of the knockback impulse, in blocks/second.
pub const MOB_KNOCKBACK_SPEED: f32 = 4.0;
/// Per-fixed-step multiplier applied to the remaining knockback impulse.
pub const MOB_KNOCKBACK_DECAY: f32 = 0.6;
/// Squared speed below which the impulse is dropped to exactly zero, so a mob
/// provably comes to rest instead of creeping forever.
pub const MOB_KNOCKBACK_EPSILON_SQ: f32 = 1e-4;

/// Seconds a passive mob keeps fleeing after a non-lethal hit.
///
/// Long enough to survive several 16-step yaw-reroll boundaries (0.8 s each)
/// and to read as a deliberate response rather than a one-step stumble.
pub const MOB_FLEE_SECS: f32 = 2.5;
/// Flee speed multiplier over [`MobKind::walk_speed`].
///
/// A fleeing mob moves faster than its wander pace without becoming
/// impossible to catch on foot.
pub const MOB_FLEE_SPEED_FACTOR: f32 = 1.6;
/// Seconds a lethal hit spends collapsing the box model before the session
/// reaper removes the record.
pub const MOB_DEATH_SECS: f32 = 0.8;

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

    /// Whether a non-lethal hit makes this kind flee (Pig and Cow).
    pub const fn is_passive(self) -> bool {
        matches!(self, MobKind::Pig | MobKind::Cow)
    }

    /// Whether this kind is hostile (Zombie and Skeleton).
    ///
    /// Hostile aggression is S06's concern; a hit never changes their state or
    /// yaw in S04.
    pub const fn is_hostile(self) -> bool {
        matches!(self, MobKind::Zombie | MobKind::Skeleton)
    }

    /// Melee profile for this kind, or `None` when it has no melee attack.
    ///
    /// Only the Zombie melees in S06. Skeleton melee must never exist: its
    /// ranged bow lives in [`MobKind::ranged_profile`], so returning `None`
    /// here keeps a skeleton from ever swinging and keeps the combat demo's
    /// melee damage sources exactly one.
    pub const fn attack_profile(self) -> Option<MobAttackProfile> {
        match self {
            MobKind::Zombie => Some(MobAttackProfile {
                damage: 3,
                range: 2.0,
                cooldown_secs: 1.0,
            }),
            MobKind::Pig | MobKind::Cow | MobKind::Skeleton => None,
        }
    }

    /// Stand-off ranged profile for this kind, or `None` when it has no bow.
    ///
    /// Only the Skeleton shoots (S08). Between `min_range` (5.0) and
    /// `fire_range` (12.0) blocks it stands still and fires one
    /// gravity-compensated arrow every 2.0 s; closer than `min_range` it backs
    /// away with the same one-block step mover, and beyond `fire_range` it
    /// closes the distance. It never melees: [`MobKind::attack_profile`] stays
    /// `None` for it.
    pub const fn ranged_profile(self) -> Option<MobRangedProfile> {
        match self {
            MobKind::Skeleton => Some(MobRangedProfile {
                fire_range: 12.0,
                min_range: 5.0,
                cooldown_secs: 2.0,
            }),
            MobKind::Pig | MobKind::Cow | MobKind::Zombie => None,
        }
    }
}

/// One melee attack shape. Kept in the mob domain so the AI stays kind-agnostic
/// and the session can look up damage without matching on [`MobKind`] again.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MobAttackProfile {
    /// Damage applied per landed hit.
    pub damage: u16,
    /// Maximum horizontal distance at which the attack can land, in blocks.
    pub range: f32,
    /// Seconds between landed hits (`Mob::cooldown` is armed to this value).
    pub cooldown_secs: f32,
}

/// One stand-off ranged shape. The mirror of [`MobAttackProfile`] for a kind
/// that shoots instead of swinging.
///
/// The mob domain owns the numbers so the hostile brain stays kind-agnostic;
/// the brain only asks "does this kind have a bow?" and reads the three
/// distances. A skeleton is the only ranged kind today.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MobRangedProfile {
    /// Maximum horizontal distance at which a shot is taken, in blocks.
    ///
    /// Beyond this the mob closes the distance first.
    pub fire_range: f32,
    /// Minimum horizontal distance at which a shot is taken, in blocks.
    ///
    /// Inside this the mob backs away instead of firing (and never stands
    /// inside the player's body).
    pub min_range: f32,
    /// Seconds between shots (`Mob::cooldown` is armed to this value).
    pub cooldown_secs: f32,
}

/// Mob AI states. S04 adds [`MobAiState::Flee`] for passive mobs; S06 adds
/// [`MobAiState::Chase`] for hostile pursuit.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MobAiState {
    /// Standing still; `walk_phase` does not advance.
    Idle,
    /// Walking along `yaw` at [`MobKind::walk_speed`].
    Wander,
    /// Moving away from the attacker for the flee window, ignoring the home
    /// leash.
    Flee,
    /// Hostile pursuit (S06): steering toward a player that has line of sight
    /// inside the aggro range.
    ///
    /// Deliberately ephemeral: [`crate::world::persistence::SavedMob`] carries
    /// no state, so a reload always starts every mob in `Wander` and a
    /// hostile re-aggros from line of sight once more.
    Chase,
}

/// One live mob record. `home` is the spawn anchor used for the leash and
/// reused by S05's spawn/bed rules.
#[derive(Clone, Debug, PartialEq)]
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
    /// Remaining hit-invulnerability time in seconds (i-frames).
    ///
    /// Deliberately separate from [`Mob::cooldown`]: S06 needs `cooldown` as
    /// the hostile attack cadence, and a player hit must never delay it.
    pub hurt_iframe: f32,
    /// Remaining horizontal knockback impulse in blocks/second.
    ///
    /// Integrated on top of the AI velocity by [`MobRuntime::step_mob`] and
    /// decayed geometrically; never written into [`Mob::velocity`], which both
    /// AI arms assign every fixed step.
    pub knockback: Vec3,
    /// Generic action cooldown in seconds; S02+ uses it.
    pub cooldown: f32,
    /// Walk cycle phase (radians), advanced while wandering.
    pub walk_phase: f32,
    /// Remaining flee time in seconds; decays only while resident.
    pub flee: f32,
    /// Remaining death-collapse time in seconds; decays only while resident.
    /// Zero on a living mob and on a corpse whose collapse has finished.
    pub death: f32,
    /// Set by the spawner when the record is beyond the despawn radius; the
    /// session's single reap site removes it on the same tick, so a despawned
    /// mob is never rendered as a corpse and never saved.
    ///
    /// Ephemeral: never persisted ([`crate::world::persistence::SavedMob`] has
    /// no such field) and always initialised to `false`, including in
    /// [`Mob::restored`], so a reload can never carry a stale marker.
    pub despawn_pending: bool,
    /// Remaining chase memory in seconds. S06's hostile steering refreshes it
    /// while line of sight holds and drops the mob back to `Idle` when it
    /// drains. Ephemeral: never persisted.
    pub aggro: f32,
    /// Seconds spent blocked while chasing. Reaching
    /// [`crate::world::mob_ai::MOB_STUCK_TIMEOUT_SECS`] triggers the stuck
    /// fallback. Ephemeral: never persisted.
    pub stuck: f32,
    /// Last anchored feet position used to measure net chase progress.
    /// Ephemeral: never persisted.
    pub stuck_anchor: Vec3,
    /// Remaining re-aggro cooldown after the stuck fallback, in seconds.
    /// Ephemeral: never persisted.
    pub reaggro: f32,
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
            hurt_iframe: 0.0,
            knockback: Vec3::ZERO,
            cooldown: 0.0,
            walk_phase: 0.0,
            flee: 0.0,
            death: 0.0,
            despawn_pending: false,
            aggro: 0.0,
            stuck: 0.0,
            stuck_anchor: position,
            reaggro: 0.0,
        }
    }

    /// A mob restored from a save record: anchors `home` at the persisted
    /// position and takes the persisted health verbatim. Never routed through
    /// [`Mob::new`], which forces the kind's maximum health and would erase
    /// damage across a reload.
    pub fn restored(kind: MobKind, position: Vec3, yaw: f32, health: u16) -> Mob {
        Mob {
            kind,
            home: position,
            position,
            velocity: Vec3::ZERO,
            yaw,
            health,
            state: MobAiState::Wander,
            hurt: 0.0,
            hurt_iframe: 0.0,
            knockback: Vec3::ZERO,
            cooldown: 0.0,
            walk_phase: 0.0,
            flee: 0.0,
            death: 0.0,
            despawn_pending: false,
            aggro: 0.0,
            stuck: 0.0,
            stuck_anchor: position,
            reaggro: 0.0,
        }
    }

    /// Clears every ephemeral hostile-chase bookkeeping field.
    ///
    /// Used by respawn (T02): a respawned player must not be chased by a mob
    /// that still remembers the old position. Health, position, yaw, and the
    /// attack cooldown are deliberately untouched, so the body and its cadence
    /// survive. Dropping `Chase` to `Idle` also zeroes the AI velocity.
    pub fn reset_hostile_tracking(&mut self) {
        self.aggro = 0.0;
        self.stuck = 0.0;
        self.reaggro = 0.0;
        self.stuck_anchor = self.position;
        if self.state == MobAiState::Chase {
            self.state = MobAiState::Idle;
        }
        self.velocity = Vec3::ZERO;
    }

    /// Applies one melee hit: saturated damage, hurt flash, i-frames, and a
    /// horizontal knockback impulse away from `away_from`.
    ///
    /// Returns `None` — and mutates nothing at all — when the record is
    /// already dead (`health == 0`), when the i-frame window is open
    /// ([`Mob::hurt_iframe`] `> 0.0`), or when `damage == 0`. `health` is
    /// unsigned and `0` is a legal wire value, so the subtraction saturates:
    /// no negative or wrapped health can ever be stored.
    ///
    /// The knockback direction is the horizontal vector from `away_from` to
    /// the mob. A degenerate or non-finite direction (attacker inside the mob,
    /// corrupted positions) falls back to the mob's own yaw forward, then to
    /// +Z, so the impulse is always finite and non-zero.
    pub fn take_hit(&mut self, damage: u16, away_from: Vec3) -> Option<MobHit> {
        if self.health == 0 || self.hurt_iframe > 0.0 || damage == 0 {
            return None;
        }

        let health_after = self.health.saturating_sub(damage);
        self.health = health_after;
        self.hurt = MOB_HURT_FLASH_SECS;
        self.hurt_iframe = MOB_IFRAME_SECS;

        let delta = self.position - away_from;
        let horizontal = Vec3::new(delta.x, 0.0, delta.z);
        let mut dir = horizontal.normalize_or_zero();
        if dir.length_squared() < 1e-6 {
            // Deterministic degenerate fallback: the mob's own facing.
            dir = Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos());
        }
        if !dir.is_finite() || dir.length_squared() < 1e-6 {
            // A corrupted yaw must still not produce a zero or NaN impulse.
            dir = Vec3::Z;
        }
        self.knockback = dir * MOB_KNOCKBACK_SPEED;

        if health_after == 0 {
            // A lethal hit starts the collapse: the body stops walking and
            // the reaper removes the record once `death` drains. The away
            // knockback and hurt flash above still apply.
            self.death = MOB_DEATH_SECS;
            self.flee = 0.0;
            self.velocity = Vec3::ZERO;
        } else if self.kind.is_passive() {
            // Passive mobs turn and run along the same away direction used
            // for knockback. Hostile kinds keep their state/yaw untouched.
            self.state = MobAiState::Flee;
            self.flee = MOB_FLEE_SECS;
            self.yaw = dir.x.atan2(dir.z);
        }

        Some(MobHit {
            damage,
            health_after,
            killed: health_after == 0,
        })
    }

    /// Kills the mob for an environmental cause (S05 burning).
    ///
    /// Returns `false` and mutates nothing when the record is already dead.
    /// Otherwise health drops to zero, the collapse is armed, and every
    /// impulse is cleared so a corpse cannot keep sliding.
    pub fn kill(&mut self) -> bool {
        if self.health == 0 {
            return false;
        }
        self.health = 0;
        self.death = MOB_DEATH_SECS;
        self.flee = 0.0;
        self.velocity = Vec3::ZERO;
        self.knockback = Vec3::ZERO;
        true
    }

    /// Whether health has reached zero (collapsing or already finished).
    #[inline]
    pub fn is_dead(&self) -> bool {
        self.health == 0
    }

    /// Whether this record's collapse timer has fully drained.
    ///
    /// A restored health-0 record arrives with `death == 0`, so it is already
    /// finished for both rendering and reaping.
    #[inline]
    pub fn death_finished(&self) -> bool {
        self.is_dead() && self.death <= 0.0
    }

    /// Collapse progress in `0..=1` for rendering.
    ///
    /// Exactly `0.0` while alive, `1.0` for a dead record whose timer reached
    /// zero, and clamped so corrupted timers cannot escape the range.
    pub fn death_progress(&self) -> f32 {
        if !self.is_dead() {
            return 0.0;
        }
        if self.death <= 0.0 {
            return 1.0;
        }
        (1.0 - self.death / MOB_DEATH_SECS).clamp(0.0, 1.0)
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

/// Outcome of one applied [`Mob::take_hit`].
///
/// A killed mob is *reported*, never removed: S04 owns death collapse,
/// removal, and drops, so the record stays in the list with `health == 0`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MobHit {
    /// Damage actually applied (equal to the requested damage).
    pub damage: u16,
    /// Health after the saturating subtraction.
    pub health_after: u16,
    /// Whether this hit dropped health to zero.
    pub killed: bool,
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

/// One ranged shot decision emitted by the hostile brain.
///
/// Geometry only: *where* the arrow starts and *which way* it points, plus the
/// shooter's kind. Speed, damage and gravity stay in the projectile domain
/// ([`crate::world::arrow::Arrow::skeleton`]) so there is exactly one
/// implementation of arrow ballistics.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MobShot {
    /// The shooter's kind.
    pub kind: MobKind,
    /// Arrow launch position: the shooter's eye, the same rule line of sight
    /// uses, so a shot leaves from a cell the brain proved open.
    pub origin: Vec3,
    /// Aim direction; deliberately not normalized — the arrow constructor
    /// normalizes it.
    pub dir: Vec3,
}

/// What one [`MobRuntime::tick`] call actually executed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MobTick {
    /// Number of fixed 20 Hz steps executed this call.
    pub ai_steps: u32,
    /// Melee attacks decided by the same fixed steps that moved the mobs,
    /// emitted in mob-list order.
    ///
    /// The session is the only consumer: it applies them through its single
    /// damage seam (S06-T04), so the world layer never names a player type.
    pub attacks: Vec<MobAttack>,
    /// Ranged shot decisions from the same fixed steps, emitted in mob-list
    /// order.
    ///
    /// Geometry only (see [`MobShot`]): the session turns each one into a real
    /// [`crate::world::arrow::Arrow::skeleton`] projectile through its shared
    /// spawn seam, so the AI never owns ballistics or damage.
    pub shots: Vec<MobShot>,
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
    ///
    /// `target` is the single player pose authority for hostile steering:
    /// `None` (Creative, the closed `--demo-combat` gate, or a dead player)
    /// means no hostile can acquire, chase, or attack. `world` is the one
    /// capability the steering brain needs (three integers in, one block id
    /// out). Attacks are collected in mob-list order across the executed
    /// steps and returned in [`MobTick::attacks`].
    pub fn tick(
        &mut self,
        game_dt: f32,
        mobs: &mut [Mob],
        is_resident: impl Fn(Vec3) -> bool,
        target: Option<MobTarget>,
        world: &impl MobWorld,
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

        let mut attacks = Vec::new();
        let mut shots = Vec::new();
        for _ in 0..steps {
            self.tick_index += 1;
            for (index, mob) in mobs.iter_mut().enumerate() {
                if !is_resident(mob.position) {
                    continue;
                }
                self.step_mob(mob, index as u32, target, world, &mut attacks, &mut shots);
            }
        }

        // Only the executed steps are consumed; a frame that wanted more is
        // clamped to at most one pending step so the backlog cannot spiral.
        self.accum = (self.accum - f64::from(steps) * tick_secs).max(0.0);
        self.accum = self.accum.min(tick_secs);

        MobTick {
            ai_steps: steps,
            attacks,
            shots,
        }
    }

    fn step_mob(
        &self,
        mob: &mut Mob,
        slot: u32,
        target: Option<MobTarget>,
        world: &impl MobWorld,
        attacks: &mut Vec<MobAttack>,
        shots: &mut Vec<MobShot>,
    ) {
        mob.hurt = (mob.hurt - MOB_TICK_SECS).max(0.0);
        mob.hurt_iframe = (mob.hurt_iframe - MOB_TICK_SECS).max(0.0);
        mob.cooldown = (mob.cooldown - MOB_TICK_SECS).max(0.0);
        mob.flee = (mob.flee - MOB_TICK_SECS).max(0.0);
        mob.death = (mob.death - MOB_TICK_SECS).max(0.0);

        if mob.flee <= 0.0 && mob.state == MobAiState::Flee {
            mob.state = MobAiState::Wander;
        }

        if !mob.position.length_squared().is_finite() {
            // A corrupted record self-heals instead of poisoning every
            // downstream render/AABB consumer.
            mob.position = mob.home;
            mob.velocity = Vec3::ZERO;
        }

        // A fleeing, chasing, or dying mob keeps the heading it was given; the
        // reroll only owns unmolested wanderers. Without the Chase gate a
        // hostile would be re-yawed and reset to Idle/Wander every 0.8 s
        // (MEM419).
        if mob.health > 0
            && mob.flee <= 0.0
            && mob.state != MobAiState::Chase
            && self.tick_index.is_multiple_of(YAW_REROLL_STEPS)
        {
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

        if mob.health == 0 {
            // Dying mobs never walk again; only the knockback integration
            // below may move the body while it collapses.
            mob.velocity = Vec3::ZERO;
        } else {
            // Hostiles run the world-aware brain on every step. It owns chase
            // movement and the melee decision, never moves a passive, and
            // never touches a dead mob (the branch above excludes those).
            //
            // With `target == None` (Creative, the closed `--demo-combat`
            // gate, a dead player) or with no line of sight, `step_hostile`
            // returns without moving the mob. The passive-style arms below
            // then still give an idle/wandering hostile its pre-S06 wander,
            // which keeps the pinned demo quartet walking while the demo gate
            // is closed. Once `step_hostile` puts a hostile into `Chase`, it
            // owns movement for that step and the arms are skipped.
            let hostile_chasing = if mob.kind.is_hostile() {
                match crate::world::mob_ai::step_hostile(
                    mob,
                    target,
                    world,
                    mob_roll(self.seed, self.tick_index, slot),
                ) {
                    Some(MobAction::Melee(attack)) => attacks.push(attack),
                    Some(MobAction::Shot(shot)) => shots.push(shot),
                    None => {}
                }
                mob.state == MobAiState::Chase
            } else {
                false
            };

            if !hostile_chasing {
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
                    MobAiState::Chase => {
                        // Only a manually-set passive can reach this arm:
                        // `step_hostile` owns every hostile chase and nothing
                        // in production puts a passive in `Chase`.
                        mob.velocity = Vec3::ZERO;
                    }
                    MobAiState::Flee => {
                        // Flee ignores the home leash: the whole point is to
                        // put distance between the mob and its attacker.
                        let speed = mob.kind.walk_speed() * MOB_FLEE_SPEED_FACTOR;
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

        // Knockback is integrated *after* the AI arms because both of them
        // assign `velocity`; writing the impulse there would be erased on the
        // next fixed step. Keeping it in its own field means an Idle mob still
        // slides, and the fixed 20 Hz cadence makes the slide deterministic.
        if !mob.knockback.is_finite() {
            mob.knockback = Vec3::ZERO;
        }
        if mob.knockback.length_squared() > 0.0 {
            mob.position += mob.knockback * MOB_TICK_SECS;
            mob.knockback *= MOB_KNOCKBACK_DECAY;
            if mob.knockback.length_squared() < MOB_KNOCKBACK_EPSILON_SQ {
                mob.knockback = Vec3::ZERO;
            }
            if !mob.position.is_finite() {
                mob.position = mob.home;
                mob.knockback = Vec3::ZERO;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    /// A world with no solid blocks: LOS is always clear and every step is
    /// passable. These runtime tests exercise timing and passive arms, not
    /// world-aware steering (which has its own suite in `mob_ai`).
    struct NoWorld;

    impl MobWorld for NoWorld {
        fn block(&self, _x: i32, _y: i32, _z: i32) -> BlockId {
            BlockId::Air
        }
    }

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
    fn ranged_profiles_are_skeleton_only_and_never_imply_melee() {
        for kind in MobKind::ALL {
            let ranged = kind.ranged_profile();
            assert_eq!(
                ranged.is_some(),
                kind == MobKind::Skeleton,
                "only the Skeleton carries a bow ({kind:?})"
            );
            if let Some(profile) = ranged {
                assert!(profile.min_range < profile.fire_range);
                assert!(profile.cooldown_secs > 0.0);
                assert_eq!(
                    kind.attack_profile(),
                    None,
                    "a ranged kind must never also melee"
                );
            }
        }
        assert_eq!(
            MobKind::Skeleton.ranged_profile(),
            Some(MobRangedProfile {
                fire_range: 12.0,
                min_range: 5.0,
                cooldown_secs: 2.0,
            })
        );
        assert_eq!(MobKind::Zombie.ranged_profile(), None);
        assert_eq!(MobKind::Pig.ranged_profile(), None);
        assert_eq!(MobKind::Cow.ranged_profile(), None);
    }

    #[test]
    fn runtime_collects_skeleton_shots_on_the_two_second_cadence() {
        let mut runtime = MobRuntime::new(17);
        let mut mobs = vec![Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        let target = MobTarget {
            feet: Vec3::new(0.5, 5.0, 8.5),
            eye: Vec3::new(0.5, 6.62, 8.5),
        };

        let mut shot_steps = Vec::new();
        for step in 1..=81u32 {
            let report = runtime.tick(0.05, &mut mobs, resident, Some(target), &NoWorld);
            assert_eq!(report.ai_steps, 1);
            assert!(
                report.attacks.is_empty(),
                "a skeleton never melees through the runtime"
            );
            if !report.shots.is_empty() {
                assert_eq!(report.shots.len(), 1, "one shot per fixed step at most");
                let shot = report.shots[0];
                assert_eq!(shot.kind, MobKind::Skeleton);
                assert!(shot.origin.is_finite() && shot.dir.is_finite());
                assert!(shot.dir.length_squared() > 0.0);
                shot_steps.push(step);
            }
            assert_eq!(
                mobs[0].position,
                Vec3::new(0.5, 5.0, 0.5),
                "a firing skeleton stands still"
            );
        }

        assert_eq!(
            shot_steps,
            vec![1, 41, 81],
            "one shot per 2.0 s, i.e. every 40 fixed 20 Hz steps"
        );
    }

    #[test]
    fn runtime_reports_melee_and_shots_side_by_side_in_mob_list_order() {
        let mut runtime = MobRuntime::new(19);
        let mut mobs = vec![
            Mob::new(MobKind::Zombie, Vec3::new(2.5, 5.0, 9.0), 0.0),
            Mob::new(MobKind::Skeleton, Vec3::new(2.5, 5.0, 2.5), 0.0),
        ];
        let target = MobTarget {
            feet: Vec3::new(2.5, 5.0, 10.5),
            eye: Vec3::new(2.5, 6.62, 10.5),
        };

        let report = runtime.tick(0.05, &mut mobs, resident, Some(target), &NoWorld);
        assert_eq!(report.ai_steps, 1);
        assert_eq!(report.attacks.len(), 1, "the adjacent zombie melees");
        assert_eq!(report.attacks[0].kind, MobKind::Zombie);
        assert_eq!(report.shots.len(), 1, "the stand-off skeleton shoots");
        assert_eq!(report.shots[0].kind, MobKind::Skeleton);

        // A default report carries neither vector: an idle frame emits nothing.
        let idle = MobTick::default();
        assert!(idle.attacks.is_empty() && idle.shots.is_empty());
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
        assert_eq!(mob.hurt_iframe, 0.0);
        assert_eq!(mob.knockback, Vec3::ZERO);
        assert_eq!(mob.cooldown, 0.0);
        assert_eq!(mob.walk_phase, 0.0);
        assert!(!mob.despawn_pending, "a fresh mob is never pre-culled");
    }

    #[test]
    fn restored_keeps_saved_health_and_resets_ephemeral_fields() {
        let pos = Vec3::new(-7.0, 70.5, 2.0);
        let mob = Mob::restored(MobKind::Skeleton, pos, -1.25, 3);
        assert_eq!(mob.kind, MobKind::Skeleton);
        assert_eq!(
            mob.home, pos,
            "home re-anchors to the persisted feet position"
        );
        assert_eq!(mob.position, pos);
        assert_eq!(mob.velocity, Vec3::ZERO);
        assert_eq!(mob.yaw, -1.25);
        assert_eq!(mob.health, 3, "damage must survive a reload");
        assert_eq!(mob.state, MobAiState::Wander);
        assert_eq!(mob.hurt, 0.0);
        assert_eq!(mob.hurt_iframe, 0.0);
        assert_eq!(mob.knockback, Vec3::ZERO);
        assert_eq!(mob.cooldown, 0.0);
        assert_eq!(mob.walk_phase, 0.0);
        assert!(
            !mob.despawn_pending,
            "a reload must never carry a stale despawn marker"
        );

        // `Mob::new` would force max health; `restored` must not.
        assert!(mob.health < MobKind::Skeleton.max_health());
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
            assert_eq!(
                a.tick(0.05, &mut mobs_a, resident, None, &NoWorld).ai_steps,
                1
            );
        }
        let mut b_steps = 0;
        for _ in 0..50 {
            b_steps += b.tick(0.01, &mut mobs_b, resident, None, &NoWorld).ai_steps;
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
        let report = runtime.tick(1.0, &mut mobs, resident, None, &NoWorld);
        assert_eq!(report.ai_steps, MAX_MOB_STEPS_PER_CALL);
        assert_eq!(runtime.tick_index(), u64::from(MAX_MOB_STEPS_PER_CALL));
        // The backlog is clamped to one pending step, not 16.
        assert_eq!(
            runtime
                .tick(0.0, &mut mobs, resident, None, &NoWorld)
                .ai_steps,
            0
        );
        assert_eq!(
            runtime
                .tick(0.05, &mut mobs, resident, None, &NoWorld)
                .ai_steps,
            2,
            "leftover accum is clamped to one step, plus the new 0.05 s"
        );
    }

    #[test]
    fn runtime_ignores_non_positive_and_non_finite_dt() {
        let mut runtime = MobRuntime::new(3);
        let mut mobs = vec![Mob::new(MobKind::Pig, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        assert_eq!(
            runtime
                .tick(0.0, &mut mobs, resident, None, &NoWorld)
                .ai_steps,
            0
        );
        assert_eq!(
            runtime
                .tick(-1.0, &mut mobs, resident, None, &NoWorld)
                .ai_steps,
            0
        );
        assert_eq!(
            runtime
                .tick(f32::NAN, &mut mobs, resident, None, &NoWorld)
                .ai_steps,
            0
        );
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
        runtime.tick(0.05, &mut mobs, |pos| pos.x < 50.0, None, &NoWorld);
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

        runtime.tick(0.05, &mut mobs, |_| false, None, &NoWorld);
        assert!((mobs[0].hurt - 0.2).abs() < 1e-6);
        assert!((mobs[0].cooldown - 0.2).abs() < 1e-6);

        for _ in 0..4 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
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
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
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
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
        }
        for mob in &mobs {
            assert!(mob.position.is_finite(), "position became non-finite");
            assert!(mob.velocity.is_finite());
            assert!(mob.walk_phase.is_finite());
            assert!(mob.knockback.is_finite());
        }
    }

    // ---- take_hit: damage, i-frames, flash, knockback -------------------

    /// A fresh Pig at +Z of an attacker standing at -Z, so "away" is +Z.
    fn pig_at_origin() -> Mob {
        Mob::new(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 0.0)
    }

    fn attacker_behind() -> Vec3 {
        Vec3::new(0.0, 5.0, -2.0)
    }

    #[test]
    fn take_hit_arithmetic_is_ten_to_six_to_two() {
        let mut mob = pig_at_origin();
        let first = mob.take_hit(4, attacker_behind()).expect("first hit lands");
        assert_eq!(
            first,
            MobHit {
                damage: 4,
                health_after: 6,
                killed: false
            }
        );
        assert_eq!(mob.health, 6);
        assert_eq!(mob.hurt, MOB_HURT_FLASH_SECS, "the flash is armed");
        assert_eq!(mob.hurt_iframe, MOB_IFRAME_SECS, "i-frames are armed");
        assert!((mob.knockback.length() - MOB_KNOCKBACK_SPEED).abs() < 1e-5);
        assert!(
            mob.knockback.z > 0.0,
            "knockback points away from the attacker"
        );
        assert!((mob.knockback.x).abs() < 1e-6 && (mob.knockback.y).abs() < 1e-6);

        // Elapse the window and hit again: 6 -> 2, still alive.
        mob.hurt_iframe = 0.0;
        let second = mob
            .take_hit(4, attacker_behind())
            .expect("second hit lands");
        assert_eq!(
            second,
            MobHit {
                damage: 4,
                health_after: 2,
                killed: false
            }
        );
        assert_eq!(mob.health, 2);
    }

    #[test]
    fn second_hit_inside_iframes_is_none_and_mutates_nothing() {
        let mut mob = pig_at_origin();
        mob.take_hit(4, attacker_behind()).expect("first hit lands");
        let snapshot = mob.clone();
        assert_eq!(mob.take_hit(4, attacker_behind()), None);
        assert_eq!(
            mob, snapshot,
            "a blocked hit must leave every field identical"
        );
        assert_eq!(mob.health, 6);
        // A larger hit cannot sneak through the window either.
        assert_eq!(mob.take_hit(200, attacker_behind()), None);
        assert_eq!(mob, snapshot);
    }

    #[test]
    fn take_hit_saturates_at_zero_and_reports_a_kill() {
        let mut mob = pig_at_origin();
        let hit = mob
            .take_hit(200, attacker_behind())
            .expect("a lethal hit lands");
        assert_eq!(hit.health_after, 0);
        assert!(hit.killed);
        assert_eq!(hit.damage, 200);
        assert_eq!(mob.health, 0, "health must saturate, never wrap");

        // A dead record refuses every further hit and stays at zero.
        mob.hurt_iframe = 0.0;
        assert_eq!(mob.take_hit(1, attacker_behind()), None);
        assert_eq!(mob.health, 0);
        assert!(mob.take_hit(1, attacker_behind()).is_none());

        // An exactly-lethal hit also reports a kill.
        let mut exact = pig_at_origin();
        let hit = exact.take_hit(10, attacker_behind()).expect("lethal hit");
        assert!(hit.killed);
        assert_eq!(hit.health_after, 0);

        // A record restored at zero health is dead on arrival.
        let mut dead = Mob::restored(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 0.0, 0);
        assert_eq!(dead.take_hit(1, attacker_behind()), None);
        assert_eq!(dead.health, 0);
    }

    #[test]
    fn zero_damage_is_a_complete_no_op() {
        let mut mob = pig_at_origin();
        let snapshot = mob.clone();
        assert_eq!(mob.take_hit(0, attacker_behind()), None);
        assert_eq!(mob, snapshot, "zero damage must not arm anything");
        assert_eq!(mob.health, MobKind::Pig.max_health());
        assert_eq!(mob.hurt, 0.0);
        assert_eq!(mob.hurt_iframe, 0.0);
        assert_eq!(mob.knockback, Vec3::ZERO);
    }

    #[test]
    fn knockback_direction_is_horizontal_and_away_from_the_attacker() {
        let mut mob = Mob::new(MobKind::Cow, Vec3::new(3.0, 5.0, 4.0), 0.0);
        // Attacker has a vertical offset: the impulse must stay horizontal.
        let hit = mob
            .take_hit(1, Vec3::new(3.0, 70.0, 1.0))
            .expect("hit lands");
        assert!(!hit.killed);
        assert!(mob.knockback.y.abs() < 1e-6, "knockback must be horizontal");
        assert!(mob.knockback.z > 0.0, "away from the attacker is +Z here");
        assert!((mob.knockback.length() - MOB_KNOCKBACK_SPEED).abs() < 1e-5);

        // Attacker on the opposite side flips the direction.
        mob.hurt_iframe = 0.0;
        mob.take_hit(1, Vec3::new(3.0, 5.0, 9.0))
            .expect("second hit lands");
        assert!(mob.knockback.z < 0.0);
    }

    #[test]
    fn degenerate_knockback_direction_uses_the_mob_yaw_and_stays_finite() {
        // Attacker exactly at the mob's feet: no horizontal direction exists.
        let mut mob = Mob::new(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 0.0);
        assert!(mob.take_hit(1, Vec3::new(0.0, 5.0, 0.0)).is_some());
        assert!(mob.knockback.is_finite());
        assert!(
            mob.knockback.length() > 0.0,
            "the impulse must never be zero"
        );
        assert!(mob.knockback.z > 0.0, "yaw 0 faces +Z");

        // Yaw 90 degrees: the fallback faces +X.
        let mut side = Mob::new(
            MobKind::Pig,
            Vec3::new(0.0, 5.0, 0.0),
            std::f32::consts::FRAC_PI_2,
        );
        assert!(side.take_hit(1, Vec3::new(0.0, 5.0, 0.0)).is_some());
        assert!(side.knockback.x > 0.0);
        assert!((side.knockback.length() - MOB_KNOCKBACK_SPEED).abs() < 1e-5);

        // A corrupted (non-finite) attacker position still yields a finite,
        // non-zero impulse.
        let mut corrupt = Mob::new(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), f32::NAN);
        assert!(corrupt.take_hit(1, Vec3::new(f32::NAN, 5.0, 0.0)).is_some());
        assert!(corrupt.knockback.is_finite());
        assert_eq!(corrupt.knockback, Vec3::Z * MOB_KNOCKBACK_SPEED);
    }

    #[test]
    fn knockback_moves_an_idle_mob_and_decays_to_rest() {
        let mut runtime = MobRuntime::new(7);
        let mut mobs = vec![pig_at_origin()];
        mobs[0].take_hit(4, attacker_behind()).expect("hit lands");
        let start = mobs[0].position;

        // 14 steps is enough to drain a 4.0 impulse at 0.6 per step (it is
        // dropped below the epsilon floor on step 12), and stays below the
        // 16-step yaw reroll so the Idle pose is ours.
        let mut rest_position = None;
        for step in 0..14 {
            mobs[0].state = MobAiState::Idle;
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
            if step == 12 {
                rest_position = Some(mobs[0].position);
            }
        }

        assert_eq!(mobs[0].state, MobAiState::Idle);
        assert_eq!(
            mobs[0].knockback,
            Vec3::ZERO,
            "the impulse must be dropped below the epsilon floor"
        );
        let displacement = mobs[0].position.z - start.z;
        assert!(
            displacement > 0.0,
            "an Idle mob must still slide, moved {displacement}"
        );
        // Geometric series bound: speed * dt / (1 - decay) = 0.5 blocks.
        let bound = MOB_KNOCKBACK_SPEED * MOB_TICK_SECS / (1.0 - MOB_KNOCKBACK_DECAY);
        assert!(
            displacement < bound + 1e-4,
            "displacement {displacement} exceeded the decay bound {bound}"
        );
        assert!(mobs[0].position.is_finite());

        // Once at rest, the position stops changing for the remaining steps.
        assert_eq!(
            Some(mobs[0].position),
            rest_position,
            "a mob at rest must not keep creeping"
        );
    }

    #[test]
    fn knockback_is_not_written_into_velocity() {
        let mut runtime = MobRuntime::new(3);
        let mut mobs = vec![pig_at_origin()];
        mobs[0].take_hit(4, attacker_behind()).expect("hit lands");
        // The AI arm assigns velocity every step; the impulse must survive in
        // its own field and never be installed as velocity.
        mobs[0].state = MobAiState::Idle;
        runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
        assert_eq!(mobs[0].velocity, Vec3::ZERO);
        assert!(mobs[0].knockback.z > 0.0);
        assert!(mobs[0].position.z > 0.0);
    }

    #[test]
    fn hurt_iframe_decays_only_while_resident() {
        let mut runtime = MobRuntime::new(5);
        let mut mobs = vec![pig_at_origin()];
        mobs[0].hurt_iframe = 0.2;

        runtime.tick(0.05, &mut mobs, |_| false, None, &NoWorld);
        assert!((mobs[0].hurt_iframe - 0.2).abs() < 1e-6);

        for _ in 0..4 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
        }
        assert!(mobs[0].hurt_iframe <= 1e-5);

        // A full 0.5 s window elapses after ten 0.05 s steps.
        let mut full = vec![pig_at_origin()];
        full[0].take_hit(4, attacker_behind()).expect("hit lands");
        for _ in 0..10 {
            runtime.tick(0.05, &mut full, resident, None, &NoWorld);
        }
        assert_eq!(full[0].hurt_iframe, 0.0);
        assert_eq!(full[0].hurt, 0.0);
    }

    #[test]
    fn non_resident_mob_keeps_iframes_and_knockback_frozen() {
        let mut runtime = MobRuntime::new(9);
        let mut mobs = vec![Mob::new(MobKind::Cow, Vec3::new(100.0, 5.0, 0.5), 0.0)];
        mobs[0].hurt = 0.4;
        mobs[0].hurt_iframe = 0.4;
        mobs[0].knockback = Vec3::Z * MOB_KNOCKBACK_SPEED;
        let snapshot = mobs[0].clone();

        runtime.tick(0.05, &mut mobs, |_| false, None, &NoWorld);
        assert_eq!(mobs[0], snapshot, "a non-resident mob must freeze entirely");
    }

    // ---- S04: passive flee, death collapse, and the reap contract --------

    #[test]
    fn kinds_partition_exactly_into_passive_and_hostile() {
        let passive: Vec<MobKind> = MobKind::ALL
            .iter()
            .copied()
            .filter(|kind| kind.is_passive())
            .collect();
        let hostile: Vec<MobKind> = MobKind::ALL
            .iter()
            .copied()
            .filter(|kind| kind.is_hostile())
            .collect();
        assert_eq!(passive, vec![MobKind::Pig, MobKind::Cow]);
        assert_eq!(hostile, vec![MobKind::Zombie, MobKind::Skeleton]);
        for kind in MobKind::ALL {
            assert_ne!(kind.is_passive(), kind.is_hostile());
            assert!(kind.is_passive() || kind.is_hostile());
        }
    }

    #[test]
    fn new_and_restored_mobs_start_with_neutral_flee_and_death() {
        let fresh = Mob::new(MobKind::Pig, Vec3::ZERO, 0.0);
        assert_eq!(fresh.flee, 0.0);
        assert_eq!(fresh.death, 0.0);
        let restored = Mob::restored(MobKind::Pig, Vec3::ZERO, 0.0, 4);
        assert_eq!(restored.flee, 0.0);
        assert_eq!(restored.death, 0.0);
    }

    #[test]
    fn non_lethal_hit_makes_only_passive_kinds_flee() {
        for kind in MobKind::ALL {
            let mut mob = Mob::new(kind, Vec3::new(0.0, 5.0, 0.0), 2.0);
            let yaw_before = mob.yaw;
            let state_before = mob.state;
            let hit = mob.take_hit(4, attacker_behind()).expect("hit lands");
            assert!(!hit.killed, "{kind:?} survives 4 damage");
            if kind.is_passive() {
                assert_eq!(mob.state, MobAiState::Flee, "{kind:?} must flee");
                assert_eq!(mob.flee, MOB_FLEE_SECS);
                assert_eq!(mob.death, 0.0);
                assert!(mob.yaw.abs() < 1e-6, "{kind:?} must face +Z away");
            } else {
                assert_eq!(mob.state, state_before, "{kind:?} state is untouched");
                assert_eq!(mob.yaw, yaw_before, "{kind:?} yaw is untouched");
                assert_eq!(mob.flee, 0.0);
                assert_eq!(mob.death, 0.0);
            }
        }
    }

    #[test]
    fn flee_yaw_points_away_from_the_attacker() {
        let mut mob = Mob::new(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 2.0);
        mob.take_hit(1, Vec3::new(2.0, 5.0, 0.0))
            .expect("hit lands");
        // Away is -X, so the yaw faces -X: sin = -1, cos = 0.
        assert!(
            (mob.yaw.sin() + 1.0).abs() < 1e-5,
            "yaw sin {}",
            mob.yaw.sin()
        );
        assert!(mob.yaw.cos().abs() < 1e-5, "yaw cos {}", mob.yaw.cos());
        assert_eq!(mob.state, MobAiState::Flee);
    }

    #[test]
    fn flee_survives_the_sixteen_step_yaw_reroll() {
        let mut runtime = MobRuntime::new(21);
        let mut mobs = vec![Mob::new(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 2.0)];
        mobs[0].take_hit(4, attacker_behind()).expect("hit lands");
        let flee_yaw = mobs[0].yaw;

        // Exactly sixteen fixed steps: the reroll boundary that would erase a
        // naive flee.
        for _ in 0..16 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
        }
        assert_eq!(runtime.tick_index(), 16);
        assert_eq!(mobs[0].state, MobAiState::Flee);
        assert_eq!(mobs[0].yaw, flee_yaw, "the reroll must not touch a flee");
        assert!(mobs[0].flee > 0.0);
    }

    #[test]
    fn flee_expires_back_to_wander_and_never_goes_negative() {
        let mut runtime = MobRuntime::new(22);
        let mut mobs = vec![Mob::new(MobKind::Cow, Vec3::new(0.0, 5.0, 0.0), 0.0)];
        mobs[0].take_hit(4, attacker_behind()).expect("hit lands");

        let mut previous = mobs[0].flee;
        for _ in 0..52 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
            let flee = mobs[0].flee;
            assert!(flee >= 0.0, "the flee timer went negative: {flee}");
            assert!(flee <= previous, "the flee timer must not grow");
            previous = flee;
        }
        assert_eq!(mobs[0].flee, 0.0);
        assert_eq!(mobs[0].state, MobAiState::Wander);
        assert_eq!(mobs[0].death, 0.0, "a fleeing mob is not dying");
    }

    #[test]
    fn flee_ignores_the_home_leash() {
        let mut runtime = MobRuntime::new(23);
        let mut mob = Mob::new(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 0.0);
        // Start five blocks (past the four-block leash) from home, with the
        // attacker behind so "away" pushes even farther out.
        mob.home = Vec3::new(0.0, 5.0, 0.0);
        mob.position = Vec3::new(0.0, 5.0, 5.0);
        mob.take_hit(1, Vec3::new(0.0, 5.0, 4.0))
            .expect("hit lands");
        assert_eq!(mob.state, MobAiState::Flee);
        let mut mobs = vec![mob];

        for _ in 0..10 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
        }
        let distance = (mobs[0].position - mobs[0].home).length();
        assert!(
            mobs[0].position.z > 5.0 && distance > 5.0,
            "flee must keep running away, position {:?} distance {distance}",
            mobs[0].position
        );
    }

    #[test]
    fn lethal_hit_arms_the_collapse_and_zeroes_velocity() {
        let mut mob = pig_at_origin();
        mob.velocity = Vec3::new(1.0, 0.0, 1.0);
        let hit = mob
            .take_hit(200, attacker_behind())
            .expect("lethal hit lands");
        assert!(hit.killed);
        assert_eq!(mob.health, 0);
        assert!(mob.is_dead());
        assert_eq!(mob.death, MOB_DEATH_SECS);
        assert_eq!(mob.flee, 0.0);
        assert_eq!(mob.velocity, Vec3::ZERO);
        assert!(
            mob.knockback.length_squared() > 0.0,
            "the away knockback still applies"
        );
        assert!(!mob.death_finished());
        assert_eq!(mob.death_progress(), 0.0);
    }

    #[test]
    fn death_progress_is_zero_monotonic_clamped_and_finished() {
        let mut runtime = MobRuntime::new(24);
        let mut mobs = vec![pig_at_origin()];
        assert_eq!(mobs[0].death_progress(), 0.0, "alive progress is zero");
        let hit = mobs[0]
            .take_hit(200, attacker_behind())
            .expect("lethal hit lands");
        assert!(hit.killed);

        let mut previous = mobs[0].death_progress();
        let mut saw_mid_collapse = false;
        for _ in 0..18 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
            let progress = mobs[0].death_progress();
            assert!((0.0..=1.0).contains(&progress), "progress {progress}");
            assert!(progress >= previous, "progress must be monotonic");
            if progress > 0.0 && progress < 1.0 {
                saw_mid_collapse = true;
            }
            previous = progress;
        }
        assert!(saw_mid_collapse, "the collapse must have a visible middle");
        assert_eq!(mobs[0].death, 0.0);
        assert!(mobs[0].death_finished());
        assert_eq!(mobs[0].death_progress(), 1.0);

        // Clamping: a corrupted timer cannot escape the render range.
        let mut restored = Mob::restored(MobKind::Pig, Vec3::ZERO, 0.0, 0);
        assert!(restored.death_finished());
        assert_eq!(restored.death_progress(), 1.0);
        restored.death = -1.0;
        assert_eq!(restored.death_progress(), 1.0);
        restored.death = MOB_DEATH_SECS * 2.0;
        assert_eq!(restored.death_progress(), 0.0);
        restored.death = MOB_DEATH_SECS * 0.5;
        assert!((restored.death_progress() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn take_hit_and_kill_on_a_corpse_mutate_nothing() {
        // Already-finished corpse.
        let mut finished = Mob::restored(MobKind::Pig, Vec3::new(0.0, 5.0, 0.0), 0.0, 0);
        let snapshot = finished.clone();
        assert_eq!(finished.take_hit(4, attacker_behind()), None);
        assert_eq!(finished, snapshot);
        assert!(!finished.kill());
        assert_eq!(finished, snapshot);

        // Mid-collapse corpse: still dead, still refuses both entry points.
        let mut fading = pig_at_origin();
        fading
            .take_hit(200, attacker_behind())
            .expect("lethal hit lands");
        fading.hurt_iframe = 0.0;
        let snapshot = fading.clone();
        assert_eq!(fading.take_hit(4, attacker_behind()), None);
        assert_eq!(fading, snapshot);
        assert!(!fading.kill());
        assert_eq!(fading, snapshot);
    }

    #[test]
    fn dying_mob_moves_only_by_knockback_and_still_decays_timers() {
        let mut runtime = MobRuntime::new(25);
        let mut mobs = vec![pig_at_origin()];
        mobs[0]
            .take_hit(200, attacker_behind())
            .expect("lethal hit lands");
        let start = mobs[0].position;
        let start_walk_phase = mobs[0].walk_phase;

        for _ in 0..4 {
            runtime.tick(0.05, &mut mobs, resident, None, &NoWorld);
        }

        assert_eq!(mobs[0].velocity, Vec3::ZERO, "a dying mob never walks");
        assert_eq!(mobs[0].walk_phase, start_walk_phase);
        assert_eq!(mobs[0].position.x, start.x);
        assert_eq!(mobs[0].position.y, start.y);
        assert!(
            mobs[0].position.z > start.z,
            "only the away knockback moved the body"
        );
        assert!(mobs[0].hurt < MOB_HURT_FLASH_SECS);
        assert!(mobs[0].hurt_iframe < MOB_IFRAME_SECS);
        assert!(mobs[0].death < MOB_DEATH_SECS);
    }

    #[test]
    fn non_resident_corpse_freezes_its_collapse() {
        let mut runtime = MobRuntime::new(26);
        let mut mob = Mob::new(MobKind::Pig, Vec3::new(100.0, 5.0, 0.5), 0.0);
        assert!(mob.kill());
        mob.death = MOB_DEATH_SECS * 0.5;
        let snapshot = mob.clone();
        let mut mobs = vec![mob];

        runtime.tick(0.05, &mut mobs, |_| false, None, &NoWorld);
        assert_eq!(mobs[0], snapshot, "an unloaded corpse freezes entirely");
    }
}
