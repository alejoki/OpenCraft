//! Pure projectile domain: arrow ballistics, lifetime, embed-then-despawn, and
//! the swept segment/AABB primitive.
//!
//! This module is pure lib code — no player-layer import, no session, no
//! renderer, no wgpu, no wall clock — so the whole flight model is assertable
//! headlessly. It only knows [`glam::Vec3`] and integers.
//!
//! ## Ballistics
//!
//! A player arrow is fired at `18 + 14 * charge` blocks per second and deals
//! `1 + round(3 * charge)` damage, so a full draw is 32 b/s and 4 damage
//! (exactly a wood sword) and the 0.2 s minimum draw is 20.8 b/s and 2 damage.
//! A skeleton arrow is the fixed 28 b/s / 2 damage pair from the 09 context.
//!
//! Gravity is a constant 20 b/s² applied to `velocity.y` **before** the
//! position update (semi-implicit Euler). Callers substep the flight with
//! [`Arrow::substeps`] so a single frame never moves an arrow more than
//! [`ARROW_SUBSTEP_MAX`] (0.25 blocks): a 32 b/s shot over a 0.1 s frame is 13
//! substeps, well inside [`ARROW_MAX_SUBSTEPS`]. Because of the velocity-first
//! ordering, `n` substeps of `h` land at `v0*t - 0.5*g*t*(t + h)` rather than
//! the continuous `v0*t - 0.5*g*t²`; the difference is the O(g·h·t) integration
//! bias, bounded by the substep size, and the tests pin the discrete form.
//!
//! ## Embed, despawn, and persistence
//!
//! A flying arrow lives [`ARROW_LIFETIME_SECS`] (20 s). Hitting a block calls
//! [`Arrow::embed`], which freezes motion and replaces the lifetime with
//! [`ARROW_EMBED_SECS`] (2 s); the arrow then drains that and
//! [`Arrow::finished`] reports it. A non-finite arrow is `finished` immediately
//! so a corrupt projectile can never be rendered or persisted.
//!
//! **Arrows are never persisted.** The v7 save tail is mob-only and in-flight
//! projectiles are ephemeral exactly like `Mob.knockback`; the session owns the
//! `Vec<Arrow>` for the lifetime of the process.

use glam::Vec3;

/// Maximum number of arrows resident in a session at once.
pub const ARROW_CAP: usize = 64;
/// Seconds a flying arrow lives before it despawns in mid-air.
pub const ARROW_LIFETIME_SECS: f32 = 20.0;
/// Seconds an embedded arrow stays stuck in a block before it despawns.
pub const ARROW_EMBED_SECS: f32 = 2.0;
/// Downward acceleration in blocks per second squared.
pub const ARROW_GRAVITY: f32 = 20.0;
/// Speed of a minimum-draw player arrow, in blocks per second.
pub const ARROW_SPEED_MIN: f32 = 18.0;
/// Speed of a full-draw player arrow, in blocks per second.
pub const ARROW_SPEED_MAX: f32 = 32.0;
/// Damage of a minimum-draw player arrow.
pub const ARROW_DAMAGE_MIN: u16 = 1;
/// Damage of a full-draw player arrow (matches a wood sword).
pub const ARROW_DAMAGE_MAX: u16 = 4;
/// Fixed speed of a skeleton arrow, in blocks per second.
pub const ARROW_SPEED_SKELETON: f32 = 28.0;
/// Fixed damage of a skeleton arrow (pinned by the 09 context).
pub const ARROW_DAMAGE_SKELETON: u16 = 2;
/// Largest per-substep displacement, in blocks.
pub const ARROW_SUBSTEP_MAX: f32 = 0.25;
/// Hard ceiling on substeps per tick: `ceil(32 * 0.1 / 0.25) = 13` plus headroom.
pub const ARROW_MAX_SUBSTEPS: u32 = 16;
/// Half extent of an arrow's render/collision AABB, in blocks.
pub const ARROW_HALF_EXTENT: f32 = 0.05;

