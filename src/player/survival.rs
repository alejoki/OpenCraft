//! Authoritative survival vocabulary: game modes, the vitals triple, and the
//! pure damage/hunger/air rules runtime.
//!
//! M008 introduces exactly one mode/vitals vocabulary so persistence, session
//! restore, the UI, and the survival mechanics (fall damage, drowning,
//! starvation, regeneration) all read and write the same types. The rules are
//! deliberately fail-closed:
//!
//! * A [`SurvivalState`] can only exist through [`SurvivalState::new`] (checked)
//!   or [`SurvivalState::full`] (the all-max baseline), so an out-of-range value
//!   can never be constructed even by future decode paths. The fields stay
//!   private; every gameplay mutation goes through a checked mutator that
//!   returns the amount actually applied.
//! * `MAX_AIR` (1200) is the wire ceiling used by the save codec, while
//!   `FULL_AIR` (300) is the gameplay refill value. The two are intentionally
//!   distinct: a save may legally carry more than a full lung (for example a
//!   future potion effect) but never more than the codec ceiling.
//! * Flight is a *mode* capability, not a player flag: `GameMode::allows_fly` is
//!   the only source of truth, and `Player::can_fly` structurally ANDs it with
//!   the toggle so Survival flight cannot be reached by assigning `fly_mode`.
//! * [`SurvivalRuntime`] is a pure, headless state machine over game time. It
//!   owns no world state: the caller measures a one-shot fall distance and a
//!   head-submerged flag, and applies the returned totals to the session's
//!   transient message/feedback surfaces. It never clears external state and
//!   never runs an unbounded loop.
//! * Every damaging rule also names its [`DamageCause`] on the returned
//!   [`SurvivalTick`]. The survival rules are the only writers of `Fall`,
//!   `Drowning`, and `Starvation`; the session's mob-melee seam is the only
//!   writer of `Zombie`. The tag is ambient (never persisted): a loaded dead
//!   save has no cause and renders "Unknown".

use crate::player::combat::DamageCause;

/// Maximum health points. Zero health is a valid state (death screen, S04).
pub const MAX_HEALTH: u16 = 20;

/// Maximum hunger points. Zero hunger is valid (starving, hand-swing/eat rules).
pub const MAX_HUNGER: u16 = 20;

/// Air value a full lung refills to during gameplay.
pub const FULL_AIR: u16 = 300;

/// Fail-closed wire ceiling for air; decode rejects anything above this.
pub const MAX_AIR: u16 = 1200;

/// Drops up to this many blocks are free; damage is `floor(distance - safe)`.
pub const FALL_SAFE_BLOCKS: f32 = 3.0;

/// Air points drained per game second while the head is submerged.
pub const AIR_DRAIN_PER_SEC: f32 = 20.0;

/// Game seconds between drowning damage ticks once the lungs are empty.
pub const DROWN_INTERVAL_SECS: f32 = 1.0;

/// Health lost per drowning tick.
pub const DROWN_DAMAGE: u16 = 2;

/// Game seconds between hunger point drains.
pub const HUNGER_DRAIN_SECS: f32 = 30.0;

/// Game seconds between starvation damage ticks at zero hunger.
pub const STARVE_INTERVAL_SECS: f32 = 4.0;

/// Health lost per starvation tick.
pub const STARVE_DAMAGE: u16 = 1;

/// Regeneration only runs at or above this hunger value.
pub const REGEN_HUNGER_THRESHOLD: u16 = 18;

/// Game seconds between regeneration heals.
pub const REGEN_INTERVAL_SECS: f32 = 4.0;

/// Hunger spent per regenerated health point.
pub const REGEN_HUNGER_COST: u16 = 1;

/// Defensive per-tick clamp: a stalled or debugger-paused frame can never
/// apply more than this many game seconds at once.
pub const MAX_TICK_GAME_SECS: f32 = 10.0;

/// Tunable survival tuning consumed by [`SurvivalRuntime::tick_with`].
///
/// [`SurvivalRules::default`] is built from the constants above, so the
/// default bundle reproduces [`SurvivalRuntime::tick`] exactly. The struct is
/// a plain value type: it carries no TOML/config vocabulary, only the numbers
/// the simulation consumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurvivalRules {
    /// Drops up to this many blocks are free.
    pub fall_safe_blocks: f32,
    /// Air points drained per game second while the head is submerged.
    pub air_drain_per_sec: f32,
    /// Game seconds between drowning damage ticks once the lungs are empty.
    pub drown_interval_secs: f32,
    /// Health lost per drowning tick.
    pub drown_damage: u16,
    /// Game seconds between hunger point drains.
    pub hunger_drain_secs: f32,
    /// Game seconds between starvation damage ticks at zero hunger.
    pub starve_interval_secs: f32,
    /// Health lost per starvation tick.
    pub starve_damage: u16,
    /// Regeneration only runs at or above this hunger value.
    pub regen_hunger_threshold: u16,
    /// Game seconds between regeneration heals.
    pub regen_interval_secs: f32,
    /// Hunger spent per regenerated health point.
    pub regen_hunger_cost: u16,
}

impl Default for SurvivalRules {
    fn default() -> Self {
        Self {
            fall_safe_blocks: FALL_SAFE_BLOCKS,
            air_drain_per_sec: AIR_DRAIN_PER_SEC,
            drown_interval_secs: DROWN_INTERVAL_SECS,
            drown_damage: DROWN_DAMAGE,
            hunger_drain_secs: HUNGER_DRAIN_SECS,
            starve_interval_secs: STARVE_INTERVAL_SECS,
            starve_damage: STARVE_DAMAGE,
            regen_hunger_threshold: REGEN_HUNGER_THRESHOLD,
            regen_interval_secs: REGEN_INTERVAL_SECS,
            regen_hunger_cost: REGEN_HUNGER_COST,
        }
    }
}

