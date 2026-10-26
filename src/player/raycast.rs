use glam::{IVec3, Vec3};

use crate::world::block::BlockId;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RaycastHit {
    pub block_pos: IVec3,
    pub adjacent_pos: IVec3,
    pub normal: IVec3,
    pub distance: f32,
}

pub fn raycast_voxels<F: Fn(i32, i32, i32) -> BlockId>(
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    get_block: F,
) -> Option<RaycastHit> {
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
        if get_block(pos.x, pos.y, pos.z).is_solid() {
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

    #[test]
    fn test_raycast_axis_aligned_zero_components() {
        let hit = raycast_voxels(
            glam::Vec3::new(2.5, 5.2, 2.5),
            glam::Vec3::new(0.0, -1.0, 0.0),
            8.0,
            |x, y, z| {
                if x == 2 && y == 3 && z == 2 {
                    BlockId::Stone
                } else {
                    BlockId::Air
                }
            },
        )
        .expect("should hit block below");
        assert_eq!(hit.block_pos, glam::IVec3::new(2, 3, 2));
        assert_eq!(hit.adjacent_pos, glam::IVec3::new(2, 4, 2));
        assert_eq!(hit.normal, glam::IVec3::new(0, 1, 0));
    }

    /// Water is passable, so the reach raycast must travel through it and stop
    /// on the first solid block underneath (the seabed), never treating the
    /// liquid as a target. This is the raycast site of the S03 water mobility
    /// matrix: water is passable-not-walkable here because `is_solid()` is the
    /// only hit predicate and `BlockId::Water` is not solid.
    #[test]
    fn raycast_skips_water_and_hits_the_stone_seabed() {
        // Stone seabed at y == 0, an 10-block water column above it (1..=10),
        // open air above the surface.
        let world = |_x: i32, y: i32, _z: i32| {
            if y == 0 {
                BlockId::Stone
            } else if (1..=10).contains(&y) {
                BlockId::Water
            } else {
                BlockId::Air
            }
        };

        // Straight down from above the surface: water cells are skipped and the
        // hit is the seabed, with the entry face at the water block above it.
        let hit = raycast_voxels(
            glam::Vec3::new(2.5, 20.2, 2.5),
            glam::Vec3::new(0.0, -1.0, 0.0),
            30.0,
            world,
        )
        .expect("the ray must reach the seabed through the water column");
        assert_eq!(hit.block_pos, glam::IVec3::new(2, 0, 2));
        assert_eq!(hit.adjacent_pos, glam::IVec3::new(2, 1, 2));
        assert_eq!(hit.normal, glam::IVec3::new(0, 1, 0));

        // A short ray that ends inside the water column finds nothing: water is
        // never a raycast target even when it is the only block in range.
        let short = raycast_voxels(
            glam::Vec3::new(2.5, 20.2, 2.5),
            glam::Vec3::new(0.0, -1.0, 0.0),
            8.0,
            world,
        );
        assert!(
            short.is_none(),
            "a ray ending in the water must not hit the liquid, got {short:?}"
        );
    }
}
