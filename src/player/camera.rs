use glam::{Mat4, Vec3, Vec4};

#[derive(Clone, Debug)]
pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y_radians: f32,
    pub z_near: f32,
    pub z_far: f32,
}

impl Camera {
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            yaw: -std::f32::consts::FRAC_PI_4,
            pitch: -0.25,
            fov_y_radians: 75.0_f32.to_radians(),
            z_near: 0.1,
            z_far: 1200.0,
        }
    }

    #[inline]
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cy * cp, sp, sy * cp).normalize_or_zero()
    }

    #[inline]
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_to_rh(self.position, self.forward(), Vec3::Y);
        let proj = Mat4::perspective_rh(self.fov_y_radians, aspect.max(0.1), self.z_near, self.z_far);
        proj * view
    }
}

#[derive(Clone, Debug)]
pub struct Frustum {
    pub planes: [Vec4; 6],
}

#[inline]
fn normalize_plane(p: Vec4) -> Vec4 {
    let len = p.truncate().length();
    if len > 1e-8 {
        p / len
    } else {
        p
    }
}

impl Frustum {
    pub fn from_view_proj(vp: &Mat4) -> Self {
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);

        Self {
            planes: [
                normalize_plane(r3 + r0), // Left
                normalize_plane(r3 - r0), // Right
                normalize_plane(r3 + r1), // Bottom
                normalize_plane(r3 - r1), // Top
                normalize_plane(r2),      // Near (wgpu [0, 1] depth)
                normalize_plane(r3 - r2), // Far
            ],
        }
    }

    #[inline]
    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        for p in &self.planes {
            let pv = Vec3::new(
                if p.x >= 0.0 { max.x } else { min.x },
                if p.y >= 0.0 { max.y } else { min.y },
                if p.z >= 0.0 { max.z } else { min.z },
            );
            if p.truncate().dot(pv) + p.w < 0.0 {
                return false;
            }
        }
        true
    }
}

#[inline]
pub fn visible_face_mask(camera_pos: Vec3, chunk_min: Vec3, chunk_max: Vec3) -> u8 {
    let mut mask = 0u8;
    if camera_pos.x > chunk_min.x {
        mask |= 1 << 0; // +X
    }
    if camera_pos.x < chunk_max.x {
        mask |= 1 << 1; // -X
    }
    if camera_pos.y > chunk_min.y {
        mask |= 1 << 2; // +Y
    }
    if camera_pos.y < chunk_max.y {
        mask |= 1 << 3; // -Y
    }
    if camera_pos.z > chunk_min.z {
        mask |= 1 << 4; // +Z
    }
    if camera_pos.z < chunk_max.z {
        mask |= 1 << 5; // -Z
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frustum_and_visible_face_mask() {
        let mut cam = Camera::new(Vec3::new(0.0, 20.0, 0.0));
        cam.yaw = 0.0;
        let vp = cam.view_proj(16.0 / 9.0);
        let frustum = Frustum::from_view_proj(&vp);
        let fwd = cam.forward();
        let in_front = cam.position + fwd * 30.0;
        let behind = cam.position - fwd * 60.0;
        assert!(frustum.intersects_aabb(in_front - Vec3::splat(16.0), in_front + Vec3::splat(16.0)));
        assert!(!frustum.intersects_aabb(behind - Vec3::splat(4.0), behind + Vec3::splat(4.0)));

        let mask = visible_face_mask(
            Vec3::new(-10.0, 16.0, 16.0),
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(32.0, 32.0, 32.0),
        );
        assert_eq!(mask & (1 << 0), 0); // +X face culled
        assert_ne!(mask & (1 << 1), 0); // -X face visible
    }
}
