//! Player reach raycast: a thin adapter from the game's block-identity lookup
//! to the neutral voxel DDA in `opencraft-physics`.
//!
//! The traversal, tie-break order and solid-cell predicate live in
//! [`opencraft_physics::raycast::raycast_voxels`]. This module only wraps the
//! caller's `Fn(i32, i32, i32) -> BlockId` closure in [`BlockIdQuery`], so every
//! existing call site keeps its signature unchanged.

use glam::Vec3;

use crate::world::block::{BlockId, BlockIdQuery};

pub use opencraft_physics::raycast::RaycastHit;

pub fn raycast_voxels<F: Fn(i32, i32, i32) -> BlockId>(
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    get_block: F,
) -> Option<RaycastHit> {
    opencraft_physics::raycast::raycast_voxels(origin, dir, max_dist, &BlockIdQuery(get_block))
}
