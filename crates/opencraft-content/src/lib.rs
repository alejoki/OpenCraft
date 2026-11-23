//! Neutral content-pack format, validation and asset vocabulary for OpenCraft.
//!
//! This crate owns the pack format/validation surface, the neutral authored
//! content model and name vocabulary, allocation, patches, placeholders and
//! FNV digest. It carries no game block id, session state, GPU, windowing,
//! audio, storage or worldgen dependency.
//!
//! Neutrality rules: the only allowed dependencies are `opencraft-core`, `toml`
//! and `png`, with `tempfile` as a dev-dependency. No module here may import
//! the `opencraft` game crate, a GPU/windowing crate, audio, storage or
//! worldgen.

pub mod asset;
pub mod catalog;
pub mod decode;
pub mod manifest;
pub mod resample;
pub mod spec;
pub mod v3;
pub mod validate;
pub mod vocabulary;
mod warning;

pub use warning::PackWarning;
