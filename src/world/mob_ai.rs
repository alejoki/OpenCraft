//! World-aware hostile steering (M009/S06-T01, ranged S08-T01).
//!
//! This module is the whole hostile brain: line-of-sight aggro, direct chase,
//! one-block step-up/down, a cliff guard, the stuck fallback, and (S08) the
//! skeleton's stand-off ranged decision with a gravity-compensated aim. It is
//! pure lib code — no wgpu, no winit, no `Instant`, no wall clock, and no
//! `crate::player::*` types beyond the shared voxel raycast. All of it is
//! headlessly testable on a synthetic block grid.
//!
//! ## The rules, and why
//!
//! * **Aggro range is 16 blocks horizontally and 8 vertically.** The demo's
//!   Fight/Drops stages park the non-Pig records 20 blocks behind the camera,
//!   so a 16-block aggro keeps the pinned stages quiet without a second gate.
//!   Aggro also requires a clear eye-to-eye ray and a live target; a
//!   `target == None` (Creative, gate off, dead player) stops a chase at
//!   once.
//! * **Chase is direct steering, never A\*.** Yaw 0 faces +Z (the mob module's
//!   convention), the speed is the kind's [`MobKind::walk_speed`], and the
//!   mob faces its target every fixed step.
//! * **Movement is one block up, one block down, else refuse.** A blocked
//!   candidate tries a one-block step-up; a candidate whose column has no
//!   support drops at most one block. A larger drop is refused (cliff guard),
//!   so a hostile never walks off a two-block ledge. Closing and retreating
//!   share the same mover ([`move_in`]) — and so do the passive wander/flee
//!   arms in [`crate::world::mob`], so every kind climbs, descends and collides
//!   with solid blocks identically.
//! * **Skeleton stand-off (S08).** A kind with a
//!   [`MobKind::ranged_profile`] never melees: beyond `fire_range` it closes,
//!   inside `min_range` it backs away, and in the band it stands still and
//!   fires. A shot needs line of sight, a live target within
//!   [`MOB_FIRE_VERTICAL`], a drained cooldown, and an open eye cell. The aim
//!   is gravity-compensated: it targets the player's eye plus the drop the
//!   arrow will accumulate over its flight time, so the shot arcs instead of
//!   landing short.
//! * **Stuck fallback.** A chase that makes less than
//!   [`MOB_STUCK_MIN_PROGRESS`] in [`MOB_STUCK_TIMEOUT_SECS`] drops aggro and
//!   opens a [`MOB_STUCK_REAGGRO_SECS`] cooldown, so a zombie boxed in a wall
//!   re-targets or idles instead of grinding forever. A skeleton standing in
//!   its fire band is deliberately stationary, not stuck, so that arm clears
//!   the timer.
//! * **AI state is never saved.** [`crate::world::persistence::SavedMob`] has
//!   no state field: a reload restores every mob in `Wander`, and hostiles
//!   re-aggro from line of sight once more.

use glam::{IVec3, Vec3};

use crate::player::raycast::raycast_voxels;
use crate::world::arrow::{ARROW_GRAVITY, ARROW_SPEED_SKELETON};
use crate::world::block::BlockId;
use crate::world::collision::solid_overlaps;
use crate::world::manager::WorldManager;
use crate::world::mob::{Mob, MobAiState, MobKind, MobShot};

/// Horizontal distance at which a hostile notices a target, in blocks.
///
/// See the module docs: 16 keeps the demo's 20-block-parked hostiles quiet.
pub const MOB_AGGRO_RANGE: f32 = 16.0;
/// Maximum absolute feet-height difference for aggro, in blocks.
pub const MOB_AGGRO_VERTICAL: f32 = 8.0;
/// Seconds a chase survives after line of sight is lost.
pub const MOB_AGGRO_MEMORY_SECS: f32 = 2.0;
/// Seconds without meaningful progress before the stuck fallback fires.
pub const MOB_STUCK_TIMEOUT_SECS: f32 = 1.0;
/// Net horizontal progress that counts as "not stuck", in blocks.
pub const MOB_STUCK_MIN_PROGRESS: f32 = 0.25;
/// Re-aggro cooldown opened by the stuck fallback, in seconds.
pub const MOB_STUCK_REAGGRO_SECS: f32 = 2.0;
/// The largest feet-height difference a skeleton will shoot across, in
/// blocks.
///
/// A 4-block band keeps a hill shot plausible (the aim compensates the drop)
/// while refusing the extreme angle that would need a lob the gravity model
/// can no longer hit.
pub const MOB_FIRE_VERTICAL: f32 = 4.0;
/// Height of the one allowed step up, in blocks.
pub const MOB_STEP_UP_HEIGHT: f32 = 1.0;
/// The largest feet drop a hostile may walk off, in blocks.
pub const MOB_CLIFF_DROP_BLOCKS: i32 = 1;

/// Same inward epsilon as the shared collision predicate, used to turn a
/// float feet position into block coordinates without boundary jitter.
const SUPPORT_EPS: f32 = 1e-4;
/// Slack on the ranged cooldown gate, in seconds.
///
/// [`MobRuntime::step_mob`] drains `mob.cooldown` by [`MOB_TICK_SECS`] with
/// repeated f32 subtraction, which can leave a few ulps of residual above zero
/// after the intended 40 steps and stretch the 2.0 s cadence to 41. A
/// thousandth of a second is far below one fixed step (0.05 s), so this makes
/// the shot cadence tick-exact without ever letting a shot fire early.
const MOB_FIRE_COOLDOWN_EPS: f32 = 1e-3;
/// Eye height as a fraction of the kind's full height. Matches the player's
/// 1.62 / 1.8 eye ratio closely enough for a fair line of sight.
const EYE_HEIGHT_FRACTION: f32 = 0.85;

