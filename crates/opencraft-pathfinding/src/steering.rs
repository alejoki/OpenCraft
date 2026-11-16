//! Neutral walking steering for a single axis-aligned body.
//!
//! The mover is one fixed step over a [`BlockQuery`]: a flat continuation when
//! the destination box is clear and its footprint still has ground, one block
//! down when the next column is exactly one block lower (a larger drop is
//! refused by the cliff guard), horizontal drift when the body is already
//! airborne, and one block up when the step is blocked at feet level. A
//! refused step zeroes the velocity and accumulates [`SteeringBody::stuck`];
//! the caller owns the timeout and re-aggro policy built on that counter.
//!
//! The body carries only values: caller-supplied dimensions, feet position,
//! velocity, walk phase and blocked time. Nothing here knows a game identity,
//! a content registry, a tick rate or an actor type.

use glam::Vec3;
use opencraft_core::block::BlockQuery;
use opencraft_core::geometry::{solid_overlaps, Aabb};

/// Height of the one allowed step up, in blocks.
pub const STEP_UP_HEIGHT: f32 = 1.0;
/// The largest feet drop a body may walk off, in blocks.
pub const CLIFF_DROP_BLOCKS: i32 = 1;

/// Same inward epsilon as the shared box predicate, used to turn a float feet
/// position into block coordinates without boundary jitter.
const SUPPORT_EPS: f32 = 1e-4;
/// Walk-phase advance per block travelled, in phase units.
const WALK_PHASE_GAIN: f32 = 4.0;

/// Caller-supplied body dimensions in blocks.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BodyBox {
    pub half_width: f32,
    pub height: f32,
}

/// The values one walking body carries between fixed steps.
///
/// `stuck` is seconds spent blocked by refusals; the caller owns the
/// timeout/re-aggro policy that reads it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SteeringBody {
    pub feet: Vec3,
    pub velocity: Vec3,
    pub walk_phase: f32,
    pub stuck: f32,
}

/// Whether the axis-aligned body box at `feet` is clear of solid cells.
///
/// Reuses the single epsilon/flooring predicate shared by every moving body,
/// so an empty or liquid cell never blocks.
pub fn body_box_clear(feet: Vec3, dims: BodyBox, world: &impl BlockQuery) -> bool {
    !solid_overlaps(Aabb::from_feet(feet, dims.half_width, dims.height), world)
}

/// Advance one walking body by one fixed step.
///
/// `dir` is a horizontal (not necessarily unit) direction, `speed` is in
/// blocks per second and `dt` is the step duration in seconds. The direction's
/// y component is discarded; the direction is never normalized. A non-finite
/// or non-positive `dt` leaves the body's motion values untouched apart from
/// zeroing the velocity (there is no valid duration to accumulate), while an
/// invalid direction or speed refuses the step and adds `dt` to
/// [`SteeringBody::stuck`].
///
/// ```
/// use glam::Vec3;
/// use opencraft_core::block::{BlockKind, BlockQuery};
/// use opencraft_pathfinding::steering::{move_in, BodyBox, SteeringBody};
///
/// let world = |_x: i32, y: i32, _z: i32| {
///     if y < 5 { BlockKind::Solid } else { BlockKind::Empty }
/// };
/// assert_eq!(world.block_kind(0, 4, 0), BlockKind::Solid);
///
/// let mut body = SteeringBody {
///     feet: Vec3::new(0.5, 5.0, 0.5),
///     velocity: Vec3::ZERO,
///     walk_phase: 0.0,
///     stuck: 0.0,
/// };
/// let dims = BodyBox { half_width: 0.35, height: 1.8 };
/// move_in(&mut body, dims, &world, Vec3::Z, 1.6, 0.05);
///
/// assert!((body.feet.z - 0.58).abs() < 1e-4);
/// assert!((body.velocity.z - 1.6).abs() < 1e-4);
/// ```
pub fn move_in(
    body: &mut SteeringBody,
    dims: BodyBox,
    world: &impl BlockQuery,
    dir: Vec3,
    speed: f32,
    dt: f32,
) {
    if !dt.is_finite() || dt <= 0.0 {
        body.velocity = Vec3::ZERO;
        return;
    }
    let dir = Vec3::new(dir.x, 0.0, dir.z);
    if !dir.is_finite() || dir.length_squared() < 1e-12 || !speed.is_finite() || speed <= 0.0 {
        refuse(body, dt);
        return;
    }
    let candidate = body.feet + dir * speed * dt;

    // Flat continuation: the candidate box must be clear and the destination
    // footprint must still have ground under it.
    if body_box_clear(candidate, dims, world) {
        let support = support_level(candidate);
        if footprint_supported(candidate, dims, support, world) {
            commit_move(body, candidate, dir, speed, dt);
            return;
        }
        // One-block step down, but never a larger drop (cliff guard).
        if CLIFF_DROP_BLOCKS >= 1 && footprint_supported(candidate, dims, support - 1, world) {
            let lowered = candidate - Vec3::Y * CLIFF_DROP_BLOCKS as f32;
            if body_box_clear(lowered, dims, world) {
                commit_move(body, lowered, dir, speed, dt);
                return;
            }
        }
        // Already airborne (a spawned/knocked body over a void, or a synthetic
        // test world with no floor): keep the pre-existing horizontal drift.
        // Terrain walking never reaches this arm because a grounded body is
        // supported at its feet level and is refused by the cliff guard above.
        if airborne(body.feet, dims, world) {
            commit_move(body, candidate, dir, speed, dt);
            return;
        }
        refuse(body, dt);
        return;
    }

    // Blocked at feet level: try the one allowed step up. The raised box must
    // be clear and the step top must support the raised feet.
    let raised = candidate + Vec3::Y * STEP_UP_HEIGHT;
    if body_box_clear(raised, dims, world)
        && footprint_supported(raised, dims, support_level(raised), world)
    {
        commit_move(body, raised, dir, speed, dt);
        return;
    }

    // A two-block wall produces neither a step nor penetration.
    refuse(body, dt);
}

