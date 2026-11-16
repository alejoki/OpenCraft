//! Axis-aligned boxes, the box-vs-solid predicate and slab-method ray
//! intersection.
//!
//! Neutral geometry only: `Aabb` stores explicit `min`/`max` corners and its
//! constructors take dimensions from the caller, so body-sized constants stay
//! in the game crate. The game's `crate::player::physics::Aabb` and
//! `crate::player::melee::ray_aabb` paths re-export these symbols unchanged.

use glam::Vec3;

use crate::block::BlockQuery;

/// Axis-aligned bounding box in world coordinates.
#[derive(Copy, Clone, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    #[inline]
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Box centred on `feet` horizontally and extending `height` upward.
    ///
    /// The caller supplies `half_width` and `height`, so this crate never
    /// carries a player (or any creature) size constant.
    #[inline]
    pub fn from_feet(feet: Vec3, half_width: f32, height: f32) -> Self {
        Self {
            min: Vec3::new(feet.x - half_width, feet.y, feet.z - half_width),
            max: Vec3::new(feet.x + half_width, feet.y + height, feet.z + half_width),
        }
    }

    #[inline]
    pub fn intersects_block(&self, bx: i32, by: i32, bz: i32) -> bool {
        let eps = 1e-4;
        self.max.x > bx as f32 + eps
            && self.min.x < (bx + 1) as f32 - eps
            && self.max.y > by as f32 + eps
            && self.min.y < (by + 1) as f32 - eps
            && self.max.z > bz as f32 + eps
            && self.min.z < (bz + 1) as f32 - eps
    }
}

/// Whether the axis-aligned box `[aabb.min, aabb.max]` overlaps any solid
/// cell in `world`.
///
/// This is the single epsilon/flooring predicate shared by every moving body:
/// expand the box by a tiny epsilon inward on `min`, shrink it inward on
/// `max`, floor both, and walk the inclusive block range (y, then z, then x).
/// A cell that is not solid (empty or liquid) never counts.
///
/// The epsilon (`1e-4`) means a box whose face sits exactly on a cell boundary
/// does not overlap the neighbouring cell, while a box pushed `1e-3` into it
/// does. Non-finite corners and inverted boxes (any `min > max` axis) are
/// rejected before the loop.
pub fn solid_overlaps(aabb: Aabb, world: &impl BlockQuery) -> bool {
    if !aabb.min.is_finite() || !aabb.max.is_finite() {
        return false;
    }

    let eps = 1e-4;
    let min_x = (aabb.min.x + eps).floor() as i32;
    let max_x = (aabb.max.x - eps).floor() as i32;
    let min_y = (aabb.min.y + eps).floor() as i32;
    let max_y = (aabb.max.y - eps).floor() as i32;
    let min_z = (aabb.min.z + eps).floor() as i32;
    let max_z = (aabb.max.z - eps).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                if world.block_kind(x, y, z).is_solid() {
                    return true;
                }
            }
        }
    }
    false
}

/// Axis-parallel epsilon below which a ray component is treated as parallel to
/// the slab. Also the floor below which a direction is structurally degenerate.
const RAY_EPSILON_SQ: f32 = 1e-12;
/// Per-axis parallel test, the square root of [`RAY_EPSILON_SQ`].
const RAY_PARALLEL_EPSILON: f32 = 1e-6;

