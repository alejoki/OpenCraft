//! Device-free physics proof over the neutral solver, raycast and sweep APIs.
//!
//! The proof steps a body onto a synthetic floor, walks it into a wall, casts
//! a voxel ray and sweeps a segment against an AABB. All scenarios read the
//! world through `opencraft_core::block::BlockQuery`; no GPU device, adapter or
//! surface is created.

use glam::{Vec2, Vec3};

use opencraft_physics::movement::{
    step_body, Body, BodyDims, FallTracker, MoveInput, MoveMode, MovementRules, StepInput,
};
use opencraft_physics::raycast::{raycast_voxels, RaycastHit};
use opencraft_physics::sweep::segment_aabb;

use crate::world::ProofWorld;

/// Upper bound the proof consumer applies to caller velocity before stepping.
pub const MAX_PROOF_SPEED: f32 = 64.0;

/// Clamp every velocity component into `-MAX_PROOF_SPEED..=MAX_PROOF_SPEED`.
///
/// `step_body` derives its sub-step count from `velocity.length() * dt` with no
/// upper bound, so a corrupt caller velocity can stall the solver. The shipped
/// crate keeps that contract; this consumer owns the bound instead.
pub fn clamp_body_velocity(body: &mut Body) {
    // shortcut: caller clamps velocity; step_body's sub-step count is uncapped,
    // upgrade by moving the cap into the solver if the game ever needs it
    let limit = Vec3::splat(MAX_PROOF_SPEED);
    body.velocity = body.velocity.clamp(-limit, limit);
}

/// The observable result of one device-free physics proof run.
#[derive(Clone, Copy, Debug)]
pub struct PhysicsProof {
    /// Body that fell from `y = 4.0` and landed on the flat floor.
    pub landing: Body,
    /// Body that walked +X into the two-block wall and stopped.
    pub wall: Body,
    /// First solid cell hit by a +X ray at feet level.
    pub ray_hit: RaycastHit,
    /// Entry parameter of the swept segment into the proof box.
    pub sweep_t: f32,
}

/// Run the landing, wall, raycast and sweep scenarios deterministically.
pub fn run() -> PhysicsProof {
    let dims = BodyDims {
        half_width: 0.35,
        height: 1.8,
    };
    let rules = MovementRules::default();
    let dt = 1.0 / 60.0;

    let flat_floor = ProofWorld::flat_floor();
    let idle = MoveInput::default();
    let mut fall = FallTracker::default();
    let mut landing = Body {
        position: Vec3::new(0.5, 4.0, 0.5),
        velocity: Vec3::ZERO,
        on_ground: false,
    };
    for _ in 0..300 {
        step_body(
            &mut landing,
            dims,
            &rules,
            &mut fall,
            StepInput {
                dt,
                yaw: 0.0,
                input: &idle,
                mode: MoveMode::Walk,
                impulse_xz: Vec2::ZERO,
            },
            &flat_floor,
        );
        if landing.on_ground {
            break;
        }
    }

    let wall_world = ProofWorld::floor_with_wall();
    let walk = MoveInput {
        forward: true,
        ..MoveInput::default()
    };
    let mut wall_fall = FallTracker::default();
    let mut wall = Body {
        position: Vec3::new(0.5, 0.0, 0.5),
        velocity: Vec3::ZERO,
        on_ground: true,
    };
    for _ in 0..120 {
        step_body(
            &mut wall,
            dims,
            &rules,
            &mut wall_fall,
            StepInput {
                dt,
                yaw: 0.0,
                input: &walk,
                mode: MoveMode::Walk,
                impulse_xz: Vec2::ZERO,
            },
            &wall_world,
        );
    }

    let ray_hit = raycast_voxels(Vec3::new(0.5, 0.5, 0.5), Vec3::X, 10.0, &wall_world)
        .expect("the synthetic wall must be raycast");

    let sweep_t = segment_aabb(
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(4.0, 0.0, 0.0),
        Vec3::new(3.0, 0.0, 0.0),
        Vec3::new(4.0, 1.0, 1.0),
    )
    .expect("the swept segment must enter the proof box");

    PhysicsProof {
        landing,
        wall,
        ray_hit,
        sweep_t,
    }
}
