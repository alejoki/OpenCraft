//! GPU layer for OpenCraft.
//!
//! The host creates the window, the `wgpu::Instance` and the surface; this
//! crate takes that host-created instance and surface and owns the adapter,
//! device, queue, surface configuration and depth attachment (with the chunk
//! buffer and mesh wire-format modules arriving beside them). It builds on
//! `opencraft-core` for neutral math and never depends on the game crate.

pub mod context;
