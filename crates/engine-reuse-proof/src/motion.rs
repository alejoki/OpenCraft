//! Device-free steering and deterministic nav-grid A* proof.
//!
//! The proof drives the neutral `opencraft-pathfinding` walking mover over the
//! synthetic [`ProofWorld`] and runs the pure A* over small closure grids.
//! Every scenario is a fixed sequence of calls: no clock, RNG, I/O or game
//! identity is involved, so the result is identical on every run and CI needs
//! no GPU driver or game package.

use glam::{IVec3, Vec3};

use opencraft_core::block::BlockKind;
use opencraft_pathfinding::{find_path, move_in, BodyBox, NavBounds, PathError, SteeringBody};

use crate::world::ProofWorld;

/// Speed every proof step uses, in blocks per second.
const SPEED: f32 = 1.6;
/// Fixed step duration every proof step uses, in seconds.
const DT: f32 = 0.05;
/// Float tolerance for the proof's reported values.
const EPS: f32 = 1e-4;

/// The observable result of one device-free motion proof run.
#[derive(Clone, Copy, Debug)]
pub struct MotionProof {
    /// Feet after 20 flat +Z steps from `z = 0.5`.
    pub steered_feet: Vec3,
    /// Feet after 30 +Z steps over the one-block step.
    pub step_up_feet: Vec3,
    /// Accumulated refusal time after walking into the two-block pit.
    pub stuck: f32,
    /// Feet after one +Z step over an empty world while airborne.
    pub airborne_feet: Vec3,
    /// Cell count of the corridor A* path, inclusive of both endpoints.
    pub path_len: usize,
    /// Whether two identical A* calls returned the same path.
    pub deterministic: bool,
}

/// Caller-supplied body dimensions shared by every walking scenario.
fn dims() -> BodyBox {
    BodyBox {
        half_width: 0.35,
        height: 1.8,
    }
}

/// A body with no motion history standing at `feet`.
fn body_at(feet: Vec3) -> SteeringBody {
    SteeringBody {
        feet,
        velocity: Vec3::ZERO,
        walk_phase: 0.0,
        stuck: 0.0,
    }
}

/// Assert a reported proof value within [`EPS`].
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= EPS,
        "{what}: expected {expected} ± {EPS}, got {actual}"
    );
}

/// Run the flat, step-up, cliff, airborne and A* scenarios deterministically.
pub fn run() -> MotionProof {
    // Flat ground: 20 steps of speed * dt = 0.08 blocks advance feet.z by 1.6.
    let flat = ProofWorld::flat_floor();
    let mut flat_body = body_at(Vec3::new(0.5, 0.0, 0.5));
    for _ in 0..20 {
        move_in(&mut flat_body, dims(), &flat, Vec3::Z, SPEED, DT);
    }
    assert_close(flat_body.feet.z, 2.1, "flat feet.z");
    assert_close(flat_body.feet.y, 0.0, "flat feet.y");
    assert_close(flat_body.feet.x, 0.5, "flat feet.x");
    assert!(
        flat_body.walk_phase > 0.0,
        "flat walk_phase must advance, got {}",
        flat_body.walk_phase
    );
    assert_close(flat_body.stuck, 0.0, "flat stuck");
    assert!(flat_body.feet.is_finite(), "flat feet {:?}", flat_body.feet);

    // One-block step at z = 2: the walker climbs and keeps moving forward.
    let step = ProofWorld::floor_with_step();
    let mut step_body = body_at(Vec3::new(0.5, 0.0, 0.5));
    for _ in 0..30 {
        move_in(&mut step_body, dims(), &step, Vec3::Z, SPEED, DT);
    }
    assert_close(step_body.feet.y, 1.0, "step-up feet.y");
    assert!(
        step_body.feet.z > 2.0,
        "step-up feet.z must pass the step, got {}",
        step_body.feet.z
    );
    assert!(
        step_body.feet.is_finite() && step_body.velocity.is_finite(),
        "step-up motion must stay finite: feet {:?}, velocity {:?}",
        step_body.feet,
        step_body.velocity
    );

    // Two-block pit: the cliff guard refuses the drop and accumulates stuck.
    let pit = ProofWorld::floor_with_pit();
    let mut pit_body = body_at(Vec3::new(0.5, 0.0, 0.5));
    for _ in 0..30 {
        move_in(&mut pit_body, dims(), &pit, Vec3::Z, SPEED, DT);
    }
    assert!(
        pit_body.stuck > 0.0,
        "cliff guard must accumulate stuck time, got {}",
        pit_body.stuck
    );
    assert_close(pit_body.feet.y, 0.0, "cliff feet.y");
    assert!(pit_body.feet.is_finite(), "cliff feet {:?}", pit_body.feet);

    // Airborne over an empty world: one horizontal drift step, no gravity.
    let empty = |_x: i32, _y: i32, _z: i32| BlockKind::Empty;
    let mut airborne_body = body_at(Vec3::new(0.5, 5.0, 0.5));
    move_in(&mut airborne_body, dims(), &empty, Vec3::Z, SPEED, DT);
    assert_close(airborne_body.feet.y, 5.0, "airborne feet.y");
    assert_close(airborne_body.feet.z, 0.58, "airborne feet.z");

    // Deterministic A*: the inclusive five-cell corridor is the only route.
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
    let first = find_path(&corridor, bounds, start, goal, usize::MAX).expect("corridor path");
    assert_eq!(first, expected, "corridor path must be the exact line");
    let second =
        find_path(&corridor, bounds, start, goal, usize::MAX).expect("repeated corridor path");
    assert_eq!(second, expected, "repeated corridor path must match");
    assert_eq!(
        find_path(&corridor, bounds, start, goal, 0),
        Err(PathError::NodeLimit),
        "a zero budget must stop before a non-goal start expands"
    );

    MotionProof {
        steered_feet: flat_body.feet,
        step_up_feet: step_body.feet,
        stuck: pit_body.stuck,
        airborne_feet: airborne_body.feet,
        path_len: first.len(),
        deterministic: first == second,
    }
}