fn commit_move(body: &mut SteeringBody, feet: Vec3, dir: Vec3, speed: f32, dt: f32) {
    body.feet = feet;
    body.velocity = dir * speed;
    body.walk_phase += speed * dt * WALK_PHASE_GAIN;
}

fn refuse(body: &mut SteeringBody, dt: f32) {
    body.velocity = Vec3::ZERO;
    body.stuck += dt;
}

/// The block y directly beneath block-level `feet` (the feet block itself is
/// at `floor(feet.y)` when standing on a top face).
fn support_level(feet: Vec3) -> i32 {
    (feet.y + SUPPORT_EPS).floor() as i32 - 1
}

/// Whether any block under the box footprint is solid at `level`.
///
/// The whole footprint is checked so a body straddling a step still counts as
/// supported; a body completely past the edge does not.
fn footprint_supported(feet: Vec3, dims: BodyBox, level: i32, world: &impl BlockQuery) -> bool {
    let half = dims.half_width;
    let min_x = (feet.x - half + SUPPORT_EPS).floor() as i32;
    let max_x = (feet.x + half - SUPPORT_EPS).floor() as i32;
    let min_z = (feet.z - half + SUPPORT_EPS).floor() as i32;
    let max_z = (feet.z + half - SUPPORT_EPS).floor() as i32;
    for z in min_z..=max_z {
        for x in min_x..=max_x {
            if world.block_kind(x, level, z).is_solid() {
                return true;
            }
        }
    }
    false
}

