//! Neutral sectioned save archive, canonical chunk-section codec and bounded
//! file helpers for OpenCraft.
//!
//! This crate owns the save mechanics that do not need game identity: the
//! sectioned archive container, the canonical per-chunk palette codec over raw
//! `u16` content indices, and the bounded/atomic file helpers. It never reads
//! a game block id, content registry, session, player or mob type, and never
//! depends on a GPU or windowing crate. Modules land with the storage slices;
//! this skeleton intentionally has none yet.
