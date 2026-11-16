//! Game-facing camera path.
//!
//! The projection/frustum primitives live in `opencraft-core`; this module
//! re-exports them so every existing `crate::player::camera::*` path keeps
//! resolving unchanged.

pub use opencraft_core::camera::*;
