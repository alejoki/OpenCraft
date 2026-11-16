//! Neutral physics primitives for OpenCraft.
//!
//! Box-vs-solid collision, the voxel raycast and swept AABB tests against the
//! neutral `BlockQuery` contract live here; the crate builds on
//! `opencraft-core` and never depends on the `opencraft` game crate.

pub mod raycast;
pub mod sweep;

pub use opencraft_core::geometry::solid_overlaps;
