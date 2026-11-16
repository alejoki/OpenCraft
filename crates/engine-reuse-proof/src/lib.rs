//! Device-free external reuse proof for the OpenCraft engine crates.
//!
//! This crate is an outside consumer of `opencraft-core`, `opencraft-gfx`,
//! `opencraft-physics`, `opencraft-pathfinding` and `opencraft-storage` (plus
//! `glam`). It never imports the `opencraft` game package, never creates a
//! `wgpu` adapter, device or surface, and never calls `create_texture`,
//! `create_view`, `upload_layer(s)` or `create_model_pipeline`. Everything it
//! proves runs headless and device-free, so CI needs no GPU driver. The
//! storage proof is in-memory only: it opens no file and reads no clock.
//!
//! The synthetic [`world::ProofWorld`] implements the neutral
//! `opencraft_core::block::BlockQuery` contract and answers only
//! `BlockKind` cells, so the consumer never names a game block identity.

pub mod gfx;
pub mod motion;
pub mod physics;
pub mod report;
pub mod storage;
pub mod world;

/// Run every device-free proof and aggregate the result.
///
/// This is the single entry point the `engine-reuse-proof` bin prints. It runs
/// the physics, gfx, motion and storage proofs in a fixed order and creates no
/// adapter, device, surface, file, thread or child process.
pub fn run() -> report::ProofReport {
    report::ProofReport {
        physics: physics::run(),
        gfx: gfx::run(),
        motion: motion::run(),
        storage: storage::run(),
    }
}
