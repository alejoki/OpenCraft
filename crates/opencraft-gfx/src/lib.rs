//! GPU layer for OpenCraft.
//!
//! The host creates the window, the `wgpu::Instance` and the surface; this
//! crate takes that host-created instance and surface and owns the adapter,
//! device, queue, surface configuration and depth attachment. It also owns
//! the chunk render store and the packed-quad/chunk-mesh wire format that the
//! host uploads and draws. It builds on `opencraft-core` for neutral math and
//! never depends on the game crate.

pub mod buffer;
pub mod context;
pub mod mesh;
