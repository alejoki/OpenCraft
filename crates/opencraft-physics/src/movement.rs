//! Neutral kinematic walk/fly solver.
//!
//! [`step_body`] is the whole walk/fly integration: input-to-direction
//! projection, dry and swim vertical motion, sub-stepped axis resolution
//! against solid cells, and dry-land fall measurement. It reads the world
//! through the neutral [`BlockQuery`] contract and never names a game block
//! identity, so the only coupling to a game is the [`MovementRules`] tuning
//! handed in by the game's config seam.

use glam::{Vec2, Vec3};

use opencraft_core::block::BlockQuery;
use opencraft_core::geometry::{solid_overlaps, Aabb};

/// Per-second exponential drag on vertical velocity while submerged.
/// With gravity this bounds an idle sink at roughly 4.7 m/s: deliberate, never
/// a teleport.
pub const SWIM_VERTICAL_DRAG: f32 = 6.0;
/// Upward acceleration while `jump_or_up` is held underwater. Net of the
/// 28.0 m/s^2 gravity below this lifts a submerged body; released, gravity plus
/// drag make it sink. There is no buoyant surface lock in either case.
pub const SWIM_RISE_ACCEL: f32 = 40.0;
/// Upward speed while `jump_or_up` is held underwater. Deliberately below the
/// 9.0 m/s land jump so swimming up reads as a steady rise, not a leap.
pub const SWIM_RISE_SPEED: f32 = 4.0;
/// Terminal sink speed underwater, so entering from a long fall never slams the
/// body into the seabed at the 45 m/s dry-land terminal velocity.
pub const SWIM_MAX_SINK_SPEED: f32 = 6.0;
/// Horizontal walk-speed multiplier while submerged: 4.6 * 0.55 = 2.53 m/s, so
/// swimming is always slower than the 4.6 m/s land walk (and the 7.2 sprint).
pub const SWIM_WALK_SPEED_MULT: f32 = 0.55;
/// Downward acceleration applied while submerged, in blocks per second
/// squared. The swim branch subtracts this so the idle sink keeps matching the
/// dry-land gravity feel while drag bounds the terminal speed.
pub const SWIM_SINK_ACCEL: f32 = 28.0;

/// Horizontal walking speed on land, in blocks per second.
pub const WALK_SPEED: f32 = 4.6;
/// Horizontal sprint speed in blocks per second. Crouching deliberately uses
/// the sprint speed too, matching the pre-config behavior.
pub const SPRINT_SPEED: f32 = 7.2;
/// Flight speed in blocks per second while not sprinting.
pub const FLY_SPEED: f32 = 18.0;
/// Flight speed in blocks per second while sprinting.
pub const FLY_SPRINT_SPEED: f32 = 48.0;
/// Fraction of flight speed applied to vertical movement.
pub const FLY_VERTICAL_FACTOR: f32 = 0.75;
/// Upward launch speed of a standing jump, in blocks per second.
pub const JUMP_VELOCITY: f32 = 9.0;
/// Downward gravity acceleration, in blocks per second squared.
pub const GRAVITY: f32 = 28.0;
/// Maximum downward fall speed, in blocks per second.
pub const TERMINAL_VELOCITY: f32 = 45.0;

/// Effective movement tuning for one body, in blocks and seconds.
///
/// The defaults reference the named constants above, so a fresh body is
/// behavior-identical to the pre-config rules. The game's config seam replaces
/// this wholesale before stepping; [`step_body`] never reads the bare
/// constants, so an applied rule set is the only thing that steers movement.
///
/// This value is trusted, validated configuration: the game rejects non-finite
/// config values, so [`step_body`] does not re-validate the rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementRules {
    pub walk_speed: f32,
    pub sprint_speed: f32,
    pub fly_speed: f32,
    pub fly_sprint_speed: f32,
    pub fly_vertical_factor: f32,
    pub jump_velocity: f32,
    pub gravity: f32,
    pub terminal_velocity: f32,
    pub swim_vertical_drag: f32,
    pub swim_rise_accel: f32,
    pub swim_rise_speed: f32,
    pub swim_max_sink_speed: f32,
    pub swim_walk_speed_mult: f32,
    pub swim_sink_accel: f32,
}

impl Default for MovementRules {
    fn default() -> Self {
        Self {
            walk_speed: WALK_SPEED,
            sprint_speed: SPRINT_SPEED,
            fly_speed: FLY_SPEED,
            fly_sprint_speed: FLY_SPRINT_SPEED,
            fly_vertical_factor: FLY_VERTICAL_FACTOR,
            jump_velocity: JUMP_VELOCITY,
            gravity: GRAVITY,
            terminal_velocity: TERMINAL_VELOCITY,
            swim_vertical_drag: SWIM_VERTICAL_DRAG,
            swim_rise_accel: SWIM_RISE_ACCEL,
            swim_rise_speed: SWIM_RISE_SPEED,
            swim_max_sink_speed: SWIM_MAX_SINK_SPEED,
            swim_walk_speed_mult: SWIM_WALK_SPEED_MULT,
            swim_sink_accel: SWIM_SINK_ACCEL,
        }
    }
}

