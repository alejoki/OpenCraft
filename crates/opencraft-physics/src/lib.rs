//! Neutral physics primitives for OpenCraft.
//!
//! Box-vs-solid collision, the voxel raycast, swept AABB tests and the
//! kinematic walk/fly solver all run against the neutral `BlockQuery` contract
//! here; the crate builds on `opencraft-core` and never depends on the
//! `opencraft` game crate.

pub mod movement;
pub mod raycast;
pub mod sweep;

pub use opencraft_core::geometry::solid_overlaps;
