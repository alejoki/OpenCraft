//! Game-side adapter over the engine box-vs-solid predicate.
//!
//! The epsilon/flooring rule lives in `opencraft-physics` (re-exported from
//! `opencraft-core`) so the player and every mob mover share one copy. This
//! module keeps the historical `(min, max, get_block)` signature so those
//! call sites stay unchanged.

use glam::Vec3;

use crate::world::block::{BlockId, BlockIdQuery};
use opencraft_core::geometry::Aabb;

/// Whether the axis-aligned box `[min, max]` overlaps any solid block.
///
/// `get_block` is the caller's world read; it may return air for unloaded
/// chunks, which is the same behaviour as the player physics path.
pub fn solid_overlaps(min: Vec3, max: Vec3, get_block: &impl Fn(i32, i32, i32) -> BlockId) -> bool {
    opencraft_physics::solid_overlaps(Aabb::new(min, max), &BlockIdQuery(get_block))
}
