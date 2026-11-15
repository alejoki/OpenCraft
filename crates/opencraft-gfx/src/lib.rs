//! GPU and windowing layer for OpenCraft.
//!
//! Owns the wgpu device/surface setup, render pipelines, and window event
//! plumbing once the extraction lands; it builds on `opencraft-core` for
//! neutral math and never depends on the `opencraft` game crate.

pub use opencraft_core as core;
