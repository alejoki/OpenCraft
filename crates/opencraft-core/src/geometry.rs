//! Axis-aligned boxes and slab-method ray intersection.
//!
//! Neutral geometry only: `Aabb` stores explicit `min`/`max` corners and its
//! constructors take dimensions from the caller, so player-sized constants
//! stay in the game crate. The game's `crate::player::physics::Aabb` and
//! `crate::player::melee::ray_aabb` paths re-export these symbols unchanged.

use glam::Vec3;

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
