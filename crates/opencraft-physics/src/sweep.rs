//! Swept segment versus axis-aligned box.
//!
//! This is the shared projectile sweep over a segment parameter `t`: given the
//! whole displacement for the step being resolved, it reports when the segment
//! first enters the box. Callers test candidate boxes per substep and keep the
//! smallest `t`.

use glam::Vec3;

/// Squared length below which a displacement is structurally degenerate.
const DEGENERATE_SEGMENT_SQ: f32 = 1e-12;
/// Per-axis parallel test for the slab sweep.
const SEGMENT_PARALLEL_EPSILON: f32 = 1e-6;

/// Swept segment versus AABB, returning the hit parameter `t` in `0.0..=1.0`.
///
/// `delta` is the whole segment displacement for the step being resolved, so
/// `t = 0.0` is the start point and `t = 1.0` is the end point.
///
/// An origin already inside the box reports `Some(0.0)`: a projectile that
/// starts inside a cell hits it immediately. The origin-inside test runs before
/// the degenerate-displacement test, so a zero `delta` inside the box is still
/// a hit.
///
/// Returns `None` for non-finite inputs, a miss, a hit entirely outside the
/// segment (`t` not in `0.0..=1.0`), and a degenerate `delta` that starts
/// outside the box.
pub fn segment_aabb(origin: Vec3, delta: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    if !origin.is_finite() || !delta.is_finite() || !min.is_finite() || !max.is_finite() {
        return None;
    }
    if origin.cmpge(min).all() && origin.cmple(max).all() {
        return Some(0.0);
    }
    if delta.length_squared() < DEGENERATE_SEGMENT_SQ {
        return None;
    }

    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    for (o, d, lo, hi) in [
        (origin.x, delta.x, min.x, max.x),
        (origin.y, delta.y, min.y, max.y),
        (origin.z, delta.z, min.z, max.z),
    ] {
        if d.abs() < SEGMENT_PARALLEL_EPSILON {
            // Parallel to this slab: the segment must already be inside it.
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

    if !t_enter.is_finite() || !(0.0..=1.0).contains(&t_enter) {
        return None;
    }
    Some(t_enter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_box() -> (Vec3, Vec3) {
        (Vec3::splat(-0.5), Vec3::splat(0.5))
    }

    fn approx(a: f32, b: f32, tolerance: f32) {
        assert!(
            (a - b).abs() <= tolerance,
            "expected {b} ± {tolerance}, got {a}"
        );
    }

    #[test]
    fn segment_aabb_hits_head_on_from_each_axis() {
        let (min, max) = unit_box();
        approx(
            segment_aabb(
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                min,
                max,
            )
            .expect("x sweep hits"),
            0.25,
            1e-6,
        );
        approx(
            segment_aabb(
                Vec3::new(0.0, -1.0, 0.0),
                Vec3::new(0.0, 2.0, 0.0),
                min,
                max,
            )
            .expect("y sweep hits"),
            0.25,
            1e-6,
        );
        approx(
            segment_aabb(
                Vec3::new(0.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 2.0),
                min,
                max,
            )
            .expect("z sweep hits"),
            0.25,
            1e-6,
        );
    }

    #[test]
    fn segment_aabb_reports_the_midpoint_of_a_half_length_segment() {
        let (min, max) = unit_box();
        approx(
            segment_aabb(
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                min,
                max,
            )
            .expect("midpoint crossing"),
            0.5,
            1e-6,
        );
        // A segment that just reaches the near face reports t == 1.
        approx(
            segment_aabb(
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(0.5, 0.0, 0.0),
                min,
                max,
            )
            .expect("end point touches the face"),
            1.0,
            1e-6,
        );
    }

    #[test]
    fn segment_aabb_reports_zero_when_the_origin_is_inside() {
        let (min, max) = unit_box();
        assert_eq!(
            segment_aabb(Vec3::ZERO, Vec3::X, min, max),
            Some(0.0),
            "a projectile that starts inside a cell hits it immediately"
        );
        assert_eq!(
            segment_aabb(Vec3::splat(0.5), Vec3::ZERO, min, max),
            Some(0.0),
            "the boundary counts as inside even with a degenerate delta"
        );
    }

    #[test]
    fn segment_aabb_rejects_misses_behind_parallel_and_degenerate_cases() {
        let (min, max) = unit_box();

        // Beside the box: the segment never enters the x slab.
        assert_eq!(
            segment_aabb(Vec3::new(2.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), min, max),
            None
        );
        // Behind the box: the entry parameter is negative.
        assert_eq!(
            segment_aabb(Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 1.0), min, max),
            None
        );
        // Parallel to the x slab while outside it.
        assert_eq!(
            segment_aabb(Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), min, max),
            None
        );
        // Degenerate delta starting outside.
        assert_eq!(
            segment_aabb(Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO, min, max),
            None
        );
        // Short segment that stops before the near face.
        assert_eq!(
            segment_aabb(
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(0.4, 0.0, 0.0),
                min,
                max
            ),
            None
        );
        // Non-finite inputs anywhere.
        let nan = f32::NAN;
        assert_eq!(
            segment_aabb(Vec3::new(nan, 0.0, 0.0), Vec3::X, min, max),
            None
        );
        assert_eq!(
            segment_aabb(Vec3::ZERO, Vec3::new(nan, 0.0, 0.0), min, max),
            None
        );
        assert_eq!(
            segment_aabb(
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::X,
                Vec3::new(nan, -0.5, -0.5),
                max
            ),
            None
        );
    }
}