/// The one capability the steering code needs from the world: three integers
/// in, one block id out. Nothing else about the world is visible to the AI.
pub trait MobWorld {
    fn block(&self, x: i32, y: i32, z: i32) -> BlockId;
}

impl MobWorld for WorldManager {
    fn block(&self, x: i32, y: i32, z: i32) -> BlockId {
        self.get_block(x, y, z)
    }
}

/// The player's combat-relevant pose. Plain values: `None` at the call site
/// means "not targetable" (Creative, AI gate off, or a dead player).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MobTarget {
    pub feet: Vec3,
    pub eye: Vec3,
}

/// One melee attack decision produced by [`step_hostile`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MobAttack {
    pub kind: MobKind,
    /// The attacker's feet position at the moment the attack fired.
    pub from: Vec3,
}

/// One attack decision produced by [`step_hostile`].
///
/// The two arms are disjoint by kind: [`MobKind::attack_profile`] and
/// [`MobKind::ranged_profile`] never both return `Some`, so a Zombie can only
/// ever produce [`MobAction::Melee`] and a Skeleton only [`MobAction::Shot`].
/// Wrapping both in one return value is what lets the runtime collect the two
/// report vectors from a single brain call without matching on the kind twice.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MobAction {
    /// A melee swing that landed this step.
    Melee(MobAttack),
    /// A shot decision; geometry only, no ballistics or damage.
    Shot(MobShot),
}

/// Whether the eye-to-eye ray reaches the target without crossing a solid
/// block.
///
/// A hit at the far endpoint is the target's own column, not a wall, so the
/// last millimetre is ignored. Non-finite endpoints are never clear; a
/// coincident eye pair (zero distance) is clear.
pub fn has_line_of_sight(world: &impl MobWorld, from_eye: Vec3, to_eye: Vec3) -> bool {
    if !from_eye.is_finite() || !to_eye.is_finite() {
        return false;
    }
    let delta = to_eye - from_eye;
    let distance = delta.length();
    if distance <= 0.0 {
        return true;
    }
    match raycast_voxels(from_eye, delta, distance, |x, y, z| world.block(x, y, z)) {
        None => true,
        Some(hit) => hit.distance >= distance - 1e-3,
    }
}

