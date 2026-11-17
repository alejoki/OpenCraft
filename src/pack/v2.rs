//! Game-side re-export of the v2 content parser now owned by `opencraft-content`.
//!
//! The parser, its per-entry diagnostics and the asset pipeline live in
//! `opencraft_content::v2`; this shim keeps the historical module path working
//! so call sites and tests need no change.

pub use opencraft_content::v2::*;
