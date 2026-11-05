//! Pure player-melee domain: damage rules, the crosshair ray test, and the
//! swing cooldown accumulator.
//!
//! This module is pure lib code — no session, no renderer, no window, and no
//! wall clock — so every damage, reach, cooldown, and hit-selection rule is
//! assertable headlessly. It mirrors the proven [`crate::player::mining`] and
//! [`crate::player::eating`] shape: a small interface, one stateful float, and
//! a caller that owns the mutation.
//!
//! The *mob-side* half of a hit (i-frames, hurt flash, knockback impulse)
//! lives on [`crate::world::mob::Mob::take_hit`], not here, because it must
//! mutate the record the fixed-rate runtime owns.
//!
//! ## Timing contract
//!
//! [`MeleeState::tick`] consumes **raw frame dt**, never `dt * time_scale`
//! (the same rule eating follows): a swing cooldown is player-facing input
//! latency, not simulation time. A non-finite or negative `dt` adds nothing
//! and a positive `dt` is clamped to [`MAX_DT`] (0.1 s), so a stalled frame
//! can never skip a cooldown.

use glam::Vec3;

use crate::content::ContentRegistry;
use crate::player::inventory::{Item, ItemId, ToolKind, ToolTier};
use crate::player::survival::GameMode;
use crate::player::tools::tier_sword_damage;
use crate::world::mob::Mob;

/// Damage of an empty hand (or of any item that is not a sword).
pub const FIST_DAMAGE: u16 = 1;
/// Damage of a wood sword in Survival.
pub const SWORD_DAMAGE_WOOD: u16 = tier_sword_damage(ToolTier::Wood);
/// Damage of a stone sword in Survival.
pub const SWORD_DAMAGE_STONE: u16 = tier_sword_damage(ToolTier::Stone);
/// Damage of an iron sword in Survival.
pub const SWORD_DAMAGE_IRON: u16 = tier_sword_damage(ToolTier::Iron);
/// Reach of a melee swing in blocks, measured camera-to-mob-AABB.
pub const MELEE_REACH: f32 = 3.0;
/// Seconds a swing cooldown takes to elapse.
pub const SWING_COOLDOWN_SECS: f32 = 0.6;
/// Largest frame delta one tick may integrate, so a stall cannot skip a swing.
pub const MAX_DT: f32 = 0.1;

/// Tunable melee tuning consumed by the `_with_rules` entry points.
///
/// [`MeleeRules::default`] is built from the constants above, so the default
/// bundle reproduces [`attack_damage`], [`attack_damage_with`], and
/// [`MeleeState::tick`] exactly. The struct is a plain value type: it carries
/// no TOML/config vocabulary, only the numbers the simulation consumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeleeRules {
    /// Survival damage of an empty hand (or any non-sword item).
    pub fist_damage: u16,
    /// Survival damage of a wood sword.
    pub sword_damage_wood: u16,
    /// Survival damage of a stone sword.
    pub sword_damage_stone: u16,
    /// Survival damage of an iron sword.
    pub sword_damage_iron: u16,
    /// Reach of a melee swing in blocks (checked by the caller's ray test).
    pub reach: f32,
    /// Seconds a swing cooldown takes to elapse.
    pub swing_cooldown_secs: f32,
}

impl Default for MeleeRules {
    fn default() -> Self {
        Self {
            fist_damage: FIST_DAMAGE,
            sword_damage_wood: SWORD_DAMAGE_WOOD,
            sword_damage_stone: SWORD_DAMAGE_STONE,
            sword_damage_iron: SWORD_DAMAGE_IRON,
            reach: MELEE_REACH,
            swing_cooldown_secs: SWING_COOLDOWN_SECS,
        }
    }
}

impl MeleeRules {
    /// Survival damage for one built-in sword tier.
    const fn sword_damage(self, tier: ToolTier) -> u16 {
        match tier {
            ToolTier::Wood => self.sword_damage_wood,
            ToolTier::Stone => self.sword_damage_stone,
            ToolTier::Iron => self.sword_damage_iron,
        }
    }

    /// Cooldown actually armed after a swing.
    ///
    /// Non-finite rules fall back to the pinned default; a zero or negative
    /// configured cooldown is a legal "swing every tick" rule.
    fn effective_swing_cooldown_secs(self) -> f32 {
        if self.swing_cooldown_secs.is_finite() {
            self.swing_cooldown_secs.max(0.0)
        } else {
            SWING_COOLDOWN_SECS
        }
    }
}

