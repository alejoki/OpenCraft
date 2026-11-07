//! Deterministic, GPU-free mob population layer (M009/S05).
//!
//! Every population rule lives behind one entry point,
//! [`SpawnRuntime::tick`]: it turns the game clock ([`daycycle::is_night`]),
//! sky light, block data, chunk residency, and the player/world-spawn anchors
//! into spawn attempts against the session's single `Vec<Mob>` list. Nothing
//! here imports `wgpu`, `winit`, `Instant`, or the wall clock, so the whole
//! layer is headlessly testable and a headless harness can pin exact counts.
//!
//! ## Capability surface
//!
//! Callers supply world facts only through [`SpawnWorld`] (`block`,
//! `sky_light`, `column_ready`); [`WorldManager`] is the one production impl.
//! Keeping the trait to three capabilities means a test can build a tiny
//! synthetic world and the spawner cannot reach around the rules for extra
//! state.
//!
//! ## The gate controls creation and burning, never culling
//!
//! [`SpawnRuntime::set_enabled`] stops *new mobs* and daylight burning; it
//! never stops the distance cull. That asymmetry is deliberate: the
//! `--demo-combat` seam needs a quiet mob list for its pinned quartet, while a
//! record that drifts out of range must still be culled as deterministically
//! as it spawned. A cull only ever marks a record that is already outside the
//! radius, so it cannot move a deterministic demo's mob list.
//!
//! ## Daylight burning
//!
//! Burning is a single global [`BURN_INTERVAL_SECS`] cadence, not a per-mob
//! accumulator: there is no new `Mob` burn field, no phase drift between
//! records, and a 20 hp hostile dies after exactly 20 cadences = 10 s of
//! daylight. A cave hostile (`sky_light <= `[`HOSTILE_CAVE_LIGHT`]) or a
//! hostile under cover (`< `[`PASSIVE_SURFACE_LIGHT`]) never burns, and water
//! at the feet shields it too. [`Mob::kill`] is the only kill path, so a burn
//! is an environmental death that drops nothing.
//!
//! ## Determinism
//!
//! Placement comes from [`spawn_roll`], a splitmix-style finalizer with its
//! own constants. It is deliberately *not* [`crate::world::mob::mob_roll`]
//! (MEM398/D114): mob AI tuning must never move the spawn table, and vice
//! versa. The attempt index is ephemeral — it lives in the runtime and is not
//! persisted — so a reload restarts the sequence exactly (S02's "reload is a
//! deterministic restart").

use glam::{IVec3, Vec3};

use crate::world::block::BlockId;
use crate::world::chunk::{ChunkPos, CHUNK_SIZE};
use crate::world::daycycle;
use crate::world::manager::{WorldManager, MAX_CHUNK_Y, MIN_CHUNK_Y};
use crate::world::mob::{Mob, MobKind};

/// Horizontal radius around the player and around the world spawn that never
/// receives a spawn, in blocks.
pub const SAFE_RADIUS_BLOCKS: i32 = 24;

/// Outer horizontal radius of the spawn ring around the player, in blocks.
pub const SPAWN_MAX_RADIUS_BLOCKS: i32 = 48;

/// Horizontal distance from the player past which a record is culled, in
/// blocks.
pub const DESPAWN_RADIUS_BLOCKS: i32 = 96;

/// Hard cap on the number of mob records in the session list, resident or
/// not.
pub const MAX_MOBS_TOTAL: usize = 32;

/// Edge length of one spawn region cell, in blocks.
pub const SPAWN_REGION_BLOCKS: i32 = 32;

/// Per-region cap. Counted across residency, exactly like
/// [`MAX_MOBS_TOTAL`], so unloading a chunk can never free a spawn slot.
pub const SPAWN_REGION_CAP: usize = 4;

/// Seconds of game time between spawn attempts.
pub const SPAWN_ATTEMPT_SECS: f32 = 0.25;

/// Seconds of game time between despawn scans.
pub const DESPAWN_SCAN_SECS: f32 = 1.0;

/// Seconds of game time between daylight-burn ticks (one global cadence).
pub const BURN_INTERVAL_SECS: f32 = 0.5;

/// Minimum horizontal distance between two mob feet positions, in blocks.
///
/// This is what keeps two records from sharing a position, which would make
/// the v7 save fail closed on a duplicate record.
pub const SPAWN_MIN_SEPARATION: f32 = 1.0;

/// At or below this sky light a column counts as cave-dark: hostiles may
/// spawn there at any hour.
pub const HOSTILE_CAVE_LIGHT: u8 = 7;

/// Full sky light. Both the night-surface and the lit-grass passive rules
/// require exactly this value.
pub const PASSIVE_SURFACE_LIGHT: u8 = 15;

/// Whole spawn attempts a single [`SpawnRuntime::tick`] may execute, so one
/// long stall cannot spiral into an unbounded catch-up burst.
pub const MAX_SPAWN_ATTEMPTS_PER_CALL: u32 = 4;

/// Creator-tunable mob population rules.
///
/// Every default reads the existing module constant, so the constants stay
/// authoritative until a live rules bundle overrides them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnRules {
    /// Radius around anchors where hostile mobs never spawn, in blocks.
    pub safe_radius_blocks: i32,
    /// Outer horizontal radius of the spawn ring, in blocks.
    pub spawn_max_radius_blocks: i32,
    /// Distance past which a record is culled, in blocks.
    pub despawn_radius_blocks: i32,
    /// Hard cap on mob records in the session list.
    pub max_mobs_total: usize,
    /// Edge length of one spawn region cell, in blocks.
    pub spawn_region_blocks: i32,
    /// Per-region mob cap.
    pub spawn_region_cap: usize,
    /// Seconds of game time between spawn attempts.
    pub spawn_attempt_secs: f32,
    /// Seconds of game time between despawn scans.
    pub despawn_scan_secs: f32,
    /// Seconds of game time between daylight-burn ticks.
    pub burn_interval_secs: f32,
    /// Minimum horizontal distance between two mob feet, in blocks.
    pub spawn_min_separation: f32,
    /// At or below this sky light a column counts as cave-dark.
    pub hostile_cave_light: u8,
    /// Minimum sky light for the lit-surface passive rules.
    pub passive_surface_light: u8,
    /// Whole spawn attempts one tick may execute.
    pub max_spawn_attempts_per_call: u32,
}

impl Default for SpawnRules {
    fn default() -> Self {
        Self {
            safe_radius_blocks: SAFE_RADIUS_BLOCKS,
            spawn_max_radius_blocks: SPAWN_MAX_RADIUS_BLOCKS,
            despawn_radius_blocks: DESPAWN_RADIUS_BLOCKS,
            max_mobs_total: MAX_MOBS_TOTAL,
            spawn_region_blocks: SPAWN_REGION_BLOCKS,
            spawn_region_cap: SPAWN_REGION_CAP,
            spawn_attempt_secs: SPAWN_ATTEMPT_SECS,
            despawn_scan_secs: DESPAWN_SCAN_SECS,
            burn_interval_secs: BURN_INTERVAL_SECS,
            spawn_min_separation: SPAWN_MIN_SEPARATION,
            hostile_cave_light: HOSTILE_CAVE_LIGHT,
            passive_surface_light: PASSIVE_SURFACE_LIGHT,
            max_spawn_attempts_per_call: MAX_SPAWN_ATTEMPTS_PER_CALL,
        }
    }
}

/// Absorbs `f32` accumulation drift when many small frame deltas are summed.
///
/// Without it, `50 x 0.01 s` can land at 0.2499997 and fire one attempt later
/// than `10 x 0.05 s`, which would break frame-rate independence (T03). The
/// tolerance is ~1000x the observed drift and ~0.04% of one attempt interval.
const SPAWN_ACCUM_EPSILON: f32 = 1e-4;

/// Float aliases of the integer radii, for distance work in world units.
pub const SAFE_RADIUS: f32 = SAFE_RADIUS_BLOCKS as f32;
/// Float alias of [`SPAWN_MAX_RADIUS_BLOCKS`].
pub const SPAWN_MAX_RADIUS: f32 = SPAWN_MAX_RADIUS_BLOCKS as f32;
/// Float alias of [`DESPAWN_RADIUS_BLOCKS`].
pub const DESPAWN_RADIUS: f32 = DESPAWN_RADIUS_BLOCKS as f32;

// The safe ring must be non-empty and inside the spawn ring, and both must be
// inside the despawn radius.
const _: () = assert!(SAFE_RADIUS_BLOCKS < SPAWN_MAX_RADIUS_BLOCKS);
const _: () = assert!(SPAWN_MAX_RADIUS_BLOCKS < DESPAWN_RADIUS_BLOCKS);
// A mob always despawns *before* its chunk can unload: the minimum
// `render_distance` clamp is 4 chunks = 128 blocks, which is larger than the
// 96-block despawn radius. That invariant is what stops S01's frozen-mob path
// from resurrecting a stale record that the culler could no longer see.
const _: () = assert!(DESPAWN_RADIUS_BLOCKS < 4 * CHUNK_SIZE as i32);
// One region may never be able to fill the whole global budget on its own.
const _: () = assert!(SPAWN_REGION_CAP < MAX_MOBS_TOTAL);

/// The only world-capability surface the spawner may read.
///
/// Three capabilities are enough for every eligibility rule and small enough
/// that a synthetic test world is a few lines.
pub trait SpawnWorld {
    /// Block at a world position (out-of-range semantics follow
    /// [`WorldManager::get_block`]: below the world is bedrock, above is air,
    /// an unloaded chunk is air).
    fn block(&self, pos: IVec3) -> BlockId;

    /// Sky light at a world position.
    ///
    /// **Landmine:** [`WorldManager::light_at_world`] returns
    /// [`PASSIVE_SURFACE_LIGHT`] when a light grid is missing, so an unlit
    /// column would read as full daylight and spawn passives everywhere.
    /// [`SpawnWorld::column_ready`] is therefore a mandatory gate before any
    /// `sky_light` read.
    fn sky_light(&self, pos: IVec3) -> u8;

    /// Whether the whole chunk column at `(cx, cz)` is resident *and* lit.
    fn column_ready(&self, cx: i32, cz: i32) -> bool;
}

impl SpawnWorld for WorldManager {
    #[inline]
    fn block(&self, pos: IVec3) -> BlockId {
        self.get_block(pos.x, pos.y, pos.z)
    }

    #[inline]
    fn sky_light(&self, pos: IVec3) -> u8 {
        self.light_at_world(pos.x, pos.y, pos.z)
    }