/// Slab-method ray/AABB intersection returning the **entry distance**.
///
/// `dir` does not have to be normalized: the returned distance is measured in
/// units of `dir`'s length, so a normalized ray yields a world-space distance.
///
/// Returns `None` when the ray or the box is non-finite, when `dir` is
/// structurally degenerate (`dir.length_squared() < 1e-12`), on a miss, and
/// when the entry distance is not finite or is negative. A ray whose origin
/// already lies inside the box has a negative entry distance and is therefore
/// rejected: only an entry *ahead of* the origin counts as a hit.
pub fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    if !origin.is_finite() || !dir.is_finite() || !min.is_finite() || !max.is_finite() {
        return None;
    }
    if dir.length_squared() < RAY_EPSILON_SQ {
        return None;
    }

    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    for (o, d, lo, hi) in [
        (origin.x, dir.x, min.x, max.x),
        (origin.y, dir.y, min.y, max.y),
        (origin.z, dir.z, min.z, max.z),
    ] {
        if d.abs() < RAY_PARALLEL_EPSILON {
            // Parallel to this slab: the origin must already be inside it.
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let inv = 1.0 / d;
        let a = (lo - o) * inv;
        let b = (hi - o) * inv;
        let (near, far) = if a <= b { (a, b) } else { (b, a) };
        t_enter = t_enter.max(near);
        t_exit = t_exit.min(far);
        if t_enter > t_exit {
            return None;
        }
    }

    if !t_enter.is_finite() || t_enter < 0.0 {
        return None;
    }
    Some(t_enter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockKind;

    fn solid_at_origin(x: i32, y: i32, z: i32) -> BlockKind {
        if x == 0 && y == 0 && z == 0 {
            BlockKind::Solid
        } else {
            BlockKind::Empty
        }
    }

    #[test]
    fn a_box_resting_exactly_on_a_solid_top_face_does_not_overlap() {
        // Cell (0,0,0) spans y in [0, 1]; feet exactly at y = 1.0.
        let min = Vec3::new(0.2, 1.0, 0.2);
        let max = Vec3::new(0.8, 2.8, 0.8);
        assert!(!solid_overlaps(Aabb::new(min, max), &solid_at_origin));
    }

    #[test]
    fn a_box_one_thousandth_inside_does_overlap() {
        let min = Vec3::new(0.2, 0.999, 0.2);
        let max = Vec3::new(0.8, 2.8, 0.8);
        assert!(solid_overlaps(Aabb::new(min, max), &solid_at_origin));
    }

    #[test]
    fn an_empty_region_returns_false() {
        let min = Vec3::new(5.0, 5.0, 5.0);
        let max = Vec3::new(5.6, 6.8, 5.6);
        assert!(!solid_overlaps(Aabb::new(min, max), &solid_at_origin));
    }

    #[test]
    fn non_solid_blocks_never_collide() {
        for kind in [BlockKind::Empty, BlockKind::Liquid] {
            let world = |x: i32, y: i32, z: i32| {
                if x == 0 && y == 0 && z == 0 {
                    kind
                } else {
                    BlockKind::Empty
                }
            };
            let min = Vec3::new(-0.2, -0.2, -0.2);
            let max = Vec3::new(0.2, 0.2, 0.2);
            assert!(
                !solid_overlaps(Aabb::new(min, max), &world),
                "{kind:?} is passable"
            );
        }
    }

    #[test]
    fn a_box_spanning_several_cells_catches_any_solid_cell() {
        // Solid at the far +X/+Y/+Z corner of the swept range only.
        let world = |x: i32, y: i32, z: i32| {
            if x == 2 && y == 2 && z == 2 {
                BlockKind::Solid
            } else {
                BlockKind::Empty
            }
        };
        let min = Vec3::new(0.5, 0.5, 0.5);
        let max = Vec3::new(2.5, 2.5, 2.5);
        assert!(solid_overlaps(Aabb::new(min, max), &world));
    }

    #[test]
    fn non_finite_and_inverted_boxes_do_not_overlap() {
        let nan_min = Vec3::new(f32::NAN, 0.0, 0.0);
        assert!(!solid_overlaps(
            Aabb::new(nan_min, Vec3::new(1.0, 1.0, 1.0)),
            &solid_at_origin
        ));

        let inverted = Aabb::new(Vec3::new(2.0, 2.0, 2.0), Vec3::new(1.0, 1.0, 1.0));
        assert!(!solid_overlaps(inverted, &solid_at_origin));
    }

    #[test]
    fn from_feet_builds_the_requested_dimensions() {
        let aabb = Aabb::from_feet(Vec3::new(1.0, 2.0, 3.0), 0.3, 1.8);
        assert_eq!(aabb.min, Vec3::new(0.7, 2.0, 2.7));
        assert_eq!(aabb.max, Vec3::new(1.3, 3.8, 3.3));

        let explicit = Aabb::new(Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(explicit.min, Vec3::new(-1.0, -2.0, -3.0));
        assert_eq!(explicit.max, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn intersects_block_rejects_touching_faces_and_sub_epsilon_overlap() {
        let aabb = Aabb::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0));
        // Exactly adjacent on every axis: strict inequality plus epsilon says no.
        assert!(!aabb.intersects_block(1, 0, 0));
        assert!(!aabb.intersects_block(0, 1, 0));
        assert!(!aabb.intersects_block(0, 0, 1));
        assert!(!aabb.intersects_block(-1, 0, 0));

        // A 1e-5 overlap is inside the epsilon and must not count...
        let barely = Aabb::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0 + 1e-5, 1.0, 1.0));
        assert!(!barely.intersects_block(1, 0, 0));

        // ...while a real overlap well past the epsilon does.
        let overlap = Aabb::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.5, 1.0, 1.0));
        assert!(overlap.intersects_block(1, 0, 0));
    }

    #[test]
    fn ray_hits_a_box_ahead_of_the_origin() {
        let origin = Vec3::new(0.0, 0.5, -5.0);
        let dir = Vec3::Z;
        let min = Vec3::new(-1.0, 0.0, -1.0);
        let max = Vec3::new(1.0, 1.0, 1.0);
        let hit = ray_aabb(origin, dir, min, max).expect("ray passes through the slab");
        assert!((hit - 4.0).abs() < 1e-6, "entry distance was {hit}");
    }

    #[test]
    fn ray_from_every_axis_hits_the_box() {
        let min = Vec3::new(-1.0, -1.0, -1.0);
        let max = Vec3::new(1.0, 1.0, 1.0);
        for (origin, dir) in [
            (Vec3::new(-5.0, 0.0, 0.0), Vec3::X),
            (Vec3::new(5.0, 0.0, 0.0), -Vec3::X),
            (Vec3::new(0.0, 5.0, 0.0), -Vec3::Y),
            (Vec3::new(0.0, 0.0, 5.0), -Vec3::Z),
        ] {
            let hit = ray_aabb(origin, dir, min, max).expect("axis ray hits");
            assert!((hit - 4.0).abs() < 1e-6, "entry distance was {hit}");
        }
    }

    #[test]
    fn ray_misses_a_box_beside_or_behind_it() {
        let min = Vec3::new(-1.0, 0.0, -1.0);
        let max = Vec3::new(1.0, 1.0, 1.0);
        // Beside: parallel offset in x.
        assert!(ray_aabb(Vec3::new(5.0, 0.5, -5.0), Vec3::Z, min, max).is_none());
        // Above: the ray never descends into the box.
        assert!(ray_aabb(Vec3::new(0.0, 5.0, -5.0), Vec3::Z, min, max).is_none());
        // Behind: pointing away from the box.
        assert!(ray_aabb(Vec3::new(0.0, 0.5, -5.0), -Vec3::Z, min, max).is_none());
        // Past it: the box is behind a positive entry that never happens.
        assert!(ray_aabb(Vec3::new(0.0, 0.5, 5.0), Vec3::Z, min, max).is_none());
    }

    #[test]
    fn ray_starting_inside_the_box_has_no_positive_entry_distance() {
        // The contract rejects a negative entry distance, so an origin already
        // inside the slab is not a valid hit: only an entry ahead counts.
        let min = Vec3::new(-1.0, -1.0, -1.0);
        let max = Vec3::new(1.0, 1.0, 1.0);
        assert_eq!(ray_aabb(Vec3::ZERO, Vec3::Z, min, max), None);
        assert_eq!(ray_aabb(Vec3::new(-0.5, 0.5, 0.5), Vec3::Z, min, max), None);
    }

    #[test]
    fn degenerate_and_non_finite_rays_are_rejected() {
        let min = Vec3::new(-1.0, -1.0, -1.0);
        let max = Vec3::new(1.0, 1.0, 1.0);
        for dir in [
            Vec3::ZERO,
            Vec3::new(1e-7, 0.0, 0.0),
            Vec3::new(f32::NAN, 0.0, 1.0),
            Vec3::new(f32::INFINITY, 0.0, 0.0),
        ] {
            assert_eq!(
                ray_aabb(Vec3::new(0.0, 0.0, -5.0), dir, min, max),
                None,
                "dir {dir:?} must be rejected"
            );
        }
        assert_eq!(
            ray_aabb(Vec3::new(f32::NAN, 0.0, -5.0), Vec3::Z, min, max),
            None
        );
        assert_eq!(
            ray_aabb(
                Vec3::new(0.0, 0.0, -5.0),
                Vec3::Z,
                Vec3::new(f32::NAN, 0.0, 0.0),
                max
            ),
            None
        );
    }

    #[test]
    fn non_normalized_dir_scales_the_returned_distance() {
        let min = Vec3::new(-1.0, -1.0, -1.0);
        let max = Vec3::new(1.0, 1.0, 1.0);
        let origin = Vec3::new(0.0, 0.0, -5.0);
        let hit = ray_aabb(origin, Vec3::Z * 2.0, min, max).expect("hits");
        assert!((hit - 2.0).abs() < 1e-6, "scaled distance was {hit}");
    }
}
