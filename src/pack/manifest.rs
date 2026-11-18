//! Pure `pack.toml` parsing with two-phase per-field fallback.
//!
//! The parser, its neutral [`PackWarning`] and [`read_manifest_text`] live in
//! `opencraft-content`; this module is the historical game path and only
//! re-exports them.

pub use opencraft_content::manifest::*;