/// Whether the body has no support at its feet level or one block below.
///
/// True means it is already airborne before this step. There is no gravity
/// here, so such a body keeps its horizontal drift instead of freezing mid-air;
/// a grounded body that reaches a two-block ledge is still refused by the
/// cliff guard.
fn airborne(feet: Vec3, dims: BodyBox, world: &impl BlockQuery) -> bool {
    let level = support_level(feet);
    !footprint_supported(feet, dims, level, world)
        && !footprint_supported(feet, dims, level - 1, world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencraft_core::block::BlockKind;
    use std::collections::HashMap;

    /// Block grid where an absent cell is empty. No game identity is involved.
    struct FakeWorld(HashMap<(i32, i32, i32), BlockKind>);

    impl FakeWorld {
        fn new() -> Self {
            Self(HashMap::new())
        }

        fn set(&mut self, x: i32, y: i32, z: i32, kind: BlockKind) {
            self.0.insert((x, y, z), kind);
        }
    }

    impl BlockQuery for FakeWorld {
        fn block_kind(&self, x: i32, y: i32, z: i32) -> BlockKind {
            self.0.get(&(x, y, z)).copied().unwrap_or(BlockKind::Empty)
        }
    }

    fn dims() -> BodyBox {
        BodyBox {
            half_width: 0.35,
            height: 1.8,
        }
    }

    fn body_at(z: f32) -> SteeringBody {
        SteeringBody {
            feet: Vec3::new(0.5, 5.0, z),
            velocity: Vec3::ZERO,
            walk_phase: 0.0,
            stuck: 0.0,
        }
    }

    /// Solid cells from y = -2 up to (but excluding) `top`, across a wide
    /// footprint so a walking body never reaches the grid edge.
    fn floor_world(top: i32) -> FakeWorld {
        let mut world = FakeWorld::new();
        for y in -2..top {
            for z in -8..=12 {
                for x in -8..=8 {
                    world.set(x, y, z, BlockKind::Solid);
                }
            }
        }
        world
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn flat_move_advances_feet_velocity_walk_phase_and_leaves_stuck_zero() {
        let world = floor_world(5);
        let mut body = body_at(0.5);

        move_in(&mut body, dims(), &world, Vec3::Z, 1.6, 0.05);

        assert_close(body.feet.z, 0.58);
        assert_close(body.feet.y, 5.0);
        assert_close(body.feet.x, 0.5);
        assert_close(body.velocity.z, 1.6);
        assert_close(body.velocity.x, 0.0);
        assert_close(body.walk_phase, 0.32);
        assert_eq!(body.stuck, 0.0);
    }

    #[test]
    fn one_block_step_up_climbs_and_continues_forward() {
        let mut world = floor_world(5);
        world.set(0, 5, 2, BlockKind::Solid);
        let mut body = body_at(1.6);

        let mut climbed = false;
        for _ in 0..8 {
            move_in(&mut body, dims(), &world, Vec3::Z, 1.6, 0.05);
            if body.feet.y > 5.5 {
                climbed = true;
                break;
            }
        }
        assert!(climbed, "body never climbed the one-block step");
        assert_close(body.feet.y, 6.0);

        let z_before = body.feet.z;
        move_in(&mut body, dims(), &world, Vec3::Z, 1.6, 0.05);
        assert_close(body.feet.y, 6.0);
        assert!(
            body.feet.z > z_before,
            "body did not continue forward after the step-up"
        );
        assert_eq!(body.stuck, 0.0);
    }

    #[test]
    fn cliff_guard_refuses_a_drop_greater_than_one_block_and_takes_a_one_block_drop() {
        // Lower ground beyond z = 2: a two-block cliff from the ledge top.
        let mut cliff = FakeWorld::new();
        for y in -2..=4 {
            for z in -8..=2 {
                for x in -8..=8 {
                    cliff.set(x, y, z, BlockKind::Solid);
                }
            }
        }

        let mut body = body_at(2.0);
        let mut refused = false;
        for _ in 0..60 {
            move_in(&mut body, dims(), &cliff, Vec3::Z, 1.6, 0.05);
            if body.stuck > 0.0 {
                refused = true;
                break;
            }
        }
        assert!(refused, "cliff guard never refused the two-block drop");
        assert!(
            body.feet.z < 3.4,
            "body walked past the cliff edge: {}",
            body.feet.z
        );
        assert_close(body.feet.y, 5.0);
        assert_close(body.velocity.z, 0.0);

        // One block lower beyond z = 2: the allowed drop is taken and motion
        // continues forward.
        let mut shelf = FakeWorld::new();
        for y in -2..=4 {
            for z in -8..=2 {
                for x in -8..=8 {
                    shelf.set(x, y, z, BlockKind::Solid);
                }
            }
        }
        for y in -2..=3 {
            for z in 3..=12 {
                for x in -8..=8 {
                    shelf.set(x, y, z, BlockKind::Solid);
                }
            }
        }

        let mut body = body_at(2.0);
        let mut dropped = false;
        for _ in 0..60 {
            move_in(&mut body, dims(), &shelf, Vec3::Z, 1.6, 0.05);
            if body.feet.y < 4.5 {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "one-block drop was never taken");
        assert_close(body.feet.y, 4.0);

        let z_before = body.feet.z;
        move_in(&mut body, dims(), &shelf, Vec3::Z, 1.6, 0.05);
        assert!(
            body.feet.z > z_before,
            "body did not continue forward after the one-block drop"
        );
    }

    #[test]
    fn footprint_support_accepts_any_solid_under_the_box() {
        // A single pedestal cell: no other floor exists in this world.
        let mut world = FakeWorld::new();
        world.set(0, 4, 0, BlockKind::Solid);

        let mut overlapping = SteeringBody {
            feet: Vec3::new(0.9, 5.0, 0.5),
            velocity: Vec3::ZERO,
            walk_phase: 0.0,
            stuck: 0.0,
        };
        move_in(&mut overlapping, dims(), &world, Vec3::X, 1.6, 0.05);
        assert!(
            overlapping.feet.x > 0.9,
            "body did not move while a footprint corner was supported"
        );
        assert_eq!(overlapping.stuck, 0.0);

        // The footprint minimum x floors to the pedestal cell, so the body is
        // grounded, but the candidate footprint has fully left it. This is a
        // support refusal, not an airborne drift.
        let start = Vec3::new(1.34, 5.0, 0.5);
        let mut body = SteeringBody {
            feet: start,
            velocity: Vec3::ZERO,
            walk_phase: 0.0,
            stuck: 0.0,
        };
        move_in(&mut body, dims(), &world, Vec3::X, 1.6, 0.05);
        assert_eq!(body.feet, start);
        assert!(body.stuck > 0.0, "expected a refused step");
        assert_close(body.velocity.x, 0.0);
    }

    #[test]
    fn airborne_body_keeps_its_horizontal_drift() {
        let world = FakeWorld::new();
        let mut body = body_at(0.5);

        move_in(&mut body, dims(), &world, Vec3::Z, 1.6, 0.05);

        assert_close(body.feet.z, 0.58);
        assert_close(body.velocity.z, 1.6);
        assert_eq!(body.stuck, 0.0);
    }

    #[test]
    fn refusals_accumulate_stuck_time_so_the_caller_can_fall_back() {
        let mut world = floor_world(5);
        world.set(0, 5, 2, BlockKind::Solid);
        world.set(0, 6, 2, BlockKind::Solid);
        let mut body = body_at(1.6);
        let feet_before = body.feet;
        let phase_before = body.walk_phase;

        move_in(&mut body, dims(), &world, Vec3::Z, 1.6, 0.05);
        move_in(&mut body, dims(), &world, Vec3::Z, 1.6, 0.05);

        assert_eq!(body.feet, feet_before);
        assert_eq!(body.walk_phase, phase_before);
        assert_eq!(body.velocity, Vec3::ZERO);
        assert_close(body.stuck, 0.1);
    }

    #[test]
    fn liquid_and_empty_cells_never_block() {
        let feet = Vec3::new(0.5, 0.0, 0.5);
        let box_dims = BodyBox {
            half_width: 0.35,
            height: 1.0,
        };

        let mut solid = FakeWorld::new();
        solid.set(0, 0, 0, BlockKind::Solid);
        assert!(!body_box_clear(feet, box_dims, &solid));

        let mut liquid = FakeWorld::new();
        liquid.set(0, 0, 0, BlockKind::Liquid);
        assert!(body_box_clear(feet, box_dims, &liquid));

        let mut empty = FakeWorld::new();
        empty.set(0, 0, 0, BlockKind::Empty);
        assert!(body_box_clear(feet, box_dims, &empty));
    }

    #[test]
    fn invalid_direction_speed_and_dt_refuse_without_panicking() {
        let world = floor_world(5);
        let start = body_at(0.5);
        let dt = 0.05;

        let invalid = [
            (Vec3::new(f32::NAN, 0.0, 0.0), 1.6),
            (Vec3::ZERO, 1.6),
            (Vec3::Z, f32::NAN),
            (Vec3::Z, 0.0),
            (Vec3::Z, -1.0),
        ];
        for (dir, speed) in invalid {
            let mut candidate = start;
            candidate.velocity = Vec3::new(1.0, 2.0, 3.0);
            move_in(&mut candidate, dims(), &world, dir, speed, dt);
            assert_eq!(candidate.feet, start.feet);
            assert_eq!(candidate.velocity, Vec3::ZERO);
            assert_close(candidate.stuck, dt);
            assert_eq!(candidate.walk_phase, start.walk_phase);
        }

        // A non-positive or non-finite duration has no valid time to
        // accumulate, so the body keeps its feet, phase and stuck time.
        for bad_dt in [f32::NAN, 0.0, -0.1] {
            let mut candidate = start;
            candidate.stuck = 0.25;
            candidate.velocity = Vec3::new(1.0, 2.0, 3.0);
            move_in(&mut candidate, dims(), &world, Vec3::Z, 1.6, bad_dt);
            assert_eq!(candidate.feet, start.feet);
            assert_eq!(candidate.velocity, Vec3::ZERO);
            assert_eq!(candidate.walk_phase, start.walk_phase);
            assert_eq!(candidate.stuck, 0.25);
        }

        // Non-finite dimensions must not panic; the step stays finite.
        let mut nan_dims = start;
        move_in(
            &mut nan_dims,
            BodyBox {
                half_width: f32::NAN,
                height: f32::NAN,
            },
            &world,
            Vec3::Z,
            1.6,
            dt,
        );
        assert!(nan_dims.feet.is_finite());
        assert!(nan_dims.velocity.is_finite());
    }
}