/// Advances one hostile mob by one fixed 20 Hz step.
///
/// `target` is the player pose when the player is targetable, or `None`.
/// `roll` is the caller's existing deterministic `mob_roll` draw; the current
/// fallback is fully deterministic and does not need a tie-break, but the
/// parameter is kept so the AI never reaches for `rand` or a wall clock.
///
/// Returns `Some(MobAction::Melee(..))` when a Zombie lands a swing this step
/// and `Some(MobAction::Shot(..))` when a Skeleton fires one. Non-hostile
/// kinds, dead mobs, and non-targetable players never mutate anything. A
/// Skeleton never melees ([`MobKind::attack_profile`] is `None` for it) and a
/// Zombie never shoots ([`MobKind::ranged_profile`] is `None` for it).
pub fn step_hostile(
    mob: &mut Mob,
    target: Option<MobTarget>,
    world: &impl MobWorld,
    roll: u32,
) -> Option<MobAction> {
    if !mob.kind.is_hostile() || mob.health == 0 || !mob.position.is_finite() {
        return None;
    }

    // The fallback is deterministic; the draw is accepted for parity with the
    // caller's fixed-step roll and never used as a wall-clock seed.
    let _ = roll;

    mob.aggro = (mob.aggro - crate::world::mob::MOB_TICK_SECS).max(0.0);
    mob.reaggro = (mob.reaggro - crate::world::mob::MOB_TICK_SECS).max(0.0);

    // Net progress since the anchor means "not stuck". Only horizontal
    // movement counts: a step-up or a knockback slide must not hide a wall.
    let progress_x = mob.position.x - mob.stuck_anchor.x;
    let progress_z = mob.position.z - mob.stuck_anchor.z;
    if progress_x * progress_x + progress_z * progress_z
        > MOB_STUCK_MIN_PROGRESS * MOB_STUCK_MIN_PROGRESS
    {
        mob.stuck_anchor = mob.position;
        mob.stuck = 0.0;
    }

    let Some(target) = target else {
        // A creative player (or a closed demo gate) must stop a chase at once.
        if mob.state == MobAiState::Chase {
            mob.state = MobAiState::Idle;
            mob.aggro = 0.0;
            mob.velocity = Vec3::ZERO;
        }
        return None;
    };

    let eye = mob.position + Vec3::new(0.0, mob.kind.height() * EYE_HEIGHT_FRACTION, 0.0);
    let dx = target.feet.x - mob.position.x;
    let dz = target.feet.z - mob.position.z;
    let horizontal = (dx * dx + dz * dz).sqrt();
    let vertical = target.feet.y - mob.position.y;

    if mob.state != MobAiState::Chase {
        // Range and cooldown are cheap; only an in-band candidate pays for a
        // raycast. Out-of-range hostiles therefore never cast per step.
        let in_band = horizontal <= MOB_AGGRO_RANGE && vertical.abs() <= MOB_AGGRO_VERTICAL;
        if in_band && mob.reaggro <= 0.0 && has_line_of_sight(world, eye, target.eye) {
            mob.state = MobAiState::Chase;
            mob.aggro = MOB_AGGRO_MEMORY_SECS;
            mob.stuck = 0.0;
            mob.stuck_anchor = mob.position;
        } else {
            return None;
        }
    }

    // A chasing hostile needs the ray every step: it refreshes chase memory
    // and gates the melee swing.
    let visible = has_line_of_sight(world, eye, target.eye);
    if visible {
        mob.aggro = MOB_AGGRO_MEMORY_SECS;
    }
    if mob.aggro <= 0.0 {
        mob.state = MobAiState::Idle;
        mob.velocity = Vec3::ZERO;
        return None;
    }

    // Face the target: yaw 0 is +Z, positive yaw rotates toward +X.
    if dx.abs() > 1e-6 || dz.abs() > 1e-6 {
        mob.yaw = dx.atan2(dz);
    }

    // The mob now faces the target, so `forward` is exactly the direction it
    // would close in. A retreat is the same direction negated, which is what
    // keeps both motions on one mover.
    let forward = Vec3::new(mob.yaw.sin(), 0.0, mob.yaw.cos());

    if let Some(profile) = mob.kind.ranged_profile() {
        // Stand-off brain (S08). The melee block below is deliberately
        // unreachable for this kind: a ranged kind's `attack_profile()` is
        // `None`, and this arm returns before the shared mover.
        if horizontal >= profile.min_range && horizontal <= profile.fire_range {
            // In the band: stand still and shoot when the shot is legal.
            // Standing is deliberate, so it must never accumulate the stuck
            // timer that a refused step would.
            mob.velocity = Vec3::ZERO;
            mob.stuck = 0.0;
            mob.stuck_anchor = mob.position;

            let eye_cell = IVec3::new(
                eye.x.floor() as i32,
                eye.y.floor() as i32,
                eye.z.floor() as i32,
            );
            let eye_open = !world.block(eye_cell.x, eye_cell.y, eye_cell.z).is_solid();
            if mob.cooldown <= MOB_FIRE_COOLDOWN_EPS
                && visible
                && vertical.abs() <= MOB_FIRE_VERTICAL
                && eye_open
            {
                // Gravity-compensated aim: lead the player's eye upward by the
                // drop the arrow will accumulate over its horizontal flight
                // time (`t = horizontal / speed`). The shot therefore arcs and
                // lands on the eye instead of short at long range.
                let flight = horizontal / ARROW_SPEED_SKELETON;
                let aim = target.eye + Vec3::Y * (0.5 * ARROW_GRAVITY * flight * flight);
                mob.cooldown = profile.cooldown_secs;
                return Some(MobAction::Shot(MobShot {
                    kind: mob.kind,
                    origin: eye,
                    dir: aim - eye,
                }));
            }

            return None;
        }

        if horizontal > profile.fire_range {
            move_in(mob, world, forward);
        } else {
            // Closer than `min_range`: back away with the same one-block
            // step-up/down, cliff guard, support and stuck rules.
            move_in(mob, world, -forward);
        }

        if mob.stuck >= MOB_STUCK_TIMEOUT_SECS {
            mob.state = MobAiState::Idle;
            mob.velocity = Vec3::ZERO;
            mob.aggro = 0.0;
            mob.reaggro = MOB_STUCK_REAGGRO_SECS;
            mob.stuck = 0.0;
            mob.stuck_anchor = mob.position;
        }

        return None;
    }

    if let Some(profile) = mob.kind.attack_profile() {
        if horizontal <= profile.range && vertical.abs() <= 2.0 && visible && mob.cooldown <= 0.0 {
            mob.cooldown = profile.cooldown_secs;
            mob.velocity = Vec3::ZERO;
            return Some(MobAction::Melee(MobAttack {
                kind: mob.kind,
                from: mob.position,
            }));
        }
    }

    move_in(mob, world, forward);

    if mob.stuck >= MOB_STUCK_TIMEOUT_SECS {
        mob.state = MobAiState::Idle;
        mob.velocity = Vec3::ZERO;
        mob.aggro = 0.0;
        mob.reaggro = MOB_STUCK_REAGGRO_SECS;
        mob.stuck = 0.0;
        mob.stuck_anchor = mob.position;
    }

    None
}

/// The shared step-up/down/cliff mover, in any horizontal direction, at the
/// kind's walk speed.
///
/// Updates `position`, `velocity`, `walk_phase`, and the stuck timer; never
/// changes yaw or state. Closing passes the facing vector and retreating passes
/// its negation, so both motions share the one-block step-up, one-block drop,
/// cliff guard, footprint support and stuck rules. `dir` must be a finite
/// horizontal direction (the callers pass the unit yaw vector or its
/// negation); a degenerate or non-finite direction refuses the step.
pub fn move_in(mob: &mut Mob, world: &impl MobWorld, dir: Vec3) {
    move_in_at_speed(mob, world, dir, mob.kind.walk_speed());
}

/// [`move_in`] with an explicit speed, used by the passive flee arm.
///
/// A non-finite or non-positive speed refuses the step instead of committing a
/// body position the rest of the game cannot reason about.
pub fn move_in_at_speed(mob: &mut Mob, world: &impl MobWorld, dir: Vec3, speed: f32) {
    let dir = Vec3::new(dir.x, 0.0, dir.z);
    if !dir.is_finite() || dir.length_squared() < 1e-12 || !speed.is_finite() || speed <= 0.0 {
        refuse(mob);
        return;
    }
    let candidate = mob.position + dir * speed * crate::world::mob::MOB_TICK_SECS;

    // Flat continuation: the candidate box must be clear and the destination
    // footprint must still have ground under it.
    if mob_box_clear(mob, candidate, world) {
        let support = support_level(candidate);
        if footprint_supported(mob, candidate, support, world) {
            commit_move(mob, candidate, dir, speed);
            return;
        }
        // One-block step down, but never a larger drop (cliff guard).
        if MOB_CLIFF_DROP_BLOCKS >= 1 && footprint_supported(mob, candidate, support - 1, world) {
            let lowered = candidate - Vec3::Y * MOB_CLIFF_DROP_BLOCKS as f32;
            if mob_box_clear(mob, lowered, world) {
                commit_move(mob, lowered, dir, speed);
                return;
            }
        }
        // Already airborne (a spawned/knocked body over a void, or a synthetic
        // test world with no floor): keep the pre-existing horizontal drift.
        // Terrain walking never reaches this arm because a grounded mob is
        // supported at its feet level and is refused by the cliff guard above.
        if airborne(mob, mob.position, world) {
            commit_move(mob, candidate, dir, speed);
            return;
        }
        refuse(mob);
        return;
    }

    // Blocked at feet level: try the one allowed step up. The raised box must
    // be clear and the step top must support the raised feet.
    let raised = candidate + Vec3::Y * MOB_STEP_UP_HEIGHT;
    if mob_box_clear(mob, raised, world)
        && footprint_supported(mob, raised, support_level(raised), world)
    {
        commit_move(mob, raised, dir, speed);
        return;
    }

    // A two-block wall produces neither a step nor penetration.
    refuse(mob);
}