/// Per-tick step cap for a damage rule, derived from the live damage value.
///
/// A smaller configured damage needs more steps to reach lethal, so the cap
/// must follow the rule rather than the default constant. Zero damage needs
/// no steps at all and is handled by the caller as an inert rule.
fn damage_step_cap(damage: u16) -> u32 {
    if damage == 0 {
        0
    } else {
        u32::from(MAX_HEALTH.div_ceil(damage))
    }
}

/// The two supported game modes.
///
/// `Player::new` starts every fresh player in [`GameMode::Creative`] so all
/// pre-M008 CLI demos, smokes, and physics tests keep their flying default; the
/// New Game row and the v6 save decide what a *restored* world actually uses.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum GameMode {
    Survival,
    Creative,
}

impl GameMode {
    /// Whether this mode permits flight at all. The player still has to have
    /// its fly toggle on; see `Player::can_fly`.
    pub const fn allows_fly(self) -> bool {
        matches!(self, Self::Creative)
    }

    /// Stable human-readable label, also the persisted UI name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Survival => "Survival",
            Self::Creative => "Creative",
        }
    }

    /// Cycles to the other mode (the New Game mode row uses this).
    pub const fn next(self) -> Self {
        match self {
            Self::Survival => Self::Creative,
            Self::Creative => Self::Survival,
        }
    }
}

/// The persisted vitals triple: health, hunger, and breath.
///
/// Fields are private and the only constructors are checked, so every existing
/// value is within `0..=MAX_HEALTH`, `0..=MAX_HUNGER`, and `0..=MAX_AIR`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SurvivalState {
    health: u16,
    hunger: u16,
    air: u16,
}

impl SurvivalState {
    /// The all-max baseline used by fresh players, v5 migration, and Creative.
    pub const fn full() -> Self {
        Self {
            health: MAX_HEALTH,
            hunger: MAX_HUNGER,
            air: FULL_AIR,
        }
    }

    /// Checked constructor. Returns `None` if any field exceeds its ceiling.
    ///
    /// Zero is accepted for health and hunger (dead / starving states are
    /// reachable gameplay, S04 owns the death screen); air above `MAX_AIR` is a
    /// malformed record rather than gameplay, so it is rejected fail-closed.
    pub const fn new(health: u16, hunger: u16, air: u16) -> Option<Self> {
        if health <= MAX_HEALTH && hunger <= MAX_HUNGER && air <= MAX_AIR {
            Some(Self {
                health,
                hunger,
                air,
            })
        } else {
            None
        }
    }

    pub const fn health(&self) -> u16 {
        self.health
    }

    pub const fn hunger(&self) -> u16 {
        self.hunger
    }

    pub const fn air(&self) -> u16 {
        self.air
    }

    /// Applies `amount` damage and returns how much was actually applied.
    ///
    /// Health saturates at zero: a lethal hit reports only the remaining
    /// health, and further damage is a no-op reporting zero.
    pub fn damage(&mut self, amount: u16) -> u16 {
        let applied = amount.min(self.health);
        self.health -= applied;
        applied
    }

    /// Restores `amount` health and returns how much was actually applied.
    ///
    /// Healing stops at [`MAX_HEALTH`]; a full-health heal reports zero.
    pub fn heal(&mut self, amount: u16) -> u16 {
        let applied = amount.min(MAX_HEALTH - self.health);
        self.health += applied;
        applied
    }

    /// Spends `amount` hunger and returns how much was actually spent.
    ///
    /// Hunger saturates at zero, mirroring [`SurvivalState::damage`].
    pub fn spend_hunger(&mut self, amount: u16) -> u16 {
        let applied = amount.min(self.hunger);
        self.hunger -= applied;
        applied
    }

    /// Restores `amount` hunger and returns how much was actually applied.
    ///
    /// Hunger stops at [`MAX_HUNGER`].
    pub fn restore_hunger(&mut self, amount: u16) -> u16 {
        let applied = amount.min(MAX_HUNGER - self.hunger);
        self.hunger += applied;
        applied
    }

    /// Drains `amount` air and returns how much was actually drained.
    ///
    /// Air saturates at zero.
    pub fn drain_air(&mut self, amount: u16) -> u16 {
        let applied = amount.min(self.air);
        self.air -= applied;
        applied
    }

    /// Refills the lungs to [`FULL_AIR`], deliberately not to [`MAX_AIR`].
    ///
    /// A save may carry more than a full lung; surfacing clamps it back to the
    /// gameplay refill value.
    pub fn refill_air(&mut self) {
        self.air = FULL_AIR;
    }
}

/// Health lost by a dry-land fall of `distance_blocks` blocks.
///
/// Non-finite distances other than positive infinity and negative distances
/// are treated as "no fall"; otherwise damage is
/// `floor(distance - FALL_SAFE_BLOCKS)` clamped to [`MAX_HEALTH`], so a
/// 3-block drop is safe, 4 blocks costs 1, and 5 blocks costs 2. This is the
/// default-rules wrapper over [`fall_damage_with`].
pub fn fall_damage(distance_blocks: f32) -> u16 {
    fall_damage_with(distance_blocks, &SurvivalRules::default())
}

/// Health lost by a dry-land fall of `distance_blocks` blocks under `rules`.
///
/// Same fail-closed shape as [`fall_damage`], but the safe distance comes from
/// the live rule value instead of [`FALL_SAFE_BLOCKS`].
pub fn fall_damage_with(distance_blocks: f32, rules: &SurvivalRules) -> u16 {
    if distance_blocks.is_nan() || distance_blocks <= rules.fall_safe_blocks {
        return 0;
    }
    let raw = (distance_blocks - rules.fall_safe_blocks).floor();
    if raw >= f32::from(MAX_HEALTH) {
        MAX_HEALTH
    } else {
        raw as u16
    }
}

/// One frame's survival input: mode, submersion, and the one-shot fall
/// distance measured by the physics layer (zero when nothing landed).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SurvivalContext {
    pub mode: GameMode,
    pub head_submerged: bool,
    pub fall_distance: f32,
}