/// Tunable arrow tuning consumed by the `_with_rules` entry points.
///
/// [`ArrowRules::default`] is built from the constants above, so the default
/// bundle reproduces every existing wrapper exactly. The struct is a plain
/// value type: it carries no TOML/config vocabulary, only the numbers the
/// simulation consumes. Spawn-time fields (speed/damage/lifetime) are stored
/// on the instance; gravity and embed time are read per tick so an in-flight
/// arrow responds to live tuning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrowRules {
    /// Downward acceleration in blocks per second squared.
    pub gravity: f32,
    /// Speed of a minimum-draw player arrow, in blocks per second.
    pub speed_min: f32,
    /// Speed of a full-draw player arrow, in blocks per second.
    pub speed_max: f32,
    /// Damage of a minimum-draw player arrow.
    pub damage_min: u16,
    /// Damage of a full-draw player arrow.
    pub damage_max: u16,
    /// Fixed speed of a skeleton arrow, in blocks per second.
    pub skeleton_speed: f32,
    /// Fixed damage of a skeleton arrow.
    pub skeleton_damage: u16,
    /// Seconds a flying arrow lives before it despawns in mid-air.
    pub lifetime_secs: f32,
    /// Seconds an embedded arrow stays stuck before it despawns.
    pub embed_secs: f32,
}

impl Default for ArrowRules {
    fn default() -> Self {
        Self {
            gravity: ARROW_GRAVITY,
            speed_min: ARROW_SPEED_MIN,
            speed_max: ARROW_SPEED_MAX,
            damage_min: ARROW_DAMAGE_MIN,
            damage_max: ARROW_DAMAGE_MAX,
            skeleton_speed: ARROW_SPEED_SKELETON,
            skeleton_damage: ARROW_DAMAGE_SKELETON,
            lifetime_secs: ARROW_LIFETIME_SECS,
            embed_secs: ARROW_EMBED_SECS,
        }
    }
}

impl ArrowRules {
    /// Lifetime actually written onto a spawned arrow.
    ///
    /// A non-finite value falls back to the pinned default; a negative value
    /// is inert and expires immediately rather than failing to drain.
    fn effective_lifetime_secs(self) -> f32 {
        if self.lifetime_secs.is_finite() {
            self.lifetime_secs.max(0.0)
        } else {
            ARROW_LIFETIME_SECS
        }
    }

    /// Embed lifetime actually armed by [`Arrow::embed_with_rules`].
    fn effective_embed_secs(self) -> f32 {
        if self.embed_secs.is_finite() {
            self.embed_secs.max(0.0)
        } else {
            ARROW_EMBED_SECS
        }
    }

    /// Gravity actually integrated.
    ///
    /// Non-finite and negative values are inert (no fall) so a corrupt rule
    /// can never inject NaN into the velocity.
    fn effective_gravity(self) -> f32 {
        if self.gravity.is_finite() {
            self.gravity.max(0.0)
        } else {
            0.0
        }
    }
}

/// Squared length below which a direction is structurally degenerate.
const DIRECTION_EPSILON_SQ: f32 = 1e-12;
/// Per-axis parallel test for the slab sweep.
const SEGMENT_PARALLEL_EPSILON: f32 = 1e-6;

/// Who fired an arrow. Ownership decides the damage path, not the physics.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArrowOwner {
    /// Fired by the player: mob hits are player-attributed (drops, XP).
    Player,
    /// Fired by a skeleton: hits the player through the combat authority.
    Skeleton,
}

/// Whether an arrow is still flying or has stuck into a block.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArrowState {
    Flying,
    Embedded,
}

/// One in-flight (or embedded) projectile.
///
/// Fields are public because the session reads and writes position/velocity
/// while resolving collisions; every constructor still validates its inputs so
/// a corrupt arrow can only appear through [`Arrow::advance`] going non-finite,
/// which [`Arrow::finished`] then fails closed on.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Arrow {
    pub owner: ArrowOwner,
    pub position: Vec3,
    pub velocity: Vec3,
    pub damage: u16,
    pub life: f32,
    pub state: ArrowState,
}

impl Arrow {
    /// Builds a player arrow using default rules.
    ///
    /// Returns `None` for a non-finite origin/direction/charge, a charge
    /// outside `0.0..=1.0`, or a zero/degenerate direction. `dir` need not be
    /// normalized.
    pub fn player(origin: Vec3, dir: Vec3, charge: f32) -> Option<Self> {
        Self::player_with_rules(origin, dir, charge, &ArrowRules::default())
    }

    /// Rules-aware [`Arrow::player`].
    ///
    /// The spawn speed, damage, and lifetime are written onto the instance
    /// from `rules`; a later rule change cannot retune an arrow already in
    /// flight (`gravity`/`embed` are the per-tick exceptions).
    pub fn player_with_rules(
        origin: Vec3,
        dir: Vec3,
        charge: f32,
        rules: &ArrowRules,
    ) -> Option<Self> {
        if !charge.is_finite() || !(0.0..=1.0).contains(&charge) {
            return None;
        }
        Self::shot(
            ArrowOwner::Player,
            origin,
            dir,
            Self::speed_for_charge_with_rules(charge, rules),
            Self::damage_for_charge_with_rules(charge, rules),
            rules.effective_lifetime_secs(),
        )
    }