/// Axis-parallel epsilon below which a ray component is treated as parallel to
/// the slab. Also the floor below which a direction is structurally degenerate.
const RAY_EPSILON_SQ: f32 = 1e-12;
/// Per-axis parallel test, the square root of [`RAY_EPSILON_SQ`].
const RAY_PARALLEL_EPSILON: f32 = 1e-6;

/// Damage one swing applies to a mob that currently has `target_health`.
///
/// Default-rules wrapper over [`attack_damage_with_rules`]. Creative one-shots:
/// the swing resolves to exactly the target's remaining health, so any weapon
/// (or none) kills instantly. Survival returns the sword's tier damage for
/// `ItemId::Tool { kind: Sword, tier }`; a fist, an empty slot, a non-sword
/// tool, and a plain material all fall back to [`FIST_DAMAGE`].
///
/// The result is never capped at `target_health` in Survival — [`Mob::take_hit`]
/// saturates the subtraction, so a strong hit on a weak mob still reports the
/// damage that was swung.
pub fn attack_damage(selected: Option<Item>, mode: GameMode, target_health: u16) -> u16 {
    attack_damage_with_rules(
        ContentRegistry::builtin(),
        &MeleeRules::default(),
        selected,
        mode,
        target_health,
    )
}

/// Registry-aware counterpart of [`attack_damage`] using default rules.
///
/// Built-in sword identities keep the frozen tier table, while a pack sword
/// (`ItemId::ModTool`) resolves its [`ToolDef`] and authored [`TierDef::damage`].
/// A pack tool that is not a sword, an unknown tool/tier index, and every
/// non-tool keep [`FIST_DAMAGE`], and Creative still one-shots regardless of
/// the selected identity. Built-in answers are byte-identical to
/// [`attack_damage`].
///
/// [`ToolDef`]: crate::content::ToolDef
/// [`TierDef`]: crate::content::TierDef
pub fn attack_damage_with(
    registry: &ContentRegistry,
    selected: Option<Item>,
    mode: GameMode,
    target_health: u16,
) -> u16 {
    attack_damage_with_rules(
        registry,
        &MeleeRules::default(),
        selected,
        mode,
        target_health,
    )
}

/// Rules-aware swing damage.
///
/// Built-in sword tiers map to the tuned `sword_damage_*` fields and every
/// other non-pack identity uses `rules.fist_damage`. A pack sword keeps the
/// damage its pack author set on [`TierDef::damage`] — content authored by a
/// pack is never overridden by live config — while an unknown/non-sword pack
/// tool falls back to the tuned fist damage. Creative still one-shots and the
/// Survival result is still never capped at `target_health`.
///
/// [`ToolDef`]: crate::content::ToolDef
/// [`TierDef`]: crate::content::TierDef
pub fn attack_damage_with_rules(
    registry: &ContentRegistry,
    rules: &MeleeRules,
    selected: Option<Item>,
    mode: GameMode,
    target_health: u16,
) -> u16 {
    if matches!(mode, GameMode::Creative) {
        return target_health;
    }
    match selected.map(|item| item.id()) {
        Some(ItemId::Tool {
            kind: ToolKind::Sword,
            tier,
        }) => rules.sword_damage(tier),
        Some(ItemId::ModTool(index)) => registry
            .tool(index)
            .filter(|tool| tool.kind == ToolKind::Sword)
            .and_then(|tool| registry.tier(tool.tier))
            .map_or(rules.fist_damage, |tier| tier.damage),
        _ => rules.fist_damage,
    }
}

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

/// One mob selected by the crosshair ray: its list index and entry distance.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MeleeHit {
    pub index: usize,
    pub distance: f32,
}

/// Nearest mob whose [`Mob::aabb`] the ray enters at a distance `<= reach`.
///
/// `mobs` is iterated exactly once and no candidate `Vec` is allocated, so the
/// selector stays allocation-free inside the frame loop. `dir` is normalized
/// internally, which keeps `reach` a block distance regardless of the caller's
/// direction magnitude. A non-finite or non-positive `reach` selects nothing.
pub fn nearest_mob_hit<'a>(
    origin: Vec3,
    dir: Vec3,
    reach: f32,
    mobs: impl Iterator<Item = (usize, &'a Mob)>,
) -> Option<MeleeHit> {
    if !reach.is_finite() || reach <= 0.0 {
        return None;
    }
    let dir = dir.normalize_or_zero();
    let mut best: Option<MeleeHit> = None;
    for (index, mob) in mobs {
        let (min, max) = mob.aabb();
        let Some(distance) = ray_aabb(origin, dir, min, max) else {
            continue;
        };
        if distance > reach {
            continue;
        }
        match best {
            Some(current) if current.distance <= distance => {}
            _ => best = Some(MeleeHit { index, distance }),
        }
    }
    best
}

