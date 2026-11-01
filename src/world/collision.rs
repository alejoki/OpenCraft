//! THE box-vs-solid predicate shared by the player and every mob (M009/S06).
//!
//! Both the player sub-step resolver and the hostile mover must agree exactly
//! on when an axis-aligned box touches a solid block. Keeping one predicate
//! (instead of a player copy and a mob copy) means the epsilon and flooring
//! rule can never drift between them: a box that rests *exactly* on a solid
//! top face is free, while any real penetration is caught.
//!
//! The rule mirrors the long-standing player physics helper:
//! expand the box by a tiny epsilon inward on `min`, shrink it inward on
//! `max`, floor both, and walk the inclusive block range. A non-solid block
//! (air, water) never counts.

use glam::Vec3;

use crate::world::block::BlockId;

/// Whether the axis-aligned box `[min, max]` overlaps any solid block.
///
/// `get_block` is the caller's world read; it may return air for unloaded
/// chunks, which is the same behaviour as the player physics path.
///
/// The epsilon (`1e-4`) is deliberately identical to the original player
/// helper: a box whose face sits exactly on a block boundary does not count
/// as overlapping the neighbouring block, while a box pushed `1e-3` into it
/// does.
pub fn solid_overlaps(min: Vec3, max: Vec3, get_block: &impl Fn(i32, i32, i32) -> BlockId) -> bool {
    let eps = 1e-4;
    let min_x = (min.x + eps).floor() as i32;
    let max_x = (max.x - eps).floor() as i32;
    let min_y = (min.y + eps).floor() as i32;
    let max_y = (max.y - eps).floor() as i32;
    let min_z = (min.z + eps).floor() as i32;
    let max_z = (max.z - eps).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                if get_block(x, y, z).is_solid() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_at(x: i32, y: i32, z: i32) -> BlockId {
        if x == 0 && y == 0 && z == 0 {
            BlockId::Stone
        } else {
            BlockId::Air
        }
    }

    #[test]
    fn a_box_resting_exactly_on_a_solid_top_face_does_not_overlap() {
        // Block (0,0,0) spans y in [0, 1]; feet exactly at y = 1.0.
        let min = Vec3::new(0.2, 1.0, 0.2);
        let max = Vec3::new(0.8, 2.8, 0.8);
        assert!(!solid_overlaps(min, max, &block_at));
    }

    #[test]
    fn a_box_one_thousandth_inside_does_overlap() {
        let min = Vec3::new(0.2, 0.999, 0.2);
        let max = Vec3::new(0.8, 2.8, 0.8);
        assert!(solid_overlaps(min, max, &block_at));
    }

    #[test]
    fn an_empty_region_returns_false() {
        let min = Vec3::new(5.0, 5.0, 5.0);
        let max = Vec3::new(5.6, 6.8, 5.6);
        assert!(!solid_overlaps(min, max, &block_at));
    }

    #[test]
    fn non_solid_blocks_never_collide() {
        for block in [BlockId::Air, BlockId::Water] {
            let get = |x: i32, y: i32, z: i32| {
                if x == 0 && y == 0 && z == 0 {
                    block
                } else {
                    BlockId::Air
                }
            };
            let min = Vec3::new(-0.2, -0.2, -0.2);
            let max = Vec3::new(0.2, 0.2, 0.2);
            assert!(!solid_overlaps(min, max, &get), "{block:?} is passable");
        }
    }

    #[test]
    fn a_box_spanning_several_cells_catches_any_solid_cell() {
        // Solid at the far +X/+Y/+Z corner of the swept range only.
        let get = |x: i32, y: i32, z: i32| {
            if x == 2 && y == 2 && z == 2 {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        };
        let min = Vec3::new(0.5, 0.5, 0.5);
        let max = Vec3::new(2.5, 2.5, 2.5);
        assert!(solid_overlaps(min, max, &get));
    }
}