/// One frame's applied survival results, for feedback/observability callers.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SurvivalTick {
    /// Total damage applied this tick (fall + drowning + starvation).
    pub damage: u16,
    /// Total health regenerated this tick.
    pub healed: u16,
    /// Ambient cause of the LAST damaging rule that applied health loss this
    /// tick, or `None` when nothing damaged.
    ///
    /// The fixed rule order (fall, drowning, starvation) decides a
    /// multiple-damage tick: the last rule to land owns the tag. Never
    /// persisted; the session copies it into its single `last_damage_cause`
    /// writer only when `damage > 0`.
    pub cause: Option<DamageCause>,
}

/// Pure game-time survival rules: fall damage, air/drowning, hunger,
/// starvation, and regeneration.
///
/// The runtime holds only fractional accumulators, so a caller can tick it
/// with any frame delta and still get deterministic game-time behavior.
/// Accumulators are reset when the tick is inert (Creative or dead) and when
/// damage reaches zero health; they are never persisted.
#[derive(Clone, Debug, Default)]
pub struct SurvivalRuntime {
    /// Game seconds accumulated toward the next hunger point.
    hunger_accum: f32,
    /// Fractional air points accumulated toward the next drained point.
    air_accum: f32,
    /// Game seconds accumulated toward the next drowning damage tick.
    drown_accum: f32,
    /// Game seconds accumulated toward the next starvation damage tick.
    starve_accum: f32,
    /// Game seconds accumulated toward the next regeneration heal.
    regen_accum: f32,
}

impl SurvivalRuntime {
    /// Clears every fractional accumulator.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Advances the survival rules by `game_dt` game seconds and applies the
    /// result to `vitals`.
    ///
    /// The order is fixed and deterministic: fall damage, air/drowning,
    /// hunger drain, starvation, then regeneration. `game_dt` is clamped to
    /// `0..=MAX_TICK_GAME_SECS` (NaN becomes zero). Creative and zero-health
    /// states are inert and reset the accumulators. The caller owns the
    /// one-shot fall distance; this runtime never clears external state.
    pub fn tick(
        &mut self,
        vitals: &mut SurvivalState,
        ctx: SurvivalContext,
        game_dt: f32,
    ) -> SurvivalTick {
        self.tick_with(vitals, ctx, game_dt, &SurvivalRules::default())
    }