/// A validated melee target: a mob that is alive *right now*.
///
/// Construction is fallible on purpose, mirroring
/// [`crate::player::mining::MiningTarget`]: a dead record (`health == 0`) can
/// never become a target, so no caller can swing at a corpse by inventing a
/// health value. The live health travels with the target so a caller does not
/// have to re-index a slice that may have changed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MeleeTarget {
    index: usize,
    health: u16,
}

impl MeleeTarget {
    /// Wraps mob `index` with its current health, or `None` when it is dead.
    pub const fn new(index: usize, health: u16) -> Option<Self> {
        if health == 0 {
            None
        } else {
            Some(Self { index, health })
        }
    }

    /// List index of the targeted mob.
    pub const fn index(self) -> usize {
        self.index
    }

    /// Health the mob had when the target was built.
    pub const fn health(self) -> u16 {
        self.health
    }
}

/// Outcome of one [`MeleeState::tick`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MeleeTick {
    /// Nothing to swing at and no cooldown running.
    Idle,
    /// A target exists but the swing cooldown has not elapsed yet.
    Cooling { remaining: f32 },
    /// Swing at `index` exactly once this tick; the cooldown just re-armed.
    Swing { index: usize },
}

/// Swing-cooldown accumulator.
///
/// Deep module, small interface: the only state is one cooldown float, and the
/// caller owns the actual damage application. A swing fires only with a
/// candidate *and* an elapsed cooldown, then re-arms to the rules' cooldown
/// (default [`SWING_COOLDOWN_SECS`]), so holding Left cannot out-damage the
/// cooldown no matter how fast the frames arrive.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MeleeState {
    cooldown: f32,
}

impl MeleeState {
    /// A ready-to-swing state (cooldown elapsed).
    pub const fn new() -> Self {
        Self { cooldown: 0.0 }
    }

    /// Clears the cooldown so the next candidate can swing immediately.
    ///
    /// Called on every input/session reset site: a stale cooldown must never
    /// survive a session swap or a focus loss.
    pub fn reset(&mut self) {
        self.cooldown = 0.0;
    }

    /// Remaining cooldown in seconds; `0.0` when a swing is available.
    pub const fn cooldown(&self) -> f32 {
        self.cooldown
    }

    /// Advances the cooldown by one frame using default rules.
    ///
    /// `dt` is raw frame time: non-finite or negative values add nothing, and
    /// positive values are clamped to [`MAX_DT`] so a stalled frame cannot
    /// skip the cooldown. The cooldown decays on every tick, swings only with
    /// a candidate and an elapsed cooldown, and otherwise reports
    /// [`MeleeTick::Cooling`] or [`MeleeTick::Idle`].
    pub fn tick(&mut self, dt: f32, candidate: Option<MeleeTarget>) -> MeleeTick {
        self.tick_with(dt, candidate, &MeleeRules::default())
    }

