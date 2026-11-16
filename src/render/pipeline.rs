//! Re-export shim: the frame seam lives in `opencraft-gfx`.
//!
//! The game still imports these names through `crate::render::pipeline` until
//! the façade is deleted; the implementation (camera/sky uniforms, overlay
//! lines, the UI-pass trait and the frame entry point) is owned by
//! [`opencraft_gfx::pipeline`].
pub use opencraft_gfx::pipeline::*;