    /// Rules-aware [`SurvivalRuntime::tick`].
    ///
    /// Every constant-driven rule reads `rules` instead of the module constant;
    /// the default bundle reproduces the old behavior exactly. Malformed rule
    /// values fail closed rather than looping or dividing by zero:
    ///
    /// - non-finite or non-positive drain/interval values make that rule inert;
    /// - zero damage makes drowning/starvation inert (their step cap would be
    ///   zero anyway);
    /// - the per-tick damage step cap follows the live damage value so a
    ///   smaller configured hit still reaches lethal;
    /// - regeneration never heals when a positive hunger cost is unaffordable.
    pub fn tick_with(
        &mut self,
        vitals: &mut SurvivalState,
        ctx: SurvivalContext,
        game_dt: f32,
        rules: &SurvivalRules,
    ) -> SurvivalTick {
        let dt = if game_dt.is_nan() {
            0.0
        } else {
            game_dt.clamp(0.0, MAX_TICK_GAME_SECS)
        };

        if ctx.mode != GameMode::Survival || vitals.health == 0 {
            self.reset();
            return SurvivalTick::default();
        }

        let mut report = SurvivalTick::default();

        // 1. One-shot fall damage from the physics latch.
        let fall = fall_damage_with(ctx.fall_distance, rules);
        if fall > 0 {
            let applied = vitals.damage(fall);
            report.damage = report.damage.saturating_add(applied);
            if applied > 0 {
                report.cause = Some(DamageCause::Fall);
            }
            if vitals.health == 0 {
                self.reset();
                return report;
            }
        }

        // 2. Air and drowning. Drowning damage only starts on a tick whose
        //    *start* already had empty lungs, so the drain tick itself is safe.
        if ctx.head_submerged {
            let air_was_empty = vitals.air == 0;

            if rules.air_drain_per_sec.is_finite() && rules.air_drain_per_sec > 0.0 {
                self.air_accum += rules.air_drain_per_sec * dt;
                let whole_air = self.air_accum.floor();
                if whole_air > 0.0 {
                    self.air_accum -= whole_air;
                    let drain = whole_air.min(f32::from(u16::MAX)) as u16;
                    vitals.drain_air(drain);
                    if vitals.air == 0 {
                        self.air_accum = 0.0;
                    }
                }
            } else {
                self.air_accum = 0.0;
            }

            if air_was_empty {
                let interval = rules.drown_interval_secs;
                if rules.drown_damage > 0 && interval.is_finite() && interval > 0.0 {
                    self.drown_accum += dt;
                    let steps = (self.drown_accum / interval).floor();
                    if steps > 0.0 {
                        self.drown_accum -= steps * interval;
                        let steps = steps.min(damage_step_cap(rules.drown_damage) as f32) as u32;
                        let total =
                            u16::try_from(steps.saturating_mul(u32::from(rules.drown_damage)))
                                .unwrap_or(u16::MAX);
                        let applied = vitals.damage(total);
                        report.damage = report.damage.saturating_add(applied);
                        if applied > 0 {
                            report.cause = Some(DamageCause::Drowning);
                        }
                        if vitals.health == 0 {
                            self.reset();
                            return report;
                        }
                    }
                } else {
                    self.drown_accum = 0.0;
                }
            } else {
                self.drown_accum = 0.0;
            }
        } else {
            self.air_accum = 0.0;
            self.drown_accum = 0.0;
            vitals.refill_air();
        }

        // 3. Hunger drain. The accumulator freezes while hunger is zero, so a
        //    starving player never builds a backlog of hunger points.
        let hunger_was_empty = vitals.hunger == 0;
        if hunger_was_empty {
            self.hunger_accum = 0.0;
        } else if rules.hunger_drain_secs.is_finite() && rules.hunger_drain_secs > 0.0 {
            self.hunger_accum += dt;
            let steps = (self.hunger_accum / rules.hunger_drain_secs).floor();
            if steps > 0.0 {
                self.hunger_accum -= steps * rules.hunger_drain_secs;
                let steps = steps.min(f32::from(MAX_HUNGER)) as u16;
                vitals.spend_hunger(steps);
            }
        } else {
            self.hunger_accum = 0.0;
        }

        // 4. Starvation. Only a tick that started at zero hunger accrues
        //    starvation time, mirroring the air/drowning rule.
        if hunger_was_empty {
            let interval = rules.starve_interval_secs;
            if rules.starve_damage > 0 && interval.is_finite() && interval > 0.0 {
                self.starve_accum += dt;
                let steps = (self.starve_accum / interval).floor();
                if steps > 0.0 {
                    self.starve_accum -= steps * interval;
                    let steps = steps.min(damage_step_cap(rules.starve_damage) as f32) as u32;
                    let total = u16::try_from(steps.saturating_mul(u32::from(rules.starve_damage)))
                        .unwrap_or(u16::MAX);
                    let applied = vitals.damage(total);
                    report.damage = report.damage.saturating_add(applied);
                    if applied > 0 {
                        report.cause = Some(DamageCause::Starvation);
                    }
                    if vitals.health == 0 {
                        self.reset();
                        return report;
                    }
                }
            } else {
                self.starve_accum = 0.0;
            }
        } else {
            self.starve_accum = 0.0;
        }

        // 5. Regeneration: one health per interval, one hunger per heal, only
        //    while hunger is at or above the threshold and health is missing.
        //    A positive hunger cost must be affordable, otherwise the heal is
        //    skipped (cost zero is free regeneration). Natural regeneration is
        //    suppressed while the head is submerged: a drowning player cannot
        //    out-heal the drowning ticks, so an empty lung always kills.
        if !ctx.head_submerged
            && vitals.health < MAX_HEALTH
            && vitals.hunger >= rules.regen_hunger_threshold
            && rules.regen_interval_secs.is_finite()
            && rules.regen_interval_secs > 0.0
        {
            self.regen_accum += dt;
            let steps = (self.regen_accum / rules.regen_interval_secs).floor();
            if steps > 0.0 {
                self.regen_accum -= steps * rules.regen_interval_secs;
                let mut remaining =
                    (steps.min(f32::from(MAX_HEALTH)) as u16).min(MAX_HEALTH - vitals.health);
                while remaining > 0
                    && vitals.health < MAX_HEALTH
                    && vitals.hunger >= rules.regen_hunger_threshold
                {
                    if rules.regen_hunger_cost > 0 && vitals.hunger < rules.regen_hunger_cost {
                        break;
                    }
                    let healed = vitals.heal(1);
                    if healed == 0 {
                        break;
                    }
                    vitals.spend_hunger(rules.regen_hunger_cost);
                    report.healed = report.healed.saturating_add(healed);
                    remaining -= 1;
                }
            }
        } else {
            self.regen_accum = 0.0;
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn survival_ctx() -> SurvivalContext {
        SurvivalContext {
            mode: GameMode::Survival,
            head_submerged: false,
            fall_distance: 0.0,
        }
    }

    fn run_ticks(
        runtime: &mut SurvivalRuntime,
        vitals: &mut SurvivalState,
        ctx: SurvivalContext,
        dt: f32,
        count: usize,
    ) -> SurvivalTick {
        let mut total = SurvivalTick::default();
        for _ in 0..count {
            let tick = runtime.tick(vitals, ctx, dt);
            total.damage = total.damage.saturating_add(tick.damage);
            total.healed = total.healed.saturating_add(tick.healed);
            if tick.damage > 0 {
                total.cause = tick.cause;
            }
        }
        total
    }

    #[test]
    fn full_is_max_health_hunger_and_full_lung() {
        let full = SurvivalState::full();
        assert_eq!(full.health(), MAX_HEALTH);
        assert_eq!(full.hunger(), MAX_HUNGER);
        assert_eq!(full.air(), FULL_AIR);
        // FULL_AIR is a gameplay value, not the wire ceiling.
        const { assert!(FULL_AIR < MAX_AIR) };
    }

    #[test]
    fn zero_and_max_are_accepted_per_field() {
        assert!(SurvivalState::new(0, 0, 0).is_some());
        assert!(SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR).is_some());
        let checked = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR).unwrap();
        assert_eq!(checked.health(), MAX_HEALTH);
        assert_eq!(checked.hunger(), MAX_HUNGER);
        assert_eq!(checked.air(), MAX_AIR);
    }