/// Movement intent for one step.
///
/// The seven flags are the raw key state; [`Self::reset`] clears them all and
/// [`Self::is_moving`] answers whether any movement key is held.
#[derive(Clone, Debug, Default)]
pub struct MoveInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump_or_up: bool,
    pub crouch_or_down: bool,
    pub sprint: bool,
}

impl MoveInput {
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether any movement key is held this step.
    ///
    /// Hold-to-eat cancels on movement, so the check lives next to the flags
    /// themselves rather than being re-derived from them at the call site.
    pub fn is_moving(&self) -> bool {
        self.forward
            || self.backward
            || self.left
            || self.right
            || self.jump_or_up
            || self.crouch_or_down
            || self.sprint
    }
}

/// Body size in blocks: horizontal half-extent and full height.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BodyDims {
    pub half_width: f32,
    pub height: f32,
}

/// The state the solver integrates.
#[derive(Copy, Clone, Debug)]
pub struct Body {
    /// Feet position in world coordinates.
    pub position: Vec3,
    /// Velocity in blocks per second.
    pub velocity: Vec3,
    /// Whether the last resolution left the body standing on a solid cell.
    pub on_ground: bool,
}

/// Which integration branch to run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MoveMode {
    /// Gravity, jumping, liquid buoyancy and sub-stepped solid collision.
    Walk,
    /// Free flight: input speed directly, no gravity, no collision response.
    Fly,
}

/// Everything one [`step_body`] call needs from the caller besides the body.
pub struct StepInput<'a> {
    /// Frame duration in seconds; clamped to `0.0..=0.1`.
    pub dt: f32,
    /// Facing yaw in radians (its sine/cosine pair is the flat basis).
    pub yaw: f32,
    /// Held movement keys.
    pub input: &'a MoveInput,
    /// Which integration branch to run.
    pub mode: MoveMode,
    /// External horizontal impulse (knockback) added after the input velocity.
    ///
    /// Deliberately 2D: the solver never applies a vertical impulse, so a hit
    /// can never push the body into the floor or ceiling.
    pub impulse_xz: Vec2,
}

/// Dry-land fall measurement carried across steps.
///
/// The solver measures distance only; damage, hunger and air rules belong to
/// the caller. Water contact always cancels a pending fall and consumes the
/// armed spawn immunity, because touching water counts as the first contact.
#[derive(Clone, Debug, Default)]
pub struct FallTracker {
    /// Highest feet height seen since the body last left the ground.
    pub peak_y: Option<f32>,
    /// One-shot dry-land distance latched on landing, consumed by the caller.
    pub landing_distance: f32,
    /// Discards the first ground (or water) contact after a fresh spawn or
    /// respawn.
    pub spawn_immune: bool,
}

impl FallTracker {
    /// Drops all fall bookkeeping, including spawn immunity.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Drops the in-flight fall and any latched landing but keeps spawn
    /// immunity.
    pub fn clear_pending(&mut self) {
        self.peak_y = None;
        self.landing_distance = 0.0;
    }

    /// Takes the latched landing distance, zeroing the latch.
    ///
    /// One-shot: a second call before the next landing returns `0.0`.
    pub fn take_landing(&mut self) -> f32 {
        let distance = self.landing_distance;
        self.landing_distance = 0.0;
        distance
    }

    /// Discards any pending fall and arms fresh-spawn immunity.
    pub fn arm_spawn_immunity(&mut self) {
        self.clear_pending();
        self.spawn_immune = true;
    }
}

