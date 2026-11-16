//! External contract for the device-free physics half of the reuse proof.
//!
//! Imports come from the consumer crate and the engine crates' public modules
//! on purpose: this target proves the proof scenarios and the neutral physics
//! APIs are usable from an outside crate that never imports `opencraft`.

use glam::{IVec3, Vec2, Vec3};

use opencraft_physics::movement::{
    step_body, Body, BodyDims, FallTracker, MoveInput, MoveMode, MovementRules, StepInput,
};
use opencraft_physics::raycast::raycast_voxels;
use opencraft_physics::sweep::segment_aabb;

use engine_reuse_proof::physics::{self, MAX_PROOF_SPEED};
use engine_reuse_proof::world::ProofWorld;

const EPS: f32 = 1e-4;

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= EPS,
        "expected {expected} ± {EPS}, got {actual}"
    );
}

fn dims() -> BodyDims {
    BodyDims {
        half_width: 0.35,
        height: 1.8,
    }
}

#[test]
fn landing_scenario_lands_on_the_floor() {
    let proof = physics::run();

    assert_close(proof.landing.position.y, 0.0);
    assert!(
        proof.landing.on_ground,
        "a body falling onto the flat floor must be grounded"
    );
    assert_eq!(proof.landing.velocity.y, 0.0);
}

#[test]
fn wall_scenario_stops_short_of_the_wall() {
    let proof = physics::run();

    assert!(proof.wall.position.is_finite(), "wall body position");
    assert!(proof.wall.velocity.is_finite(), "wall body velocity");
    assert!(proof.wall.on_ground, "the walker must stay on the floor");
    // The body stops at the wall face x = 3.0 minus the 0.35 half-width.
    assert!(
        (2.5..2.7).contains(&proof.wall.position.x),
        "wall body stopped at x = {}",
        proof.wall.position.x
    );

    // One more +X step must not pass the wall: the position is unchanged and
    // the body box still ends before the wall face.
    let world = ProofWorld::floor_with_wall();
    let input = MoveInput {
        forward: true,
        ..MoveInput::default()
    };
    let mut body = proof.wall;
    let mut fall = FallTracker::default();
    step_body(
        &mut body,
        dims(),
        &MovementRules::default(),
        &mut fall,
        StepInput {
            dt: 1.0 / 60.0,
            yaw: 0.0,
            input: &input,
            mode: MoveMode::Walk,
            impulse_xz: Vec2::ZERO,
        },
        &world,
    );
    assert_close(body.position.x, proof.wall.position.x);
    assert!(
        body.position.x + dims().half_width < 3.0 + EPS,
        "the body box at {} passed the wall face",
        body.position.x
    );
}

#[test]
fn raycast_hits_the_wall_face() {
    let world = ProofWorld::floor_with_wall();
    let hit = raycast_voxels(Vec3::new(0.5, 0.5, 0.5), Vec3::X, 10.0, &world)
        .expect("the +X ray must reach the synthetic wall");

    assert_eq!(hit.block_pos, IVec3::new(3, 0, 0));
    assert_eq!(hit.adjacent_pos, IVec3::new(2, 0, 0));
    assert_eq!(hit.normal, IVec3::new(-1, 0, 0));
    assert_close(hit.distance, 2.5);

    let proof = physics::run();
    assert_eq!(proof.ray_hit, hit);
}

#[test]
fn sweep_enters_the_box_and_a_vertical_miss_is_none() {
    let box_min = Vec3::new(3.0, 0.0, 0.0);
    let box_max = Vec3::new(4.0, 1.0, 1.0);

    let t = segment_aabb(
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(4.0, 0.0, 0.0),
        box_min,
        box_max,
    )
    .expect("the segment must enter the proof box");
    assert_close(t, 0.625);
    assert_close(physics::run().sweep_t, t);

    let miss = segment_aabb(
        Vec3::new(3.5, 2.0, 0.5),
        Vec3::new(0.0, 4.0, 0.0),
        box_min,
        box_max,
    );
    assert_eq!(miss, None, "a vertical ray above the box must miss");
}

#[test]
fn absurd_velocity_is_clamped_before_stepping() {
    let mut body = Body {
        position: Vec3::new(0.5, 0.5, 0.5),
        velocity: Vec3::splat(1e9),
        on_ground: false,
    };

    physics::clamp_body_velocity(&mut body);
    for (axis, component) in ["x", "y", "z"].into_iter().zip(body.velocity.to_array()) {
        assert!(
            component.abs() <= MAX_PROOF_SPEED,
            "clamped velocity.{axis} = {component} exceeds {MAX_PROOF_SPEED}"
        );
    }

    let mut fall = FallTracker::default();
    step_body(
        &mut body,
        dims(),
        &MovementRules::default(),
        &mut fall,
        StepInput {
            dt: 1.0 / 60.0,
            yaw: 0.0,
            input: &MoveInput::default(),
            mode: MoveMode::Walk,
            impulse_xz: Vec2::ZERO,
        },
        &ProofWorld::flat_floor(),
    );

    assert!(
        body.position.is_finite(),
        "clamped step produced position {:?}",
        body.position
    );
    assert!(
        body.velocity.is_finite(),
        "clamped step produced velocity {:?}",
        body.velocity
    );
}
