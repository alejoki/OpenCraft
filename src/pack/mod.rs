//! GPU-free texture pack loading core.
//!
//! Everything in this module is pure data handling: no wgpu or glyphon
//! imports may appear here so the whole surface is testable headlessly.
//! Later tasks add the manifest, PNG decode, and `LoadedPack` contract.

pub mod decode;
pub mod manifest;
pub mod resample;