    /// Rules-aware [`MeleeState::tick`].
    ///
    /// The swing re-arms to `rules.swing_cooldown_secs` instead of the pinned
    /// constant; every other contract (raw frame dt, [`MAX_DT`] clamp, one
    /// swing per elapsed cooldown) is identical. Reach is not stored here —
    /// the caller passes [`MeleeRules::reach`] to its ray test.
    pub fn tick_with(
        &mut self,
        dt: f32,
        candidate: Option<MeleeTarget>,
        rules: &MeleeRules,
    ) -> MeleeTick {
        let dt = if !dt.is_finite() || dt <= 0.0 {
            0.0
        } else {
            dt.min(MAX_DT)
        };
        self.cooldown = (self.cooldown - dt).max(0.0);

        let Some(target) = candidate else {
            return MeleeTick::Idle;
        };

        if self.cooldown <= 0.0 {
            self.cooldown = rules.effective_swing_cooldown_secs();
            MeleeTick::Swing {
                index: target.index,
            }
        } else {
            MeleeTick::Cooling {
                remaining: self.cooldown,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::Material;
    use crate::world::mob::MobKind;

    fn item(id: ItemId) -> Item {
        Item::new(id, 1).expect("test identity is constructible")
    }

    fn sword(tier: ToolTier) -> Option<Item> {
        Some(item(ItemId::Tool {
            kind: ToolKind::Sword,
            tier,
        }))
    }

    fn item_with(registry: &ContentRegistry, id: ItemId) -> Item {
        Item::new_with(registry, id, 1).expect("fixture identity is constructible")
    }

    fn target(index: usize) -> Option<MeleeTarget> {
        MeleeTarget::new(index, 10)
    }

    fn mobs_at(positions: &[Vec3]) -> Vec<Mob> {
        positions
            .iter()
            .map(|pos| Mob::new(MobKind::Pig, *pos, 0.0))
            .collect()
    }

    // ---- ray_aabb -----------------------------------------------------

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

    // ---- nearest_mob_hit ----------------------------------------------

    #[test]
    fn nearest_mob_hit_skips_mobs_outside_reach() {
        // A Pig is 0.7 wide, so its box entry sits 0.35 before its centre.
        let inside = mobs_at(&[Vec3::new(0.0, 0.0, MELEE_REACH - 0.05 + 0.35)]);
        let origin = Vec3::ZERO;
        let hit = nearest_mob_hit(origin, Vec3::Z, MELEE_REACH, inside.iter().enumerate())
            .expect("entry just inside reach hits");
        assert_eq!(hit.index, 0);
        assert!(
            (hit.distance - (MELEE_REACH - 0.05)).abs() < 1e-4,
            "distance was {}",
            hit.distance
        );

        let outside = mobs_at(&[Vec3::new(0.0, 0.0, MELEE_REACH + 0.05 + 0.35)]);
        assert_eq!(
            nearest_mob_hit(origin, Vec3::Z, MELEE_REACH, outside.iter().enumerate()),
            None,
            "entry just outside reach must miss"
        );
    }

    #[test]
    fn nearest_mob_hit_chooses_the_closest_of_many() {
        let mobs = mobs_at(&[
            Vec3::new(0.0, 0.0, 2.5),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, 1.8),
            Vec3::new(4.0, 0.0, 1.0),
        ]);
        let hit = nearest_mob_hit(Vec3::ZERO, Vec3::Z, MELEE_REACH, mobs.iter().enumerate())
            .expect("a mob is ahead");
        assert_eq!(hit.index, 1, "the closest mob on the ray wins");
        assert!(
            (hit.distance - 0.65).abs() < 1e-4,
            "distance was {}",
            hit.distance
        );
    }

    #[test]
    fn nearest_mob_hit_respects_the_y_axis_and_non_positive_reach() {
        // Below the crosshair: the pig's box is 0..0.9 tall, so a ray at
        // y = 3.0 passes over it.
        let mobs = mobs_at(&[Vec3::new(0.0, 0.0, 1.0)]);
        assert_eq!(
            nearest_mob_hit(
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::Z,
                MELEE_REACH,
                mobs.iter().enumerate()
            ),
            None
        );
        for reach in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                nearest_mob_hit(Vec3::ZERO, Vec3::Z, reach, mobs.iter().enumerate()),
                None,
                "reach {reach} must select nothing"
            );
        }
    }

    #[test]
    fn nearest_mob_hit_is_none_for_an_empty_world_or_degenerate_ray() {
        let empty: Vec<Mob> = Vec::new();
        assert_eq!(
            nearest_mob_hit(Vec3::ZERO, Vec3::Z, MELEE_REACH, empty.iter().enumerate()),
            None
        );
        let mobs = mobs_at(&[Vec3::new(0.0, 0.0, 1.0)]);
        assert_eq!(
            nearest_mob_hit(Vec3::ZERO, Vec3::ZERO, MELEE_REACH, mobs.iter().enumerate()),
            None
        );
    }

    // ---- attack_damage -------------------------------------------------

