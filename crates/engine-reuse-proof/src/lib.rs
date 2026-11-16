//! Device-free external reuse proof for the OpenCraft engine crates.
//!
//! This crate is an outside consumer of `opencraft-core`, `opencraft-gfx`,
//! `opencraft-physics` and `opencraft-pathfinding` (plus `glam`). It never
//! imports the `opencraft` game package, never creates a `wgpu` adapter,
//! device or surface, and never calls `create_texture`, `create_view`,
//! `upload_layer(s)` or `create_model_pipeline`. Everything it proves runs
//! headless and device-free, so CI needs no GPU driver.
//!
//! The synthetic [`world::ProofWorld`] implements the neutral
//! `opencraft_core::block::BlockQuery` contract and answers only
//! `BlockKind` cells, so the consumer never names a game block identity.

pub mod gfx;
pub mod physics;
pub mod world;
