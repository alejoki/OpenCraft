//! Navigation and pathfinding for OpenCraft mobs.
//!
//! Path search runs over the neutral `BlockQuery` contract so the crate builds
//! on `opencraft-core` and never depends on the `opencraft` game crate.
//!
//! [`steering`] holds the walking mover and body-box predicate: flat move,
//! one-block step-up/down, cliff guard, footprint support, airborne drift and
//! blocked-time accumulation over caller-supplied body values.

pub mod steering;

pub use opencraft_core as core;
pub use steering::{body_box_clear, move_in, BodyBox, SteeringBody};