fn commit_move(mob: &mut Mob, position: Vec3, dir: Vec3, speed: f32) {
    mob.position = position;
    mob.velocity = dir * speed;
    mob.walk_phase += speed * crate::world::mob::MOB_TICK_SECS * 4.0;
}

fn refuse(mob: &mut Mob) {
    mob.velocity = Vec3::ZERO;
    mob.stuck += crate::world::mob::MOB_TICK_SECS;
}

/// The mob's own box, not the player-sized `Aabb::from_feet`.
///
/// Public so the knockback integration in [`crate::world::mob`] can refuse a
/// slide into a solid block through the exact same AABB the walking mover uses.
pub fn mob_box_clear(mob: &Mob, feet: Vec3, world: &impl MobWorld) -> bool {
    let half = mob.kind.width() * 0.5;
    let min = Vec3::new(feet.x - half, feet.y, feet.z - half);
    let max = Vec3::new(feet.x + half, feet.y + mob.kind.height(), feet.z + half);
    !solid_overlaps(min, max, &|x, y, z| world.block(x, y, z))
}

/// The block y directly beneath block-level `feet` (the feet block itself is
/// at `floor(feet.y)` when standing on a top face).
fn support_level(feet: Vec3) -> i32 {
    (feet.y + SUPPORT_EPS).floor() as i32 - 1
}

/// Whether any block under the box footprint is solid at `level`.
///
/// The whole footprint is checked so a mob straddling a step still counts as
/// supported; a mob completely past the edge does not.
fn footprint_supported(mob: &Mob, feet: Vec3, level: i32, world: &impl MobWorld) -> bool {
    let half = mob.kind.width() * 0.5;
    let min_x = (feet.x - half + SUPPORT_EPS).floor() as i32;
    let max_x = (feet.x + half - SUPPORT_EPS).floor() as i32;
    let min_z = (feet.z - half + SUPPORT_EPS).floor() as i32;
    let max_z = (feet.z + half - SUPPORT_EPS).floor() as i32;
    for z in min_z..=max_z {
        for x in min_x..=max_x {
            if world.block(x, level, z).is_solid() {
                return true;
            }
        }
    }
    false
}