    /// Builds a skeleton arrow using default rules (the fixed 28 b/s / 2 pair).
    ///
    /// Validation is identical to [`Arrow::player`].
    pub fn skeleton(origin: Vec3, dir: Vec3) -> Option<Self> {
        Self::skeleton_with_rules(origin, dir, &ArrowRules::default())
    }

    /// Rules-aware [`Arrow::skeleton`].
    pub fn skeleton_with_rules(origin: Vec3, dir: Vec3, rules: &ArrowRules) -> Option<Self> {
        Self::shot(
            ArrowOwner::Skeleton,
            origin,
            dir,
            rules.skeleton_speed,
            rules.skeleton_damage,
            rules.effective_lifetime_secs(),
        )
    }

    fn shot(
        owner: ArrowOwner,
        origin: Vec3,
        dir: Vec3,
        speed: f32,
        damage: u16,
        lifetime_secs: f32,
    ) -> Option<Self> {
        if !origin.is_finite() || !dir.is_finite() || !speed.is_finite() || speed <= 0.0 {
            return None;
        }
        if dir.length_squared() < DIRECTION_EPSILON_SQ {
            return None;
        }
        let dir = dir.try_normalize()?;
        Some(Self {
            owner,
            position: origin,
            velocity: dir * speed,
            damage,
            life: lifetime_secs,
            state: ArrowState::Flying,
        })
    }

    /// Launch speed for a draw fraction using default rules, clamped to
    /// `0.0..=1.0`.
    ///
    /// Monotonic in `charge`: 0.0 → 18 b/s, 1.0 → 32 b/s.
    pub fn speed_for_charge(charge: f32) -> f32 {
        Self::speed_for_charge_with_rules(charge, &ArrowRules::default())
    }

    /// Rules-aware [`Arrow::speed_for_charge`].
    ///
    /// Interpolates linearly between `rules.speed_min` and
    /// `rules.speed_max` so a tuned pair keeps the same monotonic curve.
    pub fn speed_for_charge_with_rules(charge: f32, rules: &ArrowRules) -> f32 {
        rules.speed_min + (rules.speed_max - rules.speed_min) * charge.clamp(0.0, 1.0)
    }

    /// Damage for a draw fraction using default rules, clamped to
    /// `0.0..=1.0`.
    ///
    /// Monotonic in `charge`: 0.0 → 1, 0.2 → 2, 1.0 → 4.
    pub fn damage_for_charge(charge: f32) -> u16 {
        Self::damage_for_charge_with_rules(charge, &ArrowRules::default())
    }

    /// Rules-aware [`Arrow::damage_for_charge`].
    ///
    /// The span uses `f32` subtraction so a caller-provided inverted pair
    /// (`damage_min > damage_max`) cannot underflow the integer subtraction;
    /// the saturating float-to-int cast keeps the result in the u16 space.
    pub fn damage_for_charge_with_rules(charge: f32, rules: &ArrowRules) -> u16 {
        let span = f32::from(rules.damage_max) - f32::from(rules.damage_min);
        rules.damage_min + (span * charge.clamp(0.0, 1.0)).round() as u16
    }

    /// Substep count that keeps every step `<= ARROW_SUBSTEP_MAX`.
    ///
    /// `ceil(speed * dt / 0.25)` clamped to `1..=ARROW_MAX_SUBSTEPS`; a
    /// non-finite speed and an embedded arrow both report `1` (an embedded
    /// arrow is not moving).
    pub fn substeps(&self, dt: f32) -> u32 {
        if self.state == ArrowState::Embedded {
            return 1;
        }
        let speed = self.velocity.length();
        if !speed.is_finite() {
            return 1;
        }
        let raw = (speed * dt.max(0.0) / ARROW_SUBSTEP_MAX).ceil();
        if !raw.is_finite() {
            return 1;
        }
        (raw as u32).clamp(1, ARROW_MAX_SUBSTEPS)
    }

    /// Advances the arrow by one substep of `sub_dt` seconds using default
    /// rules.
    ///
    /// Flying: gravity is applied to `velocity.y` first, then the position
    /// integrates the new velocity. Embedded: motion is frozen. Both states
    /// drain `life` by `sub_dt`, so a caller that substeps the whole frame
    /// drains exactly that frame's `dt`.
    ///
    /// A non-finite or negative `sub_dt` is treated as `0.0`.
    pub fn advance(&mut self, sub_dt: f32) {
        self.advance_with_rules(sub_dt, &ArrowRules::default())
    }

