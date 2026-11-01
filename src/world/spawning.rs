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
        }
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

        // Ungated by design: a cull can only mark a record already outside
        // the radius, so it can never move a deterministic demo's mob list,
        // and leaving it ungated means the demo's despawn proof cannot be
        // silently disabled by the seam.
        self.despawn_accum += game_dt;
        while self.despawn_accum + SPAWN_ACCUM_EPSILON >= DESPAWN_SCAN_SECS {
            self.despawn_accum = (self.despawn_accum - DESPAWN_SCAN_SECS).max(0.0);
            scan_despawn(ctx, mobs, &mut delta);
        }

        if self.enabled {
            self.burn_accum += game_dt;
            while self.burn_accum + SPAWN_ACCUM_EPSILON >= BURN_INTERVAL_SECS {
                self.burn_accum = (self.burn_accum - BURN_INTERVAL_SECS).max(0.0);
                scan_burn(ctx, world, mobs, &mut delta);
            }

            self.attempt_accum += game_dt;
            let mut executed = 0;
            while executed < MAX_SPAWN_ATTEMPTS_PER_CALL
                && self.attempt_accum + SPAWN_ACCUM_EPSILON >= SPAWN_ATTEMPT_SECS
            {
                self.attempt_accum = (self.attempt_accum - SPAWN_ATTEMPT_SECS).max(0.0);
                attempt_spawn(ctx, world, mobs, self.attempt_index, &mut delta);
                self.attempt_index = self.attempt_index.wrapping_add(1);
                executed += 1;
            }
            // Only a call whose attempt budget ran out can leave whole
            // intervals queued. Clamp that leftover to half an interval so
            // the next call cannot immediately fire, while a normal small
            // frame delta is carried untouched.
            if self.attempt_accum + SPAWN_ACCUM_EPSILON >= SPAWN_ATTEMPT_SECS {
                self.attempt_accum = SPAWN_ATTEMPT_SECS * 0.5;
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
fn ring_sample(ctx: &SpawnContext, attempt_index: u64) -> (f32, f32) {
    let roll = spawn_roll(
        ctx.seed,
        ctx.day_index,
        ctx.time_of_day,
        attempt_index,
        SLOT_SHAPE,
    );
    let angle = (roll & 0xFFFF) as f32 / 65_535.0 * std::f32::consts::TAU;
    let radius_fraction = ((roll >> 16) & 0xFFFF) as f32 / 65_535.0;
    let radius = SAFE_RADIUS + radius_fraction * (SPAWN_MAX_RADIUS - SAFE_RADIUS);
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
) {
    // The global cap is checked before any world work: a full list makes the
    // roll and the y scan pure waste.
    if mobs.len() >= MAX_MOBS_TOTAL {
        report.cap_blocked += 1;
        return;
    }

    let (angle, radius) = ring_sample(ctx, attempt_index);
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
        Vec3::new(feet_x, feet_y as f32, feet_z),
        angle,
        attempt_index,
        report,
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

/// The 32-block spawn region cell containing a horizontal position.
fn region_bucket(x: f32, z: f32) -> (i32, i32) {
    (
        (x.floor() as i32).div_euclid(SPAWN_REGION_BLOCKS),
        (z.floor() as i32).div_euclid(SPAWN_REGION_BLOCKS),
    )
}

/// How many records already live in `bucket`, resident or not.
fn region_count(mobs: &[Mob], bucket: (i32, i32)) -> usize {
    mobs.iter()
        .filter(|mob| region_bucket(mob.position.x, mob.position.z) == bucket)
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
fn scan_despawn(ctx: &SpawnContext, mobs: &mut [Mob], report: &mut SpawnReport) {
    let radius_sq = DESPAWN_RADIUS * DESPAWN_RADIUS;
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
        if world.sky_light(feet) != PASSIVE_SURFACE_LIGHT {
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
    feet: Vec3,
    yaw: f32,
    attempt_index: u64,
    report: &mut SpawnReport,
) {
    let feet_block = IVec3::new(feet.x.floor() as i32, feet.y as i32, feet.z.floor() as i32);
    if world.block(feet_block) != BlockId::Air {
        return;
    }
    if world.block(feet_block + IVec3::Y) != BlockId::Air {
        return;
    }
    let below = world.block(feet_block - IVec3::Y);

    let light = world.sky_light(feet_block);
    let kind = if light <= HOSTILE_CAVE_LIGHT {
        // Cave-dark at any hour.
        hostile_kind(ctx, attempt_index)
    } else if light == PASSIVE_SURFACE_LIGHT && daycycle::is_night(ctx.time_of_day) {
        // Night on the lit surface.
        hostile_kind(ctx, attempt_index)
    } else if light == PASSIVE_SURFACE_LIGHT
        && below == BlockId::Grass
        && daycycle::is_day(ctx.time_of_day)
    {
        passive_kind(ctx, attempt_index)
    } else {
        // Light 8..=14 (twilight band), unlit surfaces at day, and every
        // non-grass floor are ineligible.
        return;
    };

    // The safe ring around the player is inclusive at both ends; the world
    // spawn is protected by its own radius so a wanderer cannot be ambushed
    // on the spawn pad.
    let player_distance_sq = horizontal_distance_sq(feet, ctx.player);
    if !(SAFE_RADIUS * SAFE_RADIUS..=SPAWN_MAX_RADIUS * SPAWN_MAX_RADIUS)
        .contains(&player_distance_sq)
    {
        return;
    }
    if horizontal_distance_sq(feet, ctx.world_spawn) < SAFE_RADIUS * SAFE_RADIUS {
        return;
    }

    // Two records must never share a position: the v7 save fails closed on a
    // duplicate record, and a stacked pair would be indistinguishable.
    let min_sep_sq = SPAWN_MIN_SEPARATION * SPAWN_MIN_SEPARATION;
    if mobs
        .iter()
        .any(|mob| horizontal_distance_sq(mob.position, feet) < min_sep_sq)
    {
        return;
    }

    let bucket = region_bucket(feet.x, feet.z);
    if region_count(mobs, bucket) >= SPAWN_REGION_CAP {
        report.cap_blocked += 1;
        return;
    }

    mobs.push(Mob::new(kind, feet, yaw));
    report.spawned += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::persistence::SavedMob;
    use std::collections::{HashMap, HashSet};

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
            let (angle, radius) = ring_sample(&ctx, attempt);
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
        try_spawn_at(&ctx, &world, &mut mobs, candidate, 0.0, 0, &mut report);

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
            Vec3::new(30.5, feet.y, 0.5),
            0.0,
            0,
            &mut report,
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
        assert_eq!(region_count(&mobs, (0, 0)), SPAWN_REGION_CAP);

        let mut report = SpawnReport::default();
        // 30 blocks from the player: inside the ring, same cell as the pigs.
        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            Vec3::new(30.5, feet.y, 0.5),
            0.0,
            0,
            &mut report,
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
            Vec3::new(30.5, feet.y, 33.5),
            0.0,
            0,
            &mut report,
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

        try_spawn_at(&ctx, &world, &mut mobs, occupied, 0.0, 0, &mut report);
        assert_eq!(report.spawned, 0, "an exact duplicate must be rejected");
        assert_eq!(mobs.len(), 1);

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            Vec3::new(31.6, feet.y, 0.5),
            0.0,
            0,
            &mut report,
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
            Vec3::new(20.5, feet.y, 0.5),
            0.0,
            0,
            &mut report,
        );
        assert_eq!(report.spawned, 0, "inside the 24-block safe radius");

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            Vec3::new(52.5, feet.y, 0.5),
            0.0,
            0,
            &mut report,
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
            Vec3::new(30.5, feet.y, 0.5),
            0.0,
            0,
            &mut report,
        );
        assert_eq!(report.spawned, 0, "inside the world-spawn safe radius");

        try_spawn_at(
            &ctx,
            &world,
            &mut mobs,
            Vec3::new(30.5, feet.y, 0.5),
            0.0,
            0,
            &mut report,
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
        assert_eq!(region_bucket(0.0, 0.0), (0, 0));
        assert_eq!(region_bucket(31.9, 31.9), (0, 0));
        assert_eq!(region_bucket(32.0, 32.0), (1, 1));
        assert_eq!(region_bucket(-0.1, -0.1), (-1, -1));
        assert_eq!(region_bucket(-32.0, -32.0), (-1, -1));
    }

    #[test]
    fn spawn_runtime_default_is_enabled_with_a_zero_report() {
        let runtime = SpawnRuntime::default();
        assert!(runtime.enabled());
        assert_eq!(runtime.report(), SpawnReport::default());
        assert_eq!(runtime.attempt_index, 0);
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
}
