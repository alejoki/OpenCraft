use glam::Vec3;

use crate::world::block::BlockId;

pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

#[derive(Copy, Clone, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    #[inline]
    pub fn from_feet(feet: Vec3) -> Self {
        Self {
            min: Vec3::new(
                feet.x - PLAYER_HALF_WIDTH,
                feet.y,
                feet.z - PLAYER_HALF_WIDTH,
            ),
            max: Vec3::new(
                feet.x + PLAYER_HALF_WIDTH,
                feet.y + PLAYER_HEIGHT,
                feet.z + PLAYER_HALF_WIDTH,
            ),
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

#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump_or_up: bool,
    pub crouch_or_down: bool,
    pub sprint: bool,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub fly_mode: bool,
    pub selected_slot: usize,
}

fn collides_with_world<F: Fn(i32, i32, i32) -> BlockId>(aabb: Aabb, get_block: &F) -> bool {
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
                if get_block(x, y, z).is_solid() {
                    return true;
                }
            }
        }
    }
    false
}

impl Player {
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            velocity: Vec3::ZERO,
            yaw: -std::f32::consts::FRAC_PI_4,
            pitch: -0.25,
            on_ground: false,
            fly_mode: true, // Start in fly mode so large worlds can be explored immediately; toggle with F
            selected_slot: 0,
        }
    }

    #[inline]
    pub fn aabb(&self) -> Aabb {
        Aabb::from_feet(self.position)
    }

    #[inline]
    pub fn eye_position(&self) -> Vec3 {
        self.position + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0)
    }

    pub fn update<F: Fn(i32, i32, i32) -> BlockId>(
        &mut self,
        dt: f32,
        input: &InputState,
        get_block: F,
    ) {
        let dt = dt.clamp(0.0, 0.1);
        let (sy, cy) = self.yaw.sin_cos();
        let forward_flat = Vec3::new(cy, 0.0, sy).normalize_or_zero();
        let right_flat = Vec3::new(-sy, 0.0, cy).normalize_or_zero();

        let mut move_dir = Vec3::ZERO;
        if input.forward {
            move_dir += forward_flat;
        }
        if input.backward {
            move_dir -= forward_flat;
        }
        if input.right {
            move_dir += right_flat;
        }
        if input.left {
            move_dir -= right_flat;
        }
        if move_dir.length_squared() > 0.0 {
            move_dir = move_dir.normalize();
        }

        if self.fly_mode {
            let speed = if input.sprint { 48.0 } else { 18.0 };
            let mut vel = move_dir * speed;
            if input.jump_or_up {
                vel.y += speed * 0.75;
            }
            if input.crouch_or_down {
                vel.y -= speed * 0.75;
            }
            self.velocity = vel;
            self.position += self.velocity * dt;
            self.on_ground = false;
            return;
        }

        let walk_speed = if input.sprint { 7.2 } else { 4.6 };
        self.velocity.x = move_dir.x * walk_speed;
        self.velocity.z = move_dir.z * walk_speed;

        if self.on_ground && input.jump_or_up {
            self.velocity.y = 9.0;
            self.on_ground = false;
        }

        self.velocity.y = (self.velocity.y - 28.0 * dt).max(-45.0);

        // Sub-step physics if moving fast
        let steps = 4;
        let sub_dt = dt / steps as f32;
        for _ in 0..steps {
            // 1. Resolve Y
            let dy = self.velocity.y * sub_dt;
            if dy != 0.0 {
                let candidate_y = Vec3::new(self.position.x, self.position.y + dy, self.position.z);
                if collides_with_world(Aabb::from_feet(candidate_y), &get_block) {
                    if dy < 0.0 {
                        self.position.y = (self.position.y + dy).ceil();
                        self.on_ground = true;
                    }
                    self.velocity.y = 0.0;
                } else {
                    self.position.y += dy;
                    self.on_ground = false;
                }
            }

            // 2. Resolve X
            let dx = self.velocity.x * sub_dt;
            if dx != 0.0 {
                let candidate_x = Vec3::new(self.position.x + dx, self.position.y, self.position.z);
                if !collides_with_world(Aabb::from_feet(candidate_x), &get_block) {
                    self.position.x += dx;
                } else {
                    self.velocity.x = 0.0;
                }
            }

            // 3. Resolve Z
            let dz = self.velocity.z * sub_dt;
            if dz != 0.0 {
                let candidate_z = Vec3::new(self.position.x, self.position.y, self.position.z + dz);
                if !collides_with_world(Aabb::from_feet(candidate_z), &get_block) {
                    self.position.z += dz;
                } else {
                    self.velocity.z = 0.0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cannot_place_block_overlapping_player_aabb() {
        let player = Player::new(glam::Vec3::new(0.5, 10.0, 0.5));
        let aabb = player.aabb();
        assert!(aabb.intersects_block(0, 10, 0));
        assert!(aabb.intersects_block(0, 11, 0));
        assert!(!aabb.intersects_block(0, 9, 0));
        assert!(!aabb.intersects_block(2, 10, 0));
    }

    #[test]
    fn test_player_lands_on_ground_without_falling_through() {
        let mut player = Player::new(Vec3::new(0.5, 5.2, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        for _ in 0..30 {
            player.update(1.0 / 60.0, &input, |_, y, _| {
                if y < 5 {
                    BlockId::Stone
                } else {
                    BlockId::Air
                }
            });
        }
        assert!(player.on_ground);
        assert!((player.position.y - 5.0).abs() < 1e-3);
    }
}