    fn column_ready(&self, cx: i32, cz: i32) -> bool {
        (MIN_CHUNK_Y..=MAX_CHUNK_Y).all(|cy| self.is_resident(ChunkPos::new(cx, cy, cz)))
            && self.has_column_light(cx, cz)
    }
}

/// Everything a spawn attempt may depend on that is not world data.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SpawnContext {
    /// Session seed; the only entropy source.
    pub seed: u32,
    /// Whole days elapsed, part of the roll input so each day differs.
    pub day_index: u32,
    /// Ticks since dawn, in `0..DAY_TICKS`.
    pub time_of_day: u32,
    /// Player feet position (the ring is centred here).
    pub player: Vec3,
    /// Cached world-spawn feet position (protected by the safe radius).
    pub world_spawn: Vec3,
}

/// One already-resolved placement candidate for [`try_spawn_at`].
///
/// Bundling the placement facts keeps the eligibility pipeline within the
/// argument-count budget without changing the pipeline order.
#[derive(Copy, Clone, Debug)]
struct SpawnCandidate {
    /// Candidate feet position, already snapped to the surface scan.
    feet: Vec3,
    /// Yaw the new mob record receives.
    yaw: f32,
    /// Session attempt index used by the deterministic kind roll.
    attempt_index: u64,
}

/// What one population step did.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnReport {
    /// Records created.
    pub spawned: u32,
    /// Records marked for removal by the despawn scan.
    pub despawned: u32,
    /// Hostiles finished by daylight burning.
    pub burned: u32,
    /// Attempts rejected by the global or per-region cap.
    pub cap_blocked: u32,
}

impl SpawnReport {
    /// Adds every field of `other` into `self`.
    pub fn add(&mut self, other: SpawnReport) {
        self.spawned += other.spawned;
        self.despawned += other.despawned;
        self.burned += other.burned;
        self.cap_blocked += other.cap_blocked;
    }
}

/// The population runtime owned by the session.
///
/// [`SpawnRuntime::tick`] is the whole interface: callers never reach for the
/// individual rules, so the gate and the caps cannot be bypassed.
#[derive(Debug)]
pub struct SpawnRuntime {
    /// Fractional seconds carried toward the next spawn attempt.
    attempt_accum: f32,
    /// Fractional seconds carried toward the next despawn scan. Unlike the
    /// other two accumulators this one runs while disabled, because the cull
    /// is not gated.
    despawn_accum: f32,
    /// Fractional seconds carried toward the next global burn cadence.
    burn_accum: f32,
    /// Ephemeral attempt counter; not persisted, so a reload restarts the
    /// deterministic spawn sequence.
    attempt_index: u64,
    /// Whether creation and burning are allowed (culling always runs).
    enabled: bool,
    /// Cumulative counters since construction or [`SpawnRuntime::reset_report`].
    report: SpawnReport,
    /// Live population tuning installed by [`SpawnRuntime::set_rules`].
    rules: SpawnRules,
}

impl SpawnRuntime {
    /// A fresh, enabled runtime with no attempts executed and a zero report.
    pub fn new() -> SpawnRuntime {
        SpawnRuntime {
            attempt_accum: 0.0,
            despawn_accum: 0.0,
            burn_accum: 0.0,
            attempt_index: 0,
            enabled: true,
            report: SpawnReport::default(),
            rules: SpawnRules::default(),
        }
    }

    /// The population rules currently installed.
    #[inline]
    pub fn rules(&self) -> SpawnRules {
        self.rules
    }

    /// Replaces the population rules without touching runtime phase.
    ///
    /// A live config reload must not reset a deterministic window: unlike
    /// [`SpawnRuntime::set_enabled`], this keeps `attempt_accum`,
    /// `despawn_accum`, `burn_accum`, `attempt_index`, `enabled`, and
    /// `report` exactly as they were, so the next attempt lands on the same
    /// frame boundary it would have without the reload. Callers install a
    /// whole last-good bundle ([`crate::world::session::WorldSession::apply_rules`]),
    /// never a per-field patch.
    pub fn set_rules(&mut self, rules: SpawnRules) {
        self.rules = rules;
    }

    /// Whether spawning and burning are currently allowed.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Enables or disables creation and burning.
    ///
    /// Disabling clears the partial attempt, despawn-scan, and burn intervals
    /// so re-enabling starts a fresh, deterministic window instead of firing
    /// immediately on the first tick after the gate opens. **The gate never
    /// stops the distance cull**: culling a record that has drifted out of
    /// range is safe regardless of the gate.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.attempt_accum = 0.0;
            self.despawn_accum = 0.0;
            self.burn_accum = 0.0;
        }
    }

    /// Cumulative report since construction or the last `reset_report`.
    #[inline]
    pub fn report(&self) -> SpawnReport {
        self.report
    }

    /// Zeroes the cumulative report. The attempt sequence is untouched, so a
    /// demo can re-measure a stage without perturbing determinism.
    pub fn reset_report(&mut self) {
        self.report = SpawnReport::default();
    }

    /// Advances the population layer by one frame of game time.
    ///
    /// `game_dt` must already be clamped and time-scaled by the caller. A
    /// non-finite or non-positive value is a no-op that returns a zero
    /// report.
    ///
    /// Three cadences run here, and only two of them are gated:
    /// * the despawn scan ([`DESPAWN_SCAN_SECS`]) **always** runs,
    /// * daylight burning ([`BURN_INTERVAL_SECS`]) runs only while enabled,
    /// * spawn attempts ([`SPAWN_ATTEMPT_SECS`]) run only while enabled, with
    ///   at most [`MAX_SPAWN_ATTEMPTS_PER_CALL`] per call and the leftover
    ///   clamped to at most half an interval so a long stall cannot spiral
    ///   into a catch-up burst.
    ///
    /// Returns the delta for this call; [`SpawnRuntime::report`] accumulates
    /// every delta.
    pub fn tick(
        &mut self,
        game_dt: f32,
        ctx: &SpawnContext,
        world: &impl SpawnWorld,
        mobs: &mut Vec<Mob>,
    ) -> SpawnReport {
        let mut delta = SpawnReport::default();
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return delta;
        }

        // Copy the live rules once so every cadence in this call sees one
        // consistent bundle even if a future seam mutates the runtime
        // mid-frame. Keeping the structural epsilon below means a creator
        // cannot make the accumulation loops unbounded (the registry clamps
        // intervals to >= 0.01 s, but `set_rules` accepts a hand-built rule).
        let rules = self.rules;
        let despawn_interval = rules.despawn_scan_secs.max(SPAWN_ACCUM_EPSILON);
        let burn_interval = rules.burn_interval_secs.max(SPAWN_ACCUM_EPSILON);
        let attempt_interval = rules.spawn_attempt_secs.max(SPAWN_ACCUM_EPSILON);

        // Ungated by design: a cull can only mark a record already outside
        // the radius, so it can never move a deterministic demo's mob list,
        // and leaving it ungated means the demo's despawn proof cannot be
        // silently disabled by the seam.
        self.despawn_accum += game_dt;
        while self.despawn_accum + SPAWN_ACCUM_EPSILON >= despawn_interval {
            self.despawn_accum = (self.despawn_accum - despawn_interval).max(0.0);
            scan_despawn(ctx, mobs, &mut delta, &rules);
        }

        if self.enabled {
            self.burn_accum += game_dt;
            while self.burn_accum + SPAWN_ACCUM_EPSILON >= burn_interval {
                self.burn_accum = (self.burn_accum - burn_interval).max(0.0);
                scan_burn(ctx, world, mobs, &mut delta, &rules);
            }

            self.attempt_accum += game_dt;
            let mut executed = 0;
            while executed < rules.max_spawn_attempts_per_call
                && self.attempt_accum + SPAWN_ACCUM_EPSILON >= attempt_interval
            {
                self.attempt_accum = (self.attempt_accum - attempt_interval).max(0.0);
                attempt_spawn(ctx, world, mobs, self.attempt_index, &mut delta, &rules);
                self.attempt_index = self.attempt_index.wrapping_add(1);
                executed += 1;
            }
            // Only a call whose attempt budget ran out can leave whole
            // intervals queued. Clamp that leftover to half an interval so
            // the next call cannot immediately fire, while a normal small
            // frame delta is carried untouched.
            if self.attempt_accum + SPAWN_ACCUM_EPSILON >= attempt_interval {
                self.attempt_accum = attempt_interval * 0.5;
            }
        }

        self.report.add(delta);
        delta
    }
}

impl Default for SpawnRuntime {
    fn default() -> Self {
        SpawnRuntime::new()
    }
}

