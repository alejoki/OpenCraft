//! Neutral primitives shared by the OpenCraft engine crates.
//!
//! Holds voxel math, chunk coordinates, block-query and RGBA byte helpers that
//! carry no game identity: no block ids, content registry, session state,
//! actor types, GPU or windowing dependency appears here. The game crate and
//! every engine member build on this crate, never the other way around.

pub mod block;
pub mod camera;
pub mod chunk;
pub mod geometry;
pub mod rgba;
