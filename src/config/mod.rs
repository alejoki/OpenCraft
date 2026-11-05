//! Curated, creator-tunable configuration.
//!
//! [`registry`] is the single source of truth for the M014 config contract:
//! the six files, their curated keys, types, defaults, ranges, units, apply
//! modes, and documentation. The runtime loader, digest, and `--check-config`
//! renderer build on it in later tasks.

pub mod registry;

pub use registry::{
    docs_table, index_of, ApplyMode, ConfigFile, ConfigKind, ConfigRow, ConfigValue, REGISTRY,
};