/// Deterministic draw for one spawn attempt slot.
///
/// SplitMix64-style finalizer with its **own** constants so the spawn table
/// never shares a stream with [`crate::world::mob::mob_roll`] or the terrain
/// hash (MEM398/D114). Identical inputs always return the identical value.
/// `attempt_index` is ephemeral, so a reload restarts the sequence.
fn spawn_roll(seed: u32, day_index: u32, time_of_day: u32, attempt_index: u64, slot: u32) -> u32 {
    let mut z = attempt_index
        ^ (seed as u64).wrapping_mul(0xA24B_AED4_963E_E407)
        ^ (day_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (time_of_day as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (slot as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    z = z.wrapping_add(0xE703_7ED1_A0B4_28DB);
    z = (z ^ (z >> 33)).wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    z = (z ^ (z >> 33)).wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    z ^= z >> 33;
    (z ^ (z >> 32)) as u32
}

/// Roll slot for the ring sample (angle and radius share one draw).
const SLOT_SHAPE: u32 = 0;
/// Roll slot for the kind bit (Zombie/Skeleton or Pig/Cow).
const SLOT_KIND: u32 = 1;

/// The `(angle, radius)` a spawn attempt places its candidate at.
///
/// The low 16 bits select the angle over a full turn, the high 16 bits select
/// the radius inside `[SAFE_RADIUS, SPAWN_MAX_RADIUS]`. Both are derived from
/// one draw so the ring is uniformly sampled.
fn ring_sample(ctx: &SpawnContext, attempt_index: u64, rules: &SpawnRules) -> (f32, f32) {
    let roll = spawn_roll(
        ctx.seed,
        ctx.day_index,
        ctx.time_of_day,
        attempt_index,
        SLOT_SHAPE,
    );
    let angle = (roll & 0xFFFF) as f32 / 65_535.0 * std::f32::consts::TAU;
    let radius_fraction = ((roll >> 16) & 0xFFFF) as f32 / 65_535.0;
    let min_radius = rules.safe_radius_blocks as f32;
    let max_radius = rules.spawn_max_radius_blocks as f32;
    let radius = min_radius + radius_fraction * (max_radius - min_radius);
    (angle, radius)
}

/// One spawn attempt: cap gate, ring sample, residency gate, y scan, then the
/// full eligibility/placement pipeline in [`try_spawn_at`].
fn attempt_spawn(
    ctx: &SpawnContext,
    world: &impl SpawnWorld,
    mobs: &mut Vec<Mob>,
    attempt_index: u64,
    report: &mut SpawnReport,
    rules: &SpawnRules,
) {
    // The global cap is checked before any world work: a full list makes the
    // roll and the y scan pure waste.
    if mobs.len() >= rules.max_mobs_total {
        report.cap_blocked += 1;
        return;
    }

    let (angle, radius) = ring_sample(ctx, attempt_index, rules);
    let (sin, cos) = angle.sin_cos();
    let feet_x = (ctx.player.x + sin * radius).floor() + 0.5;
    let feet_z = (ctx.player.z + cos * radius).floor() + 0.5;
    let block_x = feet_x.floor() as i32;
    let block_z = feet_z.floor() as i32;

    // Mandatory before any light read: an unlit column reports full sky light
    // from `light_at_world` and would spawn passives out of nothing.
    let (cx, cz) = (
        block_x.div_euclid(CHUNK_SIZE as i32),
        block_z.div_euclid(CHUNK_SIZE as i32),
    );
    if !world.column_ready(cx, cz) {
        // The normal "not streamed yet" case; deliberately not counted.
        return;
    }

    let Some(feet_y) = find_surface(world, block_x, block_z) else {
        return;
    };

    try_spawn_at(
        ctx,
        world,
        mobs,
        SpawnCandidate {
            feet: Vec3::new(feet_x, feet_y as f32, feet_z),
            yaw: angle,
            attempt_index,
        },
        report,
        rules,
    );
}

/// Top-down search for the first two-air stand with a solid floor.
///
/// Bounded to the loaded Y span: at most 191 sample points (159 down to -31)
/// with three block reads each, and at most
/// [`MAX_SPAWN_ATTEMPTS_PER_CALL`] attempts per call (4/s), so the scan can
/// never become a per-frame hot loop.
fn find_surface(world: &impl SpawnWorld, block_x: i32, block_z: i32) -> Option<i32> {
    let top = (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32 - 1;
    let bottom = MIN_CHUNK_Y * CHUNK_SIZE as i32 + 1;
    for y in (bottom..=top).rev() {
        let feet = IVec3::new(block_x, y, block_z);
        if world.block(feet) == BlockId::Air
            && world.block(IVec3::new(block_x, y + 1, block_z)) == BlockId::Air
            && world.block(IVec3::new(block_x, y - 1, block_z)).is_solid()
        {
            return Some(y);
        }
    }
    None
}

/// Levelled kind bit: half zombies, half skeletons.
fn hostile_kind(ctx: &SpawnContext, attempt_index: u64) -> MobKind {
    let roll = spawn_roll(
        ctx.seed,
        ctx.day_index,
        ctx.time_of_day,
        attempt_index,
        SLOT_KIND,
    );
    if roll & 1 == 0 {
        MobKind::Zombie
    } else {
        MobKind::Skeleton
    }
}

/// Levelled kind bit: half pigs, half cows.
fn passive_kind(ctx: &SpawnContext, attempt_index: u64) -> MobKind {
    let roll = spawn_roll(
        ctx.seed,
        ctx.day_index,
        ctx.time_of_day,
        attempt_index,
        SLOT_KIND,
    );
    if roll & 1 == 0 {
        MobKind::Pig
    } else {
        MobKind::Cow
    }
}

/// The spawn region cell of `region_blocks` edge length containing a
/// horizontal position.
fn region_bucket(x: f32, z: f32, region_blocks: i32) -> (i32, i32) {
    (
        (x.floor() as i32).div_euclid(region_blocks),
        (z.floor() as i32).div_euclid(region_blocks),
    )
}

/// How many records already live in `bucket`, resident or not.
fn region_count(mobs: &[Mob], bucket: (i32, i32), region_blocks: i32) -> usize {
    mobs.iter()
        .filter(|mob| region_bucket(mob.position.x, mob.position.z, region_blocks) == bucket)
        .count()
}

/// Horizontal squared distance between two feet positions.
#[inline]
fn horizontal_distance_sq(a: Vec3, b: Vec3) -> f32 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    dx * dx + dz * dz
}

/// One coarse despawn scan: mark every record beyond [`DESPAWN_RADIUS`].
///
/// **Horizontal** distance only, for the same reason the safe ring uses it: a
/// mob deep in a cave below the player is still nearby on the only axis that
/// matters, so a large `y` difference must not cull it.
///
/// Deliberately distance-only: no `column_ready`, no block or light read. A
/// record in an unloaded chunk is exactly the one that must still be marked,
/// because the mob runtime freezes it instead of moving it home.
///
/// Already-marked records are skipped so [`SpawnReport::despawned`] counts
/// marks, not observations, and a repeated scan cannot double-count.
fn scan_despawn(
    ctx: &SpawnContext,
    mobs: &mut [Mob],
    report: &mut SpawnReport,
    rules: &SpawnRules,
) {
    let radius = rules.despawn_radius_blocks as f32;
    let radius_sq = radius * radius;
    for mob in mobs.iter_mut() {
        if mob.despawn_pending {
            continue;
        }
        if horizontal_distance_sq(mob.position, ctx.player) > radius_sq {
            mob.despawn_pending = true;
            report.despawned += 1;
        }
    }
}

/// One global daylight-burn cadence.
///
/// Hostiles on the lit surface lose exactly 1 hp per [`BURN_INTERVAL_SECS`]
/// while it is day. A cave hostile (`sky_light <= `[`HOSTILE_CAVE_LIGHT`]) or
/// one under cover (`< `[`PASSIVE_SURFACE_LIGHT`]) never burns, and water at
/// the feet shields too.
///
/// `column_ready` is checked before the light read for the same landmine
/// documented on [`SpawnWorld::sky_light`]: a missing light grid reports full
/// daylight.
///
/// The killing blow goes through [`Mob::kill`], so the 0.8 s collapse is armed
/// and no drop is ever granted — [`MobKillCause::Environment`] is refused by
/// the player-attributed grant seam.
///
/// [`MobKillCause::Environment`]: crate::world::mob::MobKillCause::Environment
fn scan_burn(
    ctx: &SpawnContext,
    world: &impl SpawnWorld,
    mobs: &mut [Mob],
    report: &mut SpawnReport,
    rules: &SpawnRules,
) {
    if daycycle::is_night(ctx.time_of_day) {
        return;
    }
    for mob in mobs.iter_mut() {
        if !mob.kind.is_hostile() || mob.health == 0 {
            continue;
        }
        let feet = IVec3::new(
            mob.position.x.floor() as i32,
            mob.position.y.floor() as i32,
            mob.position.z.floor() as i32,
        );
        let (cx, cz) = (
            feet.x.div_euclid(CHUNK_SIZE as i32),
            feet.z.div_euclid(CHUNK_SIZE as i32),
        );
        if !world.column_ready(cx, cz) {
            continue;
        }
        if world.sky_light(feet) < rules.passive_surface_light {
            continue;
        }
        if world.block(feet) == BlockId::Water {
            continue;
        }
        if mob.health <= 1 {
            mob.kill();
            report.burned += 1;
        } else {
            mob.health -= 1;
        }
    }
}

/// Applies every eligibility and placement rule to an already-resolved
/// candidate stand.
///
/// The caller ([`attempt_spawn`]) owns the global cap and the ring sample. This
/// function owns clearance, light/clock/floor eligibility, the safe radii, the
/// minimum separation, and the per-region cap.
fn try_spawn_at(
    ctx: &SpawnContext,
    world: &impl SpawnWorld,
    mobs: &mut Vec<Mob>,
    candidate: SpawnCandidate,
    report: &mut SpawnReport,
    rules: &SpawnRules,
) {
    let SpawnCandidate {
        feet,
        yaw,
        attempt_index,
    } = candidate;
    let feet_block = IVec3::new(feet.x.floor() as i32, feet.y as i32, feet.z.floor() as i32);
    if world.block(feet_block) != BlockId::Air {
        return;
    }
    if world.block(feet_block + IVec3::Y) != BlockId::Air {
        return;
    }
    let below = world.block(feet_block - IVec3::Y);

    let light = world.sky_light(feet_block);
    let lit_surface = light >= rules.passive_surface_light;
    let kind = if light <= rules.hostile_cave_light {
        // Cave-dark at any hour.
        hostile_kind(ctx, attempt_index)
    } else if lit_surface && daycycle::is_night(ctx.time_of_day) {
        // Night on the lit surface.
        hostile_kind(ctx, attempt_index)
    } else if lit_surface && below == BlockId::Grass && daycycle::is_day(ctx.time_of_day) {
        passive_kind(ctx, attempt_index)
    } else {
        // The twilight band (between the two thresholds), unlit surfaces at
        // day, and every non-grass floor are ineligible.
        return;
    };

    // The safe ring around the player is inclusive at both ends; the world
    // spawn is protected by its own radius so a wanderer cannot be ambushed
    // on the spawn pad.
    let safe_radius = rules.safe_radius_blocks as f32;
    let max_radius = rules.spawn_max_radius_blocks as f32;
    let player_distance_sq = horizontal_distance_sq(feet, ctx.player);
    if !(safe_radius * safe_radius..=max_radius * max_radius).contains(&player_distance_sq) {
        return;
    }
    if horizontal_distance_sq(feet, ctx.world_spawn) < safe_radius * safe_radius {
        return;
    }

    // Two records must never share a position: the v7 save fails closed on a
    // duplicate record, and a stacked pair would be indistinguishable.
    let min_sep_sq = rules.spawn_min_separation * rules.spawn_min_separation;
    if mobs
        .iter()
        .any(|mob| horizontal_distance_sq(mob.position, feet) < min_sep_sq)
    {
        return;
    }

    let bucket = region_bucket(feet.x, feet.z, rules.spawn_region_blocks);
    if region_count(mobs, bucket, rules.spawn_region_blocks) >= rules.spawn_region_cap {
        report.cap_blocked += 1;
        return;
    }

    mobs.push(Mob::new(kind, feet, yaw));
    report.spawned += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::survival::GameMode;
    use crate::world::chunk::Chunk;
    use crate::world::daycycle::GameClock;
    use crate::world::persistence::{ChunkEditMap, SavedMob};
    use crate::world::season::DEFAULT_SEASON_LENGTH_DAYS;
    use crate::world::session::WorldSession;

    #[test]
    fn spawn_rules_default_matches_constants() {
        let rules = SpawnRules::default();
        assert_eq!(SpawnRuntime::new().rules(), rules);
        assert_eq!(rules.safe_radius_blocks, SAFE_RADIUS_BLOCKS);
        assert_eq!(rules.spawn_max_radius_blocks, SPAWN_MAX_RADIUS_BLOCKS);
        assert_eq!(rules.despawn_radius_blocks, DESPAWN_RADIUS_BLOCKS);
        assert_eq!(rules.max_mobs_total, MAX_MOBS_TOTAL);
        assert_eq!(rules.spawn_region_blocks, SPAWN_REGION_BLOCKS);
        assert_eq!(rules.spawn_region_cap, SPAWN_REGION_CAP);
        assert_eq!(rules.spawn_attempt_secs, SPAWN_ATTEMPT_SECS);
        assert_eq!(rules.despawn_scan_secs, DESPAWN_SCAN_SECS);
        assert_eq!(rules.burn_interval_secs, BURN_INTERVAL_SECS);
        assert_eq!(rules.spawn_min_separation, SPAWN_MIN_SEPARATION);
        assert_eq!(rules.hostile_cave_light, HOSTILE_CAVE_LIGHT);
        assert_eq!(rules.passive_surface_light, PASSIVE_SURFACE_LIGHT);
        assert_eq!(
            rules.max_spawn_attempts_per_call,
            MAX_SPAWN_ATTEMPTS_PER_CALL
        );
    }
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    /// Feet height of the synthetic plain.
    const SURFACE_Y: i32 = 64;

    /// A tiny synthetic world: a procedural grass plain plus sparse block and
    /// light overrides and an optional ready-column set.
    struct FakeWorld {
        surface_y: i32,
        surface_block: BlockId,
        /// Block placed at feet level (`surface_y + 1`) for every column.
        feet_cover: Option<BlockId>,
        /// Block placed at head level (`surface_y + 2`) for every column.
        head_cover: Option<BlockId>,
        /// Ground exists only inside `|x| <= half_extent && |z| <= half_extent`.
        half_extent: i32,
        /// Sparse block overrides, checked before the procedural ground.
        blocks: HashMap<(i32, i32, i32), BlockId>,
        /// Sparse sky-light overrides.
        light: HashMap<(i32, i32, i32), u8>,
        default_light: u8,
        /// `None` means every column is ready.
        ready: Option<HashSet<(i32, i32)>>,
    }

    impl FakeWorld {
        fn flat() -> FakeWorld {
            FakeWorld {
                surface_y: SURFACE_Y,
                surface_block: BlockId::Grass,
                feet_cover: None,
                head_cover: None,
                half_extent: 160,
                blocks: HashMap::new(),
                light: HashMap::new(),
                default_light: PASSIVE_SURFACE_LIGHT,
                ready: None,
            }
        }

        fn with_default_light(mut self, light: u8) -> FakeWorld {
            self.default_light = light;
            self
        }
    }

    impl SpawnWorld for FakeWorld {
        fn block(&self, pos: IVec3) -> BlockId {
            if let Some(block) = self.blocks.get(&(pos.x, pos.y, pos.z)) {
                return *block;
            }
            if pos.x.abs() > self.half_extent || pos.z.abs() > self.half_extent {
                return BlockId::Air;
            }
            if pos.y == self.surface_y + 2 {
                if let Some(cover) = self.head_cover {
                    return cover;
                }
            }
            if pos.y == self.surface_y + 1 {
                if let Some(cover) = self.feet_cover {
                    return cover;
                }
                return BlockId::Air;
            }
            if pos.y == self.surface_y {
                return self.surface_block;
            }
            if pos.y < self.surface_y {
                return BlockId::Stone;
            }
            BlockId::Air
        }

        fn sky_light(&self, pos: IVec3) -> u8 {
            self.light
                .get(&(pos.x, pos.y, pos.z))
                .copied()
                .unwrap_or(self.default_light)
        }

        fn column_ready(&self, cx: i32, cz: i32) -> bool {
            match &self.ready {
                None => true,
                Some(ready) => ready.contains(&(cx, cz)),
            }
        }
    }

    fn flat_ctx(time_of_day: u32) -> SpawnContext {
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day,
            player: feet,
            world_spawn: feet,
        }
    }

    /// Runs exactly `attempts` attempts, one per tick.
    fn run_attempts(
        runtime: &mut SpawnRuntime,
        ctx: &SpawnContext,
        world: &FakeWorld,
        mobs: &mut Vec<Mob>,
        attempts: u32,
    ) {
        for _ in 0..attempts {
            runtime.tick(SPAWN_ATTEMPT_SECS, ctx, world, mobs);
        }
    }

    #[test]
    fn spawn_roll_is_deterministic_and_does_not_reuse_mob_roll() {
        assert_eq!(
            spawn_roll(7, 3, 6_000, 11, 0),
            spawn_roll(7, 3, 6_000, 11, 0)
        );
        assert_ne!(
            spawn_roll(7, 3, 6_000, 11, 0),
            spawn_roll(7, 3, 6_000, 12, 0),
            "a different attempt index must draw differently"
        );
        assert_ne!(
            spawn_roll(7, 3, 6_000, 11, 0),
            spawn_roll(7, 3, 6_000, 11, 1),
            "different slots must be independent"
        );
        assert_ne!(
            spawn_roll(7, 3, 6_000, 11, 0),
            spawn_roll(8, 3, 6_000, 11, 0)
        );
        assert_ne!(
            spawn_roll(7, 4, 6_000, 11, 0),
            spawn_roll(7, 3, 6_000, 11, 0)
        );
        assert_ne!(
            spawn_roll(7, 3, 18_000, 11, 0),
            spawn_roll(7, 3, 6_000, 11, 0)
        );
        // The spawn table must not ride on the mob-AI stream (MEM398/D114).
        assert_ne!(
            spawn_roll(7, 0, 6_000, 42, 0),
            crate::world::mob::mob_roll(7, 42, 0)
        );
    }

    #[test]
    fn ring_sample_stays_inside_the_spawn_ring() {
        let ctx = flat_ctx(6_000);
        for attempt in 0..500u64 {
            let (angle, radius) = ring_sample(&ctx, attempt, &SpawnRules::default());
            assert!(angle.is_finite() && (0.0..std::f32::consts::TAU).contains(&angle));
            assert!(
                (SAFE_RADIUS..=SPAWN_MAX_RADIUS).contains(&radius),
                "radius {radius} escaped [{SAFE_RADIUS}, {SPAWN_MAX_RADIUS}] at {attempt}"
            );
        }
    }

    #[test]
    fn day_spawns_only_passives_on_lit_grass() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(!mobs.is_empty(), "the day plain must produce passives");
        assert_eq!(runtime.report().spawned as usize, mobs.len());
        for mob in &mobs {
            assert!(mob.kind.is_passive(), "day spawned {:?}", mob.kind);
            assert!(mob.yaw.is_finite());
        }
    }

    #[test]
    fn night_spawns_only_hostiles_on_the_lit_surface() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(18_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(!mobs.is_empty(), "night must produce hostiles");
        for mob in &mobs {
            assert!(mob.kind.is_hostile(), "night spawned {:?}", mob.kind);
        }
    }

    #[test]
    fn cave_light_spawns_hostiles_during_the_day() {
        let world = FakeWorld::flat().with_default_light(HOSTILE_CAVE_LIGHT);
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(!mobs.is_empty(), "cave darkness must produce hostiles");
        for mob in &mobs {
            assert!(mob.kind.is_hostile(), "cave spawned {:?}", mob.kind);
        }
    }

    /// A per-position light override proves eligibility reads the light at the
    /// candidate feet, not a column-wide default.
    #[test]
    fn a_dark_candidate_column_spawns_a_hostile_at_day() {
        let mut world = FakeWorld::flat();
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        let ctx = SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day: 6_000,
            player: feet,
            world_spawn: feet,
        };
        let candidate = Vec3::new(30.5, feet.y, 0.5);
        world
            .light
            .insert((30, SURFACE_Y + 1, 0), HOSTILE_CAVE_LIGHT);

        let mut mobs = Vec::new();
        let mut report = SpawnReport::default();
        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: candidate,
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );

        assert_eq!(report.spawned, 1, "the shaded stand must spawn");
        assert!(mobs[0].kind.is_hostile());
    }

    /// A per-position block override proves clearance is read from the world,
    /// not inferred from the column.
    #[test]
    fn a_water_block_at_the_candidate_feet_rejects_it() {
        let mut world = FakeWorld::flat();
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        let ctx = SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day: 6_000,
            player: feet,
            world_spawn: feet,
        };
        world.blocks.insert((30, SURFACE_Y + 1, 0), BlockId::Water);

        let mut mobs = Vec::new();
        let mut report = SpawnReport::default();
        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(30.5, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );

        assert_eq!(report.spawned, 0, "feet must be exactly Air");
        assert!(mobs.is_empty());
    }

    #[test]
    fn twilight_light_spawns_nothing() {
        let world = FakeWorld::flat().with_default_light(10);
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(mobs.is_empty(), "light 10 must be ineligible");
        assert_eq!(runtime.report(), SpawnReport::default());
    }

    #[test]
    fn water_at_feet_rejects_the_stand() {
        let mut world = FakeWorld::flat();
        world.feet_cover = Some(BlockId::Water);
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(mobs.is_empty(), "feet in water must never spawn");
    }

    #[test]
    fn blocked_head_rejects_the_stand() {
        let mut world = FakeWorld::flat();
        world.head_cover = Some(BlockId::Stone);
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(mobs.is_empty(), "no head clearance means no spawn");
    }

    #[test]
    fn passives_require_grass() {
        let mut world = FakeWorld::flat();
        world.surface_block = BlockId::Stone;
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(mobs.is_empty(), "a stone surface must not spawn passives");
    }

    #[test]
    fn no_ground_means_no_spawns() {
        let mut world = FakeWorld::flat();
        // The whole 24..48 ring falls outside the ground extent.
        world.half_extent = 8;
        let ctx = flat_ctx(18_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(mobs.is_empty(), "air below the ring must yield no stand");
    }

    #[test]
    fn global_cap_blocks_at_thirty_two() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        // 2000 ticks x 4 attempts = 8000 attempts, ample to fill the budget.
        for _ in 0..2000 {
            runtime.tick(1.0, &ctx, &world, &mut mobs);
        }

        assert_eq!(mobs.len(), MAX_MOBS_TOTAL);
        assert!(
            runtime.report().cap_blocked > 0,
            "a full list must count blocked attempts"
        );
    }

    #[test]
    fn per_region_cap_blocks_the_fifth_mob_in_one_cell() {
        let world = FakeWorld::flat();
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        let ctx = SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day: 6_000,
            player: feet,
            world_spawn: feet,
        };
        // Four pigs in the (0, 0) region cell.
        let mut mobs: Vec<Mob> = [10.5, 12.5, 14.5, 16.5]
            .iter()
            .map(|x| {
                Mob::new(
                    MobKind::Pig,
                    Vec3::new(*x, feet.y, 0.5),
                    std::f32::consts::FRAC_PI_2,
                )
            })
            .collect();
        assert_eq!(
            region_count(&mobs, (0, 0), SPAWN_REGION_BLOCKS),
            SPAWN_REGION_CAP
        );

        let mut report = SpawnReport::default();
        // 30 blocks from the player: inside the ring, same cell as the pigs.
        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(30.5, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(
            report.cap_blocked, 1,
            "the fifth record must hit the cell cap"
        );
        assert_eq!(report.spawned, 0);
        assert_eq!(mobs.len(), 4);

        // A neighbouring cell still accepts a record.
        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(30.5, feet.y, 33.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 1);
        assert_eq!(report.cap_blocked, 1);
        assert_eq!(mobs.len(), 5);
    }

    #[test]
    fn minimum_separation_rejects_a_duplicate_position() {
        let world = FakeWorld::flat();
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        let ctx = SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day: 6_000,
            player: feet,
            world_spawn: feet,
        };
        let occupied = Vec3::new(30.5, feet.y, 0.5);
        let mut mobs = vec![Mob::new(MobKind::Pig, occupied, 0.0)];
        let mut report = SpawnReport::default();

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: occupied,
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 0, "an exact duplicate must be rejected");
        assert_eq!(mobs.len(), 1);

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(31.6, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 1, "1.1 blocks apart is far enough");
    }

    #[test]
    fn safe_radius_rejects_inside_and_beyond_the_ring() {
        let world = FakeWorld::flat();
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        let ctx = SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day: 6_000,
            player: feet,
            world_spawn: feet,
        };
        let mut report = SpawnReport::default();

        let mut mobs = Vec::new();
        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(20.5, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 0, "inside the 24-block safe radius");

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(52.5, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 0, "beyond the 48-block spawn radius");

        // A candidate 30 blocks from the player but only 15 from the world
        // spawn is excluded by the world-spawn radius.
        let near_spawn = SpawnContext {
            world_spawn: Vec3::new(15.5, feet.y, 0.5),
            ..ctx
        };
        try_spawn_at(
            &near_spawn,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(30.5, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 0, "inside the world-spawn safe radius");

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            SpawnCandidate {
                feet: Vec3::new(30.5, feet.y, 0.5),
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 1, "a legal stand must spawn");
    }

    #[test]
    fn unready_column_never_spawns_even_in_perfect_light() {
        let mut world = FakeWorld::flat();
        world.ready = Some(HashSet::new());
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();
        // This is the `light_at_world` 15-default negative control: perfect
        // light with no ready column must still produce nothing.
        run_attempts(&mut runtime, &ctx, &world, &mut mobs, 400);

        assert!(mobs.is_empty());
        assert_eq!(
            runtime.report(),
            SpawnReport::default(),
            "a not-streamed column is discarded silently"
        );
    }

    #[test]
    fn disabled_runtime_performs_no_attempt() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        runtime.set_enabled(false);
        let mut mobs = Vec::new();

        for _ in 0..200 {
            assert_eq!(
                runtime.tick(1.0, &ctx, &world, &mut mobs),
                SpawnReport::default()
            );
        }

        assert!(mobs.is_empty());
        assert_eq!(runtime.attempt_index, 0, "no attempt may even be rolled");
        assert!(!runtime.enabled());
    }

    #[test]
    fn non_positive_or_non_finite_dt_is_a_no_op() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();

        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                runtime.tick(dt, &ctx, &world, &mut mobs),
                SpawnReport::default()
            );
        }

        assert!(mobs.is_empty());
        assert_eq!(runtime.attempt_index, 0);
        assert_eq!(runtime.report(), SpawnReport::default());
    }

    #[test]
    fn a_long_stall_runs_at_most_four_attempts_and_does_not_spiral() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = Vec::new();

        runtime.tick(100.0, &ctx, &world, &mut mobs);
        assert_eq!(
            runtime.attempt_index, MAX_SPAWN_ATTEMPTS_PER_CALL as u64,
            "one call may never exceed the per-call attempt budget"
        );

        // The leftover is clamped, so a tiny follow-up tick cannot fire.
        runtime.tick(1e-6, &ctx, &world, &mut mobs);
        assert_eq!(runtime.attempt_index, MAX_SPAWN_ATTEMPTS_PER_CALL as u64);
    }

    #[test]
    fn attempt_interval_accumulates_frame_independently() {
        // 0.5 s of game time is exactly two attempts, however it is sliced.
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);

        let mut fine = SpawnRuntime::new();
        let mut fine_mobs = Vec::new();
        for _ in 0..50 {
            fine.tick(0.01, &ctx, &world, &mut fine_mobs);
        }

        let mut coarse = SpawnRuntime::new();
        let mut coarse_mobs = Vec::new();
        for _ in 0..10 {
            coarse.tick(0.05, &ctx, &world, &mut coarse_mobs);
        }

        assert_eq!(fine.attempt_index, 2);
        assert_eq!(coarse.attempt_index, 2);
        assert_eq!(fine.report(), coarse.report());
    }

    #[test]
    fn report_add_sums_every_field() {
        let mut report = SpawnReport {
            spawned: 1,
            despawned: 2,
            burned: 3,
            cap_blocked: 4,
        };
        report.add(SpawnReport {
            spawned: 10,
            despawned: 20,
            burned: 30,
            cap_blocked: 40,
        });
        assert_eq!(
            report,
            SpawnReport {
                spawned: 11,
                despawned: 22,
                burned: 33,
                cap_blocked: 44,
            }
        );
    }

    #[test]
    fn region_buckets_are_floor_cells_at_the_origin_boundary() {
        assert_eq!(region_bucket(0.0, 0.0, SPAWN_REGION_BLOCKS), (0, 0));
        assert_eq!(region_bucket(31.9, 31.9, SPAWN_REGION_BLOCKS), (0, 0));
        assert_eq!(region_bucket(32.0, 32.0, SPAWN_REGION_BLOCKS), (1, 1));
        assert_eq!(region_bucket(-0.1, -0.1, SPAWN_REGION_BLOCKS), (-1, -1));
        assert_eq!(region_bucket(-32.0, -32.0, SPAWN_REGION_BLOCKS), (-1, -1));
    }

    #[test]
    fn spawn_runtime_default_is_enabled_with_a_zero_report() {
        let runtime = SpawnRuntime::default();
        assert!(runtime.enabled());
        assert_eq!(runtime.report(), SpawnReport::default());
        assert_eq!(runtime.attempt_index, 0);
    }

    #[test]
    fn tuned_spawn_cadence_and_cap_are_read_live() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);

        let mut tuned = SpawnRuntime::new();
        tuned.set_rules(SpawnRules {
            spawn_attempt_secs: 0.05,
            // Test-only sentinel: every attempt hits the global cap, so the
            // counters prove how many cadences actually fired.
            max_mobs_total: 0,
            ..SpawnRules::default()
        });
        let mut mobs = Vec::new();
        let delta = tuned.tick(0.1, &ctx, &world, &mut mobs);
        assert_eq!(
            delta.cap_blocked, 2,
            "0.1 s at a 0.05 s cadence must fire two attempts"
        );
        assert_eq!(tuned.attempt_index, 2);

        let mut default = SpawnRuntime::new();
        let default_delta = default.tick(0.1, &ctx, &world, &mut mobs);
        assert_eq!(
            default_delta.cap_blocked, 0,
            "the default 0.25 s cadence fires nothing in 0.1 s"
        );
        assert_eq!(default.attempt_index, 0);

        // The per-call budget is also live: one attempt per tick even though
        // 0.2 s at a 0.05 s cadence would otherwise fire four.
        let mut limited = SpawnRuntime::new();
        limited.set_rules(SpawnRules {
            spawn_attempt_secs: 0.05,
            max_mobs_total: 0,
            max_spawn_attempts_per_call: 1,
            ..SpawnRules::default()
        });
        let delta = limited.tick(0.2, &ctx, &world, &mut mobs);
        assert_eq!(delta.cap_blocked, 1, "the tuned per-call budget is one");
        assert_eq!(limited.attempt_index, 1);
    }

    #[test]
    fn tuned_light_thresholds_are_read_live() {
        let feet = Vec3::new(0.5, (SURFACE_Y + 1) as f32, 0.5);
        let ctx = SpawnContext {
            seed: 7,
            day_index: 0,
            time_of_day: 6_000,
            player: feet,
            world_spawn: feet,
        };
        let candidate = Vec3::new(30.5, feet.y, 0.5);

        // Light 7 is cave-dark by default, so the same stand spawns a hostile.
        let cave = FakeWorld::flat().with_default_light(HOSTILE_CAVE_LIGHT);
        let mut mobs = Vec::new();
        let mut report = SpawnReport::default();
        try_spawn_at(
            &ctx,
            &cave,
            &mut mobs,
            SpawnCandidate {
                feet: candidate,
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 1);
        assert!(mobs[0].kind.is_hostile());

        // Tuning the cave threshold below 7 makes the same stand ineligible.
        let mut mobs = Vec::new();
        let mut report = SpawnReport::default();
        try_spawn_at(
            &ctx,
            &cave,
            &mut mobs,
            SpawnCandidate {
                feet: candidate,
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules {
                hostile_cave_light: 0,
                ..SpawnRules::default()
            },
        );
        assert_eq!(report.spawned, 0, "light 7 must clear a 0 cave threshold");

        // Light 10 sits in the default twilight band. Lowering the passive
        // threshold to 10 makes the lit grass stand eligible at day.
        let twilight = FakeWorld::flat().with_default_light(10);
        let mut mobs = Vec::new();
        let mut report = SpawnReport::default();
        try_spawn_at(
            &ctx,
            &twilight,
            &mut mobs,
            SpawnCandidate {
                feet: candidate,
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules::default(),
        );
        assert_eq!(report.spawned, 0, "default twilight stays ineligible");

        let mut mobs = Vec::new();
        let mut report = SpawnReport::default();
        try_spawn_at(
            &ctx,
            &twilight,
            &mut mobs,
            SpawnCandidate {
                feet: candidate,
                yaw: 0.0,
                attempt_index: 0,
            },
            &mut report,
            &SpawnRules {
                passive_surface_light: 10,
                ..SpawnRules::default()
            },
        );
        assert_eq!(report.spawned, 1, "light 10 meets a 10 passive threshold");
        assert!(mobs[0].kind.is_passive());
    }

    #[test]
    fn tuned_despawn_radius_marks_a_nearer_record() {
        // Light 10 isolates the cull from any spawn attempt.
        let world = FakeWorld::flat().with_default_light(10);
        let ctx = flat_ctx(6_000);

        let mut tuned = SpawnRuntime::new();
        tuned.set_enabled(false);
        tuned.set_rules(SpawnRules {
            despawn_radius_blocks: 8,
            ..SpawnRules::default()
        });
        let mut mobs = vec![Mob::new(MobKind::Pig, feet_offset(10.0), 0.0)];
        let delta = tuned.tick(DESPAWN_SCAN_SECS, &ctx, &world, &mut mobs);
        assert_eq!(
            delta.despawned, 1,
            "10 blocks out is beyond an 8-block cull"
        );
        assert!(mobs[0].despawn_pending);

        let mut default = SpawnRuntime::new();
        default.set_enabled(false);
        let mut kept = vec![Mob::new(MobKind::Pig, feet_offset(10.0), 0.0)];
        let delta = default.tick(DESPAWN_SCAN_SECS, &ctx, &world, &mut kept);
        assert_eq!(delta.despawned, 0, "the default 96-block radius keeps it");
        assert!(!kept[0].despawn_pending);
    }

    #[test]
    fn tuned_ring_and_region_geometry_are_read_live() {
        let ctx = flat_ctx(6_000);
        let rules = SpawnRules {
            safe_radius_blocks: 8,
            spawn_max_radius_blocks: 12,
            spawn_region_blocks: 16,
            ..SpawnRules::default()
        };
        for attempt in 0..500u64 {
            let (_, radius) = ring_sample(&ctx, attempt, &rules);
            assert!(
                (8.0..=12.0).contains(&radius),
                "tuned radius {radius} escaped [8, 12] at {attempt}"
            );
        }
        let default = SpawnRules::default();
        for attempt in 0..500u64 {
            let (_, radius) = ring_sample(&ctx, attempt, &default);
            assert!((SAFE_RADIUS..=SPAWN_MAX_RADIUS).contains(&radius));
        }
        assert_eq!(region_bucket(20.0, 0.0, rules.spawn_region_blocks), (1, 0));
        assert_eq!(
            region_bucket(20.0, 0.0, default.spawn_region_blocks),
            (0, 0)
        );
    }

    #[test]
    fn set_rules_preserves_the_running_accumulation_phase() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        runtime.set_rules(SpawnRules {
            max_mobs_total: 0,
            ..SpawnRules::default()
        });
        let mut mobs = Vec::new();

        // Carry 0.2 s of the 0.25 s window with no attempt yet.
        let first = runtime.tick(0.2, &ctx, &world, &mut mobs);
        assert_eq!(runtime.attempt_index, 0);
        assert_eq!(first.cap_blocked, 0);

        // A same-rules reload mid-window must not reset the accumulator: the
        // next 0.05 s reaches the retained 0.25 s boundary and fires.
        let rules = runtime.rules();
        runtime.set_rules(rules);
        let second = runtime.tick(0.05, &ctx, &world, &mut mobs);
        assert_eq!(runtime.attempt_index, 1, "the retained phase must fire now");
        assert_eq!(second.cap_blocked, 1);
        assert!(runtime.enabled(), "set_rules must not toggle the gate");
        assert_eq!(
            runtime.report(),
            second,
            "set_rules must not clear the report"
        );
    }

    // ---- S05-T02: despawn marking and daylight burning ------------------

    /// Feet position `dx` blocks east of the flat-context player, on the
    /// synthetic plain's walking level.
    fn feet_offset(dx: f32) -> Vec3 {
        Vec3::new(0.5 + dx, (SURFACE_Y + 1) as f32, 0.5)
    }

    #[test]
    fn despawn_scan_marks_only_beyond_the_radius_and_never_removes() {
        // Light 10 makes every spawn attempt ineligible, isolating the cull.
        let world = FakeWorld::flat().with_default_light(10);
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = vec![
            Mob::new(MobKind::Pig, feet_offset(DESPAWN_RADIUS - 0.5), 0.0),
            Mob::new(MobKind::Pig, feet_offset(DESPAWN_RADIUS + 0.5), 0.0),
            // Directly below the player: horizontal distance is zero, so a
            // cave mob must never be culled just because `y` differs.
            Mob::new(MobKind::Zombie, Vec3::new(0.5, -60.0, 0.5), 0.0),
        ];

        let first = runtime.tick(DESPAWN_SCAN_SECS, &ctx, &world, &mut mobs);
        assert!(
            !mobs[0].despawn_pending,
            "{DESPAWN_RADIUS}-0.5 blocks must be kept"
        );
        assert!(
            mobs[1].despawn_pending,
            "{DESPAWN_RADIUS}+0.5 blocks must be marked"
        );
        assert!(
            !mobs[2].despawn_pending,
            "a mob straight below the player is not far horizontally"
        );
        assert_eq!(first.despawned, 1);
        assert_eq!(
            mobs.len(),
            3,
            "the runtime only marks; removal belongs to retain_live_mobs"
        );

        // A second scan cannot count the same record twice.
        let second = runtime.tick(DESPAWN_SCAN_SECS, &ctx, &world, &mut mobs);
        assert_eq!(second.despawned, 0, "a marked record is already counted");
        assert!(mobs[1].despawn_pending);
        assert_eq!(mobs.len(), 3);
    }

    #[test]
    fn despawn_scan_is_distance_only_for_unready_unloaded_columns() {
        let mut world = FakeWorld::flat().with_default_light(10);
        // No resident column and no ground anywhere: any world read would
        // fail the test by refusing to mark the record.
        world.ready = Some(HashSet::new());
        world.half_extent = 0;
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = vec![Mob::new(MobKind::Cow, Vec3::new(500.5, -40.0, 500.5), 0.0)];

        let delta = runtime.tick(DESPAWN_SCAN_SECS, &ctx, &world, &mut mobs);
        assert_eq!(delta.despawned, 1);
        assert!(mobs[0].despawn_pending);
    }

    #[test]
    fn disabled_runtime_still_culls_but_never_spawns_or_burns() {
        let world = FakeWorld::flat();
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        runtime.set_enabled(false);
        let mut mobs = vec![
            // Daytime, full light, lit surface: would burn if enabled.
            Mob::new(MobKind::Zombie, feet_offset(30.0), 0.0),
            // Far enough to be culled.
            Mob::new(MobKind::Pig, feet_offset(DESPAWN_RADIUS + 0.5), 0.0),
        ];
        let expected_health = MobKind::Zombie.max_health();

        let mut total = SpawnReport::default();
        for _ in 0..20 {
            total.add(runtime.tick(BURN_INTERVAL_SECS, &ctx, &world, &mut mobs));
        }

        assert!(mobs[1].despawn_pending, "the cull is never gated");
        assert_eq!(total.despawned, 1);
        assert_eq!(total.burned, 0, "disabled means no burn damage");
        assert_eq!(total.spawned, 0, "disabled means no creation");
        assert_eq!(mobs[0].health, expected_health);
        assert_eq!(mobs.len(), 2);
        assert_eq!(runtime.attempt_index, 0, "no attempt may even be rolled");
        assert!(!runtime.enabled());
    }

    #[test]
    fn daylight_burning_deals_one_hp_per_cadence_and_kills_in_ten_seconds() {
        // A stone floor keeps the day spawn rules from adding passives.
        let mut world = FakeWorld::flat();
        world.surface_block = BlockId::Stone;
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mobs = vec![Mob::new(MobKind::Zombie, feet_offset(30.0), 0.0)];
        assert_eq!(mobs[0].health, 20);

        // 19 cadences take 20 hp down to exactly 1 hp, one per 0.5 s.
        for i in 0..19 {
            let delta = runtime.tick(BURN_INTERVAL_SECS, &ctx, &world, &mut mobs);
            assert_eq!(delta.burned, 0, "burn {i} must not be lethal");
            assert_eq!(mobs[0].health, 19 - i as u16);
        }
        assert_eq!(mobs[0].health, 1);

        // The twentieth cadence is 10 s of daylight: 20 hp / 20 cadences.
        let lethal = runtime.tick(BURN_INTERVAL_SECS, &ctx, &world, &mut mobs);
        assert_eq!(lethal.burned, 1);
        assert_eq!(mobs[0].health, 0);
        assert!(mobs[0].death > 0.0, "Mob::kill must arm the 0.8 s collapse");
        assert!(
            !mobs[0].despawn_pending,
            "burning is not a despawn; only the cull sets the marker"
        );
        assert_eq!(runtime.report().burned, 1);

        // A corpse is never counted twice.
        let after = runtime.tick(BURN_INTERVAL_SECS, &ctx, &world, &mut mobs);
        assert_eq!(after.burned, 0);
    }

    /// Runs 20 s of enabled ticks and asserts the hostile never loses hp.
    fn assert_never_burns(world: &FakeWorld, ctx: &SpawnContext, position: Vec3) {
        let mut runtime = SpawnRuntime::new();
        let mut mobs = vec![Mob::new(MobKind::Skeleton, position, 0.0)];
        let expected = MobKind::Skeleton.max_health();
        for _ in 0..40 {
            let delta = runtime.tick(BURN_INTERVAL_SECS, ctx, world, &mut mobs);
            assert_eq!(delta.burned, 0);
            assert_eq!(mobs[0].health, expected);
        }
        assert_eq!(runtime.report().burned, 0);
    }

    #[test]
    fn cave_light_night_twilight_and_water_never_burn() {
        let position = feet_offset(30.0);
        let day = flat_ctx(6_000);
        let night = flat_ctx(18_000);

        // Cave-dark: hostiles spawn there but never burn.
        let cave = FakeWorld::flat().with_default_light(HOSTILE_CAVE_LIGHT);
        assert_never_burns(&cave, &day, position);

        // Twilight band sits below full sky light.
        let twilight = FakeWorld::flat().with_default_light(PASSIVE_SURFACE_LIGHT - 1);
        assert_never_burns(&twilight, &day, position);

        // Full light but the clock says night.
        let lit = FakeWorld::flat();
        assert_never_burns(&lit, &night, position);

        // Full light, day, but the feet are submerged.
        let mut water = FakeWorld::flat();
        water.blocks.insert(
            (
                position.x.floor() as i32,
                position.y as i32,
                position.z.floor() as i32,
            ),
            BlockId::Water,
        );
        assert_never_burns(&water, &day, position);
    }

    #[test]
    fn an_unready_column_never_burns_even_in_full_light() {
        let mut world = FakeWorld::flat();
        world.ready = Some(HashSet::new());
        let ctx = flat_ctx(6_000);
        assert_never_burns(&world, &ctx, feet_offset(30.0));
    }

    #[test]
    fn a_dead_record_is_never_burned_twice() {
        let mut world = FakeWorld::flat();
        world.surface_block = BlockId::Stone;
        let ctx = flat_ctx(6_000);
        let mut runtime = SpawnRuntime::new();
        let mut mob = Mob::new(MobKind::Zombie, feet_offset(30.0), 0.0);
        assert!(mob.kill());
        mob.death = crate::world::mob::MOB_DEATH_SECS * 0.5;
        let death_before = mob.death;
        let mut mobs = vec![mob];

        for _ in 0..40 {
            assert_eq!(
                runtime
                    .tick(BURN_INTERVAL_SECS, &ctx, &world, &mut mobs)
                    .burned,
                0
            );
        }

        assert_eq!(mobs[0].health, 0);
        assert_eq!(
            mobs[0].death, death_before,
            "a corpse must never be re-killed"
        );
        assert_eq!(runtime.report().burned, 0);
    }

    #[test]
    fn despawn_radius_stays_inside_the_minimum_render_distance() {
        let min_render_blocks = crate::settings::RENDER_DISTANCE_MIN * CHUNK_SIZE as i32;
        assert!(
            DESPAWN_RADIUS_BLOCKS < min_render_blocks,
            "despawn radius {DESPAWN_RADIUS_BLOCKS} blocks must stay inside the shortest \
             rendered distance {min_render_blocks} blocks, or a culled record could still be \
             visible"
        );
    }

    #[test]
    fn despawn_marker_defaults_false_and_never_reaches_the_wire() {
        let feet = Vec3::new(3.5, 70.0, -2.5);
        assert!(!Mob::new(MobKind::Pig, feet, 0.0).despawn_pending);
        assert!(!Mob::restored(MobKind::Zombie, feet, 0.0, 4).despawn_pending);

        let mut mob = Mob::new(MobKind::Zombie, feet, 1.5);
        mob.despawn_pending = true;
        let saved = SavedMob::from(&mob);
        assert_eq!(saved.kind, mob.kind);
        assert_eq!(saved.position, [3.5, 70.0, -2.5]);
        assert_eq!(saved.yaw, 1.5);
        assert_eq!(saved.health, mob.health);

        // The wire value is identical with or without the ephemeral marker.
        let mut clean = mob.clone();
        clean.despawn_pending = false;
        assert_eq!(SavedMob::from(&clean), saved);
    }

    // ---- S05-T04: headless harness over a synthetic lit WorldManager -----

    /// Fixed seed for every harness pin.
    const HARNESS_SEED: u32 = 12_345;
    /// Mid-night tick: the sun is below the horizon and surface sky light is
    /// still 15, so a lit grass plain produces surface hostiles.
    const HARNESS_NIGHT_TICK: u32 = 18_000;
    /// Morning tick: daylight, so surface hostiles burn and passives spawn.
    const HARNESS_DAY_TICK: u32 = 6_000;

    // Pins measured on 2026-11-01 by these very tests. A change here means the
    // spawn mixer, an eligibility rule, or the world/light fixture moved.
    const NIGHT_SPAWN_PIN: u32 = 8;
    const NIGHT_SPAWN_MOB_PIN: usize = 8;
    const NIGHT_CAP_SPAWN_PIN: u32 = 32;
    const DAY_SPAWN_PIN: u32 = 8;

    /// Carves the 3x3x2 cave air pocket at `surface - 10 ..= surface - 9` into
    /// whichever chunk of the column it falls in.
    fn carve_cave_into_chunk(
        chunk: &mut Chunk,
        cx: i32,
        cz: i32,
        cy: i32,
        cave_x: i32,
        cave_z: i32,
        surface: i32,
    ) {
        let cs = CHUNK_SIZE as i32;
        let base_y = cy * cs;
        for dx in -1..=1 {
            for dz in -1..=1 {
                let wx = cave_x + dx;
                let wz = cave_z + dz;
                if wx.div_euclid(cs) != cx || wz.div_euclid(cs) != cz {
                    continue;
                }
                for dy in 0..=1 {
                    let ly = surface - 10 + dy - base_y;
                    if !(0..cs).contains(&ly) {
                        continue;
                    }
                    chunk.set(
                        wx.rem_euclid(cs) as usize,
                        ly as usize,
                        wz.rem_euclid(cs) as usize,
                        BlockId::Air,
                    );
                }
            }
        }
    }

    /// Builds a 5x5 chunk-column lit grass plain centred on the seeded spawn
    /// column with the grass surface at `spawn_position(seed).y - 4` (the +4
    /// spawn rule). That puts the cached `world_spawn` three blocks above the
    /// player's feet on the same column, so the world-spawn safe radius is a
    /// real constraint rather than a no-op.
    ///
    /// `cave` carves a 3x3x2 dark pocket ten blocks under the surface; when
    /// `recompute_light` is false no light grids exist, which is the
    /// `light_at_world` 15-default negative control.
    fn build_flat_world(
        seed: u32,
        recompute_light: bool,
        cave: Option<(i32, i32)>,
    ) -> (WorldManager, Vec3) {
        let anchor = WorldSession::spawn_position(seed);
        let cs = CHUNK_SIZE as i32;
        let cx0 = (anchor.x.floor() as i32).div_euclid(cs);
        let cz0 = (anchor.z.floor() as i32).div_euclid(cs);
        let surface = anchor.y as i32 - 4;
        let surface_chunk_y = surface.div_euclid(cs);
        let surface_local = surface.rem_euclid(cs) as usize;

        let mut world = WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default());
        for cx in (cx0 - 2)..=(cx0 + 2) {
            for cz in (cz0 - 2)..=(cz0 + 2) {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let mut chunk = if cy < surface_chunk_y {
                        Chunk::new_uniform(BlockId::Stone)
                    } else if cy == surface_chunk_y {
                        let mut chunk = Chunk::new_uniform(BlockId::Air);
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                for ly in 0..=surface_local {
                                    chunk.set(lx, ly, lz, BlockId::Stone);
                                }
                                chunk.set(lx, surface_local, lz, BlockId::Grass);
                            }
                        }
                        chunk
                    } else {
                        Chunk::new_uniform(BlockId::Air)
                    };
                    if let Some((cave_x, cave_z)) = cave {
                        carve_cave_into_chunk(&mut chunk, cx, cz, cy, cave_x, cave_z, surface);
                    }
                    world.insert_chunk_for_test(ChunkPos::new(cx, cy, cz), chunk);
                }
            }
        }
        if recompute_light {
            world.recompute_light_for_test();
        }
        (world, Vec3::new(anchor.x, (surface + 1) as f32, anchor.z))
    }

    fn flat_world(seed: u32) -> (WorldManager, Vec3) {
        build_flat_world(seed, true, None)
    }

    fn harness_session_from(world: WorldManager, feet: Vec3, seed: u32) -> WorldSession {
        let mut session = WorldSession::new_session_with_mode(
            seed,
            PathBuf::from("/tmp/spawn-harness.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        session.world = world;
        session.player.position = feet;
        session.sync_camera();
        assert_eq!(
            session.world_spawn(),
            WorldSession::spawn_position(seed),
            "the cached anchor must stay the seeded world spawn"
        );
        session
    }

    fn harness_session(seed: u32) -> WorldSession {
        let (world, feet) = flat_world(seed);
        harness_session_from(world, feet, seed)
    }

    fn run_ticks(session: &mut WorldSession, ticks: u32, secs: f32) {
        for _ in 0..ticks {
            session.tick_mobs(secs, 1.0);
        }
    }

    /// Advances the session but pins every body back onto its spawn anchor
    /// after each tick. The per-region cap is enforced on *current* positions
    /// while wander can legally move a body across a cell boundary after it
    /// spawned, so this neutralizes AI drift to isolate the spawner's
    /// placement invariant.
    fn run_frozen_ticks(session: &mut WorldSession, ticks: u32, secs: f32) {
        for _ in 0..ticks {
            session.tick_mobs(secs, 1.0);
            for mob in session.mobs_mut() {
                mob.position = mob.home;
                mob.velocity = Vec3::ZERO;
            }
        }
    }

    fn hdist(a: Vec3, b: Vec3) -> f32 {
        horizontal_distance_sq(a, b).sqrt()
    }

    /// Per-region bucket counts are asserted against `home`, the immutable
    /// spawn anchor: the spawner owns placement, while later wander may move a
    /// body across a cell boundary.
    fn assert_region_caps(session: &WorldSession) {
        let mut buckets: HashMap<(i32, i32), usize> = HashMap::new();
        for mob in session.mobs() {
            *buckets
                .entry(region_bucket(mob.home.x, mob.home.z, SPAWN_REGION_BLOCKS))
                .or_default() += 1;
        }
        for (bucket, count) in buckets {
            assert!(
                count <= SPAWN_REGION_CAP,
                "region {bucket:?} holds {count} records, cap is {SPAWN_REGION_CAP}"
            );
        }
    }

    #[test]
    fn night_spawn_pin() {
        let mut session = harness_session(HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        // 40 x 0.05 s = 2.0 s, exactly 8 spawn attempts at 0.25 s.
        run_ticks(&mut session, 40, 0.05);

        let report = session.spawn_report();
        eprintln!(
            "night_spawn_pin: spawned={} mobs={} cap_blocked={}",
            report.spawned,
            session.mobs().len(),
            report.cap_blocked
        );
        assert_eq!(
            report.spawned, NIGHT_SPAWN_PIN,
            "measured night spawn count moved"
        );
        assert_eq!(session.mobs().len(), NIGHT_SPAWN_MOB_PIN);

        let player = session.player.position;
        let world_spawn = session.world_spawn();
        for mob in session.mobs() {
            assert!(
                matches!(mob.kind, MobKind::Zombie | MobKind::Skeleton),
                "night spawned {:?}",
                mob.kind
            );
            let from_player = hdist(mob.home, player);
            assert!(
                (SAFE_RADIUS..=SPAWN_MAX_RADIUS).contains(&from_player),
                "{:?} home {from_player} outside [{SAFE_RADIUS}, {SPAWN_MAX_RADIUS}]",
                mob.kind
            );
            let from_world_spawn = hdist(mob.home, world_spawn);
            assert!(
                from_world_spawn >= SAFE_RADIUS,
                "{:?} home {from_world_spawn} inside the world-spawn safe radius",
                mob.kind
            );
            let (cx, cz) = (
                (mob.home.x.floor() as i32).div_euclid(CHUNK_SIZE as i32),
                (mob.home.z.floor() as i32).div_euclid(CHUNK_SIZE as i32),
            );
            assert!(
                session.world.has_column_light(cx, cz),
                "{:?} spawned on an unlit column ({cx}, {cz})",
                mob.kind
            );
        }
        assert_region_caps(&session);
    }

    #[test]
    fn night_cap_pin() {
        let mut session = harness_session(HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        // 400 x 0.05 s = 20 s = 80 attempts, enough to fill the global budget.
        // Bodies are pinned to their anchors so the per-region assertion
        // measures placement, not later wander.
        run_frozen_ticks(&mut session, 400, 0.05);

        let report = session.spawn_report();
        eprintln!(
            "night_cap_pin: spawned={} mobs={} cap_blocked={}",
            report.spawned,
            session.mobs().len(),
            report.cap_blocked
        );
        assert_eq!(report.spawned, NIGHT_CAP_SPAWN_PIN);
        assert_eq!(session.mobs().len(), MAX_MOBS_TOTAL);
        assert!(report.cap_blocked > 0, "a full list must record blocks");
        assert_region_caps(&session);
    }

    #[test]
    fn day_spawn_pin() {
        let mut session = harness_session(HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_DAY_TICK);
        run_ticks(&mut session, 40, 0.05);

        let report = session.spawn_report();
        eprintln!(
            "day_spawn_pin: spawned={} mobs={}",
            report.spawned,
            session.mobs().len()
        );
        assert_eq!(report.spawned, DAY_SPAWN_PIN);
        assert_eq!(session.mobs().len(), DAY_SPAWN_PIN as usize);
        for mob in session.mobs() {
            assert!(mob.kind.is_passive(), "day spawned {:?}", mob.kind);
            let feet = IVec3::new(
                mob.home.x.floor() as i32,
                mob.home.y.floor() as i32,
                mob.home.z.floor() as i32,
            );
            assert_eq!(
                session.world.get_block(feet.x, feet.y - 1, feet.z),
                BlockId::Grass,
                "{:?} does not stand on grass",
                mob.kind
            );
            assert_eq!(
                session.world.light_at_world(feet.x, feet.y, feet.z),
                PASSIVE_SURFACE_LIGHT,
                "{:?} does not stand in full sky light",
                mob.kind
            );
        }
        assert_region_caps(&session);
    }

    #[test]
    fn despawn_pin() {
        let mut session = harness_session(HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        run_ticks(&mut session, 40, 0.05);
        let initial = session.mobs().len();
        assert!(initial > 0, "the despawn proof needs a night population");

        // Freeze creation/burning so the only list change is the cull. The
        // cull is deliberately ungated, so this still exercises the one reap
        // site inside `tick_mobs`.
        session.set_ambient_spawning(false);
        let far = session.player.position + Vec3::new(DESPAWN_RADIUS + 8.0, 0.0, 0.0);
        session.mobs_mut()[0].position = far;
        session.mobs_mut()[0].velocity = Vec3::ZERO;

        // 22 x 0.05 s = 1.1 s, just past one 1.0 s despawn scan.
        run_ticks(&mut session, 22, 0.05);

        assert_eq!(
            session.mobs().len(),
            initial - 1,
            "exactly one record must leave the list"
        );
        assert_eq!(
            session.spawn_report().despawned,
            1,
            "the scan counts exactly one mark"
        );
        assert!(
            session.mobs().iter().all(|mob| mob.position != far),
            "the culled record is still in the list"
        );
    }

    #[test]
    fn burn_pin() {
        let mut session = harness_session(HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        run_ticks(&mut session, 40, 0.05);

        let hostiles = session.mobs().len();
        assert!(hostiles > 0, "the burn proof needs a night population");
        assert!(session.mobs().iter().all(|mob| mob.kind.is_hostile()));
        let before = session.spawn_report().burned;

        session.clock = GameClock::from_time(HARNESS_DAY_TICK);
        // 300 x 0.05 s = 15 s: 10 s to burn 20 hp, then the 0.8 s collapse.
        run_ticks(&mut session, 300, 0.05);

        let report = session.spawn_report();
        eprintln!(
            "burn_pin: hostiles={hostiles} burned={} remaining={}",
            report.burned - before,
            session.mobs().len()
        );
        assert_eq!(
            report.burned - before,
            hostiles as u32,
            "every night hostile must burn exactly once"
        );
        assert!(
            session.mobs().iter().all(|mob| mob.kind.is_passive()),
            "a surface hostile survived 15 s of daylight"
        );
    }

    fn night_sequence(seed: u32) -> WorldSession {
        let mut session = harness_session(seed);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        run_ticks(&mut session, 40, 0.05);
        session
    }

    #[test]
    fn two_identical_runs_match() {
        let left = night_sequence(HARNESS_SEED);
        let right = night_sequence(HARNESS_SEED);

        assert_eq!(left.spawn_report(), right.spawn_report());
        assert_eq!(left.mobs().len(), right.mobs().len());
        for (left, right) in left.mobs().iter().zip(right.mobs()) {
            assert_eq!(left.kind, right.kind);
            assert_eq!(left.home, right.home, "spawn anchors must match exactly");
            assert_eq!(left.yaw, right.yaw, "deterministic yaw must match");
            assert!(
                (left.position - right.position).length() < 1e-4,
                "wander positions diverged: {:?} vs {:?}",
                left.position,
                right.position
            );
        }
    }

    #[test]
    fn no_light_means_no_spawns() {
        // No light grids: `light_at_world` falls back to 15, so only the
        // `column_ready` gate can keep this at zero spawns.
        let (world, feet) = build_flat_world(HARNESS_SEED, false, None);
        let mut session = harness_session_from(world, feet, HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        run_ticks(&mut session, 400, 0.05);

        assert!(session.mobs().is_empty(), "an unlit column must not spawn");
        assert_eq!(session.spawn_report(), SpawnReport::default());
    }

    #[test]
    fn unlit_enclosed_world_spawns_nothing() {
        let anchor = WorldSession::spawn_position(HARNESS_SEED);
        let cs = CHUNK_SIZE as i32;
        let cx0 = (anchor.x.floor() as i32).div_euclid(cs);
        let cz0 = (anchor.z.floor() as i32).div_euclid(cs);
        let mut world =
            WorldManager::with_seed_and_edits(16, HARNESS_SEED, ChunkEditMap::default());
        for cx in (cx0 - 2)..=(cx0 + 2) {
            for cz in (cz0 - 2)..=(cz0 + 2) {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    world.insert_chunk_for_test(
                        ChunkPos::new(cx, cy, cz),
                        Chunk::new_uniform(BlockId::Stone),
                    );
                }
            }
        }
        world.recompute_light_for_test();
        let feet = Vec3::new(anchor.x, (anchor.y as i32 - 3) as f32, anchor.z);
        let mut session = harness_session_from(world, feet, HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        run_ticks(&mut session, 400, 0.05);

        assert!(
            session.mobs().is_empty(),
            "a sealed stone world has no stand"
        );
        assert_eq!(session.spawn_report(), SpawnReport::default());
    }

    #[test]
    fn cave_hostile_does_not_burn() {
        let anchor = WorldSession::spawn_position(HARNESS_SEED);
        let surface = anchor.y as i32 - 4;
        let cave_x = anchor.x.floor() as i32 + 30;
        let cave_z = anchor.z.floor() as i32;
        let (world, feet) = build_flat_world(HARNESS_SEED, true, Some((cave_x, cave_z)));
        let mut session = harness_session_from(world, feet, HARNESS_SEED);

        let cave_feet = Vec3::new(
            cave_x as f32 + 0.5,
            (surface - 10) as f32,
            cave_z as f32 + 0.5,
        );
        assert!(
            session.world.light_at_world(cave_x, surface - 10, cave_z) <= HOSTILE_CAVE_LIGHT,
            "the carved pocket must be cave-dark"
        );

        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        run_ticks(&mut session, 40, 0.05);
        let surface_hostiles = session.mobs().len();
        assert!(surface_hostiles > 0);
        assert!(session.mobs().iter().all(|mob| mob.kind.is_hostile()));

        let cave_health = MobKind::Zombie.max_health();
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Zombie, cave_feet, 0.0));

        session.clock = GameClock::from_time(HARNESS_DAY_TICK);
        run_ticks(&mut session, 300, 0.05);

        let report = session.spawn_report();
        assert_eq!(
            report.burned, surface_hostiles as u32,
            "only the surface hostiles may burn"
        );
        let cave = session
            .mobs()
            .iter()
            .find(|mob| mob.home == cave_feet)
            .expect("the cave hostile must survive daylight");
        assert_eq!(
            cave.health, cave_health,
            "the cave hostile took burn damage"
        );
        assert_eq!(
            session
                .mobs()
                .iter()
                .filter(|mob| mob.kind.is_hostile())
                .count(),
            1,
            "only the cave hostile may remain"
        );
    }

    #[test]
    fn world_spawn_exclusion_is_real() {
        let mut session = harness_session(HARNESS_SEED);
        session.clock = GameClock::from_time(HARNESS_NIGHT_TICK);
        let world_spawn = session.world_spawn();
        // Shift the player 40 blocks east: part of the ring now falls inside
        // the world-spawn safe radius and must be excluded, while the rest
        // still populates.
        session.player.position = world_spawn + Vec3::new(40.0, 0.0, 0.0);
        session.sync_camera();
        let player = session.player.position;

        run_ticks(&mut session, 400, 0.05);

        let report = session.spawn_report();
        assert!(report.spawned > 0, "the shifted ring must still populate");
        for mob in session.mobs() {
            let from_spawn = hdist(mob.home, world_spawn);
            assert!(
                from_spawn >= SAFE_RADIUS,
                "{:?} home {from_spawn} inside the world-spawn safe radius",
                mob.kind
            );
            let from_player = hdist(mob.home, player);
            assert!(
                (SAFE_RADIUS..=SPAWN_MAX_RADIUS).contains(&from_player),
                "{:?} home {from_player} outside the spawn ring",
                mob.kind
            );
        }
    }
}