    /// One authored ruby tier (speed 9, damage 9) with a pickaxe and a sword.
    fn pack_tier_registry() -> ContentRegistry {
        use crate::content::spec::{PackAsset, TierEntry, ToolEntry};
        use crate::content::{build_registry, PackContentSpec, PackIdentity};

        let mut spec = PackContentSpec {
            tiers: vec![TierEntry {
                id: "mymod:ruby".to_string(),
                display_name: Some("Ruby".to_string()),
                speed: 9.0,
                durability: 900,
                damage: 9,
            }],
            tools: vec![
                ToolEntry {
                    id: "mymod:ruby_pickaxe".to_string(),
                    display_name: Some("Ruby Pickaxe".to_string()),
                    kind: ToolKind::Pickaxe,
                    tier: "mymod:ruby".to_string(),
                    icon: "pickaxe.png".to_string(),
                },
                ToolEntry {
                    id: "mymod:ruby_sword".to_string(),
                    display_name: Some("Ruby Sword".to_string()),
                    kind: ToolKind::Sword,
                    tier: "mymod:ruby".to_string(),
                    icon: "sword.png".to_string(),
                },
            ],
            ..PackContentSpec::default()
        };
        for icon in ["pickaxe.png", "sword.png"] {
            spec.assets.insert(
                icon.to_string(),
                PackAsset {
                    rgba: vec![0u8; 16 * 16 * 4],
                    source_side: 16,
                },
            );
        }
        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "fixture".to_string(),
                    version: None,
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{report:?}");
        registry
    }

    #[test]
    fn pack_sword_uses_the_authored_tier_damage() {
        let registry = pack_tier_registry();
        let ruby_pickaxe = item_with(&registry, ItemId::ModTool(0));
        let ruby_sword = item_with(&registry, ItemId::ModTool(1));

        assert_eq!(
            attack_damage_with(&registry, Some(ruby_sword), GameMode::Survival, 10),
            9,
            "the authored ruby tier damage is the sword's survival damage"
        );
        assert_eq!(
            attack_damage_with(&registry, Some(ruby_pickaxe), GameMode::Survival, 10),
            FIST_DAMAGE,
            "a pack tool that is not a sword keeps fist damage"
        );
        assert_eq!(
            attack_damage_with(&registry, Some(ruby_sword), GameMode::Creative, 10),
            10,
            "Creative still one-shots with a pack sword"
        );

        // A pack tool index the resolving registry does not know is a fist.
        assert_eq!(
            attack_damage_with(
                ContentRegistry::builtin(),
                Some(ruby_sword),
                GameMode::Survival,
                10
            ),
            FIST_DAMAGE
        );
    }

    #[test]
    fn attack_damage_with_matches_every_builtin_answer() {
        let registry = ContentRegistry::builtin();
        let mut selections = vec![None];
        selections.push(Some(item(ItemId::Material(Material::Stick))));
        for kind in [
            ToolKind::Pickaxe,
            ToolKind::Axe,
            ToolKind::Shovel,
            ToolKind::Hoe,
            ToolKind::Sword,
            ToolKind::Bow,
        ] {
            for &tier in kind.allowed_tiers() {
                selections.push(Some(item(ItemId::Tool { kind, tier })));
            }
        }
        for selected in selections {
            for mode in [GameMode::Survival, GameMode::Creative] {
                for health in [1, 10, 20] {
                    assert_eq!(
                        attack_damage_with(registry, selected, mode, health),
                        attack_damage(selected, mode, health),
                        "built-in answer diverged for {selected:?}/{mode:?}/{health}"
                    );
                }
            }
        }
    }

    #[test]
    fn attack_damage_follows_the_weapon_table() {
        // Survival: sword tiers, then fist fallbacks.
        assert_eq!(
            attack_damage(sword(ToolTier::Wood), GameMode::Survival, 10),
            SWORD_DAMAGE_WOOD
        );
        assert_eq!(
            attack_damage(sword(ToolTier::Stone), GameMode::Survival, 10),
            SWORD_DAMAGE_STONE
        );
        assert_eq!(
            attack_damage(sword(ToolTier::Iron), GameMode::Survival, 10),
            SWORD_DAMAGE_IRON
        );
        assert_eq!(attack_damage(None, GameMode::Survival, 10), FIST_DAMAGE);
        assert_eq!(
            attack_damage(
                Some(item(ItemId::Material(Material::Stick))),
                GameMode::Survival,
                10
            ),
            FIST_DAMAGE
        );
        for (kind, tier) in [
            (ToolKind::Pickaxe, ToolTier::Iron),
            (ToolKind::Axe, ToolTier::Iron),
            (ToolKind::Shovel, ToolTier::Iron),
            (ToolKind::Hoe, ToolTier::Iron),
            (ToolKind::Bow, ToolTier::Wood),
        ] {
            assert_eq!(
                attack_damage(
                    Some(item(ItemId::Tool { kind, tier })),
                    GameMode::Survival,
                    10
                ),
                FIST_DAMAGE,
                "{kind:?} is not a melee weapon"
            );
        }
        // Damage is never capped at the target's health in Survival.
        assert_eq!(
            attack_damage(sword(ToolTier::Iron), GameMode::Survival, 2),
            SWORD_DAMAGE_IRON
        );
    }

