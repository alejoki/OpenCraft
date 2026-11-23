//! Voxel DDA raycast against the neutral [`BlockQuery`] contract.
//!
//! The traversal walks cell boundaries in `t` order, stepping the axis with the
//! smallest pending boundary crossing and recording the entry face as the
//! outward normal. A call stops on the first *targetable* cell
//! ([`BlockKind::is_targetable`]): solid blocks and plants are hittable, while
//! empty and liquid cells are passable, so a ray travels through a liquid
//! column and stops on the first solid cell.

use glam::{IVec3, Vec3};

use opencraft_core::block::BlockQuery;

/// Where a reach ray first meets a targetable cell.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RaycastHit {
    /// The targetable cell that was hit.
    pub block_pos: IVec3,
    /// The passable cell the ray entered from (the hit cell minus `normal`).
    pub adjacent_pos: IVec3,
    /// Outward face normal of the entered cell.
    pub normal: IVec3,
    /// Distance travelled along the normalized ray, in blocks.
    pub distance: f32,
}

/// Step a normalized ray through the voxel grid and return the first
/// targetable (solid or plant) cell within `max_dist`.
///
/// The direction is normalized internally, so `dir` may have any non-zero
/// length; a direction shorter than the degeneracy threshold (`1e-8`) yields
/// `None`. Non-finite `origin`, `dir` or `max_dist` yields `None` before any
/// arithmetic, so corrupt input fails closed.
///
/// At most 128 cells are visited, which bounds the work on a ray through open
/// space.
pub fn raycast_voxels(
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    world: &impl BlockQuery,
) -> Option<RaycastHit> {
    if !origin.is_finite() || !dir.is_finite() || !max_dist.is_finite() {
        return None;
    }

    let d = dir.normalize_or_zero();
    if d.length_squared() < 1e-8 {
        return None;
    }

    let mut pos = IVec3::new(
        origin.x.floor() as i32,
        origin.y.floor() as i32,
        origin.z.floor() as i32,
    );

    let step = IVec3::new(
        if d.x >= 0.0 { 1 } else { -1 },
        if d.y >= 0.0 { 1 } else { -1 },
        if d.z >= 0.0 { 1 } else { -1 },
    );

    let t_delta = Vec3::new(
        if d.x.abs() < 1e-8 {
            f32::INFINITY
        } else {
            (1.0 / d.x).abs()
        },
        if d.y.abs() < 1e-8 {
            f32::INFINITY
        } else {
            (1.0 / d.y).abs()
        },
        if d.z.abs() < 1e-8 {
            f32::INFINITY
        } else {
            (1.0 / d.z).abs()
        },
    );

    let mut t_max = Vec3::new(
        if d.x.abs() < 1e-8 {
            f32::INFINITY
        } else if d.x > 0.0 {
            ((pos.x + 1) as f32 - origin.x) * t_delta.x
        } else {
            (origin.x - pos.x as f32) * t_delta.x
        },
        if d.y.abs() < 1e-8 {
            f32::INFINITY
        } else if d.y > 0.0 {
            ((pos.y + 1) as f32 - origin.y) * t_delta.y
        } else {
            (origin.y - pos.y as f32) * t_delta.y
        },
        if d.z.abs() < 1e-8 {
            f32::INFINITY
        } else if d.z > 0.0 {
            ((pos.z + 1) as f32 - origin.z) * t_delta.z
        } else {
            (origin.z - pos.z as f32) * t_delta.z
        },
    );

    let mut normal = IVec3::ZERO;
    let mut dist = 0.0f32;

    for _ in 0..128 {
        if dist > max_dist {
            break;
        }
        if world.block_kind(pos.x, pos.y, pos.z).is_targetable() {
            return Some(RaycastHit {
                block_pos: pos,
                adjacent_pos: pos + normal,
                normal,
                distance: dist,
            });
        }

        if t_max.x < t_max.y && t_max.x < t_max.z {
            dist = t_max.x;
            t_max.x += t_delta.x;
            pos.x += step.x;
            normal = IVec3::new(-step.x, 0, 0);
        } else if t_max.y < t_max.z {
            dist = t_max.y;
            t_max.y += t_delta.y;
            pos.y += step.y;
            normal = IVec3::new(0, -step.y, 0);
        } else {
            dist = t_max.z;
            t_max.z += t_delta.z;
            pos.z += step.z;
            normal = IVec3::new(0, 0, -step.z);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencraft_core::block::BlockKind;

    #[test]
    fn test_raycast_axis_aligned_zero_components() {
        let hit = raycast_voxels(
            Vec3::new(2.5, 5.2, 2.5),
            Vec3::new(0.0, -1.0, 0.0),
            8.0,
            &|x, y, z| {
                if x == 2 && y == 3 && z == 2 {
                    BlockKind::Solid
                } else {
                    BlockKind::Empty
                }
            },
        )
        .expect("should hit block below");
        assert_eq!(hit.block_pos, IVec3::new(2, 3, 2));
        assert_eq!(hit.adjacent_pos, IVec3::new(2, 4, 2));
        assert_eq!(hit.normal, IVec3::new(0, 1, 0));
    }

    /// Liquid is passable, so the reach raycast must travel through it and stop
    /// on the first solid cell underneath (the floor), never treating the
    /// liquid as a target: `is_targetable()` excludes liquids.
    #[test]
    fn raycast_skips_liquid_and_hits_the_solid_floor() {
        // Solid floor at y == 0, a 10-cell liquid column above it (1..=10),
        // open air above the surface.
        let world = |_x: i32, y: i32, _z: i32| {
            if y == 0 {
                BlockKind::Solid
            } else if (1..=10).contains(&y) {
                BlockKind::Liquid
            } else {
                BlockKind::Empty
            }
        };

        // Straight down from above the surface: liquid cells are skipped and
        // the hit is the floor, with the entry face at the liquid cell above.
        let hit = raycast_voxels(
            Vec3::new(2.5, 20.2, 2.5),
            Vec3::new(0.0, -1.0, 0.0),
            30.0,
            &world,
        )
        .expect("the ray must reach the floor through the liquid column");
        assert_eq!(hit.block_pos, IVec3::new(2, 0, 2));
        assert_eq!(hit.adjacent_pos, IVec3::new(2, 1, 2));
        assert_eq!(hit.normal, IVec3::new(0, 1, 0));

        // A short ray that ends inside the liquid column finds nothing: liquid
        // is never a raycast target even when it is the only cell in range.
        let short = raycast_voxels(
            Vec3::new(2.5, 20.2, 2.5),
            Vec3::new(0.0, -1.0, 0.0),
            8.0,
            &world,
        );
        assert!(
            short.is_none(),
            "a ray ending in the liquid must not hit it, got {short:?}"
        );
    }

    /// A plant is passable for movement yet still selectable by a reach ray:
    /// `is_targetable()` — not `is_solid()` — is the hit predicate.
    #[test]
    fn raycast_hits_a_plant_but_not_a_liquid() {
        let plant = |_x: i32, y: i32, _z: i32| {
            if y == 4 {
                BlockKind::Plant
            } else {
                BlockKind::Empty
            }
        };
        let hit = raycast_voxels(
            Vec3::new(2.5, 8.5, 2.5),
            Vec3::new(0.0, -1.0, 0.0),
            10.0,
            &plant,
        )
        .expect("a plant cell must be hittable");
        assert_eq!(hit.block_pos, IVec3::new(2, 4, 2));
        assert_eq!(hit.adjacent_pos, IVec3::new(2, 5, 2));

        // The same geometry classified as liquid is transparent to the ray.
        let liquid = |_x: i32, y: i32, _z: i32| {
            if y == 4 {
                BlockKind::Liquid
            } else {
                BlockKind::Empty
            }
        };
        assert_eq!(
            raycast_voxels(
                Vec3::new(2.5, 8.5, 2.5),
                Vec3::new(0.0, -1.0, 0.0),
                10.0,
                &liquid,
            ),
            None,
            "a liquid cell must never be a raycast target"
        );
    }

    #[test]
    fn non_finite_raycast_inputs_return_none() {
        let world = |_x: i32, _y: i32, _z: i32| BlockKind::Solid;
        let nan = f32::NAN;

        assert_eq!(
            raycast_voxels(Vec3::new(nan, 0.0, 0.0), Vec3::X, 8.0, &world),
            None,
            "a NaN origin has no cell"
        );
        assert_eq!(
            raycast_voxels(Vec3::ZERO, Vec3::new(0.0, nan, 0.0), 8.0, &world),
            None,
            "a NaN direction has no traversal"
        );
        assert_eq!(
            raycast_voxels(Vec3::ZERO, Vec3::X, f32::INFINITY, &world),
            None,
            "an infinite reach is not a finite segment"
        );
        assert_eq!(
            raycast_voxels(Vec3::ZERO, Vec3::new(f32::NAN, 1.0, 0.0), nan, &world),
            None,
            "every non-finite field fails closed before normalization"
        );
    }
}