    /// Rules-aware [`Arrow::advance`].
    ///
    /// Gravity is read from the per-tick rules, so an arrow fired before a
    /// live edit responds to the new value on its next substep. The stored
    /// `damage`/spawn lifetime are deliberately untouched: those are spawn
    /// facts and an existing arrow keeps its stored damage.
    pub fn advance_with_rules(&mut self, sub_dt: f32, rules: &ArrowRules) {
        let sub_dt = if !sub_dt.is_finite() || sub_dt < 0.0 {
            0.0
        } else {
            sub_dt
        };
        if self.state == ArrowState::Flying {
            self.velocity.y -= rules.effective_gravity() * sub_dt;
            self.position += self.velocity * sub_dt;
        }
        self.life -= sub_dt;
    }

    /// Sticks the arrow into a block it just hit using default rules.
    ///
    /// Only a flying arrow embeds. Position is unchanged (it stays where the
    /// sweep found the wall), velocity is kept for render orientation, and the
    /// lifetime restarts at [`ARROW_EMBED_SECS`].
    pub fn embed(&mut self) {
        self.embed_with_rules(&ArrowRules::default())
    }

    /// Rules-aware [`Arrow::embed`].
    ///
    /// The restart lifetime is `rules.embed_secs`; a non-finite value falls
    /// back to the pinned default and a negative one expires immediately.
    pub fn embed_with_rules(&mut self, rules: &ArrowRules) {
        if self.state != ArrowState::Flying {
            return;
        }
        self.state = ArrowState::Embedded;
        self.life = rules.effective_embed_secs();
    }

    /// Whether the arrow should be dropped from the session.
    ///
    /// True when the lifetime is spent, or when the position/velocity went
    /// non-finite: failing closed here is what keeps a corrupt projectile out
    /// of the renderer and out of any future persistence path.
    pub fn finished(&self) -> bool {
        self.life <= 0.0 || !self.position.is_finite() || !self.velocity.is_finite()
    }

    /// Axis-aligned bounds of the arrow, `± ARROW_HALF_EXTENT` around position.
    pub fn aabb(&self) -> (Vec3, Vec3) {
        let half = Vec3::splat(ARROW_HALF_EXTENT);
        (self.position - half, self.position + half)
    }
}

/// Damage an arrow lands on a mob, given the effective game mode.
///
/// Survival passes `base` through unchanged. Creative returns the
/// target's **remaining** health, so any surviving mob dies to a single
/// player arrow. The rule takes a plain `bool` rather than a
/// [`crate::player::GameMode`] so this module keeps its no-player-import
/// invariant, exactly like [`crate::player::melee::attack_damage`].
///
/// The flag is the *effective* mode read at hit time, never stored on
/// [`Arrow`]: a shot's geometry must not depend on the mode, and the mode
/// can change between fire and hit (Edit Background). The caller must also
/// scope it to player-owned arrows; a skeleton arrow's friendly fire keeps
/// its pinned damage.
///
/// [`Mob::take_hit`]: crate::world::mob::Mob::take_hit
///
/// The returned value never exceeds `target_health` in Creative, and
/// `damage == 0` is unreachable from the entity sweep because a dead
/// record (`health == 0`) is skipped before the hit test. `take_hit`
/// saturates the subtraction regardless.
pub const fn arrow_hit_damage(base: u16, creative: bool, target_health: u16) -> u16 {
    if creative {
        target_health
    } else {
        base
    }
}