/// Whether the mob has no support at its feet level or one block below.
///
/// True means it is already airborne before this step. There is no mob gravity,
/// so such a body keeps its horizontal drift instead of freezing mid-air; a
/// grounded body that reaches a two-block ledge is still refused by the cliff
/// guard.
fn airborne(mob: &Mob, feet: Vec3, world: &impl MobWorld) -> bool {
    let level = support_level(feet);
    !footprint_supported(mob, feet, level, world)
        && !footprint_supported(mob, feet, level - 1, world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::arrow::Arrow;
    use crate::world::mob::{MobRuntime, MOB_TICK_SECS};
    use std::collections::HashMap;

    /// Synthetic block grid: absent cells are air.
    struct FakeWorld(HashMap<IVec3, BlockId>);

    impl FakeWorld {
        fn new() -> Self {
            Self(HashMap::new())
        }

        fn set(&mut self, x: i32, y: i32, z: i32, block: BlockId) {
            self.0.insert(IVec3::new(x, y, z), block);
        }

        fn clear(&mut self, x: i32, y: i32, z: i32) {
            self.0.remove(&IVec3::new(x, y, z));
        }
    }

    impl MobWorld for FakeWorld {
        fn block(&self, x: i32, y: i32, z: i32) -> BlockId {
            self.0
                .get(&IVec3::new(x, y, z))
                .copied()
                .unwrap_or(BlockId::Air)
        }
    }

    /// An empty world for the runtime-level checks that do not steer.
    struct NoWorld;

    impl MobWorld for NoWorld {
        fn block(&self, _x: i32, _y: i32, _z: i32) -> BlockId {
            BlockId::Air
        }
    }

    /// Solid slab with its top face at y = 5.0 (feet level in these tests).
    fn floor_world() -> FakeWorld {
        let mut world = FakeWorld::new();
        for x in -8..=48 {
            for z in -8..=48 {
                for y in -4..=4 {
                    world.set(x, y, z, BlockId::Stone);
                }
            }
        }
        world
    }

    fn target(x: f32, feet_y: f32, z: f32) -> MobTarget {
        let feet = Vec3::new(x, feet_y, z);
        MobTarget {
            feet,
            eye: feet + Vec3::new(0.0, 1.62, 0.0),
        }
    }

    fn horizontal(a: Vec3, b: Vec3) -> f32 {
        ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt()
    }

    fn chasing_mob(kind: MobKind, position: Vec3) -> Mob {
        let mut mob = Mob::new(kind, position, 0.0);
        mob.state = MobAiState::Chase;
        mob.aggro = 10.0;
        mob
    }

    #[test]
    fn line_of_sight_handles_walls_degenerate_eyes_and_bad_input() {
        let mut world = floor_world();
        assert!(has_line_of_sight(
            &world,
            Vec3::new(0.5, 6.5, 0.5),
            Vec3::new(0.5, 6.5, 5.5)
        ));
        world.set(0, 6, 2, BlockId::Stone);
        assert!(!has_line_of_sight(
            &world,
            Vec3::new(0.5, 6.5, 0.5),
            Vec3::new(0.5, 6.5, 5.5)
        ));

        let eye = Vec3::new(1.0, 6.5, 1.0);
        assert!(
            has_line_of_sight(&world, eye, eye),
            "a coincident eye pair is clear"
        );
        assert!(
            !has_line_of_sight(&world, Vec3::new(f32::NAN, 6.5, 1.0), eye),
            "non-finite origin is never clear"
        );
        assert!(
            !has_line_of_sight(&world, eye, Vec3::new(1.0, f32::INFINITY, 1.0)),
            "non-finite endpoint is never clear"
        );
    }

    #[test]
    fn aggro_requires_range_los_and_a_target() {
        let world = floor_world();

        // Exactly 16 blocks horizontally, clear air: aggro.
        let mut mob = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(
            step_hostile(&mut mob, Some(target(0.5, 5.0, 16.5)), &world, 0),
            None
        );
        assert_eq!(mob.state, MobAiState::Chase);

        // 16.5 blocks: out of range, stays Wander.
        let mut far = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(
            step_hostile(&mut far, Some(target(0.5, 5.0, 17.0)), &world, 0),
            None
        );
        assert_eq!(far.state, MobAiState::Wander);

        // One solid block at eye level between the eyes: no LOS.
        let mut walled_world = floor_world();
        walled_world.set(0, 6, 5, BlockId::Stone);
        let mut walled = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(
            step_hostile(&mut walled, Some(target(0.5, 5.0, 10.5)), &walled_world, 0),
            None
        );
        assert_eq!(walled.state, MobAiState::Wander);

        // No target at all: never acquire.
        let mut orphan = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(step_hostile(&mut orphan, None, &world, 0), None);
        assert_eq!(orphan.state, MobAiState::Wander);

        // Nine blocks of vertical separation exceeds the 8-block band.
        let mut high = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(
            step_hostile(&mut high, Some(target(0.5, 14.0, 0.5)), &world, 0),
            None
        );
        assert_eq!(high.state, MobAiState::Wander);

        // Passives keep their own S04 arms: a Pig with a target stays Wander.
        let mut pig = Mob::new(MobKind::Pig, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(
            step_hostile(&mut pig, Some(target(0.5, 5.0, 4.0)), &world, 0),
            None
        );
        assert_eq!(pig.state, MobAiState::Wander);
    }

    #[test]
    fn chase_closes_distance_and_faces_the_target() {
        let world = floor_world();
        let mut mob = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let target = target(3.5, 5.0, 8.5);
        let start = mob.position;
        let mut previous = horizontal(mob.position, target.feet);

        for _ in 0..10 {
            assert_eq!(step_hostile(&mut mob, Some(target), &world, 0), None);
            let current = horizontal(mob.position, target.feet);
            assert!(
                current < previous,
                "distance must shrink every step: {current} !< {previous}"
            );
            previous = current;
        }

        let dx = target.feet.x - mob.position.x;
        let dz = target.feet.z - mob.position.z;
        assert!(
            (mob.yaw - dx.atan2(dz)).abs() < 1e-4,
            "yaw {} must equal atan2(dx, dz) {}",
            mob.yaw,
            dx.atan2(dz)
        );
        let moved = (mob.position - start).length();
        let expected = MobKind::Zombie.walk_speed() * MOB_TICK_SECS * 10.0;
        assert!(
            (moved - expected).abs() < 1e-3,
            "ten steps must cover {expected} blocks, moved {moved}"
        );
        assert_eq!(mob.state, MobAiState::Chase);
    }

    #[test]
    fn one_block_step_up_and_two_block_wall_refusal() {
        // One-high wall at z = 2, top at y = 6.
        let mut world = floor_world();
        world.set(0, 5, 2, BlockId::Stone);
        let mut mob = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let target = target(0.5, 5.0, 8.5);

        let mut stepped_up = false;
        for _ in 0..40 {
            step_hostile(&mut mob, Some(target), &world, 0);
            if mob.position.y > 5.5 {
                stepped_up = true;
                break;
            }
        }
        assert!(stepped_up, "a one-block wall must be stepped up");
        assert!((mob.position.y - 6.0).abs() < 1e-5);
        let z_after_step = mob.position.z;
        step_hostile(&mut mob, Some(target), &world, 0);
        assert!(
            mob.position.z > z_after_step,
            "movement must continue after the step up"
        );

        // The same wall two blocks high produces no step and no penetration.
        let mut wall = floor_world();
        wall.set(0, 5, 2, BlockId::Stone);
        wall.set(0, 6, 2, BlockId::Stone);
        let mut blocked = chasing_mob(MobKind::Zombie, Vec3::new(0.5, 5.0, 1.6));
        let start = blocked.position;
        assert_eq!(step_hostile(&mut blocked, Some(target), &wall, 0), None);
        assert_eq!(blocked.position, start, "a two-block wall is impassable");
        assert!(
            blocked.stuck > 0.0,
            "a refused step must accumulate stuck time"
        );
    }

    #[test]
    fn cliff_guard_refuses_a_drop_greater_than_one_block() {
        // Ground only for z <= 2; beyond that is open air.
        let mut world = FakeWorld::new();
        for x in -8..=48 {
            for z in -8..=2 {
                for y in -4..=4 {
                    world.set(x, y, z, BlockId::Stone);
                }
            }
        }
        let mut mob = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let target = target(0.5, 5.0, 10.5);
        for _ in 0..40 {
            step_hostile(&mut mob, Some(target), &world, 0);
        }
        let edge_position = mob.position;
        step_hostile(&mut mob, Some(target), &world, 0);
        assert_eq!(
            mob.position, edge_position,
            "a drop greater than one block must stop horizontal movement"
        );
        assert!(mob.position.z < 3.5, "the mob must not cross the ledge");
        assert!(mob.stuck > 0.0, "the refusal must accumulate stuck time");

        // One-block drop: ground at y = 3 beyond the edge (top face y = 4).
        let mut shelf = FakeWorld::new();
        for x in -8..=48 {
            for z in -8..=2 {
                for y in -4..=4 {
                    shelf.set(x, y, z, BlockId::Stone);
                }
            }
            for z in 3..=48 {
                for y in -4..=3 {
                    shelf.set(x, y, z, BlockId::Stone);
                }
            }
        }
        let mut stepping = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let mut dropped = None;
        for _ in 0..60 {
            step_hostile(&mut stepping, Some(target), &shelf, 0);
            if (stepping.position.y - 4.0).abs() < 1e-5 {
                dropped = Some(stepping.position);
                break;
            }
        }
        let dropped = dropped.expect("a one-block drop must be taken");
        assert!(dropped.z > 3.0, "the step down must continue forward");
        assert!((dropped.y - 4.0).abs() < 1e-5);
    }

    #[test]
    fn stuck_hostile_idles_then_reaggros() {
        // A 1x1 pit walled two blocks high on +X; the target is also +X so the
        // mob grinds the wall instead of stepping up.
        let mut world = floor_world();
        world.set(1, 5, 0, BlockId::Stone);
        world.set(1, 6, 0, BlockId::Stone);
        let mut mob = chasing_mob(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5));
        let target = target(8.5, 5.0, 0.5);

        let mut fell_back = false;
        for _ in 0..40 {
            step_hostile(&mut mob, Some(target), &world, 0);
            if mob.state == MobAiState::Idle && mob.reaggro > 0.0 {
                fell_back = true;
                break;
            }
        }
        assert!(fell_back, "a boxed-in chase must fall back to Idle");
        assert_eq!(mob.aggro, 0.0);
        assert!((mob.reaggro - MOB_STUCK_REAGGRO_SECS).abs() < 1e-5);
        assert_eq!(mob.stuck, 0.0);

        // Remove the wall: LOS is clear, but the re-aggro cooldown blocks
        // acquisition while it remains positive.
        world.clear(1, 5, 0);
        world.clear(1, 6, 0);
        for _ in 0..10 {
            step_hostile(&mut mob, Some(target), &world, 0);
        }
        assert_eq!(
            mob.state,
            MobAiState::Idle,
            "the re-aggro cooldown must block immediate re-acquisition"
        );
        assert!(mob.reaggro > 0.0);

        // Drain the cooldown (2.0 s at 20 Hz = 40 steps, plus one).
        for _ in 0..41 {
            step_hostile(&mut mob, Some(target), &world, 0);
        }
        assert_eq!(
            mob.state,
            MobAiState::Chase,
            "the hostile must re-aggro once the cooldown drains and LOS is clear"
        );
    }

    #[test]
    fn zombie_attacks_in_range_with_cooldown_but_skeleton_never_does() {
        let world = floor_world();
        let in_range = target(0.5, 5.0, 1.5);
        let mut zombie = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let Some(MobAction::Melee(attack)) = step_hostile(&mut zombie, Some(in_range), &world, 0)
        else {
            panic!("an in-range zombie lands one melee attack");
        };
        assert_eq!(attack.kind, MobKind::Zombie);
        assert_eq!(attack.from, zombie.position);
        assert_eq!(
            MobKind::Zombie
                .attack_profile()
                .expect("zombie melee")
                .damage,
            3
        );
        assert_eq!(
            zombie.cooldown,
            MobKind::Zombie
                .attack_profile()
                .expect("zombie melee")
                .cooldown_secs
        );
        assert_eq!(
            step_hostile(&mut zombie, Some(in_range), &world, 0),
            None,
            "the cooldown must gate the next swing"
        );

        // Same geometry, Skeleton: no melee ever. Because 1.0 block is inside
        // its 5.0-block stand-off floor, the shared brain makes it back away
        // (S08) instead of swinging.
        let mut skeleton = Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(step_hostile(&mut skeleton, Some(in_range), &world, 0), None);
        assert_eq!(skeleton.cooldown, 0.0);
        assert_eq!(skeleton.state, MobAiState::Chase);
        assert_eq!(MobKind::Skeleton.attack_profile(), None);
        assert!(MobKind::Skeleton.ranged_profile().is_some());

        // Out of melee range: no attack.
        let mut far = Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(
            step_hostile(&mut far, Some(target(0.5, 5.0, 3.0)), &world, 0),
            None
        );

        // In range but no line of sight: no attack.
        let mut wall = floor_world();
        wall.set(0, 6, 1, BlockId::Stone);
        let mut hidden = chasing_mob(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5));
        assert_eq!(step_hostile(&mut hidden, Some(in_range), &wall, 0), None);
    }

    #[test]
    fn chase_survives_the_sixteen_step_yaw_reroll() {
        // T02 drives the real brain from the runtime: a Zombie that acquires
        // through LOS must keep its Chase state, heading, and forward motion
        // across two 16-step reroll boundaries.
        let world = floor_world();
        let mut runtime = MobRuntime::new(31);
        let mut mobs = vec![Mob::new(MobKind::Zombie, Vec3::new(0.5, 5.0, 0.5), 0.7)];
        let target = target(8.5, 5.0, 0.5);
        let start = mobs[0].position;

        for _ in 0..32 {
            runtime.tick(0.05, &mut mobs, |_| true, Some(target), &world);
        }
        assert_eq!(runtime.tick_index(), 32);
        assert_eq!(
            mobs[0].state,
            MobAiState::Chase,
            "the 16-step yaw reroll must not erase a chase"
        );
        let dx = target.feet.x - mobs[0].position.x;
        let dz = target.feet.z - mobs[0].position.z;
        assert!(
            (mobs[0].yaw - dx.atan2(dz)).abs() < 1e-4,
            "a chasing yaw must keep facing the target, not be rerolled"
        );
        assert!(
            mobs[0].position.x > start.x,
            "the wired runtime must move a chasing hostile"
        );
    }

    #[test]
    fn new_and_restored_initialize_the_ephemeral_tracking_fields() {
        let position = Vec3::new(2.5, 6.0, -1.5);
        let fresh = Mob::new(MobKind::Zombie, position, 0.0);
        assert_eq!(fresh.aggro, 0.0);
        assert_eq!(fresh.stuck, 0.0);
        assert_eq!(fresh.stuck_anchor, position);
        assert_eq!(fresh.reaggro, 0.0);

        let restored = Mob::restored(MobKind::Skeleton, position, 0.0, 7);
        assert_eq!(restored.aggro, 0.0);
        assert_eq!(restored.stuck, 0.0);
        assert_eq!(restored.stuck_anchor, position);
        assert_eq!(restored.reaggro, 0.0);
    }

    #[test]
    fn reset_hostile_tracking_drops_chase_without_moving_the_body() {
        let position = Vec3::new(4.5, 5.0, 4.5);
        let mut mob = Mob::new(MobKind::Zombie, position, 1.1);
        mob.state = MobAiState::Chase;
        mob.aggro = 1.5;
        mob.stuck = 0.4;
        mob.stuck_anchor = Vec3::new(1.0, 5.0, 1.0);
        mob.reaggro = 0.75;
        mob.velocity = Vec3::new(1.0, 0.0, 0.0);
        mob.cooldown = 0.6;
        let health_before = mob.health;

        mob.reset_hostile_tracking();

        assert_eq!(mob.state, MobAiState::Idle);
        assert_eq!(mob.aggro, 0.0);
        assert_eq!(mob.stuck, 0.0);
        assert_eq!(mob.reaggro, 0.0);
        assert_eq!(mob.stuck_anchor, position);
        assert_eq!(mob.velocity, Vec3::ZERO);
        assert_eq!(mob.position, position);
        assert_eq!(mob.yaw, 1.1);
        assert_eq!(mob.health, health_before);
        assert_eq!(mob.cooldown, 0.6);
    }

    #[test]
    fn mob_runtime_still_runs_20hz_for_passives() {
        let mut runtime = MobRuntime::new(1);
        let mut mobs = vec![Mob::new(MobKind::Pig, Vec3::new(0.5, 5.0, 0.5), 0.0)];
        for _ in 0..10 {
            assert_eq!(
                runtime
                    .tick(0.05, &mut mobs, |_| true, None, &NoWorld)
                    .ai_steps,
                1
            );
        }
        assert_eq!(runtime.tick_index(), 10);
        assert_eq!(mobs[0].state, MobAiState::Wander);
    }

    // -----------------------------------------------------------------
    // S08: the skeleton stand-off ranged brain
    // -----------------------------------------------------------------

    /// Builds a skeleton shot at `distance` blocks horizontally by running the
    /// real decision layer, never by re-evaluating the aim formula.
    fn shot_at(world: &FakeWorld, distance: f32, target_feet_y: f32) -> (MobShot, MobTarget) {
        let target = target(0.5, target_feet_y, 0.5 + distance);
        let mut mob = Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0);
        for _ in 0..4 {
            match step_hostile(&mut mob, Some(target), world, 0) {
                Some(MobAction::Shot(shot)) => return (shot, target),
                Some(MobAction::Melee(_)) => panic!("a skeleton must never melee"),
                None => {}
            }
        }
        panic!("a skeleton in its fire band must fire within four steps");
    }

    /// Flies the shot through the real [`Arrow`] with the session's substep
    /// rule and reports whether it passes inside the target AABB
    /// (`± 0.35` on x/z, `feet..feet + 1.8` on y).
    fn shot_passes_through_target(shot: &MobShot, target: MobTarget) -> bool {
        let mut arrow =
            Arrow::skeleton(shot.origin, shot.dir).expect("the shot must build an arrow");
        let frame = 0.05f32;
        for _ in 0..200 {
            let substeps = arrow.substeps(frame);
            let sub_dt = frame / substeps as f32;
            for _ in 0..substeps {
                arrow.advance(sub_dt);
                let p = arrow.position;
                if (p.x - target.feet.x).abs() <= 0.35
                    && (p.z - target.feet.z).abs() <= 0.35
                    && p.y >= target.feet.y
                    && p.y <= target.feet.y + 1.8
                {
                    return true;
                }
            }
            if arrow.finished() || arrow.position.z > target.feet.z + 1.0 {
                break;
            }
        }
        false
    }

    #[test]
    fn skeleton_shots_arc_through_the_target_at_six_eight_and_twelve_blocks() {
        let world = floor_world();
        for distance in [6.0f32, 8.0, 12.0] {
            let (shot, target) = shot_at(&world, distance, 5.0);
            assert_eq!(shot.kind, MobKind::Skeleton);
            assert_eq!(
                shot.origin,
                Vec3::new(0.5, 5.0 + MobKind::Skeleton.height() * 0.85, 0.5),
                "the arrow must leave from the same eye rule line of sight uses"
            );
            assert!(
                shot.dir.y > 0.0,
                "the aim must point above the straight line at {distance} blocks"
            );
            assert!(
                shot_passes_through_target(&shot, target),
                "the simulated arrow must pass through the target AABB at {distance} blocks"
            );
        }
    }

    #[test]
    fn skeleton_shots_arc_through_the_target_uphill_and_downhill() {
        let world = floor_world();

        // Target feet one block above the skeleton's feet.
        let (up, up_target) = shot_at(&world, 8.0, 6.0);
        assert!(
            shot_passes_through_target(&up, up_target),
            "an uphill shot must still land in the target AABB"
        );

        // Target feet one block below the skeleton's feet.
        let (down, down_target) = shot_at(&world, 8.0, 4.0);
        assert!(
            shot_passes_through_target(&down, down_target),
            "a downhill shot must still land in the target AABB"
        );
        assert!(
            down.dir.y < up.dir.y,
            "the downhill aim must point lower than the uphill aim"
        );
    }

    #[test]
    fn skeleton_under_min_range_retreats_without_firing() {
        let world = floor_world();
        let min_range = MobKind::Skeleton
            .ranged_profile()
            .expect("skeleton bow")
            .min_range;
        let mut mob = Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let target = target(0.5, 5.0, 4.5);
        let mut previous = horizontal(mob.position, target.feet);
        assert!(
            previous < min_range,
            "the fixture must start inside the floor"
        );

        for _ in 0..10 {
            assert_eq!(
                step_hostile(&mut mob, Some(target), &world, 0),
                None,
                "inside min_range the skeleton retreats, never fires"
            );
            let current = horizontal(mob.position, target.feet);
            assert!(
                current > previous,
                "retreat must grow the distance: {current} !> {previous}"
            );
            previous = current;
        }
        assert_eq!(mob.cooldown, 0.0, "a retreat never arms the shot cooldown");
        assert_eq!(mob.state, MobAiState::Chase);
        assert!(
            (mob.velocity.z + MobKind::Skeleton.walk_speed()).abs() < 1e-4,
            "the retreat must move away from the target at walk speed, got {}",
            mob.velocity
        );
    }

    #[test]
    fn a_wall_between_skeleton_and_target_suppresses_the_shot() {
        let target = target(0.5, 5.0, 8.5);

        // Control: the same chasing skeleton with a clear ray fires at once.
        let open_world = floor_world();
        let mut clear = chasing_mob(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5));
        assert!(
            matches!(
                step_hostile(&mut clear, Some(target), &open_world, 0),
                Some(MobAction::Shot(_))
            ),
            "control: an unobstructed skeleton in its band fires"
        );

        // Wall the eye ray half-way: no shot, and the skeleton holds still.
        let mut world = floor_world();
        world.set(0, 6, 4, BlockId::Stone);
        let mut mob = chasing_mob(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5));
        let held = mob.position;
        for _ in 0..4 {
            assert_eq!(step_hostile(&mut mob, Some(target), &world, 0), None);
        }
        assert_eq!(
            mob.position, held,
            "a suppressed shot must not make the skeleton wander"
        );
        assert_eq!(mob.cooldown, 0.0, "a suppressed shot arms no cooldown");
        assert!(
            mob.aggro > 0.0,
            "the wall must suppress the shot, not aggro"
        );
    }

    #[test]
    fn a_solid_eye_cell_suppresses_the_shot() {
        // Control: the same geometry with an open eye cell fires.
        let world = floor_world();
        let target = target(0.5, 5.0, 8.5);
        let mut clear = Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert!(
            matches!(
                step_hostile(&mut clear, Some(target), &world, 0),
                Some(MobAction::Shot(_))
            ),
            "control: an open eye cell fires"
        );

        // A solid block in the skeleton's own eye cell suppresses the shot.
        // The ray also fails from inside that cell, but the explicit eye-cell
        // guard is what keeps the rule a property of the decision layer, so the
        // observable result is pinned here: no shot, no cooldown armed.
        let mut blocked_world = floor_world();
        blocked_world.set(0, 6, 0, BlockId::Stone);
        let mut mob = chasing_mob(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5));
        for _ in 0..4 {
            assert_eq!(
                step_hostile(&mut mob, Some(target), &blocked_world, 0),
                None
            );
        }
        assert_eq!(mob.cooldown, 0.0, "a blocked eye cell never arms a shot");
        assert_eq!(mob.velocity, Vec3::ZERO);
    }

    #[test]
    fn a_skeleton_closes_from_beyond_fire_range_before_it_shoots() {
        let world = floor_world();
        let mut mob = Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0);
        let target = target(0.5, 5.0, 15.5); // 15 blocks: inside aggro, outside fire
        let start = horizontal(mob.position, target.feet);
        let mut closed = None;
        for _ in 0..60 {
            if let Some(MobAction::Shot(shot)) = step_hostile(&mut mob, Some(target), &world, 0) {
                closed = Some(horizontal(mob.position, target.feet));
                assert_eq!(shot.kind, MobKind::Skeleton);
                break;
            }
        }
        let fired_at = closed.expect("a closing skeleton must eventually reach its band");
        assert!(fired_at < start, "it must close distance before firing");
        assert!(
            fired_at <= 12.0 + 1e-3,
            "the first shot must come from inside fire_range, got {fired_at}"
        );
    }
}
