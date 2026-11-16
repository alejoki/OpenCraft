//! Game-side render content.
//!
//! The game keeps content identity (the block texture array spec and its
//! procedural art) and mob scene assembly here. The GPU frame seam — context,
//! buffers, sky, weather and the renderer/frame pipeline — lives in
//! `opencraft-gfx`.

pub mod mob;
pub mod texture;
