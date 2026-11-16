//! External contract for the device-free steering and A* half of the reuse
//! proof.
//!
//! This target is an outside consumer: the imports below come from the
//! published crate roots of the engine crates, never from a private module and
//! never from the `opencraft` game package. Every scenario is a fixed sequence
//! of calls with no clock, RNG or I/O, so the target runs headless and
//! deterministically.

use glam::{IVec3, Vec3};

use opencraft_core::block::BlockKind;
use opencraft_pathfinding::{
    body_box_clear, find_path, move_in, BodyBox, NavBounds, PathError, SteeringBody,
};

use engine_reuse_proof::motion;
use engine_reuse_proof::world::ProofWorld;

const SPEED: f32 = 1.6;
const DT: f32 = 0.05;
const EPS: f32 = 1e-4;

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= EPS,
        "expected {expected} ± {EPS}, got {actual}"
    );
}

fn dims() -> BodyBox {
    BodyBox {
        half_width: 0.35,
        height: 1.8,
    }
}

fn body_at(feet: Vec3) -> SteeringBody {
    SteeringBody {
        feet,
        velocity: Vec3::ZERO,
        walk_phase: 0.0,
        stuck: 0.0,
    }
}

#[test]
fn proof_reports_the_flat_steer_and_step_up_feet() {
    let proof = motion::run();

    assert_close(proof.steered_feet.x, 0.5);
    assert_close(proof.steered_feet.y, 0.0);
    assert_close(proof.steered_feet.z, 2.1);
    assert!(proof.steered_feet.is_finite());

    assert_close(proof.step_up_feet.y, 1.0);
    assert!(
        proof.step_up_feet.z > 2.0,
        "the proof walker must pass the step, got z = {}",
        proof.step_up_feet.z
    );
    assert!(proof.step_up_feet.is_finite());
}

#[test]
fn proof_reports_the_cliff_refusal_and_airborne_drift() {
    let proof = motion::run();

    assert!(
        proof.stuck > 0.0,
        "the two-block pit must refuse the drop, got stuck {}",
        proof.stuck
    );
    assert_close(proof.airborne_feet.x, 0.5);
    assert_close(proof.airborne_feet.y, 5.0);
    assert_close(proof.airborne_feet.z, 0.58);
    assert!(proof.airborne_feet.is_finite());
}

#[test]
fn one_flat_step_advances_feet_velocity_phase_and_leaves_stuck_zero() {
    let world = ProofWorld::flat_floor();
    let mut body = body_at(Vec3::new(0.5, 0.0, 0.5));

    move_in(&mut body, dims(), &world, Vec3::Z, SPEED, DT);

    assert_close(body.feet.z, 0.58);
    assert_close(body.feet.y, 0.0);
    assert_close(body.feet.x, 0.5);
    assert_close(body.velocity.z, SPEED);
    assert_close(body.walk_phase, 0.32);
    assert_close(body.stuck, 0.0);
    assert!(body.feet.is_finite() && body.velocity.is_finite());
}

#[test]
fn one_block_step_up_raises_the_feet_and_continues_forward() {
    let world = ProofWorld::floor_with_step();
    let mut body = body_at(Vec3::new(0.5, 0.0, 0.5));

    for _ in 0..30 {
        move_in(&mut body, dims(), &world, Vec3::Z, SPEED, DT);
    }

    assert_close(body.feet.y, 1.0);
    assert!(
        body.feet.z > 2.0,
        "the walker must continue past the step, got z = {}",
        body.feet.z
    );
    assert_close(body.stuck, 0.0);
    assert!(body.feet.is_finite() && body.velocity.is_finite());
}

#[test]
fn cliff_guard_refuses_the_two_block_pit() {
    let world = ProofWorld::floor_with_pit();
    let mut body = body_at(Vec3::new(0.5, 0.0, 0.5));

    for _ in 0..30 {
        move_in(&mut body, dims(), &world, Vec3::Z, SPEED, DT);
    }

    assert!(body.stuck > 0.0, "expected a refused cliff, got 0 stuck");
    assert_close(body.feet.y, 0.0);
    assert_close(body.velocity.z, 0.0);
    assert!(body.feet.is_finite());
}

#[test]
fn airborne_body_drifts_over_an_empty_world() {
    let empty = |_x: i32, _y: i32, _z: i32| BlockKind::Empty;
    let mut body = body_at(Vec3::new(0.5, 5.0, 0.5));

    move_in(&mut body, dims(), &empty, Vec3::Z, SPEED, DT);

    assert_close(body.feet.y, 5.0);
    assert_close(body.feet.z, 0.58);
    assert_close(body.stuck, 0.0);
}

