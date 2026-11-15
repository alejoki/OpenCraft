//! Navigation and pathfinding for OpenCraft mobs.
//!
//! Path search runs over the neutral `BlockQuery` contract so the crate builds
//! on `opencraft-core` and never depends on the `opencraft` game crate.

pub use opencraft_core as core;
