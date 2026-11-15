//! Neutral primitives shared by the OpenCraft engine crates.
//!
//! Holds voxel math, chunk coordinates, block-query and RGBA byte helpers that
//! carry no game identity: there is no `BlockId`, `ContentRegistry`, `Player`,
//! `WorldSession`, GPU or windowing dependency here. The game crate and every
//! engine member build on this crate, never the other way around.

pub mod block;
pub mod chunk;