#[test]
fn corridor_path_is_the_exact_five_cell_line_and_repeats() {
    let proof = motion::run();
    assert_eq!(proof.path_len, 5);
    assert!(proof.deterministic, "two A* runs must agree");

    let bounds = NavBounds::new(IVec3::ZERO, IVec3::new(4, 0, 0));
    let corridor = |pos: IVec3| pos.y == 0 && pos.z == 0 && (0..=4).contains(&pos.x);
    let start = IVec3::ZERO;
    let goal = IVec3::new(4, 0, 0);
    let expected = vec![
        IVec3::new(0, 0, 0),
        IVec3::new(1, 0, 0),
        IVec3::new(2, 0, 0),
        IVec3::new(3, 0, 0),
        IVec3::new(4, 0, 0),
    ];

    assert_eq!(
        find_path(&corridor, bounds, start, goal, usize::MAX),
        Ok(expected.clone())
    );
    assert_eq!(expected.len(), proof.path_len);
    for _ in 0..3 {
        assert_eq!(
            find_path(&corridor, bounds, start, goal, usize::MAX),
            Ok(expected.clone()),
            "repeated A* calls must return the same route"
        );
    }
    // Four interior expansions reach the goal exactly at the budget boundary.
    assert_eq!(
        find_path(&corridor, bounds, start, goal, 4),
        Ok(expected.clone())
    );
    assert_eq!(
        find_path(&corridor, bounds, start, goal, 3),
        Err(PathError::NodeLimit)
    );
}

#[test]
fn astar_endpoint_errors_cover_blocked_out_of_bounds_and_budget() {
    let bounds = NavBounds::new(IVec3::ZERO, IVec3::new(4, 4, 4));
    let blocked = IVec3::new(2, 0, 2);
    let grid = |pos: IVec3| bounds.contains(pos) && pos != blocked;
    let start = IVec3::ZERO;
    let goal = IVec3::new(4, 0, 4);

    assert_eq!(
        find_path(&grid, bounds, blocked, goal, usize::MAX),
        Err(PathError::StartBlocked),
        "a blocked start must report StartBlocked"
    );
    assert_eq!(
        find_path(&grid, bounds, IVec3::new(-1, 0, 0), goal, usize::MAX),
        Err(PathError::StartBlocked),
        "an out-of-bounds start must report StartBlocked"
    );
    assert_eq!(
        find_path(&grid, bounds, start, blocked, usize::MAX),
        Err(PathError::GoalBlocked),
        "a blocked goal must report GoalBlocked"
    );
    assert_eq!(
        find_path(&grid, bounds, start, IVec3::new(5, 0, 4), usize::MAX),
        Err(PathError::GoalBlocked),
        "an out-of-bounds goal must report GoalBlocked"
    );
    assert_eq!(
        find_path(&grid, bounds, start, goal, 0),
        Err(PathError::NodeLimit),
        "a zero budget cannot expand a non-goal start"
    );
    assert_eq!(
        find_path(&grid, bounds, start, start, 0),
        Ok(vec![start]),
        "start == goal returns one cell without expanding"
    );
}

#[test]
fn body_box_clear_accepts_the_floor_and_rejects_the_wall() {
    let flat = ProofWorld::flat_floor();
    assert!(body_box_clear(Vec3::new(0.5, 0.0, 0.5), dims(), &flat));
    assert!(body_box_clear(Vec3::new(0.5, 0.5, 0.5), dims(), &flat));

    let wall = ProofWorld::floor_with_wall();
    assert!(!body_box_clear(Vec3::new(2.8, 0.0, 0.5), dims(), &wall));
    assert!(!body_box_clear(Vec3::new(3.0, 0.0, 0.5), dims(), &wall));
    assert!(
        body_box_clear(Vec3::new(2.6, 0.0, 0.5), dims(), &wall),
        "a box that stops short of the wall face stays clear"
    );
}

#[test]
fn invalid_steering_inputs_refuse_without_panicking() {
    let world = ProofWorld::flat_floor();
    let start = body_at(Vec3::new(0.5, 0.0, 0.5));

    let invalid = [
        (Vec3::new(f32::NAN, 0.0, 0.0), SPEED),
        (Vec3::ZERO, SPEED),
        (Vec3::Z, f32::NAN),
        (Vec3::Z, 0.0),
        (Vec3::Z, -1.0),
    ];
    for (dir, speed) in invalid {
        let mut body = start;
        move_in(&mut body, dims(), &world, dir, speed, DT);
        assert_eq!(body.feet, start.feet, "refused step must not move");
        assert_eq!(body.velocity, Vec3::ZERO);
        assert_close(body.stuck, DT);
        assert_eq!(body.walk_phase, start.walk_phase);
    }

    // A non-finite duration has no valid time to accumulate: feet, phase and
    // stuck stay put while the velocity is zeroed.
    let mut body = start;
    body.stuck = 0.25;
    move_in(&mut body, dims(), &world, Vec3::Z, SPEED, f32::NAN);
    assert_eq!(body.feet, start.feet);
    assert_eq!(body.velocity, Vec3::ZERO);
    assert_eq!(body.walk_phase, start.walk_phase);
    assert_close(body.stuck, 0.25);
}
