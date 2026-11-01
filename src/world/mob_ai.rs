//! World-aware hostile steering (M009/S06-T01).
//!
//! This module is the whole hostile brain: line-of-sight aggro, direct chase,
//! one-block step-up/down, a cliff guard, and the stuck fallback. It is pure
//! lib code — no wgpu, no winit, no `Instant`, no wall clock, and no
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
//!   so a hostile never walks off a two-block ledge.
//! * **Stuck fallback.** A chase that makes less than
//!   [`MOB_STUCK_MIN_PROGRESS`] in [`MOB_STUCK_TIMEOUT_SECS`] drops aggro and
//!   opens a [`MOB_STUCK_REAGGRO_SECS`] cooldown, so a zombie boxed in a wall
//!   re-targets or idles instead of grinding forever.
//! * **AI state is never saved.** [`crate::world::persistence::SavedMob`] has
//!   no state field: a reload restores every mob in `Wander`, and hostiles
//!   re-aggro from line of sight once more.

use glam::Vec3;

use crate::player::raycast::raycast_voxels;
use crate::world::block::BlockId;
use crate::world::collision::solid_overlaps;
use crate::world::manager::WorldManager;
use crate::world::mob::{Mob, MobAiState, MobKind};

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
/// Height of the one allowed step up, in blocks.
pub const MOB_STEP_UP_HEIGHT: f32 = 1.0;
/// The largest feet drop a hostile may walk off, in blocks.
pub const MOB_CLIFF_DROP_BLOCKS: i32 = 1;

/// Same inward epsilon as the shared collision predicate, used to turn a
/// float feet position into block coordinates without boundary jitter.
const SUPPORT_EPS: f32 = 1e-4;
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
/// Returns `Some` when a Zombie lands an attack this step. Non-hostile kinds,
/// dead mobs, and non-targetable players never mutate anything.
pub fn step_hostile(
    mob: &mut Mob,
    target: Option<MobTarget>,
    world: &impl MobWorld,
    roll: u32,
) -> Option<MobAttack> {
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

    if let Some(profile) = mob.kind.attack_profile() {
        if horizontal <= profile.range && vertical.abs() <= 2.0 && visible && mob.cooldown <= 0.0 {
            mob.cooldown = profile.cooldown_secs;
            mob.velocity = Vec3::ZERO;
            return Some(MobAttack {
                kind: mob.kind,
                from: mob.position,
            });
        }
    }

    move_toward(mob, world);

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

/// The shared step-up/down/cliff mover. Updates `position`, `velocity`,
/// `walk_phase`, and the stuck timer; never changes yaw or state.
fn move_toward(mob: &mut Mob, world: &impl MobWorld) {
    let speed = mob.kind.walk_speed();
    let forward = Vec3::new(mob.yaw.sin(), 0.0, mob.yaw.cos());
    let candidate = mob.position + forward * speed * crate::world::mob::MOB_TICK_SECS;

    // Flat continuation: the candidate box must be clear and the destination
    // footprint must still have ground under it.
    if mob_box_clear(mob, candidate, world) {
        let support = support_level(candidate);
        if footprint_supported(mob, candidate, support, world) {
            commit_move(mob, candidate, forward, speed);
            return;
        }
        // One-block step down, but never a larger drop (cliff guard).
        if MOB_CLIFF_DROP_BLOCKS >= 1 && footprint_supported(mob, candidate, support - 1, world) {
            let lowered = candidate - Vec3::Y * MOB_CLIFF_DROP_BLOCKS as f32;
            if mob_box_clear(mob, lowered, world) {
                commit_move(mob, lowered, forward, speed);
                return;
            }
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
        commit_move(mob, raised, forward, speed);
        return;
    }

    // A two-block wall produces neither a step nor penetration.
    refuse(mob);
}

fn commit_move(mob: &mut Mob, position: Vec3, forward: Vec3, speed: f32) {
    mob.position = position;
    mob.velocity = forward * speed;
    mob.walk_phase += speed * crate::world::mob::MOB_TICK_SECS * 4.0;
}

fn refuse(mob: &mut Mob) {
    mob.velocity = Vec3::ZERO;
    mob.stuck += crate::world::mob::MOB_TICK_SECS;
}

/// The mob's own box, not the player-sized `Aabb::from_feet`.
fn mob_box_clear(mob: &Mob, feet: Vec3, world: &impl MobWorld) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::mob::{MobRuntime, MOB_TICK_SECS};
    use glam::IVec3;
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
        let attack = step_hostile(&mut zombie, Some(in_range), &world, 0)
            .expect("an in-range zombie lands one attack");
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

        // Same geometry, Skeleton: shared steering may chase but never melee.
        let mut skeleton = Mob::new(MobKind::Skeleton, Vec3::new(0.5, 5.0, 0.5), 0.0);
        assert_eq!(step_hostile(&mut skeleton, Some(in_range), &world, 0), None);
        assert_eq!(skeleton.cooldown, 0.0);
        assert_eq!(skeleton.state, MobAiState::Chase);
        assert_eq!(MobKind::Skeleton.attack_profile(), None);

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
}