    #[test]
    fn one_over_max_is_rejected_per_field() {
        assert!(SurvivalState::new(MAX_HEALTH + 1, MAX_HUNGER, MAX_AIR).is_none());
        assert!(SurvivalState::new(MAX_HEALTH, MAX_HUNGER + 1, MAX_AIR).is_none());
        assert!(SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR + 1).is_none());
        // u16::MAX is the extreme malformed wire value and must also fail.
        assert!(SurvivalState::new(u16::MAX, 0, 0).is_none());
        assert!(SurvivalState::new(0, u16::MAX, 0).is_none());
        assert!(SurvivalState::new(0, 0, u16::MAX).is_none());
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(GameMode::Survival.label(), "Survival");
        assert_eq!(GameMode::Creative.label(), "Creative");
    }

    #[test]
    fn only_creative_allows_flight() {
        assert!(GameMode::Creative.allows_fly());
        assert!(!GameMode::Survival.allows_fly());
    }

    #[test]
    fn next_cycles_between_the_two_modes() {
        assert_eq!(GameMode::Survival.next(), GameMode::Creative);
        assert_eq!(GameMode::Creative.next(), GameMode::Survival);
        assert_eq!(GameMode::Survival.next().next(), GameMode::Survival);
    }

    #[test]
    fn damage_returns_applied_amount_and_never_goes_below_zero() {
        let mut vitals = SurvivalState::full();
        assert_eq!(vitals.damage(5), 5);
        assert_eq!(vitals.health(), 15);
        assert_eq!(vitals.damage(u16::MAX), 15);
        assert_eq!(vitals.health(), 0);
        assert_eq!(vitals.damage(1), 0);
        assert_eq!(vitals.health(), 0);
    }

    #[test]
    fn heal_returns_applied_amount_and_stops_at_max_health() {
        let mut vitals = SurvivalState::new(0, MAX_HUNGER, FULL_AIR).unwrap();
        assert_eq!(vitals.heal(3), 3);
        assert_eq!(vitals.health(), 3);
        assert_eq!(vitals.heal(u16::MAX), MAX_HEALTH - 3);
        assert_eq!(vitals.health(), MAX_HEALTH);
        assert_eq!(vitals.heal(1), 0);
        assert_eq!(vitals.health(), MAX_HEALTH);
    }

    #[test]
    fn spend_and_restore_hunger_return_applied_amounts() {
        let mut vitals = SurvivalState::full();
        assert_eq!(vitals.spend_hunger(5), 5);
        assert_eq!(vitals.hunger(), 15);
        assert_eq!(vitals.spend_hunger(u16::MAX), 15);
        assert_eq!(vitals.hunger(), 0);
        assert_eq!(vitals.spend_hunger(1), 0);
        assert_eq!(vitals.restore_hunger(5), 5);
        assert_eq!(vitals.hunger(), 5);
        assert_eq!(vitals.restore_hunger(u16::MAX), MAX_HUNGER - 5);
        assert_eq!(vitals.hunger(), MAX_HUNGER);
        assert_eq!(vitals.restore_hunger(1), 0);
    }

    #[test]
    fn drain_air_and_refill_air_use_full_air_not_the_wire_ceiling() {
        let mut vitals = SurvivalState::full();
        assert_eq!(vitals.drain_air(50), 50);
        assert_eq!(vitals.air(), 250);
        assert_eq!(vitals.drain_air(u16::MAX), 250);
        assert_eq!(vitals.air(), 0);
        assert_eq!(vitals.drain_air(1), 0);
        vitals.refill_air();
        assert_eq!(vitals.air(), FULL_AIR);

        let mut over_full = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR).unwrap();
        over_full.refill_air();
        assert_eq!(over_full.air(), FULL_AIR);
        const { assert!(FULL_AIR < MAX_AIR) };
    }

    #[test]
    fn fall_damage_is_floor_above_three_blocks_and_clamped_to_max_health() {
        assert_eq!(fall_damage(0.0), 0);
        assert_eq!(fall_damage(3.0), 0);
        assert_eq!(fall_damage(3.9), 0);
        assert_eq!(fall_damage(4.0), 1);
        assert_eq!(fall_damage(4.9), 1);
        assert_eq!(fall_damage(5.0), 2);
        assert_eq!(fall_damage(22.0), 19);
        assert_eq!(fall_damage(23.0), MAX_HEALTH);
        assert_eq!(fall_damage(1000.0), MAX_HEALTH);
    }

    #[test]
    fn fall_damage_ignores_nan_and_negative_distances() {
        assert_eq!(fall_damage(f32::NAN), 0);
        assert_eq!(fall_damage(-0.5), 0);
        assert_eq!(fall_damage(-100.0), 0);
        assert_eq!(fall_damage(f32::NEG_INFINITY), 0);
    }

    #[test]
    fn creative_context_is_inert_even_when_submerged_and_falling() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 0, 0).unwrap();
        let ctx = SurvivalContext {
            mode: GameMode::Creative,
            head_submerged: true,
            fall_distance: 100.0,
        };
        let tick = runtime.tick(&mut vitals, ctx, 5.0);
        assert_eq!(tick, SurvivalTick::default());
        assert_eq!(vitals, SurvivalState::new(10, 0, 0).unwrap());
    }

    #[test]
    fn zero_health_is_inert() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(0, MAX_HUNGER, FULL_AIR).unwrap();
        let ctx = SurvivalContext {
            mode: GameMode::Survival,
            head_submerged: true,
            fall_distance: 100.0,
        };
        let tick = runtime.tick(&mut vitals, ctx, 5.0);
        assert_eq!(tick, SurvivalTick::default());
        assert_eq!(vitals, SurvivalState::new(0, MAX_HUNGER, FULL_AIR).unwrap());
    }

    #[test]
    fn fall_damage_is_applied_once_from_the_context() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let falling = SurvivalContext {
            mode: GameMode::Survival,
            head_submerged: false,
            fall_distance: 5.0,
        };
        let tick = runtime.tick(&mut vitals, falling, 0.016);
        assert_eq!(tick.damage, 2);
        assert_eq!(vitals.health(), MAX_HEALTH - 2);

        let landed = SurvivalContext {
            fall_distance: 0.0,
            ..falling
        };
        let tick = runtime.tick(&mut vitals, landed, 0.016);
        assert_eq!(tick.damage, 0);
        assert_eq!(vitals.health(), MAX_HEALTH - 2);
    }

    #[test]
    fn lethal_fall_resets_the_runtime_and_returns_applied_totals() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(1, MAX_HUNGER, FULL_AIR).unwrap();
        let ctx = SurvivalContext {
            mode: GameMode::Survival,
            head_submerged: false,
            fall_distance: 100.0,
        };
        let tick = runtime.tick(&mut vitals, ctx, 1.0);
        assert_eq!(tick.damage, 1);
        assert_eq!(vitals.health(), 0);

        // A dead state never accrues hunger, air, starvation, or regen.
        let tick = runtime.tick(&mut vitals, ctx, 1.0);
        assert_eq!(tick, SurvivalTick::default());
        assert_eq!(vitals.health(), 0);
        assert_eq!(vitals.hunger(), MAX_HUNGER);
    }

    #[test]
    fn air_drains_to_zero_after_fifteen_game_seconds() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };
        let total = run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 15);
        assert_eq!(vitals.air(), 0);
        assert_eq!(vitals.health(), MAX_HEALTH);
        assert_eq!(total.damage, 0);
    }

    #[test]
    fn ten_seconds_of_empty_air_kills_from_full_health() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };
        run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 15);
        assert_eq!(vitals.air(), 0);
        assert_eq!(vitals.health(), MAX_HEALTH);

        let total = run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 10);
        assert_eq!(total.damage, MAX_HEALTH);
        assert_eq!(vitals.health(), 0);
    }

    #[test]
    fn drowning_damage_only_starts_when_air_was_empty_at_tick_start() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, 1).unwrap();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };

        let tick = runtime.tick(&mut vitals, ctx, 1.0);
        assert_eq!(vitals.air(), 0);
        assert_eq!(tick.damage, 0);

        let tick = runtime.tick(&mut vitals, ctx, 1.0);
        assert_eq!(tick.damage, DROWN_DAMAGE);
        assert_eq!(vitals.health(), MAX_HEALTH - DROWN_DAMAGE);
    }

    #[test]
    fn surfacing_refills_air_and_resets_drown_timing() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, MAX_HUNGER, 0).unwrap();
        let submerged = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };
        let tick = runtime.tick(&mut vitals, submerged, 0.9);
        assert_eq!(vitals.air(), 0);
        assert_eq!(tick.damage, 0);

        let surfaced = SurvivalContext {
            head_submerged: false,
            ..submerged
        };
        let tick = runtime.tick(&mut vitals, surfaced, 0.1);
        assert_eq!(tick.damage, 0);
        assert_eq!(vitals.air(), FULL_AIR);
        assert_eq!(vitals.health(), 10);

        // Re-submerging starts the drown timer from zero, not from 0.9.
        let tick = runtime.tick(&mut vitals, submerged, 0.2);
        assert_eq!(tick.damage, 0);
        assert_eq!(vitals.air(), FULL_AIR - 4);
    }

    #[test]
    fn hunger_drains_one_point_per_thirty_game_seconds() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let ctx = survival_ctx();

        run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 29);
        assert_eq!(vitals.hunger(), MAX_HUNGER);
        run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 1);
        assert_eq!(vitals.hunger(), MAX_HUNGER - 1);
    }

    #[test]
    fn hunger_reaches_zero_after_six_hundred_game_seconds() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let ctx = survival_ctx();

        run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 599);
        assert_eq!(vitals.hunger(), 1);
        assert_eq!(vitals.health(), MAX_HEALTH);
        run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 1);
        assert_eq!(vitals.hunger(), 0);
        assert_eq!(vitals.health(), MAX_HEALTH);
    }

    #[test]
    fn starvation_kills_eighty_game_seconds_after_hunger_hits_zero() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, 0, FULL_AIR).unwrap();
        let ctx = survival_ctx();

        let early = run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 79);
        assert_eq!(early.damage, 19);
        assert_eq!(vitals.health(), 1);

        let lethal = run_ticks(&mut runtime, &mut vitals, ctx, 1.0, 1);
        assert_eq!(lethal.damage, 1);
        assert_eq!(vitals.health(), 0);
    }

    #[test]
    fn regen_needs_hunger_eighteen_and_costs_one_hunger_per_heal() {
        let ctx = survival_ctx();

        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 17, FULL_AIR).unwrap();
        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 0);
        assert_eq!(vitals.health(), 10);
        assert_eq!(vitals.hunger(), 17);

        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 18, FULL_AIR).unwrap();
        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 1);
        assert_eq!(vitals.health(), 11);
        assert_eq!(vitals.hunger(), 17);

        // One heal drops hunger below the threshold, so regeneration stops.
        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 0);
        assert_eq!(vitals.health(), 11);
        assert_eq!(vitals.hunger(), 17);
    }

    #[test]
    fn regen_is_suppressed_while_the_head_is_submerged() {
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, MAX_HUNGER, FULL_AIR).unwrap();

        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 0);
        assert_eq!(vitals.health(), 10);
        assert_eq!(vitals.hunger(), MAX_HUNGER);
    }

    #[test]
    fn regen_stops_at_full_health_and_never_overspends_hunger() {
        let ctx = survival_ctx();
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(18, 20, FULL_AIR).unwrap();

        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 1);
        assert_eq!(vitals.health(), 19);
        assert_eq!(vitals.hunger(), 19);

        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 1);
        assert_eq!(vitals.health(), MAX_HEALTH);
        assert_eq!(vitals.hunger(), 18);

        // Full health stops regeneration and never spends more hunger.
        let tick = runtime.tick(&mut vitals, ctx, REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 0);
        assert_eq!(vitals.health(), MAX_HEALTH);
        assert_eq!(vitals.hunger(), 18);
    }

    #[test]
    fn fractional_accumulators_are_exact_with_quarter_second_ticks() {
        let ctx = survival_ctx();

        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        run_ticks(&mut runtime, &mut vitals, ctx, 0.25, 120);
        assert_eq!(vitals.hunger(), MAX_HUNGER - 1);

        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let submerged = SurvivalContext {
            head_submerged: true,
            ..ctx
        };
        run_ticks(&mut runtime, &mut vitals, submerged, 0.25, 60);
        assert_eq!(vitals.air(), 0);

        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, MAX_HUNGER, FULL_AIR).unwrap();
        run_ticks(&mut runtime, &mut vitals, ctx, 0.25, 16);
        assert_eq!(vitals.health(), 11);
        assert_eq!(vitals.hunger(), MAX_HUNGER - 1);
    }

    #[test]
    fn huge_dt_is_clamped_to_the_maximum_game_tick() {
        let ctx = survival_ctx();

        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, 0, FULL_AIR).unwrap();
        let tick = runtime.tick(&mut vitals, ctx, 1000.0);
        // Ten clamped game seconds at one starvation point per four seconds.
        assert_eq!(tick.damage, 2);
        assert_eq!(vitals.health(), MAX_HEALTH - 2);

        let submerged = SurvivalContext {
            head_submerged: true,
            ..ctx
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let tick = runtime.tick(&mut vitals, submerged, 1000.0);
        // Ten clamped game seconds drain 200 air, not the whole lung.
        assert_eq!(tick.damage, 0);
        assert_eq!(vitals.air(), FULL_AIR - 200);
    }

    #[test]
    fn fall_only_tick_reports_the_fall_cause() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let ctx = SurvivalContext {
            fall_distance: 5.0,
            ..survival_ctx()
        };

        let tick = runtime.tick(&mut vitals, ctx, 0.016);
        assert_eq!(tick.damage, 2);
        assert_eq!(tick.cause, Some(DamageCause::Fall));
    }

    #[test]
    fn drown_only_tick_reports_the_drowning_cause() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, 0).unwrap();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };

        let tick = runtime.tick(&mut vitals, ctx, 1.0);
        assert_eq!(tick.damage, DROWN_DAMAGE);
        assert_eq!(tick.cause, Some(DamageCause::Drowning));
    }

    #[test]
    fn starve_only_tick_reports_the_starvation_cause() {
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, 0, FULL_AIR).unwrap();

        let tick = runtime.tick(&mut vitals, survival_ctx(), STARVE_INTERVAL_SECS);
        assert_eq!(tick.damage, STARVE_DAMAGE);
        assert_eq!(tick.cause, Some(DamageCause::Starvation));
    }

    #[test]
    fn undamaging_ticks_report_no_cause() {
        // Hunger drain only: three clamped 10 s ticks reach the 30 s hunger
        // interval, so hunger falls but no health is lost.
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        let mut damaged = 0u16;
        for _ in 0..3 {
            let tick = runtime.tick(&mut vitals, survival_ctx(), MAX_TICK_GAME_SECS);
            damaged = damaged.saturating_add(tick.damage);
            assert_eq!(tick.cause, None, "hunger drain is not a damage cause");
        }
        assert_eq!(damaged, 0);
        assert!(vitals.hunger() < MAX_HUNGER, "hunger still drains");

        // Regeneration only: health rises, no cause.
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, MAX_HUNGER, FULL_AIR).unwrap();
        let tick = runtime.tick(&mut vitals, survival_ctx(), REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 1);
        assert_eq!(tick.cause, None);
    }

    #[test]
    fn the_last_damaging_rule_owns_the_cause_on_a_multi_damage_tick() {
        // Fall damage (rule 1) and drowning damage (rule 2) land in the same
        // tick: the fixed rule order means drowning owns the tag.
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, 0).unwrap();
        let ctx = SurvivalContext {
            head_submerged: true,
            fall_distance: 5.0,
            ..survival_ctx()
        };

        let tick = runtime.tick(&mut vitals, ctx, DROWN_INTERVAL_SECS);
        assert_eq!(tick.damage, 2 + DROWN_DAMAGE);
        assert_eq!(tick.cause, Some(DamageCause::Drowning));
    }

    #[test]
    fn negative_and_nan_dt_are_clamped_to_zero() {
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();

        assert_eq!(
            runtime.tick(&mut vitals, ctx, -5.0),
            SurvivalTick::default()
        );
        assert_eq!(vitals, SurvivalState::full());
        assert_eq!(
            runtime.tick(&mut vitals, ctx, f32::NAN),
            SurvivalTick::default()
        );
        assert_eq!(vitals, SurvivalState::full());
    }

    #[test]
    fn survival_rules_default_matches_the_module_constants() {
        let rules = SurvivalRules::default();
        assert_eq!(rules.fall_safe_blocks, FALL_SAFE_BLOCKS);
        assert_eq!(rules.air_drain_per_sec, AIR_DRAIN_PER_SEC);
        assert_eq!(rules.drown_interval_secs, DROWN_INTERVAL_SECS);
        assert_eq!(rules.drown_damage, DROWN_DAMAGE);
        assert_eq!(rules.hunger_drain_secs, HUNGER_DRAIN_SECS);
        assert_eq!(rules.starve_interval_secs, STARVE_INTERVAL_SECS);
        assert_eq!(rules.starve_damage, STARVE_DAMAGE);
        assert_eq!(rules.regen_hunger_threshold, REGEN_HUNGER_THRESHOLD);
        assert_eq!(rules.regen_interval_secs, REGEN_INTERVAL_SECS);
        assert_eq!(rules.regen_hunger_cost, REGEN_HUNGER_COST);
    }

    #[test]
    fn default_rules_tick_with_reproduces_the_default_wrapper() {
        let ctx = SurvivalContext {
            head_submerged: true,
            fall_distance: 4.0,
            ..survival_ctx()
        };
        let mut plain_runtime = SurvivalRuntime::default();
        let mut plain_vitals = SurvivalState::new(MAX_HEALTH - 3, 17, 5).unwrap();
        let mut rules_runtime = SurvivalRuntime::default();
        let mut rules_vitals = plain_vitals;
        let rules = SurvivalRules::default();

        let plain = plain_runtime.tick(&mut plain_vitals, ctx, 3.0);
        let tuned = rules_runtime.tick_with(&mut rules_vitals, ctx, 3.0, &rules);
        assert_eq!(plain, tuned);
        assert_eq!(plain_vitals, rules_vitals);
    }

    #[test]
    fn tuned_hunger_drain_uses_the_live_interval() {
        let rules = SurvivalRules {
            hunger_drain_secs: 1.0,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();

        let tick = runtime.tick_with(&mut vitals, survival_ctx(), 1.0, &rules);
        assert_eq!(tick.damage, 0, "hunger drain is not damage");
        assert_eq!(vitals.hunger(), MAX_HUNGER - 1);

        // The default 30 s interval does not drain after one second.
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::full();
        runtime.tick(&mut vitals, survival_ctx(), 1.0);
        assert_eq!(vitals.hunger(), MAX_HUNGER);
    }

    #[test]
    fn tuned_fall_safe_blocks_moves_the_damage_threshold() {
        let rules = SurvivalRules {
            fall_safe_blocks: 0.0,
            ..SurvivalRules::default()
        };
        assert_eq!(fall_damage_with(0.9, &rules), 0);
        assert_eq!(fall_damage_with(4.0, &rules), 4);

        let rules = SurvivalRules {
            fall_safe_blocks: 10.0,
            ..SurvivalRules::default()
        };
        assert_eq!(fall_damage_with(9.0, &rules), 0);
        assert_eq!(fall_damage_with(12.0, &rules), 2);

        // The default wrapper keeps the 3-block threshold.
        assert_eq!(fall_damage(4.0), 1);
        assert_eq!(fall_damage(5.0), 2);
    }

    #[test]
    fn tuned_drown_interval_and_damage_apply_per_tick() {
        let rules = SurvivalRules {
            drown_interval_secs: 0.5,
            drown_damage: 3,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, 0).unwrap();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };

        let tick = runtime.tick_with(&mut vitals, ctx, 1.0, &rules);
        assert_eq!(tick.damage, 6, "two 0.5 s ticks at 3 damage each");
        assert_eq!(vitals.health(), MAX_HEALTH - 6);
        assert_eq!(tick.cause, Some(DamageCause::Drowning));
    }

    #[test]
    fn tuned_starve_interval_and_damage_apply_per_tick() {
        let rules = SurvivalRules {
            starve_interval_secs: 1.0,
            starve_damage: 3,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, 0, FULL_AIR).unwrap();

        let tick = runtime.tick_with(&mut vitals, survival_ctx(), 2.0, &rules);
        assert_eq!(tick.damage, 6, "two 1 s ticks at 3 damage each");
        assert_eq!(vitals.health(), MAX_HEALTH - 6);
        assert_eq!(tick.cause, Some(DamageCause::Starvation));
    }

    #[test]
    fn small_configured_damage_still_reaches_lethal_within_the_step_cap() {
        // interval 0.5 s with the 10 s per-tick clamp is 20 steps; damage 1
        // must apply all 20, i.e. the old default-derived cap of 10 would
        // wrongly leave the player alive.
        let rules = SurvivalRules {
            drown_interval_secs: 0.5,
            drown_damage: 1,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, 0).unwrap();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };
        let tick = runtime.tick_with(&mut vitals, ctx, MAX_TICK_GAME_SECS, &rules);
        assert_eq!(tick.damage, MAX_HEALTH);
        assert_eq!(vitals.health(), 0);

        // Starvation has the same live-cap behavior.
        let rules = SurvivalRules {
            starve_interval_secs: 0.5,
            starve_damage: 1,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(MAX_HEALTH, 0, FULL_AIR).unwrap();
        let tick = runtime.tick_with(&mut vitals, survival_ctx(), MAX_TICK_GAME_SECS, &rules);
        assert_eq!(tick.damage, MAX_HEALTH);
        assert_eq!(vitals.health(), 0);
    }

    #[test]
    fn malformed_zero_damage_and_zero_intervals_fail_closed() {
        let rules = SurvivalRules {
            drown_damage: 0,
            starve_damage: 0,
            drown_interval_secs: 0.0,
            starve_interval_secs: 0.0,
            hunger_drain_secs: 0.0,
            regen_interval_secs: 0.0,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 0, 0).unwrap();
        let ctx = SurvivalContext {
            head_submerged: true,
            ..survival_ctx()
        };

        let tick = runtime.tick_with(&mut vitals, ctx, MAX_TICK_GAME_SECS, &rules);
        assert_eq!(tick, SurvivalTick::default());
        assert_eq!(vitals, SurvivalState::new(10, 0, 0).unwrap());
    }

    #[test]
    fn tuned_regen_threshold_interval_and_cost_drive_healing() {
        let rules = SurvivalRules {
            regen_hunger_threshold: 5,
            regen_interval_secs: 1.0,
            regen_hunger_cost: 1,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 5, FULL_AIR).unwrap();

        let tick = runtime.tick_with(&mut vitals, survival_ctx(), 1.0, &rules);
        assert_eq!(tick.healed, 1);
        assert_eq!(vitals.health(), 11);
        assert_eq!(vitals.hunger(), 4, "cost is paid after the heal");

        // The default threshold of 18 refuses the same vitals.
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 5, FULL_AIR).unwrap();
        let tick = runtime.tick(&mut vitals, survival_ctx(), REGEN_INTERVAL_SECS);
        assert_eq!(tick.healed, 0);
        assert_eq!(vitals.health(), 10);
    }

    #[test]
    fn zero_regen_cost_is_free_regeneration() {
        let rules = SurvivalRules {
            regen_hunger_threshold: 0,
            regen_interval_secs: 1.0,
            regen_hunger_cost: 0,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 0, FULL_AIR).unwrap();

        let tick = runtime.tick_with(&mut vitals, survival_ctx(), 1.0, &rules);
        assert_eq!(tick.healed, 1);
        assert_eq!(vitals.health(), 11);
        assert_eq!(vitals.hunger(), 0);
    }

    #[test]
    fn positive_regen_cost_is_not_healed_when_unaffordable() {
        let rules = SurvivalRules {
            regen_hunger_threshold: 3,
            regen_interval_secs: 1.0,
            regen_hunger_cost: 5,
            ..SurvivalRules::default()
        };
        let mut runtime = SurvivalRuntime::default();
        let mut vitals = SurvivalState::new(10, 4, FULL_AIR).unwrap();

        let tick = runtime.tick_with(&mut vitals, survival_ctx(), 1.0, &rules);
        assert_eq!(tick.healed, 0);
        assert_eq!(vitals.health(), 10);
        assert_eq!(vitals.hunger(), 4, "an unaffordable heal spends nothing");
    }
}
