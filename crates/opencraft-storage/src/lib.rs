//! Neutral sectioned save archive, canonical chunk-section codec and bounded
//! file helpers for OpenCraft.
//!
//! This crate owns the save mechanics that do not need game identity: the
//! sectioned archive container, the canonical per-chunk palette codec over raw
//! `u16` content indices, and the bounded/atomic file helpers. It never reads
//! a game block id, content registry, session, player or mob type, and never
//! depends on a GPU or windowing crate. `archive` is the neutral container;
//! `chunk_section` is the canonical palette codec.

pub mod archive;
pub mod chunk_section;
mod error;

pub use error::StorageError;
