//! Neutral world generation for OpenCraft.
//!
//! This crate owns biome/tag data, height and ruin queries, prefab and village
//! codecs/plans/placement, loot vocabulary and terrain generation over raw
//! `u16` block ids.
//!
//! Neutrality: no module may import the `opencraft` game crate, a game
//! `BlockId`/`ItemId`/`ContentRegistry`/`Chunk`/`WorldSession`, or a GPU,
//! windowing, audio or storage crate. The crate speaks raw `u16` block ids and
//! neutral tags only; the game adapts them at its own boundary.

pub mod biome;
pub mod block;
pub mod height;
pub mod loot;
mod noise;
pub mod ruin;