    #[test]
    fn creative_one_shots_regardless_of_the_held_item() {
        for health in [1, 10, 20] {
            assert_eq!(
                attack_damage(None, GameMode::Creative, health),
                health,
                "a fist must one-shot in Creative"
            );
            assert_eq!(
                attack_damage(sword(ToolTier::Wood), GameMode::Creative, health),
                health
            );
            assert_eq!(
                attack_damage(
                    Some(item(ItemId::Material(Material::Apple))),
                    GameMode::Creative,
                    health
                ),
                health
            );
        }
    }

    // ---- MeleeTarget / MeleeState --------------------------------------

    #[test]
    fn dead_records_can_never_become_targets() {
        assert_eq!(MeleeTarget::new(3, 0), None);
        let target = MeleeTarget::new(3, 1).expect("one health is alive");
        assert_eq!(target.index(), 3);
        assert_eq!(target.health(), 1);
    }

    #[test]
    fn fresh_state_swings_immediately_with_a_candidate() {
        let mut melee = MeleeState::new();
        assert_eq!(melee.cooldown(), 0.0);
        assert_eq!(melee.tick(0.1, target(0)), MeleeTick::Swing { index: 0 });
        assert_eq!(melee.cooldown(), SWING_COOLDOWN_SECS);
    }

    #[test]
    fn eight_tenth_second_ticks_produce_exactly_one_more_swing() {
        let mut melee = MeleeState::new();
        assert_eq!(melee.tick(0.1, target(0)), MeleeTick::Swing { index: 0 });
        let mut swings = 0;
        for _ in 0..8 {
            if let MeleeTick::Swing { index } = melee.tick(0.1, target(0)) {
                assert_eq!(index, 0);
                swings += 1;
            }
        }
        assert_eq!(
            swings, 1,
            "the 0.6 s cooldown must allow exactly one swing in eight 0.1 s ticks"
        );
    }