/// Integrate one step of walk or fly movement, updating `body` and `fall`.
///
/// The contract is all-or-nothing on corrupt input: when `step.dt`,
/// `step.yaw`, `body.position`, `body.velocity` or `step.impulse_xz` is
/// non-finite, or `dims` is non-finite or not strictly positive, the function
/// returns without mutating `body` or `fall`. `rules` is trusted, validated
/// configuration and is not re-checked.
///
/// Walk mode reassigns horizontal velocity from the input, adds the caller's
/// horizontal impulse, then integrates vertical motion (swim drag/rise/sink, or
/// jump plus gravity) and resolves the whole displacement in Y, X, Z order
/// across sub-steps no larger than `0.25` blocks. Fly mode assigns velocity
/// from the input speed directly and moves by `velocity * dt` with no gravity
/// and no collision response.
pub fn step_body(
    body: &mut Body,
    dims: BodyDims,
    rules: &MovementRules,
    fall: &mut FallTracker,
    step: StepInput<'_>,
    world: &impl BlockQuery,
) {
    if !step.dt.is_finite()
        || !step.yaw.is_finite()
        || !body.position.is_finite()
        || !body.velocity.is_finite()
        || !step.impulse_xz.is_finite()
        || !dims.half_width.is_finite()
        || !dims.height.is_finite()
        || dims.half_width <= 0.0
        || dims.height <= 0.0
    {
        return;
    }

    let dt = step.dt.clamp(0.0, 0.1);
    let (sy, cy) = step.yaw.sin_cos();
    let forward_flat = Vec3::new(cy, 0.0, sy).normalize_or_zero();
    let right_flat = Vec3::new(-sy, 0.0, cy).normalize_or_zero();

    let mut move_dir = Vec3::ZERO;
    if step.input.forward {
        move_dir += forward_flat;
    }
    if step.input.backward {
        move_dir -= forward_flat;
    }
    if step.input.right {
        move_dir += right_flat;
    }
    if step.input.left {
        move_dir -= right_flat;
    }
    if move_dir.length_squared() > 0.0 {
        move_dir = move_dir.normalize();
    }

    if step.mode == MoveMode::Fly {
        let speed = if step.input.sprint {
            rules.fly_sprint_speed
        } else {
            rules.fly_speed
        };
        let mut vel = move_dir * speed;
        if step.input.jump_or_up {
            vel.y += speed * rules.fly_vertical_factor;
        }
        if step.input.crouch_or_down {
            vel.y -= speed * rules.fly_vertical_factor;
        }
        body.velocity = vel;
        body.position += body.velocity * dt;
        body.on_ground = false;
        // Flight is never a fall: drop any in-flight measurement.
        fall.clear_pending();
        return;
    }

    let start_y = body.position.y;

    // Submersion is derived from the already-in-scope world query — no
    // signature change and no new call-site parameter. Sampling the feet block
    // catches a wading body, and the mid-body block keeps a body whose feet
    // have just cleared the surface in the water branch until it is actually
    // out of the water (a generated ocean fills every column below sea level).
    let body_x = body.position.x.floor() as i32;
    let body_z = body.position.z.floor() as i32;
    let feet_block_y = (body.position.y + 1e-4).floor() as i32;
    let mid_block_y = (body.position.y + dims.height * 0.5).floor() as i32;
    let submerged = world.block_kind(body_x, feet_block_y, body_z).is_liquid()
        || world.block_kind(body_x, mid_block_y, body_z).is_liquid();

    // Crouching deliberately uses the sprint speed too, matching the
    // pre-config behavior.
    let mut walk_speed = if step.input.sprint || step.input.crouch_or_down {
        rules.sprint_speed
    } else {
        rules.walk_speed
    };
    if submerged {
        walk_speed *= rules.swim_walk_speed_mult;
    }
    body.velocity.x = move_dir.x * walk_speed;
    body.velocity.z = move_dir.z * walk_speed;
    // The caller's horizontal impulse is integrated *after* the input
    // assignment above. Input reassigns `velocity.x/z` every frame, so this is
    // deliberately not a second owner of the velocity: the impulse itself
    // lives only in the caller's combat state, is never persisted, and has no
    // vertical component so the body is never pushed into the floor or ceiling.
    body.velocity.x += step.impulse_xz.x;
    body.velocity.z += step.impulse_xz.y;

    // Swim branch of walk mode. Honesty note: this asserts mechanics only
    // (positions, sink/rise, on_ground). Fall distance is measured by the
    // dry-land bookkeeping after the sub-step loop below; the damage, hunger,
    // and air rules live in the caller's survival runtime and are applied
    // outside physics. Water contact cancels a fall.
    if submerged {
        // No buoyant surface lock. A submerged walk-mode body is dragged
        // vertically and then pulled down by gravity while `jump_or_up` is
        // released; holding it applies the rise acceleration instead. Both
        // directions clamp to their terminal swim speeds, so an idle body
        // sinks away from the surface and a held Jump rises back up.
        let drag = (1.0 - rules.swim_vertical_drag * dt).clamp(0.0, 1.0);
        body.velocity.y *= drag;
        let vertical_accel = if step.input.jump_or_up {
            rules.swim_rise_accel
        } else {
            -rules.swim_sink_accel
        };
        body.velocity.y = (body.velocity.y + vertical_accel * dt)
            .clamp(-rules.swim_max_sink_speed, rules.swim_rise_speed);
        // A body in water is never standing: no on-ground land jump of 9.0.
        // The sub-step Y resolution below may still turn this true, but only
        // on a solid block (the seabed), never on the liquid surface.
        body.on_ground = false;
    } else {
        if body.on_ground && step.input.jump_or_up {
            body.velocity.y = rules.jump_velocity;
            body.on_ground = false;
        }

        body.velocity.y = (body.velocity.y - rules.gravity * dt).max(-rules.terminal_velocity);
    }

    // Sub-step physics dynamically so no single step exceeds 0.25 blocks
    let steps = ((body.velocity.length() * dt) / 0.25).ceil().max(4.0) as usize;
    let sub_dt = dt / steps as f32;
    for _ in 0..steps {
        // 1. Resolve Y
        let dy = body.velocity.y * sub_dt;
        if dy != 0.0 {
            let candidate_y = Vec3::new(body.position.x, body.position.y + dy, body.position.z);
            if solid_overlaps(
                Aabb::from_feet(candidate_y, dims.half_width, dims.height),
                world,
            ) {
                if dy < 0.0 {
                    body.position.y = (body.position.y + dy).ceil();
                    body.on_ground = true;
                }
                body.velocity.y = 0.0;
            } else {
                body.position.y += dy;
                body.on_ground = false;
            }
        }

        // 2. Resolve X
        let dx = body.velocity.x * sub_dt;
        if dx != 0.0 {
            let candidate_x = Vec3::new(body.position.x + dx, body.position.y, body.position.z);
            if !solid_overlaps(
                Aabb::from_feet(candidate_x, dims.half_width, dims.height),
                world,
            ) {
                body.position.x += dx;
            } else {
                body.velocity.x = 0.0;
            }
        }

        // 3. Resolve Z
        let dz = body.velocity.z * sub_dt;
        if dz != 0.0 {
            let candidate_z = Vec3::new(body.position.x, body.position.y, body.position.z + dz);
            if !solid_overlaps(
                Aabb::from_feet(candidate_z, dims.half_width, dims.height),
                world,
            ) {
                body.position.z += dz;
            } else {
                body.velocity.z = 0.0;
            }
        }
    }

    // Fall measurement (dry land only). Physics owns the distance; the
    // caller's survival runtime owns the damage. Re-sample submersion at the
    // new position because the body may have entered water during this step.
    let body_x_after = body.position.x.floor() as i32;
    let body_z_after = body.position.z.floor() as i32;
    let feet_block_y_after = (body.position.y + 1e-4).floor() as i32;
    let mid_block_y_after = (body.position.y + dims.height * 0.5).floor() as i32;
    let submerged_after = world
        .block_kind(body_x_after, feet_block_y_after, body_z_after)
        .is_liquid()
        || world
            .block_kind(body_x_after, mid_block_y_after, body_z_after)
            .is_liquid();

    if submerged || submerged_after {
        // Water contact counts as the first contact: cancel the fall and spend
        // the fresh-spawn immunity.
        fall.clear_pending();
        fall.spawn_immune = false;
    } else if body.on_ground {
        let peak = fall.peak_y.take();
        if fall.spawn_immune {
            // Fresh spawn/respawn: discard the first dry landing whole.
            fall.spawn_immune = false;
        } else if let Some(peak) = peak {
            fall.landing_distance = (peak - body.position.y).max(0.0);
        }
    } else {
        // A ledge walk-off keeps its takeoff height, and a jump still peaks at
        // the apex instead of only recording the final height.
        fall.peak_y = Some(
            fall.peak_y
                .unwrap_or(start_y)
                .max(start_y)
                .max(body.position.y),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencraft_core::block::BlockKind;

    const HALF_WIDTH: f32 = 0.3;
    const HEIGHT: f32 = 1.8;

    fn dims() -> BodyDims {
        BodyDims {
            half_width: HALF_WIDTH,
            height: HEIGHT,
        }
    }

    fn body_at(x: f32, y: f32, z: f32) -> Body {
        Body {
            position: Vec3::new(x, y, z),
            velocity: Vec3::ZERO,
            on_ground: false,
        }
    }

    fn empty_world(_x: i32, _y: i32, _z: i32) -> BlockKind {
        BlockKind::Empty
    }

    /// Bedrock floor at every cell below `y = 0`, air above.
    fn floor_world(_x: i32, y: i32, _z: i32) -> BlockKind {
        if y < 0 {
            BlockKind::Solid
        } else {
            BlockKind::Empty
        }
    }

    /// The whole world is liquid: no solid cell can ever be touched.
    fn liquid_world(_x: i32, _y: i32, _z: i32) -> BlockKind {
        BlockKind::Liquid
    }

    /// Bedrock floor, a liquid column up to `y = 2` (inclusive), air above.
    fn pool_world(_x: i32, y: i32, _z: i32) -> BlockKind {
        if y < 0 {
            BlockKind::Solid
        } else if y <= 2 {
            BlockKind::Liquid
        } else {
            BlockKind::Empty
        }
    }

    /// Bedrock floor plus a one-block solid wall at `x = 2`.
    fn wall_world(x: i32, y: i32, _z: i32) -> BlockKind {
        if x == 2 || y < 0 {
            BlockKind::Solid
        } else {
            BlockKind::Empty
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn step_walk(
        body: &mut Body,
        fall: &mut FallTracker,
        rules: &MovementRules,
        input: &MoveInput,
        yaw: f32,
        dt: f32,
        impulse_xz: Vec2,
        world: &impl BlockQuery,
    ) {
        step_body(
            body,
            dims(),
            rules,
            fall,
            StepInput {
                dt,
                yaw,
                input,
                mode: MoveMode::Walk,
                impulse_xz,
            },
            world,
        );
    }

    /// Jump once from standing and run until the body is back on the floor.
    ///
    /// Panics if the body never leaves the ground or never lands, so a broken
    /// jump or a tunnelled floor fails loudly instead of hanging.
    fn jump_and_land(
        body: &mut Body,
        fall: &mut FallTracker,
        rules: &MovementRules,
        world: &impl BlockQuery,
    ) {
        let input = MoveInput {
            jump_or_up: true,
            ..MoveInput::default()
        };
        let mut left_ground = false;
        for _ in 0..600 {
            step_walk(
                body,
                fall,
                rules,
                &input,
                0.0,
                1.0 / 60.0,
                Vec2::ZERO,
                world,
            );
            if !body.on_ground {
                left_ground = true;
            }
            if left_ground && body.on_ground {
                return;
            }
        }
        panic!("body never left the ground and landed again");
    }

    fn approx(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected} ± {tolerance}, got {actual}"
        );
    }

    fn body_bits(body: &Body) -> ([u32; 3], [u32; 3], bool) {
        (
            body.position.to_array().map(f32::to_bits),
            body.velocity.to_array().map(f32::to_bits),
            body.on_ground,
        )
    }

    #[test]
    fn move_input_tracks_every_movement_key() {
        let input = MoveInput::default();
        assert!(!input.is_moving(), "an idle input is not moving");
        let keys = [
            "forward",
            "backward",
            "left",
            "right",
            "jump_or_up",
            "crouch_or_down",
            "sprint",
        ];
        for (index, key) in keys.iter().enumerate() {
            let mut input = MoveInput::default();
            match index {
                0 => input.forward = true,
                1 => input.backward = true,
                2 => input.left = true,
                3 => input.right = true,
                4 => input.jump_or_up = true,
                5 => input.crouch_or_down = true,
                _ => input.sprint = true,
            }
            assert!(input.is_moving(), "{key} must count as moving");
            input.reset();
            assert!(!input.is_moving(), "reset clears every movement key");
        }
    }

    #[test]
    fn free_fall_reaches_terminal_velocity_without_overshoot() {
        let rules = MovementRules::default();
        let input = MoveInput::default();
        let mut body = body_at(0.5, 100.0, 0.5);
        let mut fall = FallTracker::default();
        let mut saw_terminal = false;

        for _ in 0..60 {
            step_walk(
                &mut body,
                &mut fall,
                &rules,
                &input,
                0.0,
                0.1,
                Vec2::ZERO,
                &empty_world,
            );
            assert!(
                body.velocity.y >= -rules.terminal_velocity,
                "fall speed {} overshot terminal velocity {}",
                body.velocity.y,
                rules.terminal_velocity
            );
            if body.velocity.y == -rules.terminal_velocity {
                saw_terminal = true;
            }
        }

        assert!(saw_terminal, "a long fall must reach terminal velocity");
        assert_eq!(body.velocity.y, -rules.terminal_velocity);
        assert!(body.position.y < 100.0, "the fall must actually descend");
    }

    #[test]
    fn grounded_jump_leaves_the_floor_and_lands_with_a_positive_latch() {
        let rules = MovementRules::default();
        let mut body = body_at(0.5, 0.0, 0.5);
        body.on_ground = true;
        let mut fall = FallTracker::default();

        jump_and_land(&mut body, &mut fall, &rules, &floor_world);

        assert_eq!(body.position.y, 0.0, "the body must land back on the floor");
        assert!(body.on_ground);
        assert_eq!(body.velocity.y, 0.0);
        assert!(
            fall.take_landing() > 0.0,
            "a jump landing must latch a positive distance"
        );
    }

    #[test]
    fn terminal_velocity_fall_lands_on_the_floor_without_embedding() {
        let rules = MovementRules::default();
        let input = MoveInput::default();
        let mut body = body_at(0.5, 5.0, 0.5);
        body.velocity.y = -rules.terminal_velocity;
        let mut fall = FallTracker::default();

        for _ in 0..60 {
            step_walk(
                &mut body,
                &mut fall,
                &rules,
                &input,
                0.0,
                0.1,
                Vec2::ZERO,
                &floor_world,
            );
            assert!(
                body.position.y >= 0.0,
                "feet {} embedded below the floor",
                body.position.y
            );
        }

        assert_eq!(body.position.y, 0.0);
        assert!(body.on_ground);
        assert_eq!(body.velocity.y, 0.0);
        let aabb = Aabb::from_feet(body.position, HALF_WIDTH, HEIGHT);
        assert!(
            !solid_overlaps(aabb, &floor_world),
            "a landed body must not overlap the floor cell"
        );
    }

    #[test]
    fn idle_submerged_body_sinks_and_clamps_at_max_sink_speed() {
        let rules = MovementRules::default();
        let input = MoveInput::default();
        let mut body = body_at(0.5, 5.0, 0.5);
        // A body that just dropped into the water carries a fall speed far
        // past the swim cap; the drag/clamp must bound it in one step.
        body.velocity.y = -20.0;
        let mut fall = FallTracker::default();

        step_walk(
            &mut body,
            &mut fall,
            &rules,
            &input,
            0.0,
            0.1,
            Vec2::ZERO,
            &liquid_world,
        );
        assert_eq!(body.velocity.y, -rules.swim_max_sink_speed);
        let first_y = body.position.y;
        assert!(first_y < 5.0, "an idle submerged body must sink");

        for _ in 0..30 {
            step_walk(
                &mut body,
                &mut fall,
                &rules,
                &input,
                0.0,
                0.1,
                Vec2::ZERO,
                &liquid_world,
            );
            assert!(
                body.velocity.y >= -rules.swim_max_sink_speed,
                "sink speed {} exceeded the swim cap",
                body.velocity.y
            );
            assert!(
                body.velocity.y <= 0.0,
                "idle sink must never rise, got {}",
                body.velocity.y
            );
        }

        assert!(body.position.y < first_y, "the body must keep sinking");
        assert!(!body.on_ground, "liquid is not ground");
        assert_eq!(fall.peak_y, None);
    }

    #[test]
    fn held_jump_rises_and_clamps_at_swim_rise_speed() {
        let rules = MovementRules::default();
        let input = MoveInput {
            jump_or_up: true,
            ..MoveInput::default()
        };
        let mut body = body_at(0.5, 5.0, 0.5);
        let mut fall = FallTracker::default();

        step_walk(
            &mut body,
            &mut fall,
            &rules,
            &input,
            0.0,
            0.1,
            Vec2::ZERO,
            &liquid_world,
        );
        assert_eq!(body.velocity.y, rules.swim_rise_speed);
        let first_y = body.position.y;
        assert!(first_y > 5.0, "a held jump underwater must rise");

        for _ in 0..10 {
            step_walk(
                &mut body,
                &mut fall,
                &rules,
                &input,
                0.0,
                0.1,
                Vec2::ZERO,
                &liquid_world,
            );
            assert_eq!(body.velocity.y, rules.swim_rise_speed);
        }

        assert!(body.position.y > first_y, "the body must keep rising");
        assert!(!body.on_ground);
    }

    #[test]
    fn liquid_surface_is_never_on_ground() {
        let rules = MovementRules::default();
        let input = MoveInput::default();
        let mut body = body_at(0.5, 2.0, 0.5);
        // Even a body that arrives "standing" must be cleared by the swim
        // branch: water can never act as a floor.
        body.on_ground = true;
        let mut fall = FallTracker::default();

        for _ in 0..3 {
            step_walk(
                &mut body,
                &mut fall,
                &rules,
                &input,
                0.0,
                0.1,
                Vec2::ZERO,
                &pool_world,
            );
            assert!(
                !body.on_ground,
                "feet at {} on a liquid surface counted as ground",
                body.position.y
            );
        }
        assert!(body.position.y > 0.0, "the body must stay in the pool");
    }

    #[test]
    fn fly_mode_moves_by_velocity_times_dt_and_clears_pending_falls() {
        let rules = MovementRules::default();
        let input = MoveInput {
            forward: true,
            ..MoveInput::default()
        };
        let mut body = body_at(0.5, 10.0, 0.5);
        let mut fall = FallTracker {
            peak_y: Some(12.0),
            landing_distance: 2.0,
            spawn_immune: true,
        };
        let dt = 1.0 / 60.0;

        step_body(
            &mut body,
            dims(),
            &rules,
            &mut fall,
            StepInput {
                dt,
                yaw: 0.0,
                input: &input,
                mode: MoveMode::Fly,
                impulse_xz: Vec2::ZERO,
            },
            &floor_world,
        );

        approx(body.position.x, 0.5 + rules.fly_speed * dt, 1e-6);
        assert_eq!(body.position.y, 10.0, "flight ignores gravity");
        assert_eq!(
            body.velocity,
            Vec3::new(rules.fly_speed, 0.0, 0.0),
            "flight velocity is move_dir * speed"
        );
        assert!(!body.on_ground);
        assert_eq!(fall.peak_y, None, "flight drops an in-flight fall");
        assert_eq!(fall.landing_distance, 0.0, "flight drops a latched landing");
        assert!(
            fall.spawn_immune,
            "flight keeps fresh-spawn immunity for the first real landing"
        );

        let sprint_up = MoveInput {
            forward: true,
            sprint: true,
            jump_or_up: true,
            ..MoveInput::default()
        };
        let before = body.position;
        step_body(
            &mut body,
            dims(),
            &rules,
            &mut fall,
            StepInput {
                dt,
                yaw: 0.0,
                input: &sprint_up,
                mode: MoveMode::Fly,
                impulse_xz: Vec2::new(50.0, 50.0),
            },
            &floor_world,
        );
        approx(
            body.position.y,
            before.y + rules.fly_sprint_speed * rules.fly_vertical_factor * dt,
            1e-6,
        );
        assert_eq!(
            body.velocity.y,
            rules.fly_sprint_speed * rules.fly_vertical_factor
        );
        assert!(
            !body.on_ground,
            "a liquid-free flight over a floor is never grounded"
        );
    }

    #[test]
    fn impulse_is_added_after_input_velocity_and_never_touches_y() {
        let rules = MovementRules::default();
        let input = MoveInput {
            forward: true,
            ..MoveInput::default()
        };
        let mut plain = body_at(0.5, 10.0, 0.5);
        plain.velocity = Vec3::new(0.0, 3.0, 0.0);
        let mut pushed = plain;
        let mut fall = FallTracker::default();

        step_walk(
            &mut plain,
            &mut fall,
            &rules,
            &input,
            0.0,
            0.1,
            Vec2::ZERO,
            &empty_world,
        );
        step_walk(
            &mut pushed,
            &mut fall,
            &rules,
            &input,
            0.0,
            0.1,
            Vec2::new(1.5, -2.0),
            &empty_world,
        );

        assert_eq!(plain.velocity.x, rules.walk_speed);
        approx(pushed.velocity.x, rules.walk_speed + 1.5, 1e-6);
        approx(pushed.velocity.z, -2.0, 1e-6);
        assert_eq!(
            pushed.velocity.y, plain.velocity.y,
            "an x/z impulse must not alter the vertical result"
        );
        assert!(plain.velocity.y < 3.0, "gravity must still apply");
    }

    #[test]
    fn high_speed_x_move_cannot_tunnel_through_a_one_block_wall() {
        let rules = MovementRules::default();
        let input = MoveInput {
            forward: true,
            ..MoveInput::default()
        };
        let mut body = body_at(0.5, 0.0, 0.5);
        body.on_ground = true;
        let mut fall = FallTracker::default();

        step_walk(
            &mut body,
            &mut fall,
            &rules,
            &input,
            0.0,
            0.1,
            Vec2::new(100.0, 0.0),
            &wall_world,
        );

        assert_eq!(body.velocity.x, 0.0, "the wall must cancel x velocity");
        assert!(
            body.position.x + HALF_WIDTH < 2.0,
            "body at {} tunneled into the wall at x = 2",
            body.position.x
        );
        assert!(body.position.x < 2.0);
    }

    #[test]
    fn degenerate_body_dims_are_a_no_op() {
        let rules = MovementRules::default();
        let input = MoveInput {
            forward: true,
            jump_or_up: true,
            ..MoveInput::default()
        };
        let bad_dims = [
            BodyDims {
                half_width: 0.0,
                height: HEIGHT,
            },
            BodyDims {
                half_width: -HALF_WIDTH,
                height: HEIGHT,
            },
            BodyDims {
                half_width: f32::NAN,
                height: HEIGHT,
            },
            BodyDims {
                half_width: HALF_WIDTH,
                height: 0.0,
            },
            BodyDims {
                half_width: HALF_WIDTH,
                height: -HEIGHT,
            },
            BodyDims {
                half_width: HALF_WIDTH,
                height: f32::INFINITY,
            },
        ];

        for bad in bad_dims {
            let mut body = body_at(1.5, 4.5, 0.5);
            body.velocity = Vec3::new(2.0, -3.0, 1.0);
            let mut fall = FallTracker {
                peak_y: Some(9.0),
                landing_distance: 1.0,
                spawn_immune: true,
            };
            let before = body_bits(&body);
            let fall_before = (
                fall.peak_y.map(f32::to_bits),
                fall.landing_distance.to_bits(),
                fall.spawn_immune,
            );

            step_body(
                &mut body,
                bad,
                &rules,
                &mut fall,
                StepInput {
                    dt: 1.0 / 60.0,
                    yaw: 0.0,
                    input: &input,
                    mode: MoveMode::Walk,
                    impulse_xz: Vec2::new(1.0, 1.0),
                },
                &floor_world,
            );

            assert_eq!(body_bits(&body), before, "dims {bad:?} mutated the body");
            assert_eq!(
                (
                    fall.peak_y.map(f32::to_bits),
                    fall.landing_distance.to_bits(),
                    fall.spawn_immune
                ),
                fall_before,
                "dims {bad:?} mutated the fall tracker"
            );
        }
    }

    #[test]
    fn non_finite_step_input_is_a_no_op() {
        let rules = MovementRules::default();
        let input = MoveInput {
            forward: true,
            jump_or_up: true,
            ..MoveInput::default()
        };
        let base = body_at(1.5, 4.5, 0.5);

        // (body position override, body velocity override, dt, yaw, impulse)
        let cases = [
            (None, None, f32::NAN, 0.0, Vec2::ZERO),
            (None, None, f32::INFINITY, 0.0, Vec2::ZERO),
            (None, None, 1.0 / 60.0, f32::NAN, Vec2::ZERO),
            (None, None, 1.0 / 60.0, f32::NEG_INFINITY, Vec2::ZERO),
            (
                Some(Vec3::new(f32::NAN, 4.5, 0.5)),
                None,
                1.0 / 60.0,
                0.0,
                Vec2::ZERO,
            ),
            (
                Some(Vec3::new(1.5, f32::INFINITY, 0.5)),
                None,
                1.0 / 60.0,
                0.0,
                Vec2::ZERO,
            ),
            (
                None,
                Some(Vec3::new(0.0, f32::NAN, 0.0)),
                1.0 / 60.0,
                0.0,
                Vec2::ZERO,
            ),
            (None, None, 1.0 / 60.0, 0.0, Vec2::new(f32::NAN, 0.0)),
            (None, None, 1.0 / 60.0, 0.0, Vec2::new(0.0, f32::INFINITY)),
        ];

        for (position, velocity, dt, yaw, impulse_xz) in cases {
            let mut body = base;
            if let Some(position) = position {
                body.position = position;
            }
            if let Some(velocity) = velocity {
                body.velocity = velocity;
            }
            let mut fall = FallTracker {
                peak_y: Some(9.0),
                landing_distance: 1.0,
                spawn_immune: true,
            };
            let before = body_bits(&body);
            let fall_before = (
                fall.peak_y.map(f32::to_bits),
                fall.landing_distance.to_bits(),
                fall.spawn_immune,
            );

            step_body(
                &mut body,
                dims(),
                &rules,
                &mut fall,
                StepInput {
                    dt,
                    yaw,
                    input: &input,
                    mode: MoveMode::Walk,
                    impulse_xz,
                },
                &floor_world,
            );

            assert_eq!(
                body_bits(&body),
                before,
                "non-finite input (dt {dt}, yaw {yaw}, impulse {impulse_xz:?}) mutated the body"
            );
            assert_eq!(
                (
                    fall.peak_y.map(f32::to_bits),
                    fall.landing_distance.to_bits(),
                    fall.spawn_immune,
                ),
                fall_before,
                "non-finite input (dt {dt}, yaw {yaw}, impulse {impulse_xz:?}) mutated the fall tracker"
            );
        }
    }

    #[test]
    fn fall_tracker_spawn_immunity_discards_only_the_first_landing() {
        let rules = MovementRules::default();
        let mut body = body_at(0.5, 0.0, 0.5);
        body.on_ground = true;
        let mut fall = FallTracker::default();
        fall.arm_spawn_immunity();
        assert!(fall.spawn_immune);
        assert_eq!(fall.peak_y, None);

        // The fresh-spawn landing is discarded whole.
        jump_and_land(&mut body, &mut fall, &rules, &floor_world);
        assert_eq!(fall.landing_distance, 0.0);
        assert!(!fall.spawn_immune);

        // The next landing latches a real distance.
        jump_and_land(&mut body, &mut fall, &rules, &floor_world);
        assert!(fall.landing_distance > 0.0, "the second landing must latch");

        // take_landing is one-shot.
        assert!(fall.take_landing() > 0.0);
        assert_eq!(fall.take_landing(), 0.0);
        assert_eq!(fall.landing_distance, 0.0);

        // clear_pending keeps immunity; clear drops it.
        let mut tracker = FallTracker {
            peak_y: Some(3.0),
            landing_distance: 2.0,
            spawn_immune: true,
        };
        tracker.clear_pending();
        assert_eq!(tracker.peak_y, None);
        assert_eq!(tracker.landing_distance, 0.0);
        assert!(
            tracker.spawn_immune,
            "clear_pending must keep fresh-spawn immunity"
        );

        tracker.peak_y = Some(3.0);
        tracker.landing_distance = 2.0;
        tracker.arm_spawn_immunity();
        assert_eq!(tracker.peak_y, None, "arming drops the pending fall");
        assert_eq!(tracker.landing_distance, 0.0);
        assert!(tracker.spawn_immune);

        tracker.clear();
        assert_eq!(tracker.peak_y, None);
        assert_eq!(tracker.landing_distance, 0.0);
        assert!(!tracker.spawn_immune, "clear must drop spawn immunity");
    }
}