/// Swept segment versus AABB, returning the hit parameter `t` in `0.0..=1.0`.
///
/// `delta` is the whole segment displacement for the step being resolved, so
/// `t = 0.0` is the start point and `t = 1.0` is the end point. This is the
/// projectile sweep primitive for mob and player collision: the session calls
/// it once per substep against every candidate AABB and keeps the smallest `t`.
///
/// The slab math is intentionally duplicated from
/// [`crate::player::melee::ray_aabb`] instead of imported: a world module must
/// not depend on the player layer. The differences are that this returns a
/// **segment** parameter clamped to the swept range and that an origin already
/// inside the box reports `Some(0.0)` (a projectile that starts inside a block
/// hits it immediately) rather than being rejected.
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
    if delta.length_squared() < DIRECTION_EPSILON_SQ {
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

    // -- charge scaling -----------------------------------------------------

    #[test]
    fn speed_and_damage_scale_monotonically_and_hit_the_pinned_endpoints() {
        approx(Arrow::speed_for_charge(0.0), 18.0, 1e-6);
        approx(Arrow::speed_for_charge(0.2), 20.8, 1e-4);
        approx(Arrow::speed_for_charge(1.0), 32.0, 1e-6);

        assert_eq!(Arrow::damage_for_charge(0.0), 1);
        assert_eq!(Arrow::damage_for_charge(0.2), 2);
        assert_eq!(Arrow::damage_for_charge(1.0), 4);

        let mut previous_speed = f32::NEG_INFINITY;
        let mut previous_damage = 0;
        for step in 0..=100 {
            let charge = step as f32 / 100.0;
            let speed = Arrow::speed_for_charge(charge);
            let damage = Arrow::damage_for_charge(charge);
            assert!(speed >= previous_speed, "speed must never decrease");
            assert!(damage >= previous_damage, "damage must never decrease");
            previous_speed = speed;
            previous_damage = damage;
        }
    }

    #[test]
    fn charge_scaling_clamps_outside_the_unit_range() {
        approx(Arrow::speed_for_charge(-5.0), ARROW_SPEED_MIN, 1e-6);
        approx(Arrow::speed_for_charge(5.0), ARROW_SPEED_MAX, 1e-6);
        assert_eq!(Arrow::damage_for_charge(-5.0), ARROW_DAMAGE_MIN);
        assert_eq!(Arrow::damage_for_charge(5.0), ARROW_DAMAGE_MAX);
    }

    #[test]
    fn skeleton_arrows_use_the_pinned_speed_and_damage() {
        let arrow = Arrow::skeleton(Vec3::ZERO, Vec3::X).expect("valid skeleton shot");
        assert_eq!(arrow.owner, ArrowOwner::Skeleton);
        approx(arrow.velocity.length(), 28.0, 1e-5);
        assert_eq!(arrow.damage, 2);
        assert_eq!(arrow.state, ArrowState::Flying);
        assert_eq!(arrow.life, ARROW_LIFETIME_SECS);
    }

    // -- hit damage rule ----------------------------------------------------

    #[test]
    fn arrow_hit_damage_passes_the_survival_base_through_unchanged() {
        for base in [1, 4, 255] {
            assert_eq!(
                arrow_hit_damage(base, false, 20),
                base,
                "Survival must never amplify the shot's own damage"
            );
        }
    }

    #[test]
    fn arrow_hit_damage_returns_the_targets_remaining_health_in_creative() {
        for (base, health) in [(1, 1), (4, 4), (4, 20)] {
            assert_eq!(
                arrow_hit_damage(base, true, health),
                health,
                "a Creative arrow one-shots a {health} hp mob whatever its charge"
            );
        }
    }

    #[test]
    fn arrow_hit_damage_never_exceeds_the_targets_health() {
        for health in [0, 1, 7, 20, u16::MAX] {
            let damage = arrow_hit_damage(ARROW_DAMAGE_MAX, true, health);
            assert_eq!(damage, health);
            assert!(damage <= health);
        }
        assert_eq!(
            arrow_hit_damage(ARROW_DAMAGE_MAX, true, 0),
            0,
            "a dead record can never be over-damaged"
        );
    }

    // -- construction validation -------------------------------------------

    #[test]
    fn construction_rejects_bad_geometry_and_bad_charge() {
        for bad in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(
                Arrow::player(Vec3::ZERO, Vec3::X, bad).is_none(),
                "charge {bad} must be refused"
            );
        }
        assert!(Arrow::player(Vec3::new(f32::NAN, 0.0, 0.0), Vec3::X, 1.0).is_none());
        assert!(Arrow::player(Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 0.0), 1.0).is_none());
        assert!(Arrow::player(Vec3::ZERO, Vec3::ZERO, 1.0).is_none());
        assert!(Arrow::skeleton(Vec3::ZERO, Vec3::ZERO).is_none());
        assert!(Arrow::skeleton(Vec3::new(0.0, f32::INFINITY, 0.0), Vec3::X).is_none());

        // A non-unit direction is normalized rather than trusted.
        let arrow = Arrow::player(Vec3::ZERO, Vec3::new(0.0, 0.0, 7.0), 1.0).expect("valid shot");
        approx(arrow.velocity.length(), ARROW_SPEED_MAX, 1e-5);
        approx(arrow.velocity.z, ARROW_SPEED_MAX, 1e-4);
    }

    // -- substeps and flight ------------------------------------------------

    #[test]
    fn substeps_keep_each_step_under_the_substep_budget() {
        let arrow = Arrow::player(Vec3::ZERO, Vec3::X, 1.0).expect("full charge");
        assert_eq!(arrow.substeps(0.1), 13);
        assert_eq!(arrow.substeps(0.0), 1);
        assert_eq!(arrow.substeps(-1.0), 1);

        let mut fast = arrow;
        fast.velocity = Vec3::new(400.0, 0.0, 0.0);
        assert_eq!(fast.substeps(0.1), ARROW_MAX_SUBSTEPS);

        let mut broken = arrow;
        broken.velocity = Vec3::new(f32::NAN, 0.0, 0.0);
        assert_eq!(broken.substeps(0.1), 1);

        let mut embedded = arrow;
        embedded.embed();
        assert_eq!(embedded.substeps(0.1), 1);
    }

    #[test]
    fn a_straight_up_shot_follows_the_discrete_parabola() {
        // Semi-implicit Euler with velocity-first ordering lands at
        // v0*t - 0.5*g*t*(t + h) exactly, which is the closed form of this
        // integrator. The continuous parabola differs by the documented
        // 0.5*g*h*t bias, asserted below.
        let speed = 32.0f32;
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::Y, 1.0).expect("valid shot");
        let h = 0.05f32;
        for _ in 0..10 {
            arrow.advance(h);
        }
        let t = 10.0 * h;
        let discrete = speed * t - 0.5 * ARROW_GRAVITY * t * (t + h);
        approx(arrow.position.y, discrete, 1e-3);

        let continuous = speed * t - 0.5 * ARROW_GRAVITY * t * t;
        let bias = 0.5 * ARROW_GRAVITY * h * t;
        approx(arrow.position.y, continuous - bias, 1e-3);
        approx(arrow.velocity.y, speed - ARROW_GRAVITY * t, 1e-4);
        approx(arrow.position.x, 0.0, 1e-6);
    }

    #[test]
    fn advance_drains_exactly_the_summed_dt() {
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        for _ in 0..10 {
            arrow.advance(0.01);
        }
        approx(arrow.life, ARROW_LIFETIME_SECS - 0.1, 1e-4);
        assert!(!arrow.finished());
    }

    #[test]
    fn advance_ignores_non_finite_and_negative_sub_dt() {
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        let before = arrow;
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.05] {
            arrow.advance(bad);
        }
        assert_eq!(
            arrow, before,
            "a bad substep must not move or age the arrow"
        );
    }

    // -- embed / despawn ----------------------------------------------------

    #[test]
    fn embed_freezes_motion_and_starts_the_short_timer() {
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 1.0).expect("valid shot");
        arrow.advance(0.01);
        let stuck_at = arrow.position;
        let orientation = arrow.velocity;
        arrow.embed();
        assert_eq!(arrow.state, ArrowState::Embedded);
        assert_eq!(arrow.life, ARROW_EMBED_SECS);
        assert_eq!(arrow.position, stuck_at);
        assert_eq!(
            arrow.velocity, orientation,
            "orientation is kept for render"
        );

        arrow.advance(0.5);
        assert_eq!(arrow.position, stuck_at, "an embedded arrow cannot move");
        approx(arrow.life, ARROW_EMBED_SECS - 0.5, 1e-6);
        assert!(!arrow.finished());
    }

    #[test]
    fn finished_becomes_true_exactly_when_the_life_runs_out() {
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        arrow.advance(ARROW_LIFETIME_SECS - 1e-3);
        assert!(!arrow.finished());
        arrow.advance(2e-3);
        assert!(arrow.finished());

        let mut embedded = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        embedded.embed();
        embedded.advance(ARROW_EMBED_SECS);
        assert!(embedded.finished());
    }

    #[test]
    fn a_non_finite_arrow_is_finished_immediately() {
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        arrow.position = Vec3::new(f32::NAN, 0.0, 0.0);
        assert!(arrow.finished());

        let mut broken = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        broken.velocity = Vec3::new(0.0, f32::INFINITY, 0.0);
        assert!(broken.finished());

        // The same fail-closed path fires through the integrator.
        let mut exploding = Arrow::player(Vec3::ZERO, Vec3::X, 1.0).expect("valid shot");
        exploding.velocity = Vec3::new(f32::MAX, f32::MAX, f32::MAX);
        exploding.advance(f32::MAX / 2.0);
        assert!(exploding.finished());
    }

    #[test]
    fn aabb_is_a_small_box_around_the_position() {
        let mut arrow = Arrow::player(Vec3::new(1.0, 2.0, 3.0), Vec3::X, 0.5).expect("valid shot");
        arrow.position = Vec3::new(1.0, 2.0, 3.0);
        let (min, max) = arrow.aabb();
        assert_eq!(min, Vec3::new(0.95, 1.95, 2.95));
        assert_eq!(max, Vec3::new(1.05, 2.05, 3.05));
    }

    // -- segment sweep ------------------------------------------------------

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
            "an arrow that starts inside a block hits it immediately"
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

    #[test]
    fn a_full_charge_arrow_crosses_a_one_block_wall_in_one_substep_chain() {
        // A wall face at x = 1.0 spanning a full block vertically and in z.
        let wall_min = Vec3::new(1.0, -1.0, -1.0);
        let wall_max = Vec3::new(2.0, 1.0, 1.0);

        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 1.0).expect("full charge");
        let dt = 0.1f32;
        let substeps = arrow.substeps(dt);
        assert_eq!(substeps, 13);
        let sub_dt = dt / substeps as f32;

        let mut hit = None;
        for step in 0..substeps {
            let before = arrow.position;
            arrow.advance(sub_dt);
            let delta = arrow.position - before;
            assert!(
                delta.length() <= ARROW_SUBSTEP_MAX + 1e-4,
                "substep {step} moved {} blocks, over the 0.25 budget",
                delta.length()
            );
            if hit.is_none() {
                hit = segment_aabb(before, delta, wall_min, wall_max);
            }
        }

        assert!(
            hit.is_some(),
            "the wall must be detected inside the substep chain, not skipped"
        );
        assert!(
            arrow.position.x > wall_min.x,
            "the arrow genuinely crossed the wall plane in this frame"
        );
    }

    // -- ArrowRules ---------------------------------------------------------

    fn tuned_rules() -> ArrowRules {
        ArrowRules {
            gravity: 5.0,
            speed_min: 10.0,
            speed_max: 50.0,
            damage_min: 2,
            damage_max: 6,
            skeleton_speed: 40.0,
            skeleton_damage: 5,
            lifetime_secs: 7.0,
            embed_secs: 0.5,
        }
    }

    #[test]
    fn default_arrow_rules_are_the_pinned_constants() {
        assert_eq!(
            ArrowRules::default(),
            ArrowRules {
                gravity: ARROW_GRAVITY,
                speed_min: ARROW_SPEED_MIN,
                speed_max: ARROW_SPEED_MAX,
                damage_min: ARROW_DAMAGE_MIN,
                damage_max: ARROW_DAMAGE_MAX,
                skeleton_speed: ARROW_SPEED_SKELETON,
                skeleton_damage: ARROW_DAMAGE_SKELETON,
                lifetime_secs: ARROW_LIFETIME_SECS,
                embed_secs: ARROW_EMBED_SECS,
            }
        );
    }

    #[test]
    fn tuned_player_arrow_rules_change_speed_damage_and_lifetime() {
        let rules = tuned_rules();
        approx(Arrow::speed_for_charge_with_rules(0.0, &rules), 10.0, 1e-6);
        approx(Arrow::speed_for_charge_with_rules(0.5, &rules), 30.0, 1e-6);
        approx(Arrow::speed_for_charge_with_rules(1.0, &rules), 50.0, 1e-6);
        assert_eq!(Arrow::damage_for_charge_with_rules(0.0, &rules), 2);
        assert_eq!(Arrow::damage_for_charge_with_rules(0.5, &rules), 4);
        assert_eq!(Arrow::damage_for_charge_with_rules(1.0, &rules), 6);

        let arrow = Arrow::player_with_rules(Vec3::ZERO, Vec3::X, 0.5, &rules)
            .expect("tuned shot constructs");
        approx(arrow.velocity.length(), 30.0, 1e-5);
        assert_eq!(arrow.damage, 4);
        assert_eq!(arrow.life, 7.0, "spawn lifetime comes from the rules");
        assert_eq!(arrow.owner, ArrowOwner::Player);

        // An existing arrow keeps its stored damage when a later tick uses
        // different rules; only gravity/embed are per-tick.
        let newer = ArrowRules {
            damage_min: 9,
            damage_max: 9,
            ..ArrowRules::default()
        };
        let mut in_flight = arrow;
        in_flight.advance_with_rules(0.1, &newer);
        assert_eq!(in_flight.damage, 4, "stored damage must not be retuned");
    }

    #[test]
    fn tuned_skeleton_rules_change_speed_damage_and_lifetime() {
        let rules = tuned_rules();
        let arrow = Arrow::skeleton_with_rules(Vec3::ZERO, Vec3::X, &rules)
            .expect("tuned skeleton shot constructs");
        assert_eq!(arrow.owner, ArrowOwner::Skeleton);
        approx(arrow.velocity.length(), 40.0, 1e-5);
        assert_eq!(arrow.damage, 5);
        assert_eq!(arrow.life, 7.0);
    }

    #[test]
    fn tuned_gravity_and_embed_are_read_per_tick() {
        let rules = tuned_rules();
        let mut arrow = Arrow::player(Vec3::ZERO, Vec3::X, 1.0).expect("valid shot");
        let start_y = arrow.position.y;
        arrow.advance_with_rules(0.1, &rules);
        approx(arrow.velocity.y, -5.0 * 0.1, 1e-6);
        approx(arrow.position.y, start_y - 5.0 * 0.1 * 0.1, 1e-6);

        // An in-flight arrow responds to gravity changing again mid-flight.
        let gravity_free = ArrowRules {
            gravity: 0.0,
            ..ArrowRules::default()
        };
        let y = arrow.position.y;
        let vy = arrow.velocity.y;
        arrow.advance_with_rules(0.1, &gravity_free);
        assert_eq!(arrow.velocity.y, vy, "zero gravity must not accelerate");
        approx(arrow.position.y, y + vy * 0.1, 1e-6);

        arrow.embed_with_rules(&rules);
        assert_eq!(arrow.state, ArrowState::Embedded);
        assert_eq!(arrow.life, 0.5, "the tuned embed timer is armed");
        arrow.advance(0.5);
        assert!(arrow.finished());

        // A second embed under new rules is ignored, exactly like the default.
        let mut once = Arrow::player(Vec3::ZERO, Vec3::X, 0.5).expect("valid shot");
        once.embed_with_rules(&rules);
        once.embed_with_rules(&ArrowRules::default());
        assert_eq!(once.life, 0.5);
    }

    #[test]
    fn malformed_arrow_rules_fail_closed() {
        let rules = ArrowRules {
            gravity: f32::NAN,
            lifetime_secs: f32::NAN,
            embed_secs: f32::INFINITY,
            ..ArrowRules::default()
        };
        let mut arrow = Arrow::player_with_rules(Vec3::ZERO, Vec3::X, 0.5, &rules)
            .expect("corrupt tuning still constructs");
        assert_eq!(
            arrow.life, ARROW_LIFETIME_SECS,
            "a corrupt lifetime falls back to the pinned default"
        );
        arrow.advance_with_rules(0.1, &rules);
        assert_eq!(
            arrow.velocity.y, 0.0,
            "corrupt gravity is inert and cannot leak NaN"
        );
        arrow.embed_with_rules(&rules);
        assert_eq!(arrow.life, ARROW_EMBED_SECS);

        // A negative lifetime expires immediately; a negative embed timer
        // does too, rather than never draining.
        let negative = ArrowRules {
            lifetime_secs: -1.0,
            embed_secs: -1.0,
            ..ArrowRules::default()
        };
        let expired =
            Arrow::player_with_rules(Vec3::ZERO, Vec3::X, 0.5, &negative).expect("constructs");
        assert!(expired.finished(), "a negative lifetime must expire");
        let mut embedded =
            Arrow::player_with_rules(Vec3::ZERO, Vec3::X, 0.5, &negative).expect("constructs");
        embedded.embed_with_rules(&negative);
        assert!(embedded.finished());

        // Zero configured speeds make the shot structurally impossible, not
        // a panic: spawn still fails closed through the existing validation.
        let no_speed = ArrowRules {
            speed_min: 0.0,
            speed_max: 0.0,
            ..ArrowRules::default()
        };
        assert!(Arrow::player_with_rules(Vec3::ZERO, Vec3::X, 1.0, &no_speed).is_none());
    }

    #[test]
    fn default_arrow_rules_reproduce_the_existing_wrappers() {
        let rules = ArrowRules::default();
        for charge in [0.0, 0.2, 0.5, 1.0] {
            approx(
                Arrow::speed_for_charge_with_rules(charge, &rules),
                Arrow::speed_for_charge(charge),
                0.0,
            );
            assert_eq!(
                Arrow::damage_for_charge_with_rules(charge, &rules),
                Arrow::damage_for_charge(charge)
            );
            assert_eq!(
                Arrow::player_with_rules(Vec3::ZERO, Vec3::X, charge, &rules),
                Arrow::player(Vec3::ZERO, Vec3::X, charge)
            );
        }
        assert_eq!(
            Arrow::skeleton_with_rules(Vec3::ZERO, Vec3::X, &rules),
            Arrow::skeleton(Vec3::ZERO, Vec3::X)
        );

        let mut legacy = Arrow::player(Vec3::ZERO, Vec3::Y, 0.5).expect("valid shot");
        let mut tuned = legacy;
        for _ in 0..10 {
            legacy.advance(0.05);
            tuned.advance_with_rules(0.05, &rules);
            assert_eq!(legacy, tuned, "default advance diverged");
        }
        legacy.embed();
        tuned.embed_with_rules(&rules);
        assert_eq!(legacy, tuned, "default embed diverged");
    }
}