    #[test]
    fn cooling_remaining_decreases_and_never_drives_a_swing() {
        let mut melee = MeleeState::new();
        assert_eq!(melee.tick(0.1, target(0)), MeleeTick::Swing { index: 0 });
        let mut previous = melee.cooldown();
        for _ in 0..4 {
            match melee.tick(0.1, target(0)) {
                MeleeTick::Cooling { remaining } => {
                    assert!(
                        remaining < previous,
                        "remaining {remaining} not below {previous}"
                    );
                    assert!(remaining > 0.0);
                    previous = remaining;
                }
                other => panic!("expected Cooling, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_huge_dt_is_clamped_and_cannot_skip_the_cooldown() {
        let mut melee = MeleeState::new();
        assert_eq!(
            melee.tick(10.0, target(0)),
            MeleeTick::Swing { index: 0 },
            "the first tick still swings"
        );
        assert!(
            (melee.cooldown() - SWING_COOLDOWN_SECS).abs() < 1e-6,
            "clamped dt must leave a full cooldown, got {}",
            melee.cooldown()
        );
        match melee.tick(10.0, target(0)) {
            MeleeTick::Cooling { remaining } => {
                assert!(
                    remaining > 0.4,
                    "a 10 s frame must not skip the cooldown, got {remaining}"
                );
            }
            other => panic!("expected Cooling, got {other:?}"),
        }
    }

    #[test]
    fn no_candidate_never_swings_and_nan_dt_adds_nothing() {
        let mut melee = MeleeState::new();
        for _ in 0..10 {
            assert_eq!(melee.tick(0.1, None), MeleeTick::Idle);
            assert_eq!(melee.cooldown(), 0.0);
        }
        // A swing, then only non-positive/non-finite deltas: the cooldown
        // must not decay at all.
        assert_eq!(melee.tick(0.1, target(0)), MeleeTick::Swing { index: 0 });
        let before = melee.cooldown();
        for dt in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            match melee.tick(dt, target(0)) {
                MeleeTick::Cooling { remaining } => {
                    assert!(
                        (remaining - before).abs() < 1e-6,
                        "dt {dt} changed the cooldown"
                    );
                }
                other => panic!("expected Cooling, got {other:?}"),
            }
        }
    }

    #[test]
    fn reset_makes_the_next_candidate_available() {
        let mut melee = MeleeState::new();
        assert_eq!(melee.tick(0.1, target(0)), MeleeTick::Swing { index: 0 });
        assert!(melee.cooldown() > 0.0);
        melee.reset();
        assert_eq!(melee.cooldown(), 0.0);
        assert_eq!(melee.tick(0.1, target(7)), MeleeTick::Swing { index: 7 });
    }

    #[test]
    fn idle_candidate_with_elapsed_cooldown_swings_at_the_new_index() {
        let mut melee = MeleeState::new();
        melee.tick(0.1, target(0));
        // No candidate for longer than the cooldown: the state stays Idle and
        // the cooldown fully elapses.
        for _ in 0..10 {
            assert_eq!(melee.tick(0.1, None), MeleeTick::Idle);
        }
        assert_eq!(melee.cooldown(), 0.0);
        assert_eq!(melee.tick(0.1, target(2)), MeleeTick::Swing { index: 2 });
    }

    // ---- MeleeRules ----------------------------------------------------

    #[test]
    fn default_melee_rules_are_the_pinned_constants() {
        assert_eq!(
            MeleeRules::default(),
            MeleeRules {
                fist_damage: FIST_DAMAGE,
                sword_damage_wood: SWORD_DAMAGE_WOOD,
                sword_damage_stone: SWORD_DAMAGE_STONE,
                sword_damage_iron: SWORD_DAMAGE_IRON,
                reach: MELEE_REACH,
                swing_cooldown_secs: SWING_COOLDOWN_SECS,
            }
        );
    }

    #[test]
    fn tuned_melee_rules_change_fist_and_sword_damage() {
        let rules = MeleeRules {
            fist_damage: 3,
            sword_damage_wood: 7,
            sword_damage_stone: 8,
            sword_damage_iron: 9,
            ..MeleeRules::default()
        };
        let registry = ContentRegistry::builtin();

        assert_eq!(
            attack_damage_with_rules(registry, &rules, None, GameMode::Survival, 10),
            3
        );
        assert_eq!(
            attack_damage_with_rules(
                registry,
                &rules,
                sword(ToolTier::Wood),
                GameMode::Survival,
                10
            ),
            7
        );
        assert_eq!(
            attack_damage_with_rules(
                registry,
                &rules,
                sword(ToolTier::Stone),
                GameMode::Survival,
                10
            ),
            8
        );
        assert_eq!(
            attack_damage_with_rules(
                registry,
                &rules,
                sword(ToolTier::Iron),
                GameMode::Survival,
                10
            ),
            9
        );
        // A non-sword item uses the tuned fist damage.
        assert_eq!(
            attack_damage_with_rules(
                registry,
                &rules,
                Some(item(ItemId::Material(Material::Stick))),
                GameMode::Survival,
                10
            ),
            3
        );
        // Clamp/one-shot semantics are unchanged: Survival never caps at the
        // target's health, and Creative still resolves to exactly it.
        assert_eq!(
            attack_damage_with_rules(
                registry,
                &rules,
                sword(ToolTier::Iron),
                GameMode::Survival,
                2
            ),
            9
        );
        assert_eq!(
            attack_damage_with_rules(
                registry,
                &rules,
                sword(ToolTier::Wood),
                GameMode::Creative,
                2
            ),
            2
        );
    }

    #[test]
    fn tuned_melee_rules_keep_pack_authored_damage_and_creative_one_shot() {
        let registry = pack_tier_registry();
        let ruby_sword = item_with(&registry, ItemId::ModTool(1));
        let ruby_pickaxe = item_with(&registry, ItemId::ModTool(0));
        let rules = MeleeRules {
            fist_damage: 3,
            sword_damage_wood: 7,
            sword_damage_stone: 8,
            sword_damage_iron: 9,
            ..MeleeRules::default()
        };

        assert_eq!(
            attack_damage_with_rules(&registry, &rules, Some(ruby_sword), GameMode::Survival, 10),
            9,
            "the pack's authored tier damage must beat the tuned built-in table"
        );
        assert_eq!(
            attack_damage_with_rules(
                &registry,
                &rules,
                Some(ruby_pickaxe),
                GameMode::Survival,
                10
            ),
            3,
            "a non-sword pack tool falls back to the tuned fist damage"
        );
        assert_eq!(
            attack_damage_with_rules(&registry, &rules, Some(ruby_sword), GameMode::Creative, 6),
            6,
            "Creative still one-shots under tuned rules"
        );
    }

    #[test]
    fn tuned_reach_extends_the_callers_ray_test_past_the_default() {
        // Pig box entry is 3.65 blocks ahead: outside the default 3.0 reach,
        // inside the tuned 5.0 reach.
        let mobs = mobs_at(&[Vec3::new(0.0, 0.0, 4.0)]);
        let rules = MeleeRules {
            reach: 5.0,
            ..MeleeRules::default()
        };
        assert_eq!(
            nearest_mob_hit(Vec3::ZERO, Vec3::Z, MELEE_REACH, mobs.iter().enumerate()),
            None,
            "the default reach must not see a mob 3.65 blocks away"
        );
        let hit = nearest_mob_hit(Vec3::ZERO, Vec3::Z, rules.reach, mobs.iter().enumerate())
            .expect("the tuned reach reaches the mob");
        assert_eq!(hit.index, 0);
    }

    #[test]
    fn tuned_swing_cooldown_changes_the_swing_cadence() {
        let rules = MeleeRules {
            swing_cooldown_secs: 0.25,
            ..MeleeRules::default()
        };
        let mut tuned = MeleeState::new();
        assert_eq!(
            tuned.tick_with(0.1, target(0), &rules),
            MeleeTick::Swing { index: 0 }
        );
        for _ in 0..2 {
            assert!(matches!(
                tuned.tick_with(0.1, target(0), &rules),
                MeleeTick::Cooling { .. }
            ));
        }
        assert_eq!(
            tuned.tick_with(0.1, target(0), &rules),
            MeleeTick::Swing { index: 0 },
            "a 0.25 s cooldown must allow a second swing on the fourth 0.1 s tick"
        );

        // The same four ticks only swing once at the default 0.6 s cadence.
        let mut legacy = MeleeState::new();
        assert_eq!(legacy.tick(0.1, target(0)), MeleeTick::Swing { index: 0 });
        for _ in 0..3 {
            assert!(matches!(
                legacy.tick(0.1, target(0)),
                MeleeTick::Cooling { .. }
            ));
        }
    }

    #[test]
    fn zero_and_malformed_cooldown_rules_fail_closed() {
        let zero = MeleeRules {
            swing_cooldown_secs: 0.0,
            ..MeleeRules::default()
        };
        let mut melee = MeleeState::new();
        assert_eq!(
            melee.tick_with(0.1, target(0), &zero),
            MeleeTick::Swing { index: 0 }
        );
        assert_eq!(melee.cooldown(), 0.0);
        assert_eq!(
            melee.tick_with(0.1, target(0), &zero),
            MeleeTick::Swing { index: 0 },
            "zero cooldown means every candidate tick swings"
        );

        let nan = MeleeRules {
            swing_cooldown_secs: f32::NAN,
            ..MeleeRules::default()
        };
        let mut fallback = MeleeState::new();
        assert_eq!(
            fallback.tick_with(0.1, target(0), &nan),
            MeleeTick::Swing { index: 0 }
        );
        assert_eq!(
            fallback.cooldown(),
            SWING_COOLDOWN_SECS,
            "a corrupt cooldown falls back to the pinned default"
        );
    }

    #[test]
    fn default_melee_rules_reproduce_the_existing_wrappers() {
        let registry = ContentRegistry::builtin();
        let rules = MeleeRules::default();
        let selections = [
            None,
            sword(ToolTier::Wood),
            sword(ToolTier::Stone),
            sword(ToolTier::Iron),
            Some(item(ItemId::Material(Material::Stick))),
        ];
        for selected in selections {
            for mode in [GameMode::Survival, GameMode::Creative] {
                for health in [1, 10, 20] {
                    assert_eq!(
                        attack_damage_with_rules(registry, &rules, selected, mode, health),
                        attack_damage_with(registry, selected, mode, health),
                    );
                    assert_eq!(
                        attack_damage_with(registry, selected, mode, health),
                        attack_damage(selected, mode, health),
                    );
                }
            }
        }

        let mut legacy = MeleeState::new();
        let mut tuned = MeleeState::new();
        for step in 0..12 {
            let candidate = if step % 3 == 0 { target(step) } else { None };
            assert_eq!(
                legacy.tick(0.1, candidate),
                tuned.tick_with(0.1, candidate, &rules),
                "default rules diverged on step {step}"
            );
            assert_eq!(legacy.cooldown(), tuned.cooldown());
        }
    }
}
