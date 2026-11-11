//! Scripted combat demo seam (`--demo-combat`).
//!
//! A pure move out of the bin crate (S09): the pinned quartet, the Ambient
//! population gate, the hostile death/respawn leg, the shot and skeleton bow
//! proofs, the creative inertness window and the save/restore tail.
//! `scripts/combat_smoke.sh` runs this seam through the real binary and pins
//! its `combat_demo:` line.
//!
//! Every item here is a byte-identical move from `src/main.rs`; only imports
//! and visibility changed.

use std::time::Instant;

use glam::{IVec3, Vec3};

use crate::commands::{mining_after_attack, mob_drop_notice, save_current_world};
use crate::player::bow::{BowState, BowTarget, BowTick};
use crate::player::combat::DamageCause;
use crate::player::inventory::{
    Inventory, Item, ItemId, Material, ToolKind, ToolTier, INVENTORY_SLOTS,
};
use crate::player::melee::{
    attack_damage, MeleeState, MeleeTarget, MeleeTick, MELEE_REACH, SWORD_DAMAGE_WOOD,
};
use crate::player::mining::MiningTarget;
use crate::player::raycast::raycast_voxels;
use crate::player::survival::GameMode;
use crate::render::mob::{build_mob_vertices, MobAnim};
use crate::ui::screens::death_cause_line;
use crate::world::arrow::{Arrow, ArrowOwner, ArrowState, ARROW_DAMAGE_SKELETON};
use crate::world::block::BlockId;
use crate::world::chunk::CHUNK_SIZE;
use crate::world::daycycle::GameClock;
use crate::world::mob::{Mob, MobAiState, MobHit, MobKind, MobShot};
use crate::world::mob_ai::MobAttack;
use crate::world::mob_loot::{drop_roll, mob_drops, MobDropItem, MobDropReport, MobKillCause};
use crate::world::persistence::load_world_from_path;
use crate::world::session::mob_drop_items;
use crate::world::spawning::{
    SpawnReport, SpawnWorld, DESPAWN_RADIUS, MAX_MOBS_TOTAL, PASSIVE_SURFACE_LIGHT, SAFE_RADIUS,
};
use crate::world::terrain::terrain_height_at_seeded;
use crate::WorldSession;

// ---------------------------------------------------------------------------
// Scripted combat demo (`--demo-combat`, M009/S01-T05)
// ---------------------------------------------------------------------------

/// Wall-clock safety net for the scripted combat loop. The Walk stage is also
/// bounded by its own 600-update cap, so this only catches a run whose world
/// never streams in (the Settle wait).
pub const COMBAT_DEMO_DEADLINE_SECS: u64 = 120;

/// Minimum Walk observations before success may latch (120 Playing frames =
/// 2 s at 60 Hz). Besides proving sustained walking rather than a single
/// lucky step, this gives the shutdown `frame_budget:` digest a meaningful
/// sample set: with only a handful of samples p95 collapses onto the maximum
/// sample, so one startup-heavy frame would dominate the printed p95.
pub const COMBAT_WALK_OBSERVATIONS: u32 = 120;
/// Hard cap on Walk observations before the stage aborts with a digest.
pub const COMBAT_WALK_ABORT_UPDATES: u32 = 600;

/// The pinned clock for a fresh `--demo-combat` world. 12_000 ticks is late
/// afternoon (just before dusk), so the demo proves the box-model path at a
/// deterministic, mob-visible time of day. Never derived from the wall clock.
///
/// **12_000 is still DAY.** [`daycycle::is_night`] opens at
/// [`daycycle::STAR_DUSK_CLEAR_TICK`] (12_306), the first tick whose sun
/// brightness is exactly zero, so a dusk demo cannot spawn a hostile on the lit
/// surface. That is exactly why the Ambient stage pins its own real night tick
/// ([`COMBAT_NIGHT_TICK`]) instead of reusing this one.
pub const COMBAT_DUSK_TICK: u32 = 12_000;

/// The pinned night clock for the Ambient stage: 18_000 ticks is midnight.
///
/// The sun is exactly below the horizon there, so [`daycycle::is_night`] is true
/// while the surface sky light is still [`PASSIVE_SURFACE_LIGHT`] (sky light is
/// time-invariant in this engine): the surface-hostile night rule is what the
/// stage proves, not the cave-light rule.
pub const COMBAT_NIGHT_TICK: u32 = 18_000;

/// The pinned daytime clock for the Ambient burn phase: 06_000 ticks is
/// mid-morning, well clear of both night edges (12_306 and 23_694), so
/// [`daycycle::is_day`] is true and surface hostiles must burn.
pub const COMBAT_DAY_TICK: u32 = 6_000;

/// Hard cap on the `Hostile` stage's fixed-step observations.
///
/// 400 x 0.1 s = 40 s of game time. The stage needs 7 landed zombie hits at a
/// 1.0 s cadence (six seconds) plus the walk-in and knockback settle, so this
/// leaves a wide margin while staying inside the demo's own 120 s wall-clock
/// deadline. It is bounded rather than "until dead" so a steering or cadence
/// regression latches a self-describing abort instead of hanging the run.
pub const COMBAT_HOSTILE_ABORT_TICKS: u32 = 400;

/// Hard cap on each Skeleton stage phase's fixed-step observations.
///
/// 400 x 0.1 s = 40 s of game time. The fire phase needs one skeleton shot
/// (the brain fires on its first step) plus ~0.4 s of arrow flight; the
/// death-cause phase needs the next 2.0 s shot plus flight; the kill proof
/// needs one arrow flight. Bounded rather than "until dead" so a regression in
/// the ranged brain or the projectile sweep latches a self-describing abort
/// instead of hanging the demo's 120 s deadline.
pub const COMBAT_SKELETON_ABORT_TICKS: u32 = 400;

/// Fixed-step observations the Creative stage's inertness window runs.
///
/// 40 x 0.1 s = 4 s of game time with the hostile gate OPEN and the player's
/// effective mode Creative. The stage requires zero decided attacks, zero
/// decided shots, zero applied player damage, and no mob ever in `Chase` for
/// the whole window, so a leak in either Creative authority site fails instead
/// of passing on one quiet frame.
pub const COMBAT_CREATIVE_INERT_TICKS: u32 = 40;

/// Hard cap on each Creative stage fixed-step observation loop (the inertness
/// window and the arrow's flight).
///
/// 40 x 0.1 s = 4 s of game time. The charge-0 arrow crosses the two blocks to
/// a pinned Zombie inside the first 0.1 s tick, so this is only the bound that
/// turns a projectile-sweep regression into a self-describing abort instead of
/// a hung 120 s demo deadline.
pub const COMBAT_CREATIVE_ABORT_TICKS: u32 = 40;

/// Minimum Ambient spawn-phase observations before the stage may advance.
///
/// 60 observations x 0.1 s of game time = 6 s, i.e. 24 fixed spawn attempts at
/// [`crate::world::spawning::SPAWN_ATTEMPT_SECS`]; measured night runs place
/// at least one hostile well before that, so the stage proves sustained
/// population rather than one lucky attempt.
pub const COMBAT_AMBIENT_MIN_UPDATES: u32 = 60;
/// Hard cap on Ambient spawn-phase observations before the stage aborts with a
/// self-describing digest.
///
/// 3000 x 0.1 s = 300 s of game time (1200 attempts), but the real bound is
/// wall time: with default settings the present mode is uncapped (~1000 fps
/// here), so 600 observations elapsed in under a second of wall time before the
/// streamer had lit the 24..48 block spawn ring — a measured zero-spawn abort.
/// 3000 observations gives that warm-up room (a few seconds uncapped, ~50 s at
/// 60 Hz) while staying inside the demo's own 120 s wall-clock deadline.
pub const COMBAT_AMBIENT_ABORT_UPDATES: u32 = 3000;
/// Ticks of `tick_mobs(0.05, 1.0)` the Ambient despawn phase runs after moving
/// one record past [`DESPAWN_RADIUS`]. 22 x 0.05 s = 1.1 s, just past the 1.0 s
/// despawn scan cadence, so the mark and the single reap site both run.
pub const COMBAT_AMBIENT_DESPAWN_TICKS: u32 = 22;
/// Hard cap on the Ambient burn phase's ticks. 400 x 0.05 s = 20 s of game
/// time = 40 burn intervals of 0.5 s, twice the 20 intervals a full-health
/// hostile needs, so a stuck burn is a real failure rather than a slow clock.
pub const COMBAT_AMBIENT_BURN_TICKS: u32 = 400;

/// Pinned damaged health for the demo quartet, in [`MobKind::ALL`] order.
/// Every value is strictly below its kind's [`MobKind::max_health`] (Pig/Cow
/// 10, Zombie/Skeleton 20), so the saved tail proves damaged health survives a
/// relaunch instead of silently restoring a full-health default.
pub const COMBAT_PINNED_HEALTH: [u16; 4] = [4, 7, 11, 14];

/// Pinned non-round facing for the demo quartet, in [`MobKind::ALL`] order.
/// Non-round values make a yaw round-trip failure visible instead of hiding
/// behind a default facing.
pub const COMBAT_PINNED_YAW: [f32; 4] = [0.25, 0.75, -1.25, 2.5];

/// Measured byte length of the fresh `--demo-combat` save (seed 12345), after
/// the Drops stage earns the wood sword plus the pig/cow drops and the Shot
/// stage earns the wood bow and its arrows, and the Save stage writes the two
/// surviving hostile records.
///
/// Measured by running the fresh leg (`combat_demo_fresh_leg_writes_and_verifies_a_v9_save`)
/// and reading the file back, never computed by hand: every earned stack changes
/// the serialized inventory size. The same run measures the mob-count offset.
/// Test-only pins: nothing in production reads the file length.
pub const COMBAT_SAVE_BYTES: usize = 317;
/// Bytes of the v8 pack-identity + digest tail (12) plus the v9 config tail
/// (10) after the mob records.
pub const COMBAT_SAVE_TAIL_BYTES: usize = 22;
/// Measured offset of the v9 mob-count word in that save: the 19-byte Zombie
/// record starts 4 bytes later and the Skeleton record follows it, so the tail
/// ends [`COMBAT_SAVE_TAIL_BYTES`] bytes before [`COMBAT_SAVE_BYTES`].
pub const COMBAT_SAVE_MOB_COUNT_OFFSET: usize = 253;

/// The scripted combat loop's fixed stage order. Each stage is fed once per
/// `about_to_wait` call; Settle returns `false` until the fresh Survival
/// spawn chunks are resident.
///
/// Fight runs in the SAME step that Walk reports success so no redraw can
/// slip between the walk verdict and the scripted hit sequence. Drops runs in
/// its own one-call step: it re-pins the mob it is about to kill, so a redraw
/// between Fight and Drops cannot perturb a single observed number. It then
/// latches its verdict and advances to Save, which re-pins the two surviving
/// hostile records, so neither the fight nor the kills can leak into the save.
/// The Restore stage is the loaded-save leg's first and only stage.
///
/// The Ambient stage (M009/S05-T05) sits between Settle and Spawn: it proves
/// the real population layer through the same session seams the shipped game
/// uses (night surface spawning, the safe radius, the global cap, the despawn
/// radius, and daylight burning), then restores the pinned dusk clock and
/// closes the ambient gate so the pinned quartet stages below are never
/// perturbed.
///
/// The Hostile stage (M009/S06-T06) sits between Drops and Shot: it proves the
/// hostile steering brain and the zombie melee seam on the two surviving
/// records (line-of-sight aggro, a chase, 3-damage hits with knockback, a
/// mob-caused death, and a respawn that keeps the inventory), then restores the
/// pinned player pose and closes the hostile gate so the Shot proof and the
/// Save witness start from a known state.
///
/// The Shot stage (M009/S07-T06) sits between Hostile and Save: it proves the
/// bow charge/cancel curve through [`BowState`], the `fire_bow` ammo/wear seam,
/// a full-charge arrow hitting a body with knockback, and a straight-down shot
/// embedding then expiring. It earns the real wood bow and arrows the save
/// witness now carries.
///
/// The Skeleton stage (M009/S08-T04) sits between Shot and Creative: it proves
/// the stand-off ranged brain on one synthetic skeleton pinned at a spot whose
/// [`drop_roll`] lands the rare bow, the skeleton arrow's 2-damage player hit
/// and `SkeletonArrow` death cause, the respawn, and the player-attributed
/// bone/arrow/bow drops. It removes its synthetic victim before Creative, so
/// the two-record witness and every earlier digest stay byte-identical.
///
/// The Creative stage (M009/S09-T05) sits between Skeleton and Save: it proves
/// the effective-mode mob rule from a Survival save whose live player switches
/// to Creative rules — no target selection, no melee, no arrows, no damage —
/// and then that one melee swing and one charge-0 arrow each one-shot a
/// full-health Zombie with the pinned drop table intact. It consumes exactly
/// the two rotten-flesh units its kills earned, so the measured 317-byte v9
/// witness and every earlier digest stay byte-identical. Full order: `Settle -> Ambient ->
/// Spawn -> Walk -> Fight -> Drops -> Hostile -> Shot -> Skeleton -> Creative ->
/// Save -> Restore -> Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatStage {
    Settle,
    Ambient,
    Spawn,
    Walk,
    Fight,
    Drops,
    Hostile,
    Shot,
    Skeleton,
    Creative,
    Save,
    Restore,
    Done,
}

/// The Ambient stage's three phases, in order.
///
/// `Spawn` pins midnight and lets the production population layer fill the
/// night surface. `Despawn` moves one record past the despawn radius and proves
/// the single reap site removes it. `Burn` pins morning and proves surface
/// hostiles burn to death. Each phase latches its own counter so the printed
/// digest can be re-measured against a fixed seed and clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbientPhase {
    Spawn,
    Despawn,
    Burn,
}

/// The `--demo-combat` seam's whole state: the pinned seed, whether the world
/// is a fresh launch (the only branch allowed to be seeded with demo mobs),
/// the stage cursor, the one latched digest, and the observed numbers the
/// success digest reports.
#[derive(Debug)]
pub struct CombatDemo {
    pub seed: u32,
    /// True only for the fresh Survival startup branch. A loaded save is
    /// never re-seeded: it starts at the Restore stage and proves the saved
    /// quartet instead.
    pub fresh: bool,
    pub stage: CombatStage,
    /// The digest latched when the run ends (success or abort). Printed by the
    /// demo-shutdown block with the other bounded-demo diagnostics.
    pub digest: Option<String>,
    /// Whether the latched outcome was an abort. `main` turns this into a
    /// non-zero process status.
    pub failed: bool,
    /// Wall-clock safety net, armed only for a direct-to-gameplay launch.
    pub deadline: Option<Instant>,
    /// The feet position recorded at spawn, in mob-list order, for the Walk
    /// stage's movement check.
    pub spawn_positions: [Vec3; 4],
    /// Mobs that had both a walking phase and a recorded displacement at the
    /// last Walk observation (reported in the success digest).
    pub walked: u32,
    /// Walk observations so far; the stage aborts after 600.
    pub walk_updates: u32,
    /// Mob records the fresh Save stage wrote (reported in the success
    /// digest; `4` on a successful save leg).
    pub saved: u32,
    /// Mob records the fresh Save stage read back and verified.
    pub records: u32,
    /// Melee hits the Fight stage landed through `WorldSession::attack_mob`
    /// (`2` on a successful fresh leg: 10 -> 6 -> 2 on the Pig).
    pub melee_hits: u32,
    /// Damage per landed hit, reported by the Fight stage's sword tier.
    pub melee_damage: u16,
    /// Swings the i-frame window refused (`1` on a successful fresh leg).
    pub melee_blocked: u32,
    /// Mob records the Drops stage killed (`2` on a successful fresh leg).
    pub killed: u32,
    /// Drop units the Drops stage delivered into the real 36-slot inventory.
    pub drops: u32,
    /// Drop units a full inventory refused (zero on the pinned demo path).
    pub drops_lost: u32,
    /// Mob records the Walk verdict observed, frozen at the moment the walk
    /// succeeded: Drops removes two records and Save re-pins the rest, so a
    /// live count read at digest time would no longer describe the walk.
    pub walk_mobs: u32,
    /// Resident mobs at the walk verdict, frozen with [`CombatDemo::walk_mobs`].
    pub walk_resident: u32,
    /// Rendered vertices at the walk verdict, frozen with
    /// [`CombatDemo::walk_mobs`].
    pub walk_verts: usize,
    /// Cursor inside the Ambient stage (only meaningful while
    /// `stage == CombatStage::Ambient`).
    pub ambient_phase: AmbientPhase,
    /// Ambient spawn-phase observations so far (see
    /// [`COMBAT_AMBIENT_MIN_UPDATES`] / [`COMBAT_AMBIENT_ABORT_UPDATES`]).
    pub ambient_updates: u32,
    /// Largest live mob-list length seen during the Ambient spawn phase. The
    /// global cap makes this `<= MAX_MOBS_TOTAL` by construction.
    pub ambient_peak: usize,
    /// Records observed inside the safe radius during the Ambient spawn phase.
    /// Any value other than zero aborts the stage: the population layer must
    /// never place a mob near the player or the world spawn.
    pub ambient_safe: u32,
    /// The spawn report snapshot latched when the Ambient spawn phase ended.
    pub spawn_phase_report: SpawnReport,
    /// Records the Ambient despawn phase proved removed (`1` on success).
    pub spawn_despawned: u32,
    /// Hostiles the Ambient burn phase proved killed by daylight.
    pub spawn_burned: u32,
    /// The latched `combat_spawn:` line, printed immediately after the pinned
    /// `combat_demo:` line on the demo-shutdown path.
    pub spawn_digest: Option<String>,
    /// Zombie melee hits the Hostile stage accepted (`7` on success: 3 damage
    /// each, the last saturating 2 -> 0).
    pub hostile_attacks: u32,
    /// Applied damage across those hits (`20` = the player's whole health bar).
    pub hostile_damage: u16,
    /// Whether the last damage event before the death was tagged `Zombie`.
    pub hostile_cause: bool,
    /// Whether the stage respawned the player at the deterministic spawn.
    pub hostile_respawn: bool,
    /// Whether the inventory total survived the respawn unchanged.
    pub hostile_inventory_kept: bool,
    /// Whether at least one accepted hit left a live knockback impulse.
    pub hostile_knockback: bool,
    /// The latched `combat_hostile:` line, printed immediately after the
    /// pinned `combat_spawn:` line on the demo-shutdown path.
    pub hostile_digest: Option<String>,
    /// Whether the Shot stage reached a full draw before releasing (`true` on
    /// success).
    pub shot_charged: bool,
    /// Whether a sub-threshold release canceled with no ammo or wear (`true`).
    pub shot_canceled: bool,
    /// Fully charged shots the Shot stage fired through
    /// [`WorldSession::fire_bow`] (`1` on success: the embed probe below is
    /// proven by `embed`/`expired` instead).
    pub shot_fired: u32,
    /// Mob hits the charged shot's first tick reported (`1`).
    pub shot_mob_hits: u32,
    /// Damage that arrow applied to the Zombie (`4` at full charge).
    pub shot_damage: u16,
    /// Whether the mob hit left a knockback impulse pointing away from the
    /// shooter.
    pub shot_knockback: bool,
    /// Whether the straight-down shot was observed embedded after a block hit.
    pub shot_embed: bool,
    /// Whether the embedded arrow later expired and left the list.
    pub shot_expired: bool,
    /// Arrow units after both shots (`2`: four granted minus two fired).
    pub shot_ammo: u32,
    /// Bow durability after both shots (`Some(57)`: 59 minus one wear per
    /// shot).
    pub shot_bow: Option<u16>,
    /// The latched `combat_shot:` line, printed immediately after the
    /// `combat_hostile:` line on the demo-shutdown path.
    pub shot_digest: Option<String>,
    /// Skeleton shots the Skeleton stage observed the brain decide (the digest's
    /// `fired`; `1` on the pinned leg because the stage stops at the first hit).
    pub skeleton_fired: u32,
    /// Player hits the skeleton arrows landed (`1`: the stage stops at the
    /// first accepted hit).
    pub skeleton_player_hits: u32,
    /// Applied skeleton-arrow damage across those hits (`2`).
    pub skeleton_damage: u16,
    /// Whether the first observed shot's direction pointed above the straight
    /// line to the player eye (the gravity-compensation proof, `true`).
    pub skeleton_arc: bool,
    /// Mob records the Skeleton stage's player-fired arrow killed (`1`).
    pub skeleton_killed: u32,
    /// Bone units the kill delivered (`1`).
    pub skeleton_bones: u32,
    /// Arrow units the kill delivered (`2`).
    pub skeleton_arrows: u32,
    /// Whether the kill delivered the rare full-durability bow (`true` on the
    /// pinned leg: the spot search only accepts a cell whose roll lands it).
    pub skeleton_bow: bool,
    /// Whether the respawn returned the player to the deterministic spawn.
    pub skeleton_respawn: bool,
    /// The latched `combat_skeleton:` line, printed immediately after the
    /// `combat_shot:` line on the demo-shutdown path.
    pub skeleton_digest: Option<String>,
    /// Whether the Creative inertness window observed no hostile decision at all
    /// (`true` on success: zero attacks, zero shots, zero damage, no chase).
    pub creative_inert: bool,
    /// Melee attacks the Creative inertness window observed (`0` on success).
    pub creative_attacks: u32,
    /// Skeleton shots the Creative inertness window observed (`0` on success).
    pub creative_shots: u32,
    /// Player damage applied by arrows during the Creative window (`0`).
    pub creative_damage: u16,
    /// Damage the Creative melee swing applied (`20`: exactly one shot).
    pub creative_melee_damage: u16,
    /// Damage the Creative charge-0 arrow applied (`20`: exactly one shot).
    pub creative_arrow_damage: u16,
    /// Mob kills the Creative stage proved (`2`: one melee, one arrow).
    pub creative_kills: u32,
    /// Drop units those two kills delivered (`2` rotten flesh).
    pub creative_drops: u32,
    /// Drop units a full inventory refused (`0`).
    pub creative_drops_lost: u32,
    /// Whether the inventory total was conserved after both earned drops were
    /// consumed again (`true` on success).
    pub creative_inventory_kept: bool,
    /// The latched `combat_creative:` line, printed immediately after the
    /// `combat_skeleton:` line on the demo-shutdown path.
    pub creative_digest: Option<String>,
}

impl CombatDemo {
    pub fn new(seed: u32, fresh: bool, deadline: Option<Instant>) -> Self {
        Self {
            seed,
            fresh,
            stage: if fresh {
                CombatStage::Settle
            } else {
                CombatStage::Restore
            },
            digest: None,
            failed: false,
            deadline,
            spawn_positions: [Vec3::ZERO; 4],
            walked: 0,
            walk_updates: 0,
            saved: 0,
            records: 0,
            melee_hits: 0,
            melee_damage: 0,
            melee_blocked: 0,
            killed: 0,
            drops: 0,
            drops_lost: 0,
            walk_mobs: 0,
            walk_resident: 0,
            walk_verts: 0,
            ambient_phase: AmbientPhase::Spawn,
            ambient_updates: 0,
            ambient_peak: 0,
            ambient_safe: 0,
            spawn_phase_report: SpawnReport::default(),
            spawn_despawned: 0,
            spawn_burned: 0,
            spawn_digest: None,
            hostile_attacks: 0,
            hostile_damage: 0,
            hostile_cause: false,
            hostile_respawn: false,
            hostile_inventory_kept: false,
            hostile_knockback: false,
            hostile_digest: None,
            shot_charged: false,
            shot_canceled: false,
            shot_fired: 0,
            shot_mob_hits: 0,
            shot_damage: 0,
            shot_knockback: false,
            shot_embed: false,
            shot_expired: false,
            shot_ammo: 0,
            shot_bow: None,
            shot_digest: None,
            skeleton_fired: 0,
            skeleton_player_hits: 0,
            skeleton_damage: 0,
            skeleton_arc: false,
            skeleton_killed: 0,
            skeleton_bones: 0,
            skeleton_arrows: 0,
            skeleton_bow: false,
            skeleton_respawn: false,
            skeleton_digest: None,
            creative_inert: false,
            creative_attacks: 0,
            creative_shots: 0,
            creative_damage: 0,
            creative_melee_damage: 0,
            creative_arrow_damage: 0,
            creative_kills: 0,
            creative_drops: 0,
            creative_drops_lost: 0,
            creative_inventory_kept: false,
            creative_digest: None,
        }
    }

    pub fn finished(&self) -> bool {
        self.digest.is_some()
    }

    /// Whether this demo currently owns mob time.
    ///
    /// The Ambient stage drives its own fixed `tick_mobs(0.05)` calls and pins
    /// exact population counters. The real Playing frame would otherwise add a
    /// profile-dependent number of extra ticks (and therefore spawn attempts)
    /// between observations, so the frame loop must stand down for this stage.
    /// Every other stage keeps the production tick: the Walk stage's proof is
    /// that the real frame loop moves the quartet.
    pub fn owns_mob_ticks(&self) -> bool {
        !self.finished() && self.stage == CombatStage::Ambient
    }

    /// Latch a self-describing abort. Idempotent: the first outcome wins.
    pub fn abort(&mut self, stage: &'static str, observed: &str) {
        if self.finished() {
            return;
        }
        self.digest = Some(format!(
            "combat_demo: seed={} stage={} observed={} ok=false",
            self.seed, stage, observed
        ));
        self.failed = true;
    }

    /// The exact `combat_spawn:` digest printed once on the demo-shutdown path
    /// (immediately after the pinned `combat_demo:` line).
    ///
    /// Only latched after all three Ambient phases pass, so the literal
    /// `ok=true` is the latching contract, not a computed value.
    pub fn spawn_digest(&self) -> String {
        format!(
            "combat_spawn: seed={} spawned={} despawned={} burned={} cap_blocked={} safe={} peak={} ok=true",
            self.seed,
            self.spawn_phase_report.spawned,
            self.spawn_despawned,
            self.spawn_burned,
            self.spawn_phase_report.cap_blocked,
            self.ambient_safe,
            self.ambient_peak
        )
    }

    /// The exact `combat_hostile:` digest printed once on the demo-shutdown
    /// path (immediately after the pinned `combat_spawn:` line).
    ///
    /// Only latched after the Hostile stage proved aggro, the 3-damage
    /// i-frame-gated hits, knockback, the mob-caused death, the respawn, and the
    /// kept inventory, so the trailing literals are the latching contract: the
    /// harness pins the literal shape (`cause=zombie respawn=spawn
    /// knockback=true inventory=kept ok=true`) rather than four computed
    /// words. `attacks` and `damage` are the measured values.
    pub fn hostile_digest(&self) -> String {
        format!(
            "combat_hostile: seed={} attacks={} damage={} cause=zombie respawn=spawn knockback=true inventory=kept ok=true",
            self.seed, self.hostile_attacks, self.hostile_damage
        )
    }

    /// The exact `combat_shot:` digest printed once on the demo-shutdown path
    /// (immediately after the `combat_hostile:` line).
    ///
    /// Only latched after the Shot stage proved the sub-threshold cancel, the
    /// full draw, the fired arrow, the mob hit with away knockback, and the
    /// block embed that expired, so the trailing literal `ok=true` is the
    /// latching contract. `fired` counts the fully charged shot; the
    /// straight-down embed probe is reported by `embed`/`expired`.
    pub fn shot_digest(&self) -> String {
        format!(
            "combat_shot: seed={} charged={} canceled={} fired={} mob_hits={} damage={} knockback={} embed={} expired={} ammo={} bow={} ok=true",
            self.seed,
            self.shot_charged,
            self.shot_canceled,
            self.shot_fired,
            self.shot_mob_hits,
            self.shot_damage,
            self.shot_knockback,
            self.shot_embed,
            self.shot_expired,
            self.shot_ammo,
            self.shot_bow.unwrap_or(0)
        )
    }

    /// The exact `combat_skeleton:` digest printed once on the demo-shutdown
    /// path (immediately after the `combat_shot:` line).
    ///
    /// Only latched after the Skeleton stage proved the gravity-compensated
    /// shot, the 2-damage player hit with the `SkeletonArrow` cause, the
    /// `Skeleton arrow` death-screen line, the respawn, and the
    /// player-attributed bone/arrow/bow drops, so the trailing literals are the
    /// latching contract. `fired`, `player_hits`, `damage`, and `bow` are the
    /// measured values the stage asserts before latching.
    pub fn skeleton_digest(&self) -> String {
        format!(
            "combat_skeleton: seed={} fired={} player_hits={} damage={} cause=skeleton_arrow arc={} killed={} bones={} arrows={} bow={} respawn={} ok=true",
            self.seed,
            self.skeleton_fired,
            self.skeleton_player_hits,
            self.skeleton_damage,
            self.skeleton_arc,
            self.skeleton_killed,
            self.skeleton_bones,
            self.skeleton_arrows,
            self.skeleton_bow,
            if self.skeleton_respawn { "spawn" } else { "none" }
        )
    }

    /// The exact `combat_creative:` digest printed once on the demo-shutdown
    /// path (immediately after the `combat_skeleton:` line).
    ///
    /// Only latched after the Creative stage proved the inert authority window,
    /// both direct authority seams, the 20-damage melee and arrow one-shots
    /// with their drops, and the conserved inventory, so the trailing
    /// `inventory=kept ok=true` literals are the latching contract.
    /// `attacks`/`shots`/`damage` are the measured zeroes; both damages and the
    /// kill/drop counts are the measured values the stage asserts before it
    /// prints them.
    pub fn creative_digest(&self) -> String {
        format!(
            "combat_creative: seed={} inert={} attacks={} shots={} damage={} melee_damage={} arrow_damage={} kills={} drops={} drops_lost={} inventory=kept ok=true",
            self.seed,
            self.creative_inert,
            self.creative_attacks,
            self.creative_shots,
            self.creative_damage,
            self.creative_melee_damage,
            self.creative_arrow_damage,
            self.creative_kills,
            self.creative_drops,
            self.creative_drops_lost
        )
    }

    /// The exact success digest printed once on the demo-shutdown path.
    ///
    /// Every walk number is the frozen value latched when the Walk verdict
    /// passed: the Drops stage removes two records and the Save stage re-pins
    /// the survivors, so reading the live session here would report counts
    /// that describe the end of the run instead of the walk that was proven.
    pub fn success_digest(&self) -> String {
        format!(
            "combat_demo: seed={} mobs={} resident={} walked={} verts={} melee_hits={} melee_damage={} melee_blocked={} killed={} drops={} drops_lost={} saved={} records={} ok=true",
            self.seed,
            self.walk_mobs,
            self.walk_resident,
            self.walked,
            self.walk_verts,
            self.melee_hits,
            self.melee_damage,
            self.melee_blocked,
            self.killed,
            self.drops,
            self.drops_lost,
            self.saved,
            self.records
        )
    }
}

/// The demo quartet's pinned feet positions and facings, derived from one
/// player pose and the world seed.
///
/// Spawn, Save, and Restore all call this single helper, so the three stages
/// cannot drift apart: a save leg that recomputed the pose differently from
/// the spawn leg (or a restore leg that recomputed it differently again) would
/// fail its own exact comparison. The constants are the pre-S02 spawn math
/// unchanged: three and a half blocks ahead along the yaw forward axis, the
/// quartet fanned across the right axis, feet on the seeded terrain surface,
/// each mob turned toward the player (`atan2(dx, dz)` because mob yaw 0 faces
/// +Z).
pub fn combat_quartet(player_position: Vec3, yaw: f32, seed: u32) -> ([Vec3; 4], [f32; 4]) {
    let forward = Vec3::new(yaw.cos(), 0.0, yaw.sin());
    let right = Vec3::new(-yaw.sin(), 0.0, yaw.cos());
    let offsets = [-1.8_f32, -0.6, 0.6, 1.8];
    let mut positions = [Vec3::ZERO; 4];
    let mut yaws = [0.0_f32; 4];
    for (index, offset) in offsets.iter().enumerate() {
        let x = player_position.x + forward.x * 3.5 + right.x * offset;
        let z = player_position.z + forward.z * 3.5 + right.z * offset;
        let y = terrain_height_at_seeded(x.floor() as i32, z.floor() as i32, seed) as f32 + 1.0;
        positions[index] = Vec3::new(x, y, z);
        // Mob yaw 0 faces +Z, so `atan2(dx, dz)` turns the quartet toward the
        // player (the render path proves the facing).
        yaws[index] = (player_position.x - x).atan2(player_position.z - z);
    }
    (positions, yaws)
}

/// Drive one Playing frame of the scripted combat loop. Returns true when this
/// call ended the run (success or abort).
///
/// The demo never ticks the mob runtime itself: the real Playing tick owns
/// `session.tick_mobs`, exactly as it owns `tick_survival`/`tick_crops`. This
/// seam only spawns the pinned quartet, observes them, and latches one digest.
pub fn combat_demo_step(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    if demo.finished() {
        return true;
    }
    if let Some(deadline) = demo.deadline {
        if Instant::now() >= deadline {
            demo.abort(
                "deadline",
                &format!("bounded combat run exceeded {COMBAT_DEMO_DEADLINE_SECS}s"),
            );
            return true;
        }
    }
    match demo.stage {
        CombatStage::Settle => {
            if !session.settle_spawn() {
                return false;
            }
            // The Ambient stage owns the population gate from here: it opens
            // it for its own night spawn / daylight burn proof and closes it
            // again before handing over. Closing it now means a Settle frame
            // that races the gate can never create a record the pinned quartet
            // stages would have to reason about.
            session.set_ambient_spawning(false);
            demo.stage = CombatStage::Ambient;
            false
        }
        CombatStage::Ambient => combat_ambient_stage(demo, session),
        CombatStage::Spawn => {
            // A demo run owns its mob list: clearing first makes a re-armed
            // seam deterministic instead of stacking two quartets.
            session.mobs_mut().clear();
            let (positions, yaws) =
                combat_quartet(session.player.position, session.player.yaw, demo.seed);
            {
                let mobs = session.mobs_mut();
                for (index, kind) in MobKind::ALL.iter().copied().enumerate() {
                    mobs.push(Mob::new(kind, positions[index], yaws[index]));
                }
            }
            let count = session.mobs().len();
            if count != 4 {
                demo.abort("spawn", &format!("mobs={count}"));
                return true;
            }
            demo.spawn_positions = positions;
            demo.stage = CombatStage::Walk;
            false
        }
        CombatStage::Walk => {
            // The observation count is bumped BEFORE the success check so the
            // demo can require a minimum walking window (see
            // [`COMBAT_WALK_OBSERVATIONS`]).
            demo.walk_updates += 1;
            let mut resident = 0usize;
            let mut walked = 0u32;
            for (index, mob) in session.mobs().iter().enumerate() {
                if !session.mob_is_resident(mob) {
                    continue;
                }
                resident += 1;
                let spawn = demo.spawn_positions.get(index).copied().unwrap_or(mob.home);
                let dx = mob.position.x - spawn.x;
                let dy = mob.position.y - spawn.y;
                let dz = mob.position.z - spawn.z;
                if mob.walk_phase > 0.1 && (dx * dx + dy * dy + dz * dz).sqrt() > 1e-3 {
                    walked += 1;
                }
            }
            demo.walked = walked;
            let total = session.mobs().len();
            if total == 4
                && resident == 4
                && walked == 4
                && demo.walk_updates >= COMBAT_WALK_OBSERVATIONS
            {
                // Freeze the walk verdict before any later stage rewrites the
                // mob list: the digest must describe the walk that was proven,
                // not the two-record world the Drops stage leaves behind. The
                // vertex count is the sum over resident mobs of the real
                // [`build_mob_vertices`] output (4 mobs x 6 cuboids x 6 faces x
                // 6 vertices = 864), so it pins the render path's batch size.
                demo.walk_mobs = total as u32;
                demo.walk_resident = resident as u32;
                demo.walk_verts = session
                    .resident_mobs()
                    .map(|mob| {
                        build_mob_vertices(
                            mob.kind,
                            MobAnim {
                                walk_phase: mob.walk_phase,
                                walk_amount: 1.0,
                                light: 1.0,
                            },
                            0.0,
                            0.0,
                        )
                        .len()
                    })
                    .sum();
                // Fight runs in THIS step, not a later one, for the same
                // reason the old direct Save call did: no redraw may slip
                // between the walk verdict and the scripted hit sequence.
                demo.stage = CombatStage::Fight;
                return combat_fight_stage(demo, session);
            }
            if demo.walk_updates >= COMBAT_WALK_ABORT_UPDATES {
                demo.abort(
                    "walk",
                    &format!(
                        "walked={walked} resident={resident} total={total} updates={}",
                        demo.walk_updates
                    ),
                );
                return true;
            }
            false
        }
        CombatStage::Fight => combat_fight_stage(demo, session),
        CombatStage::Drops => combat_drops_stage(demo, session),
        CombatStage::Hostile => combat_hostile_stage(demo, session),
        CombatStage::Shot => combat_shot_stage(demo, session),
        CombatStage::Skeleton => combat_skeleton_stage(demo, session),
        CombatStage::Creative => combat_creative_stage(demo, session),
        CombatStage::Save => combat_save_stage(demo, session),
        CombatStage::Restore => combat_restore_stage(demo, session),
        CombatStage::Done => true,
    }
}

/// The Ambient stage: the slice's real-binary proof that the deterministic
/// population layer works through the shipped `WorldSession::tick_mobs` seam.
///
/// Three phases run in order, each re-measured from a fresh report baseline:
///
/// * [`AmbientPhase::Spawn`] pins [`COMBAT_NIGHT_TICK`], resets the report,
///   opens the ambient gate and drives 0.1 s of game time per observation. It
///   rejects any record inside [`SAFE_RADIUS`] of the player or the cached
///   world spawn, rejects a peak above [`MAX_MOBS_TOTAL`], and advances once at
///   least two records spawned AND at least two burn-eligible surface hostiles
///   exist after [`COMBAT_AMBIENT_MIN_UPDATES`] observations: one record proves
///   the despawn cull, the other proves daylight burning. Requiring the burn
///   target up front is what makes the stage's own `burned >= 1` contract
///   reachable — a run that spawned only one record would otherwise have the
///   cull consume its only burn witness. [`COMBAT_AMBIENT_ABORT_UPDATES`] bounds
///   a world that never populates enough.
/// * [`AmbientPhase::Despawn`] closes the gate (the distance cull still runs),
///   moves one record past [`DESPAWN_RADIUS`], and ages 1.1 s so the 1.0 s scan
///   marks it and the single reap site removes it. It prefers a record that the
///   burner would *not* touch, so the burn witness always survives the cull.
/// * [`AmbientPhase::Burn`] pins [`COMBAT_DAY_TICK`] and reopens the gate: every
///   surface hostile burns 1 hp per 0.5 s. It advances once no burnable hostile
///   remains and at least one burned.
///
/// On success it restores the pinned dusk clock, closes the ambient gate, and
/// latches [`CombatDemo::spawn_digest`], so the pinned quartet stages that
/// follow are byte-for-byte what they were before this stage existed. Every
/// mutation goes through a production seam (`session.clock`,
/// [`WorldSession::set_ambient_spawning`], [`WorldSession::reset_spawn_report`],
/// [`WorldSession::spawn_report`], [`WorldSession::mobs`],
/// [`WorldSession::tick_mobs`]). Every abort names the observed value and exits
/// the run non-zero through [`CombatDemo::abort`].
pub fn combat_ambient_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    match demo.ambient_phase {
        AmbientPhase::Spawn => {
            if demo.ambient_updates == 0 {
                // The Ambient stage pins exact population counters, and a spawn
                // attempt against a not-yet-streamed column is silently
                // refused. Wait until every candidate column in the 24..48
                // block ring is resident and lit so debug and release see the
                // same ready set for the whole attempt sequence. The frame
                // loop stands down for this stage (see
                // [`CombatDemo::owns_mob_ticks`]), so nothing ticks while the
                // ring streams in.
                if !session.spawn_ring_ready() {
                    return false;
                }
                // First entry: a known night clock, a zero report baseline, an
                // open gate, and a fresh fixed-step runtime. The attempt
                // sequence and the wander ticks are then a pure function of
                // the seed.
                session.clock = GameClock::from_time(COMBAT_NIGHT_TICK);
                session.reset_spawn_report();
                session.reset_mob_runtime();
                session.set_ambient_spawning(true);
                demo.ambient_peak = 0;
                demo.ambient_safe = 0;
            }
            // 0.1 s of game time per observation keeps the stage headlessly
            // driveable; the real frame loop adds its own Playing tick on top.
            session.tick_mobs(0.05, 1.0);
            session.tick_mobs(0.05, 1.0);
            demo.ambient_updates += 1;
            demo.ambient_peak = demo.ambient_peak.max(session.mobs().len());

            // A record inside either safe radius is a hard contract failure:
            // the layer must never place a mob on the player or the spawn pad.
            // The safe radius is a *placement* rule: a record must never be
            // spawned on the player or the world-spawn pad. Wandering after a
            // legal placement is ordinary gameplay (the burn phase even pins
            // records back to `home` for the same reason), so the violation is
            // measured on the immutable spawn anchor, never the live feet.
            let safe_violations = session
                .mobs()
                .iter()
                .filter(|mob| {
                    combat_horizontal_distance(mob.home, session.player.position) < SAFE_RADIUS
                        || combat_horizontal_distance(mob.home, session.world_spawn()) < SAFE_RADIUS
                })
                .count();
            demo.ambient_safe += safe_violations as u32;
            if safe_violations > 0 {
                demo.abort(
                    "ambient",
                    &format!(
                        "safe={} peak={} updates={}",
                        demo.ambient_safe, demo.ambient_peak, demo.ambient_updates
                    ),
                );
                return true;
            }
            if demo.ambient_peak > MAX_MOBS_TOTAL {
                demo.abort(
                    "ambient",
                    &format!("peak={} cap={MAX_MOBS_TOTAL}", demo.ambient_peak),
                );
                return true;
            }

            let report = session.spawn_report();
            // Two independent witnesses are needed downstream: one record for
            // the despawn cull and one surface hostile for the daylight burn.
            let burnable = session
                .mobs()
                .iter()
                .filter(|mob| combat_mob_burn_eligible(session, mob))
                .count();
            if report.spawned >= 2
                && demo.ambient_updates >= COMBAT_AMBIENT_MIN_UPDATES
                && burnable >= 2
            {
                demo.spawn_phase_report = report;
                demo.ambient_phase = AmbientPhase::Despawn;
                return false;
            }
            if demo.ambient_updates >= COMBAT_AMBIENT_ABORT_UPDATES {
                demo.abort(
                    "ambient",
                    &format!(
                        "spawned={} burnable={burnable} updates={}",
                        report.spawned, demo.ambient_updates
                    ),
                );
                return true;
            }
            false
        }
        AmbientPhase::Despawn => {
            // Spawns and burning stay off, but the despawn scan is ungated, so
            // the radius proof still runs.
            session.set_ambient_spawning(false);
            let before = session.mobs().len();
            let player = session.player.position;
            // Prefer a record the burner would never touch (e.g. a cave-dark
            // hostile) so the surface burn witness survives the cull. When every
            // record is burn-eligible the spawn phase guaranteed at least two, so
            // removing the first still leaves one.
            let victim = session
                .mobs()
                .iter()
                .position(|mob| !combat_mob_burn_eligible(session, mob))
                .unwrap_or(0);
            let Some(first) = session.mobs_mut().get_mut(victim) else {
                demo.abort("ambient", "despawn_no_record");
                return true;
            };
            first.position = player + Vec3::new(DESPAWN_RADIUS + 8.0, 0.0, 0.0);
            first.velocity = Vec3::ZERO;
            for _ in 0..COMBAT_AMBIENT_DESPAWN_TICKS {
                session.tick_mobs(0.05, 1.0);
            }
            let after = session.mobs().len();
            let marked = session.spawn_report().despawned;
            if after + 1 != before || marked != 1 {
                demo.abort(
                    "ambient",
                    &format!("despawn before={before} after={after} marked={marked}"),
                );
                return true;
            }
            demo.spawn_despawned = 1;
            demo.ambient_phase = AmbientPhase::Burn;
            false
        }
        AmbientPhase::Burn => {
            session.clock = GameClock::from_time(COMBAT_DAY_TICK);
            // Burning must be on. Day passives may also spawn here; every
            // assertion below ignores them.
            session.set_ambient_spawning(true);
            let before = session.spawn_report().burned;
            let start_burnable = session
                .mobs()
                .iter()
                .filter(|mob| combat_mob_burn_eligible(session, mob))
                .count();
            let mut finished = false;
            for _ in 0..COMBAT_AMBIENT_BURN_TICKS {
                // Demo pin, not a spawn rule (same shape as the Drops stage's
                // `combat_pin_mob_for_hit`): hold every record on its immutable
                // spawn anchor so a wander under a tree canopy cannot pause the
                // burn cadence mid-kill and leave the run without a witness.
                // Cave-dark records stay dark at their anchor and the burner
                // simply ignores them.
                for mob in session.mobs_mut() {
                    mob.position = mob.home;
                    mob.velocity = Vec3::ZERO;
                }
                session.tick_mobs(0.05, 1.0);
                if !session
                    .mobs()
                    .iter()
                    .any(|mob| combat_mob_burn_eligible(session, mob))
                {
                    finished = true;
                    break;
                }
            }
            let burned = session.spawn_report().burned.saturating_sub(before);
            if !finished || burned < 1 {
                // Self-describing: name the observed population so a world that
                // only produced cave-dark hostiles (which never burn) is
                // distinguishable from a clock or gate failure.
                let total = session.mobs().len();
                let hostiles = session
                    .mobs()
                    .iter()
                    .filter(|mob| mob.kind.is_hostile() && mob.health > 0)
                    .count();
                let burnable = session
                    .mobs()
                    .iter()
                    .filter(|mob| combat_mob_burn_eligible(session, mob))
                    .count();
                demo.abort(
                    "ambient",
                    &format!(
                        "burned={burned} finished={finished} start_burnable={start_burnable} mobs={total} hostiles={hostiles} burnable={burnable}"
                    ),
                );
                return true;
            }
            demo.spawn_burned = burned;
            // Restore the pinned dusk clock and close the gate before the
            // quartet stages: at dusk (`COMBAT_DUSK_TICK` is still day) the
            // Zombie/Skeleton quartet would otherwise burn during a slow Walk
            // and break the pinned `total == 4` verdict.
            session.clock.time_of_day = COMBAT_DUSK_TICK;
            session.set_ambient_spawning(false);
            demo.spawn_digest = Some(demo.spawn_digest());
            demo.stage = CombatStage::Spawn;
            false
        }
    }
}

/// Horizontal (XZ) distance between two feet positions. The population layer's
/// safe and despawn radii are horizontal by design, so the demo's checks are
/// too: a mob deep in a cave below the player is still nearby.
pub fn combat_horizontal_distance(a: Vec3, b: Vec3) -> f32 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

/// Whether the burner's own eligibility rule would burn this record while the
/// clock is day: a live hostile standing on an evaluated, fully sky-lit,
/// non-water surface.
///
/// Mirrors `spawning::scan_burn` (which is private) through the public
/// [`SpawnWorld`] capability, including the `column_ready` gate before the light
/// read; the day/clock half of that rule is checked by the caller. The Ambient
/// stage uses it both to require a burn witness up front and to decide when the
/// burn phase is finished.
pub fn combat_mob_burn_eligible(session: &WorldSession, mob: &Mob) -> bool {
    if !mob.kind.is_hostile() || mob.health == 0 {
        return false;
    }
    let feet = IVec3::new(
        mob.position.x.floor() as i32,
        mob.position.y.floor() as i32,
        mob.position.z.floor() as i32,
    );
    let (cx, cz) = (
        feet.x.div_euclid(CHUNK_SIZE as i32),
        feet.z.div_euclid(CHUNK_SIZE as i32),
    );
    if !SpawnWorld::column_ready(&session.world, cx, cz) {
        return false;
    }
    SpawnWorld::sky_light(&session.world, feet) == PASSIVE_SURFACE_LIGHT
        && SpawnWorld::block(&session.world, feet) != BlockId::Water
}

/// The Fight stage: the slice's real-binary proof that a crafted wood sword
/// hits the mob under the crosshair.
///
/// The whole hit sequence runs in ONE call, for the same reason
/// [`combat_save_stage`] does: a redraw between two steps would run the mob
/// runtime and make the reported numbers non-deterministic. Every value comes
/// from a production seam — `selected_item`, `melee_candidate`,
/// [`mining_after_attack`], [`MeleeState`], [`attack_damage`] and
/// [`WorldSession::attack_mob`] — so the demo proves the shipped path instead
/// of a parallel copy of it.
///
/// On success the stage latches `melee_hits`/`melee_damage`/`melee_blocked`
/// and advances to `Drops`, which kills the two passive records and delivers
/// their drops. The crafted wood sword stays in the inventory: it is the
/// player's earned item and the milestone requires the inventory to persist, so
/// it is part of the saved witness rather than a throwaway fixture. Every
/// mismatch latches `stage=fight` naming the observed value, so a partial proof
/// can never be mistaken for a pass.
pub fn combat_fight_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    let pig_kind = MobKind::ALL[0];
    debug_assert_eq!(pig_kind, MobKind::Pig, "index 0 is the demo's Pig pin");

    // Weapon preflight through production seams only. The fresh demo world
    // starts with an empty inventory, so the stage inserts a real wood sword
    // and selects it exactly like a player would.
    let sword_id = ItemId::Tool {
        kind: ToolKind::Sword,
        tier: ToolTier::Wood,
    };
    let Ok(sword) = Item::new(sword_id, 1) else {
        demo.abort("fight", "could_not_build_sword");
        return true;
    };
    if !session.player.collect_item(sword) || !session.player.select_slot(0) {
        demo.abort("fight", "sword_not_held");
        return true;
    }
    let held = session.player.selected_item().map(|item| item.id());
    if held != Some(sword_id) {
        demo.abort("fight", &format!("held={held:?}"));
        return true;
    }
    let origin = session.camera.position;
    let player_position = session.player.position;
    let forward = Vec3::new(session.player.yaw.cos(), 0.0, session.player.yaw.sin());

    // Demo pose pin, NOT a spawn rule: the pig's feet are placed two blocks
    // along the yaw forward axis at the player's own feet height, at full
    // health with every combat timer cleared, so the hit sequence starts from
    // a known state. `combat_save_stage` re-pins the real quartet witness
    // afterwards, so nothing here can leak into the save.
    let pig_feet = Vec3::new(
        player_position.x + forward.x * 2.0,
        player_position.y,
        player_position.z + forward.z * 2.0,
    );
    let pig_centre = pig_feet + Vec3::Y * (pig_kind.height() * 0.5);
    {
        let mobs = session.mobs_mut();
        if mobs.len() != 4 {
            demo.abort("fight", &format!("mobs={}", mobs.len()));
            return true;
        }
        let Some(pig) = mobs.first_mut() else {
            demo.abort("fight", "mobs=0");
            return true;
        };
        if pig.kind != pig_kind {
            demo.abort("fight", &format!("kind={:?}", pig.kind));
            return true;
        }
        pig.position = pig_feet;
        pig.health = pig_kind.max_health();
        pig.hurt = 0.0;
        pig.hurt_iframe = 0.0;
        pig.knockback = Vec3::ZERO;
        // The other three records are parked far behind the camera (beyond
        // any melee reach) so the crosshair test below is unambiguous no
        // matter how far the Walk stage let them wander. `combat_save_stage`
        // re-pins their real quartet pose in the next step.
        for mob in mobs.iter_mut().skip(1) {
            mob.position = player_position - forward * 20.0;
            mob.health = mob.kind.max_health();
            mob.hurt = 0.0;
            mob.hurt_iframe = 0.0;
            mob.knockback = Vec3::ZERO;
        }
    }
    if session.mobs().is_empty() {
        demo.abort("fight", "mobs=0");
        return true;
    }
    let pig_health = session.mobs()[0].health;

    // Crosshair ray: camera eye -> the pinned pig's AABB centre, normalized so
    // `MELEE_REACH` is a block distance.
    let dir = (pig_centre - origin).normalize_or_zero();
    let Some(crosshair) = session.melee_candidate(origin, dir, MELEE_REACH) else {
        demo.abort("fight", &format!("crosshair=miss reach={MELEE_REACH}"));
        return true;
    };
    if crosshair.index != 0 {
        demo.abort("fight", &format!("crosshair_index={}", crosshair.index));
        return true;
    }
    if crosshair.distance > MELEE_REACH {
        demo.abort(
            "fight",
            &format!("crosshair_distance={}", crosshair.distance),
        );
        return true;
    }

    // Priority proof with a guaranteed block: raycast the ground one block
    // ahead of the player through the real voxel raycast and require a real
    // `MiningTarget`, so the suppression rule below is proven against a
    // genuine mining candidate rather than a fabricated one.
    let mining_dir = (session.player.position + forward - origin).normalize_or_zero();
    let mine_hit = raycast_voxels(origin, mining_dir, 8.0, |x, y, z| {
        session.world.get_block(x, y, z)
    });
    let Some(mine_hit) = mine_hit else {
        demo.abort("fight", "mining=no_block_ahead");
        return true;
    };
    let mine_block = session.world.get_block(
        mine_hit.block_pos.x,
        mine_hit.block_pos.y,
        mine_hit.block_pos.z,
    );
    let Some(mining) = MiningTarget::from_block_with(
        &session.registry,
        mine_hit.block_pos,
        mine_block,
        session.player.selected_item(),
    ) else {
        demo.abort("fight", &format!("mining=unbreakable {mine_block:?}"));
        return true;
    };
    if mining_after_attack(Some(crosshair), Some(mining)).is_some() {
        demo.abort("fight", "mining=not_suppressed_by_attack");
        return true;
    }
    if mining_after_attack(None, Some(mining)) != Some(mining) {
        demo.abort("fight", "mining=suppressed_without_attack");
        return true;
    }

    // First swing: the initial cooldown is zero, so one tick swings.
    let mut melee = MeleeState::new();
    let Some(target) = MeleeTarget::new(0, pig_health) else {
        demo.abort("fight", &format!("target_health={pig_health}"));
        return true;
    };
    if melee.tick(0.1, Some(target)) != (MeleeTick::Swing { index: 0 }) {
        demo.abort("fight", "first_swing=blocked");
        return true;
    }
    let damage = attack_damage(
        session.player.selected_item(),
        session.player.mode(),
        target.health(),
    );
    if damage != SWORD_DAMAGE_WOOD {
        demo.abort("fight", &format!("damage={damage}"));
        return true;
    }
    let Some(first) = session.attack_mob(0, damage, origin) else {
        demo.abort("fight", "first_hit=refused");
        return true;
    };
    if first.damage != SWORD_DAMAGE_WOOD || first.health_after != 6 || first.killed {
        demo.abort(
            "fight",
            &format!(
                "first_hit damage={} health_after={} killed={}",
                first.damage, first.health_after, first.killed
            ),
        );
        return true;
    }
    {
        let pig = &session.mobs()[0];
        if !(pig.hurt.is_finite() && pig.hurt > 0.0) {
            demo.abort("fight", &format!("hurt={}", pig.hurt));
            return true;
        }
        if !(pig.hurt_iframe.is_finite() && pig.hurt_iframe > 0.0) {
            demo.abort("fight", &format!("hurt_iframe={}", pig.hurt_iframe));
            return true;
        }
        let knockback = Vec3::new(pig.knockback.x, 0.0, pig.knockback.z);
        let away = Vec3::new(pig.position.x - origin.x, 0.0, pig.position.z - origin.z);
        if knockback.length_squared() <= 0.0 || knockback.dot(away) <= 0.0 {
            demo.abort(
                "fight",
                &format!(
                    "knockback=({:.3},{:.3}) away=({:.3},{:.3})",
                    knockback.x, knockback.z, away.x, away.z
                ),
            );
            return true;
        }
    }

    // Blocked re-hit: the 0.5 s i-frame window refuses a second hit through
    // the exact production call the frame loop uses.
    let mut blocked = 0u32;
    if session.attack_mob(0, SWORD_DAMAGE_WOOD, origin).is_some() {
        demo.abort("fight", "iframe=not_enforced");
        return true;
    }
    blocked += 1;
    if matches!(melee.tick(0.0, Some(target)), MeleeTick::Swing { .. }) {
        demo.abort("fight", "cooldown=not_enforced");
        return true;
    }

    // Cooldown re-arm WITHOUT skipping the clamp: `MeleeState::tick` clamps a
    // frame to `MAX_DT` (0.1 s), so one `tick(SWING_COOLDOWN_SECS, ..)` would
    // prove nothing. Walk the 0.6 s window in eight 0.1 s frames and require
    // exactly one swing across it (the cooldown elapses after six or seven
    // decays, so only the count is asserted).
    let live_target = MeleeTarget::new(0, session.mobs()[0].health);
    let mut swings = 0u32;
    for _ in 0..8 {
        if let MeleeTick::Swing { index } = melee.tick(0.1, live_target) {
            if index != 0 {
                demo.abort("fight", &format!("cooldown_index={index}"));
                return true;
            }
            swings += 1;
        }
    }
    if swings != 1 {
        demo.abort("fight", &format!("cooldown_swings={swings}"));
        return true;
    }

    // The pure swing clock above advanced 0.8 s of input time, while the mob's
    // timers only decay inside the fixed-rate runtime. Age the runtime by the
    // same 0.8 s so the second hit lands on an expired 0.5 s i-frame window
    // rather than being refused by the window the blocked re-hit just proved.
    for _ in 0..8 {
        session.tick_mobs(0.1, 1.0);
    }
    let iframe = session.mobs()[0].hurt_iframe;
    if iframe > 0.0 {
        demo.abort("fight", &format!("iframe_not_expired={iframe}"));
        return true;
    }
    let Some(second) = session.attack_mob(0, SWORD_DAMAGE_WOOD, origin) else {
        demo.abort("fight", "second_hit=refused");
        return true;
    };
    if second.damage != SWORD_DAMAGE_WOOD || second.health_after != 2 || second.killed {
        demo.abort(
            "fight",
            &format!(
                "second_hit damage={} health_after={} killed={}",
                second.damage, second.health_after, second.killed
            ),
        );
        return true;
    }
    let final_health = session.mobs()[0].health;
    if final_health != 2 {
        demo.abort("fight", &format!("health={final_health}"));
        return true;
    }

    // The demo sword is a real earned item, not a fixture: it stays in the
    // hotbar so the saved witness carries the player's inventory into the
    // Drops stage and through the reload.

    // Latch the observed numbers the success digest reports, then hand over to
    // the Drops stage, which kills the two passive records (fleeing, collapsing
    // and being reaped) and delivers their drops into this same inventory.
    demo.melee_hits = 2;
    demo.melee_damage = SWORD_DAMAGE_WOOD;
    demo.melee_blocked = blocked;
    demo.stage = CombatStage::Drops;
    false
}

/// Re-pins one demo mob at `feet` with every ephemeral combat timer cleared, so
/// each scripted hit starts from the same known state.
///
/// Demo pose pin, NOT a gameplay rule: the Drops stage owns the pose of the
/// record it is about to hit, exactly like the Fight stage owns the Pig's pin.
pub fn combat_pin_mob_for_hit(session: &mut WorldSession, index: usize, feet: Vec3) {
    if let Some(mob) = session.mobs_mut().get_mut(index) {
        mob.position = feet;
        mob.home = feet;
        mob.velocity = Vec3::ZERO;
        mob.knockback = Vec3::ZERO;
        mob.hurt = 0.0;
        mob.hurt_iframe = 0.0;
        mob.flee = 0.0;
        mob.death = 0.0;
        mob.state = MobAiState::Wander;
    }
}

/// Total `id` units across all 36 inventory slots (order-independent).
pub fn combat_inventory_units(session: &WorldSession, id: ItemId) -> u32 {
    session
        .player
        .inventory()
        .slots()
        .iter()
        .filter_map(|slot| slot.as_ref())
        .filter(|item| item.id() == id)
        .map(|item| u32::from(item.count()))
        .sum()
}

/// Aims the crosshair ray at `index`'s AABB centre and applies one real wood
/// sword hit through [`WorldSession::melee_candidate`] +
/// [`WorldSession::attack_mob`].
///
/// Returns the observed failure as a string instead of aborting directly, so
/// the caller latches one self-describing `stage=drops` digest.
pub fn combat_sword_hit(
    session: &mut WorldSession,
    index: usize,
    origin: Vec3,
) -> Result<MobHit, String> {
    let Some(mob) = session.mobs().get(index) else {
        return Err(format!("index={index}"));
    };
    let (min, max) = mob.aabb();
    let centre = (min + max) * 0.5;
    let dir = (centre - origin).normalize_or_zero();
    let Some(candidate) = session.melee_candidate(origin, dir, MELEE_REACH) else {
        return Err(format!("crosshair=miss index={index}"));
    };
    if candidate.index != index {
        return Err(format!(
            "crosshair_index={} expected={index}",
            candidate.index
        ));
    }
    session
        .attack_mob(index, SWORD_DAMAGE_WOOD, origin)
        .ok_or_else(|| format!("hit_refused index={index}"))
}

/// The Drops stage: the slice's real-binary proof that a hit makes a passive mob
/// flee away, that a lethal hit collapses the box model and then removes the
/// record, and that a player-attributed kill lands its raw meat and leather in
/// the real 36-slot inventory.
///
/// Runs entirely in ONE call, like the Fight stage: every number here is
/// observable, and a redraw between two partial phases would let the mob runtime
/// move a record the next check describes. Every mutation goes through a
/// production seam — [`WorldSession::melee_candidate`],
/// [`WorldSession::attack_mob`], [`WorldSession::tick_mobs`] (which reaps
/// internally), [`WorldSession::resolve_mob_kill`] and [`mob_drop_items`] — so
/// the demo proves the shipped path instead of a parallel copy of it.
///
/// On success it latches `killed`/`drops`/`drops_lost` and advances to `Save`,
/// which pins the two surviving hostile records and writes the witness.
pub fn combat_drops_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    let sword_id = ItemId::Tool {
        kind: ToolKind::Sword,
        tier: ToolTier::Wood,
    };
    if session.player.selected_item().map(|item| item.id()) != Some(sword_id) {
        demo.abort("drops", "sword_not_held");
        return true;
    }
    let origin = session.camera.position;
    let player_position = session.player.position;
    let forward = Vec3::new(session.player.yaw.cos(), 0.0, session.player.yaw.sin());
    // The pin point: two blocks along the yaw forward axis at the player's own
    // feet height, always inside the resident spawn chunks the Walk stage
    // proved. Both passive phases reuse it.
    let pin_feet = Vec3::new(
        player_position.x + forward.x * 2.0,
        player_position.y,
        player_position.z + forward.z * 2.0,
    );
    let horizontal_from_player = |feet: Vec3| {
        Vec3::new(feet.x - player_position.x, 0.0, feet.z - player_position.z).length()
    };
    let mut accepted = 0u32;
    let mut lost = 0u32;

    // ---- Pig: 10 -> 6 (flee) -> 2 -> 0 (collapse) -> reap ----------------
    if session.mobs().first().map(|mob| mob.kind) != Some(MobKind::Pig) {
        demo.abort("drops", "pig_index0_missing");
        return true;
    }
    session.mobs_mut()[0].health = MobKind::Pig.max_health();
    combat_pin_mob_for_hit(session, 0, pin_feet);
    let hit = match combat_sword_hit(session, 0, origin) {
        Ok(hit) => hit,
        Err(observed) => {
            demo.abort("drops", &observed);
            return true;
        }
    };
    if hit.health_after != 6 || hit.killed {
        demo.abort(
            "drops",
            &format!(
                "pig_first health_after={} killed={}",
                hit.health_after, hit.killed
            ),
        );
        return true;
    }
    {
        let pig = &session.mobs()[0];
        if pig.state != MobAiState::Flee || pig.flee <= 0.0 || pig.death != 0.0 {
            demo.abort(
                "drops",
                &format!(
                    "pig_flee state={:?} flee={} death={}",
                    pig.state, pig.flee, pig.death
                ),
            );
            return true;
        }
    }

    // Flee is a runtime behaviour, not a field flip: age the fixed-rate runtime
    // (six 0.1 s frames = twelve fixed steps) and require the Pig to be farther
    // from the player, with the 0.5 s i-frame window expired (ten steps).
    let before_flee = horizontal_from_player(session.mobs()[0].position);
    for _ in 0..6 {
        session.tick_mobs(0.1, 1.0);
    }
    let after_flee = horizontal_from_player(session.mobs()[0].position);
    if after_flee <= before_flee {
        demo.abort(
            "drops",
            &format!("pig_flee before={before_flee:.3} after={after_flee:.3}"),
        );
        return true;
    }
    let iframe = session.mobs()[0].hurt_iframe;
    if iframe != 0.0 {
        demo.abort("drops", &format!("pig_iframe={iframe}"));
        return true;
    }

    combat_pin_mob_for_hit(session, 0, pin_feet);
    let hit = match combat_sword_hit(session, 0, origin) {
        Ok(hit) => hit,
        Err(observed) => {
            demo.abort("drops", &observed);
            return true;
        }
    };
    if hit.health_after != 2 || hit.killed {
        demo.abort(
            "drops",
            &format!(
                "pig_second health_after={} killed={}",
                hit.health_after, hit.killed
            ),
        );
        return true;
    }

    combat_pin_mob_for_hit(session, 0, pin_feet);
    let hit = match combat_sword_hit(session, 0, origin) {
        Ok(hit) => hit,
        Err(observed) => {
            demo.abort("drops", &observed);
            return true;
        }
    };
    if hit.health_after != 0 || !hit.killed {
        demo.abort(
            "drops",
            &format!(
                "pig_lethal health_after={} killed={}",
                hit.health_after, hit.killed
            ),
        );
        return true;
    }
    {
        let pig = &session.mobs()[0];
        if pig.death <= 0.0 || pig.death_progress() >= 1.0 || pig.velocity != Vec3::ZERO {
            demo.abort(
                "drops",
                &format!(
                    "pig_collapse death={} progress={:.3} velocity=({:.3},{:.3},{:.3})",
                    pig.death,
                    pig.death_progress(),
                    pig.velocity.x,
                    pig.velocity.y,
                    pig.velocity.z
                ),
            );
            return true;
        }
    }
    let before_reap = session.mobs().len();
    let report = session.resolve_mob_kill(0, hit, MobKillCause::Player);
    if report != MobDropReport::new(2, 0) {
        demo.abort(
            "drops",
            &format!(
                "pig_drops accepted={} lost={}",
                report.accepted, report.lost
            ),
        );
        return true;
    }
    accepted += report.accepted;
    lost += report.lost;

    // The collapse must finish before the reaper may remove the record: age
    // more than the pinned 0.8 s collapse in fixed steps.
    for _ in 0..10 {
        session.tick_mobs(0.1, 1.0);
    }
    if session.mobs().len() + 1 != before_reap {
        demo.abort(
            "drops",
            &format!(
                "pig_reap before={before_reap} after={}",
                session.mobs().len()
            ),
        );
        return true;
    }
    if session.mobs().iter().any(|mob| mob.kind == MobKind::Pig) {
        demo.abort("drops", "pig_record_survived_reap");
        return true;
    }
    let porkchop = combat_inventory_units(session, ItemId::Material(Material::RawPorkchop));
    let leather = combat_inventory_units(session, ItemId::Material(Material::Leather));
    if porkchop != 1 || leather != 1 {
        demo.abort(
            "drops",
            &format!("pig_inventory porkchop={porkchop} leather={leather}"),
        );
        return true;
    }

    // ---- Cow: 10 -> 6 (flee) -> 2 -> 0 (collapse) -> reap ----------------
    // The index is looked up by kind: the Pig's record was reaped above, so
    // every later slot shifted down by one.
    let Some(cow_index) = session
        .mobs()
        .iter()
        .position(|mob| mob.kind == MobKind::Cow)
    else {
        demo.abort("drops", "cow_missing");
        return true;
    };
    session.mobs_mut()[cow_index].health = MobKind::Cow.max_health();
    combat_pin_mob_for_hit(session, cow_index, pin_feet);
    let hit = match combat_sword_hit(session, cow_index, origin) {
        Ok(hit) => hit,
        Err(observed) => {
            demo.abort("drops", &observed);
            return true;
        }
    };
    if hit.health_after != 6 || hit.killed {
        demo.abort(
            "drops",
            &format!(
                "cow_first health_after={} killed={}",
                hit.health_after, hit.killed
            ),
        );
        return true;
    }
    {
        let cow = &session.mobs()[cow_index];
        if cow.state != MobAiState::Flee || cow.flee <= 0.0 || cow.death != 0.0 {
            demo.abort(
                "drops",
                &format!(
                    "cow_flee state={:?} flee={} death={}",
                    cow.state, cow.flee, cow.death
                ),
            );
            return true;
        }
    }

    combat_pin_mob_for_hit(session, cow_index, pin_feet);
    let hit = match combat_sword_hit(session, cow_index, origin) {
        Ok(hit) => hit,
        Err(observed) => {
            demo.abort("drops", &observed);
            return true;
        }
    };
    if hit.health_after != 2 || hit.killed {
        demo.abort(
            "drops",
            &format!(
                "cow_second health_after={} killed={}",
                hit.health_after, hit.killed
            ),
        );
        return true;
    }

    combat_pin_mob_for_hit(session, cow_index, pin_feet);
    let hit = match combat_sword_hit(session, cow_index, origin) {
        Ok(hit) => hit,
        Err(observed) => {
            demo.abort("drops", &observed);
            return true;
        }
    };
    if hit.health_after != 0 || !hit.killed {
        demo.abort(
            "drops",
            &format!(
                "cow_lethal health_after={} killed={}",
                hit.health_after, hit.killed
            ),
        );
        return true;
    }
    {
        let cow = &session.mobs()[cow_index];
        if cow.death <= 0.0 || cow.death_progress() >= 1.0 || cow.velocity != Vec3::ZERO {
            demo.abort(
                "drops",
                &format!(
                    "cow_collapse death={} progress={:.3} velocity=({:.3},{:.3},{:.3})",
                    cow.death,
                    cow.death_progress(),
                    cow.velocity.x,
                    cow.velocity.y,
                    cow.velocity.z
                ),
            );
            return true;
        }
    }
    let before_reap = session.mobs().len();
    let report = session.resolve_mob_kill(cow_index, hit, MobKillCause::Player);
    if report != MobDropReport::new(3, 0) {
        demo.abort(
            "drops",
            &format!(
                "cow_drops accepted={} lost={}",
                report.accepted, report.lost
            ),
        );
        return true;
    }
    accepted += report.accepted;
    lost += report.lost;
    for _ in 0..10 {
        session.tick_mobs(0.1, 1.0);
    }
    if session.mobs().len() + 1 != before_reap {
        demo.abort(
            "drops",
            &format!(
                "cow_reap before={before_reap} after={}",
                session.mobs().len()
            ),
        );
        return true;
    }
    if session.mobs().iter().any(|mob| mob.kind == MobKind::Cow) {
        demo.abort("drops", "cow_record_survived_reap");
        return true;
    }
    let beef = combat_inventory_units(session, ItemId::Material(Material::RawBeef));
    let leather = combat_inventory_units(session, ItemId::Material(Material::Leather));
    if beef != 1 || leather != 3 {
        demo.abort(
            "drops",
            &format!("cow_inventory beef={beef} leather={leather}"),
        );
        return true;
    }

    // Overflow self-check one level below the Player forwarder: a 35-full
    // inventory with one empty slot must accept exactly one unit, report the
    // other, and produce the exact overflow notice. The solved batch comes from
    // the same `mob_drop_items` bridge the kill seam uses.
    let Some(stick) = Item::new(ItemId::Material(Material::Stick), 64).ok() else {
        demo.abort("drops", "stick_stack_build_failed");
        return true;
    };
    let mut slots = [None; INVENTORY_SLOTS];
    for slot in slots.iter_mut().take(INVENTORY_SLOTS - 1) {
        *slot = Some(stick);
    }
    let mut inventory = Inventory::from_parts(slots, None);
    let Some(pig_items) = mob_drop_items(MobKind::Pig, session.world.seed(), IVec3::new(0, 0, 0))
    else {
        demo.abort("drops", "pig_table_unresolved");
        return true;
    };
    let outcome = inventory.collect_items(&pig_items);
    if outcome.accepted != 1 || outcome.lost != 1 {
        demo.abort(
            "drops",
            &format!(
                "overflow accepted={} lost={}",
                outcome.accepted, outcome.lost
            ),
        );
        return true;
    }
    let notice = mob_drop_notice(MobDropReport::new(outcome.accepted, outcome.lost));
    if notice != Some("Inventory full: some mob drops were lost") {
        demo.abort("drops", &format!("overflow_notice={notice:?}"));
        return true;
    }

    demo.killed = 2;
    demo.drops = accepted;
    demo.drops_lost = lost;
    demo.stage = CombatStage::Hostile;
    false
}

/// Total item units across all 36 inventory slots plus the cursor stack.
///
/// [`combat_inventory_units`] answers "how much of this item id"; this answers
/// "how much of anything", which is what the Hostile stage needs to prove a
/// respawn keeps the whole inventory instead of only the sword.
pub fn combat_inventory_units_total(session: &WorldSession) -> u32 {
    let inventory = session.player.inventory();
    let slots: u32 = inventory
        .slots()
        .iter()
        .filter_map(|slot| slot.as_ref())
        .map(|item| u32::from(item.count()))
        .sum();
    slots + inventory.cursor().map_or(0, |item| u32::from(item.count()))
}

/// Whether a zombie can be pinned with its feet at `feet` and still see the
/// player's eyes from `eye`.
///
/// The stage's spot rule: the feet and head blocks must be air, the block below
/// must be solid, and the eye-to-eye ray must be clear. It is a demo placement
/// guard, not a gameplay rule — the production steering brain re-checks LOS
/// every step and would refuse the aggro on its own.
pub fn combat_hostile_spot_ok(session: &WorldSession, feet: Vec3, eye: Vec3) -> bool {
    if !feet.is_finite() {
        return false;
    }
    let x = feet.x.floor() as i32;
    let y = feet.y.floor() as i32;
    let z = feet.z.floor() as i32;
    if session.world.get_block(x, y, z) != BlockId::Air
        || session.world.get_block(x, y + 1, z) != BlockId::Air
        || !session.world.get_block(x, y - 1, z).is_solid()
    {
        return false;
    }
    crate::world::mob_ai::has_line_of_sight(&session.world, feet + Vec3::Y * 1.5, eye)
}

/// Snap a player's feet height to the block top it is standing on.
///
/// Physics leaves a resting player within a few ULPs of the integer block top.
/// A spot search that floors the raw float can therefore pick the block *below*
/// the feet on one run and the feet block on the next, which is exactly the
/// kind of sub-millimetre drift a pinned real-binary proof must not inherit.
/// The game has no half blocks, so the epsilon floor is the physical feet cell.
pub fn combat_spot_feet_y(y: f32) -> f32 {
    (y + 1e-3).floor()
}

/// How far below the current float feet the demo settles a body before its
/// spot search. The fresh spawn drops the body from terrain + 4, so four blocks
/// is the physically reachable fall; the extra margin only absorbs a raised
/// terrain edge under the same column.
pub const COMBAT_GROUND_SETTLE_BLOCKS: i32 = 8;

/// The block top physically under `position`, derived from the world.
///
/// Scans down from the column's float feet cell for the first solid block and
/// returns its top as the grounded feet height, or `None` when the column has
/// no support within [`COMBAT_GROUND_SETTLE_BLOCKS`]. The game has no half
/// blocks, so a body that lands there rests at an integer feet height; a body
/// still mid-fall keeps a fractional one. The distinction matters because the
/// spot rule floors the float feet: a mid-fall y floors into the *empty* cell
/// above the standing cell, so every candidate fails the air/air/solid guard
/// even though the body is about to land on a perfectly good block.
pub fn combat_grounded_feet_y(session: &WorldSession, position: Vec3) -> Option<f32> {
    if !position.is_finite() {
        return None;
    }
    let x = position.x.floor() as i32;
    let z = position.z.floor() as i32;
    let start = position.y.floor() as i32;
    let lowest = start.saturating_sub(COMBAT_GROUND_SETTLE_BLOCKS);
    for y in (lowest..=start).rev() {
        if session.world.get_block(x, y, z).is_solid() {
            return Some((y + 1) as f32);
        }
    }
    None
}

/// The Hostile stage: the slice's real-binary proof that a Zombie acquires the
/// player through line of sight, closes to melee range, and lands repeated
/// 3-damage hits at a 1.0 s cadence with knockback until the player dies with a
/// `Zombie` cause; then that `respawn()` returns to the deterministic spawn
/// with the whole inventory kept.
///
/// Runs entirely in ONE call, like Fight and Drops: every number here is
/// observable, and a redraw between two partial phases would let the mob
/// runtime move a record the next check describes. Every mutation goes through
/// a production seam — [`WorldSession::set_hostile_ai_enabled`],
/// [`WorldSession::tick_mobs`] (the same fixed 20 Hz runtime the frame loop
/// owns, which builds the `MobTarget`, runs the steering brain, decides the
/// melee swing and applies it), [`WorldSession::is_dead`],
/// [`WorldSession::last_damage_cause`], [`WorldSession::respawn`] and
/// [`WorldSession::spawn_position`] — so the demo proves the shipped path
/// instead of a parallel copy of it.
///
/// Three documented demo pins, not gameplay rules:
///
/// * the live Zombie is re-pinned at a chosen spot next to the player (the
///   Drops stage leaves the quartet pose, and the stage needs a measured
///   starting distance);
/// * the fresh-spawn body is settled onto the world's real block top before
///   the spot search, because release frame pacing can reach this stage while
///   the spawn fall is still mid-settle (debug has already landed by the same
///   frame count). On the debug profile this is a no-op — the body already
///   rests there — so every pinned digest stays byte-identical;
/// * the player pose is restored afterwards, so the Shot stage and the Save
///   witness start from the same pinned pose (the v9 layout itself is measured
///   by the fresh-leg test, never by this stage).
///
/// The Skeleton is left where Drops left it is hostile and will chase, but
/// it has no attack profile, so it can never add a damage event.
pub fn combat_hostile_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    let mobs = session.mobs().len();
    let zombies = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Zombie)
        .count();
    if mobs != 2 || zombies != 1 {
        demo.abort("hostile", &format!("mobs={mobs} zombies={zombies}"));
        return true;
    }
    if session.player.mode() != GameMode::Survival {
        demo.abort("hostile", &format!("mode={:?}", session.player.mode()));
        return true;
    }

    // Settle the demo pose before the spot search. A fresh world drops the body
    // from terrain + 4 and physics lands it on an integer block top, but release
    // frame pacing can reach this stage while the body is still mid-settle
    // (debug has already landed by the same frame count). The spot rule floors
    // the float feet, so a mid-fall y floors into the empty cell above the
    // standing cell and all eight candidates are rejected. Snapping onto the
    // world's real block top first is a no-op on the debug profile (the body
    // already rests there), which is what keeps every pinned digest and the
    // 317-byte v9 witness byte-identical; a mid-fall release pose becomes the
    // exact resting pose the debug run reaches instead of aborting the demo.
    let pose_before_settle = session.player.position;
    let Some(grounded_y) = combat_grounded_feet_y(session, pose_before_settle) else {
        demo.abort(
            "hostile",
            &format!(
                "observed=ground player=({:.3},{:.3},{:.3})",
                pose_before_settle.x, pose_before_settle.y, pose_before_settle.z
            ),
        );
        return true;
    };
    if pose_before_settle.y != grounded_y {
        session.player.position.y = grounded_y;
        session.player.velocity = Vec3::ZERO;
        session.player.on_ground = true;
        session.sync_camera();
    }

    // The respawn assertion compares against the pose the Save witness expects,
    // and the inventory total is the one number a respawn must not disturb.
    let pose = (
        session.player.position,
        session.player.yaw,
        session.player.pitch,
    );
    let items_before = combat_inventory_units_total(session);

    // Spot selection: two blocks along the player's yaw forward axis first,
    // then the same radius at 45-degree yaw offsets in a fixed order, so a
    // terrain edge cannot make the stage flaky while the choice stays
    // deterministic. The seed-1 demo pose is measured against this list.
    let feet_y = combat_spot_feet_y(session.player.position.y);
    let eye = session.player.eye_position();
    let mut spot = None;
    for step in 0..8u32 {
        let angle = session.player.yaw + step as f32 * std::f32::consts::FRAC_PI_4;
        let candidate = Vec3::new(
            session.player.position.x + angle.cos() * 2.0,
            feet_y,
            session.player.position.z + angle.sin() * 2.0,
        );
        if combat_hostile_spot_ok(session, candidate, eye) {
            spot = Some(candidate);
            break;
        }
    }
    let Some(spot) = spot else {
        demo.abort(
            "hostile",
            &format!(
                "observed=spot player=({:.3},{:.3},{:.3}) yaw={:.3} pitch={:.3} eye=({:.3},{:.3},{:.3})",
                session.player.position.x,
                session.player.position.y,
                session.player.position.z,
                session.player.yaw,
                session.player.pitch,
                eye.x,
                eye.y,
                eye.z
            ),
        );
        return true;
    };

    let player = session.player.position;
    let Some(zombie_index) = session
        .mobs()
        .iter()
        .position(|mob| mob.kind == MobKind::Zombie)
    else {
        demo.abort("hostile", "observed=zombie_missing");
        return true;
    };
    {
        let mob = &mut session.mobs_mut()[zombie_index];
        mob.position = spot;
        // Mob yaw 0 faces +Z, so `atan2(dx, dz)` turns it toward the player.
        mob.yaw = (player.x - spot.x).atan2(player.z - spot.z);
        mob.health = MobKind::Zombie.max_health();
        mob.state = MobAiState::Wander;
        mob.aggro = 0.0;
        mob.stuck = 0.0;
        mob.reaggro = 0.0;
        mob.cooldown = 0.0;
        mob.velocity = Vec3::ZERO;
        mob.knockback = Vec3::ZERO;
    }

    session.set_hostile_ai_enabled(true);
    let mut attacks = 0u32;
    let mut damage = 0u16;
    let mut knockback = false;
    for _ in 0..COMBAT_HOSTILE_ABORT_TICKS {
        let before = session.player.survival.health();
        session.tick_mobs(0.1, 1.0);
        let lost = before.saturating_sub(session.player.survival.health());
        if lost > 0 {
            // One accepted hit per player i-frame window, never more: a single
            // 20 Hz frame cannot land two 3-damage hits. The only accepted
            // shortfall is the saturating final hit (2 hp left, 3 requested),
            // which is also the hit that kills.
            if lost != 3 && !(session.is_dead() && lost < 3) {
                demo.abort("hostile", &format!("damage={lost}"));
                return true;
            }
            attacks += 1;
            damage = damage.saturating_add(lost);
            if session.player.combat.knockback().length_squared() > 1e-6 {
                knockback = true;
            }
        }
        if session.is_dead() {
            break;
        }
    }

    // 7 requested hits x 3 damage = 21, but the applied total is 20 because the
    // last hit saturates the 2 hp left. Report APPLIED damage, not requested.
    let cause = session.last_damage_cause();
    if !session.is_dead()
        || attacks != 7
        || damage != 20
        || !knockback
        || cause != Some(DamageCause::Zombie)
    {
        demo.abort(
            "hostile",
            &format!(
                "dead={} attacks={attacks} damage={damage} knockback={knockback} cause={:?}",
                session.is_dead(),
                cause
            ),
        );
        return true;
    }

    // Respawn through the production seam, then prove every post-condition the
    // Death screen and the HUD depend on.
    let spawn = WorldSession::spawn_position(demo.seed);
    session.respawn();
    let items_after = combat_inventory_units_total(session);
    let chasing = session
        .mobs()
        .iter()
        .any(|mob| mob.state == MobAiState::Chase);
    let alive = !session.is_dead();
    let health = session.player.survival.health();
    let position = session.player.position;
    let cause_cleared = session.last_damage_cause().is_none();
    let flash = session.player_hurt_intensity();
    if !alive
        || health != 20
        || position != spawn
        || !cause_cleared
        || flash != 0.0
        || items_after != items_before
        || chasing
    {
        demo.abort(
            "hostile",
            &format!(
                "respawn alive={alive} health={health} spawn={} cause={cause_cleared} flash={flash} items={items_after}/{items_before} chase={chasing}",
                position == spawn
            ),
        );
        return true;
    }

    // Documented demo pin: restore the pose the Save stage's witness and the
    // reload leg's quartet recomputation depend on. `respawn()` moved the body
    // to the world spawn; the saved player state must stay byte-identical.
    session.player.position = pose.0;
    session.player.yaw = pose.1;
    session.player.pitch = pose.2;
    session.sync_camera();
    session.set_hostile_ai_enabled(false);
    session.player.combat.reset();

    demo.hostile_attacks = attacks;
    demo.hostile_damage = damage;
    demo.hostile_knockback = knockback;
    demo.hostile_cause = true;
    demo.hostile_respawn = true;
    demo.hostile_inventory_kept = true;
    demo.hostile_digest = Some(demo.hostile_digest());
    demo.stage = CombatStage::Shot;
    false
}

/// The Shot stage: the real-binary proof that a bow charge fires through the
/// shipped ammo/wear seam and that the projectile resolves against both a body
/// and a block.
///
/// Runs entirely in ONE call, like Fight/Drops/Hostile: every number here is
/// observable, and a redraw between two partial phases could let the mob
/// runtime move a record the next check describes. The draw curve is driven by
/// the same pure [`BowState`] the input path owns, and the only host mutations
/// go through [`WorldSession::fire_bow`] and [`WorldSession::tick_arrows`] —
/// the exact seams the Playing frame calls — so the stage proves the shipped
/// path instead of a parallel copy.
///
/// Two documented demo pins, not gameplay rules:
///
/// * a real wood bow and four real arrows are granted through
///   [`crate::player::physics::Player::collect_items`] (the demo has no
///   crafting step of its own, and the save witness must carry the earned
///   bow);
/// * the Zombie is re-pinned two blocks along the yaw forward axis at the
///   player's feet height (the Fight stage's pig pin) with every combat timer
///   cleared, and the Skeleton is parked 20 blocks behind the camera so the
///   shot ray is unambiguous. [`combat_save_stage`] re-pins both survivors
///   afterwards, so nothing here can leak into the witness.
///
/// Every mismatch latches `stage=shot` with the named observed fact and ends
/// the run non-zero, so a partial proof can never be mistaken for a pass.
pub fn combat_shot_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    // Preflight: the two hostile survivors alone, at full health, alive.
    let mobs = session.mobs().len();
    let zombies = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Zombie)
        .count();
    let skeletons = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Skeleton)
        .count();
    if mobs != 2 || zombies != 1 || skeletons != 1 {
        demo.abort(
            "shot",
            &format!("mobs={mobs} zombies={zombies} skeletons={skeletons}"),
        );
        return true;
    }
    if session.player.mode() != GameMode::Survival {
        demo.abort("shot", &format!("mode={:?}", session.player.mode()));
        return true;
    }
    let health = session.player.survival.health();
    if session.is_dead() || health != 20 {
        demo.abort(
            "shot",
            &format!("dead={} health={health}", session.is_dead()),
        );
        return true;
    }

    // Grant the real bow and arrows through the production pickup seam.
    let bow_id = ItemId::Tool {
        kind: ToolKind::Bow,
        tier: ToolTier::Wood,
    };
    let arrow_id = ItemId::Material(Material::Arrow);
    let (Ok(bow), Ok(arrows)) = (Item::new(bow_id, 1), Item::new(arrow_id, 4)) else {
        demo.abort("shot", "observed=item");
        return true;
    };
    let grant = session.player.collect_items(&[bow, arrows]);
    if grant.lost != 0 {
        demo.abort("shot", &format!("grant_lost={}", grant.lost));
        return true;
    }
    let ammo_before = combat_inventory_units(session, arrow_id);
    if ammo_before != 4 {
        demo.abort("shot", &format!("ammo={ammo_before}"));
        return true;
    }
    let Some(bow_slot) = session
        .player
        .inventory()
        .slots()
        .iter()
        .position(|slot| slot.as_ref().map(|item| item.id()) == Some(bow_id))
    else {
        demo.abort("shot", "observed=bow_slot");
        return true;
    };
    if !session.player.select_slot(bow_slot)
        || session.player.selected_item().map(|item| item.id()) != Some(bow_id)
    {
        demo.abort("shot", &format!("held=slot{bow_slot}"));
        return true;
    }
    let bow_before = session
        .player
        .selected_item()
        .and_then(|item| item.durability());
    if bow_before != Some(ToolTier::Wood.max_durability()) {
        demo.abort("shot", &format!("bow={bow_before:?}"));
        return true;
    }

    // Demo pose pin: the Zombie two blocks forward at the player's feet height,
    // the Skeleton far behind the camera. Both records start at full health with
    // every combat timer cleared.
    let player_position = session.player.position;
    let forward = Vec3::new(session.player.yaw.cos(), 0.0, session.player.yaw.sin());
    let zombie_feet = Vec3::new(
        player_position.x + forward.x * 2.0,
        player_position.y,
        player_position.z + forward.z * 2.0,
    );
    {
        let mobs = session.mobs_mut();
        let Some(zombie) = mobs.iter_mut().find(|mob| mob.kind == MobKind::Zombie) else {
            demo.abort("shot", "observed=zombie_missing");
            return true;
        };
        zombie.position = zombie_feet;
        zombie.home = zombie_feet;
        zombie.health = MobKind::Zombie.max_health();
        zombie.state = MobAiState::Wander;
        zombie.velocity = Vec3::ZERO;
        zombie.knockback = Vec3::ZERO;
        zombie.hurt = 0.0;
        zombie.hurt_iframe = 0.0;
        zombie.cooldown = 0.0;
        zombie.flee = 0.0;
        zombie.death = 0.0;
        zombie.aggro = 0.0;
        zombie.stuck = 0.0;
        zombie.reaggro = 0.0;
        let Some(skeleton) = mobs.iter_mut().find(|mob| mob.kind == MobKind::Skeleton) else {
            demo.abort("shot", "observed=skeleton_missing");
            return true;
        };
        skeleton.position = player_position - forward * 20.0;
        skeleton.home = skeleton.position;
        skeleton.health = MobKind::Skeleton.max_health();
        skeleton.state = MobAiState::Wander;
        skeleton.velocity = Vec3::ZERO;
        skeleton.knockback = Vec3::ZERO;
        skeleton.hurt = 0.0;
        skeleton.hurt_iframe = 0.0;
        skeleton.cooldown = 0.0;
        skeleton.flee = 0.0;
        skeleton.death = 0.0;
        skeleton.aggro = 0.0;
        skeleton.stuck = 0.0;
        skeleton.reaggro = 0.0;
    }
    session.sync_camera();

    let Some(target) = session
        .player
        .selected_item()
        .and_then(|item| BowTarget::new(bow_slot, item))
    else {
        demo.abort("shot", "observed=bow_target");
        return true;
    };

    // 1. Sub-threshold release: a 0.1 s draw is below the 0.2 s minimum, so the
    //    release cancels and nothing is spent.
    let mut bow = BowState::new();
    let partial = bow.tick(0.1, Some(target));
    if partial != (BowTick::Charging { progress: 0.1 }) {
        demo.abort("shot", &format!("partial={partial:?}"));
        return true;
    }
    if bow.release().is_some() {
        demo.abort("shot", "observed=cancel_fired");
        return true;
    }
    let ammo_after_cancel = combat_inventory_units(session, arrow_id);
    let bow_after_cancel = session
        .player
        .selected_item()
        .and_then(|item| item.durability());
    if !session.arrows().is_empty()
        || ammo_after_cancel != ammo_before
        || bow_after_cancel != bow_before
    {
        demo.abort(
            "shot",
            &format!(
                "cancel arrows={} ammo={ammo_after_cancel} bow={bow_after_cancel:?}",
                session.arrows().len()
            ),
        );
        return true;
    }

    // 2. Full draw at ten 0.1 s ticks, then one real shot.
    let mut last = BowTick::Idle;
    for _ in 0..10 {
        last = bow.tick(0.1, Some(target));
    }
    if last != BowTick::Full {
        demo.abort("shot", &format!("charge={last:?}"));
        return true;
    }
    let Some(charge) = bow.release() else {
        demo.abort("shot", "observed=release");
        return true;
    };
    if (charge - 1.0).abs() > 1e-6 {
        demo.abort("shot", &format!("charge={charge}"));
        return true;
    }
    let origin = session.camera.position;
    let centre = zombie_feet + Vec3::Y * (MobKind::Zombie.height() * 0.5);
    let dir = (centre - origin).normalize_or_zero();
    let fired = match session.fire_bow(origin, dir, charge) {
        Ok(arrow) => arrow,
        Err(error) => {
            demo.abort("shot", &format!("fire={error:?}"));
            return true;
        }
    };
    let speed = fired.velocity.length();
    if fired.damage != 4 || (speed - 32.0).abs() > 1e-3 {
        demo.abort(
            "shot",
            &format!("arrow damage={} speed={speed}", fired.damage),
        );
        return true;
    }
    let ammo_after_first = combat_inventory_units(session, arrow_id);
    let bow_after_first = session
        .player
        .selected_item()
        .and_then(|item| item.durability());
    if session.arrows().len() != 1
        || ammo_after_first != 3
        || bow_after_first != Some(ToolTier::Wood.max_durability() - 1)
    {
        demo.abort(
            "shot",
            &format!(
                "first arrows={} ammo={ammo_after_first} bow={bow_after_first:?}",
                session.arrows().len()
            ),
        );
        return true;
    }

    // 3. One 0.1 s tick: the arrow reaches the Zombie, damages it, knocks it
    //    away from the shooter, and is spent.
    let hit_tick = session.tick_arrows(0.1, 1.0);
    let Some(zombie) = session
        .mobs()
        .iter()
        .find(|mob| mob.kind == MobKind::Zombie)
    else {
        demo.abort("shot", "observed=zombie_missing");
        return true;
    };
    let expected_health = MobKind::Zombie.max_health() - fired.damage;
    let away = Vec3::new(
        zombie.position.x - origin.x,
        0.0,
        zombie.position.z - origin.z,
    );
    if hit_tick.mob_hits != 1
        || !session.arrows().is_empty()
        || zombie.health != expected_health
        || zombie.hurt <= 0.0
        || zombie.knockback.length_squared() <= 1e-6
        || zombie.knockback.dot(away) <= 0.0
    {
        demo.abort(
            "shot",
            &format!(
                "hit hits={} arrows={} health={} hurt={} knockback={}",
                hit_tick.mob_hits,
                session.arrows().len(),
                zombie.health,
                zombie.hurt,
                zombie.knockback.length_squared() > 1e-6
            ),
        );
        return true;
    }
    let damage = fired.damage;

    // 4. A straight-down shot embeds in the ground and expires ~2 s later. The
    //    loop is bounded so a broken block test fails instead of hanging, and
    //    it requires both halves: an observed Embedded arrow after a block hit,
    //    and an empty list with a counted expiry.
    if session.player.selected_item().map(|item| item.id()) != Some(bow_id)
        && (!session.player.select_slot(bow_slot)
            || session.player.selected_item().map(|item| item.id()) != Some(bow_id))
    {
        demo.abort("shot", "observed=bow_reselect");
        return true;
    }
    if let Err(error) = session.fire_bow(session.camera.position, -Vec3::Y, 1.0) {
        demo.abort("shot", &format!("embed_fire={error:?}"));
        return true;
    }
    let mut embed_seen = false;
    let mut expired = 0u32;
    let mut emptied = false;
    for _ in 0..60 {
        let tick = session.tick_arrows(0.1, 1.0);
        if tick.block_hits >= 1
            && session
                .arrows()
                .iter()
                .any(|arrow| arrow.state == ArrowState::Embedded)
        {
            embed_seen = true;
        }
        expired = expired.saturating_add(tick.expired);
        if session.arrows().is_empty() {
            emptied = true;
            break;
        }
    }
    if !embed_seen || !emptied || expired == 0 {
        demo.abort(
            "shot",
            &format!(
                "embed seen={embed_seen} emptied={emptied} expired={expired} arrows={}",
                session.arrows().len()
            ),
        );
        return true;
    }
    let ammo_after = combat_inventory_units(session, arrow_id);
    let bow_after = session
        .player
        .selected_item()
        .and_then(|item| item.durability());
    if ammo_after != 2 || bow_after != Some(ToolTier::Wood.max_durability() - 2) {
        demo.abort("shot", &format!("ammo={ammo_after} bow={bow_after:?}"));
        return true;
    }

    demo.shot_charged = true;
    demo.shot_canceled = true;
    demo.shot_fired = 1;
    demo.shot_mob_hits = hit_tick.mob_hits;
    demo.shot_damage = damage;
    demo.shot_knockback = true;
    demo.shot_embed = true;
    demo.shot_expired = true;
    demo.shot_ammo = ammo_after;
    demo.shot_bow = bow_after;
    demo.shot_digest = Some(demo.shot_digest());
    demo.stage = CombatStage::Skeleton;
    false
}

/// One pinned survivor snapshot the Skeleton stage parks, re-pins, and restores.
///
/// The stage keys every write on `home` (never a list index): the synthetic
/// victim is removed by its `home`, so a reaped corpse or a record that streams
/// in can never shift the record the stage is proving.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CombatSkeletonSurvivor {
    pub kind: MobKind,
    pub position: Vec3,
    pub home: Vec3,
    pub yaw: f32,
    pub health: u16,
}

/// Write one survivor snapshot onto the record whose `home` is `key`.
///
/// Returns false when no record carries that key, so the caller latches an abort
/// instead of silently proving a world it does not own.
pub fn combat_skeleton_pin(
    session: &mut WorldSession,
    key: Vec3,
    survivor: &CombatSkeletonSurvivor,
) -> bool {
    let Some(mob) = session
        .mobs_mut()
        .iter_mut()
        .find(|mob| mob.home.distance_squared(key) <= 1e-6)
    else {
        return false;
    };
    mob.position = survivor.position;
    mob.home = survivor.home;
    mob.yaw = survivor.yaw;
    mob.health = survivor.health;
    mob.state = MobAiState::Wander;
    mob.velocity = Vec3::ZERO;
    mob.knockback = Vec3::ZERO;
    mob.hurt = 0.0;
    mob.hurt_iframe = 0.0;
    mob.cooldown = 0.0;
    mob.flee = 0.0;
    mob.death = 0.0;
    mob.aggro = 0.0;
    mob.stuck = 0.0;
    mob.stuck_anchor = survivor.position;
    mob.reaggro = 0.0;
    true
}

/// The Skeleton stage's deterministic stand-off spot finder.
///
/// Scans 16 angles around the player's facing, the five fire-band radii, and a
/// four-block vertical window for a cell that is a legal skeleton standing
/// place *and* whose rare-bow [`drop_roll`] lands the one-in-sixteen gate. The
/// scan order is fixed and no RNG or clock enters it, so the same world always
/// proves the same spot.
///
/// Acceptance is the production placement contract the brain re-checks every
/// step: feet and head blocks air, a solid block under the feet, an open eye
/// cell, and a clear ray from the skeleton's eye (feet + 1.5) to the player's
/// eye. The rare-bow clause is what lets the kill proof below exercise the real
/// drop table's rare branch instead of asserting a `false` the harness would
/// never see.
pub fn combat_skeleton_spot(session: &WorldSession, seed: u32) -> Option<Vec3> {
    let player_feet = session.player.position;
    let player_eye = session.player.eye_position();
    let bow = mob_drops(MobKind::Skeleton)
        .iter()
        .find(|drop| drop.item == MobDropItem::Bow)?;
    for step in 0..16u32 {
        let angle = session.player.yaw + step as f32 * (std::f32::consts::TAU / 16.0);
        for radius in [5.5_f32, 6.5, 7.5, 8.5, 9.5] {
            for offset in [-1.0_f32, 0.0, 1.0, 2.0] {
                let feet = Vec3::new(
                    player_feet.x + angle.cos() * radius,
                    player_feet.y + offset,
                    player_feet.z + angle.sin() * radius,
                );
                if !feet.is_finite() {
                    continue;
                }
                let cell = IVec3::new(
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                );
                if session.world.get_block(cell.x, cell.y, cell.z) != BlockId::Air
                    || session.world.get_block(cell.x, cell.y + 1, cell.z) != BlockId::Air
                    || !session
                        .world
                        .get_block(cell.x, cell.y - 1, cell.z)
                        .is_solid()
                {
                    continue;
                }
                let eye = feet + Vec3::Y * 1.5;
                let eye_cell = IVec3::new(
                    eye.x.floor() as i32,
                    eye.y.floor() as i32,
                    eye.z.floor() as i32,
                );
                if session
                    .world
                    .get_block(eye_cell.x, eye_cell.y, eye_cell.z)
                    .is_solid()
                {
                    continue;
                }
                if !crate::world::mob_ai::has_line_of_sight(&session.world, eye, player_eye) {
                    continue;
                }
                if bow.rolls(drop_roll(seed, MobKind::Skeleton, cell)) {
                    return Some(feet);
                }
            }
        }
    }
    None
}

/// The Skeleton stage: the slice's real-binary proof that a stand-off skeleton
/// fires a gravity-compensated arrow that damages the player and names
/// `Skeleton arrow` as the death cause, that the respawn keeps the inventory,
/// and that a player-killed skeleton delivers its bone/arrow/rare-bow table.
///
/// Runs entirely in ONE call, like every other stage: every number here is
/// observable, and a redraw between two partial phases could let the mob
/// runtime move a record the next check describes. Every mutation goes through
/// a production seam — [`WorldSession::set_hostile_ai_enabled`],
/// [`WorldSession::mobs_mut`], [`WorldSession::tick_mobs`] (the same fixed 20 Hz
/// runtime the frame loop owns), [`WorldSession::tick_arrows`],
/// [`WorldSession::fire_bow`], [`crate::player::survival::SurvivalState::damage`],
/// [`WorldSession::respawn`], [`WorldSession::spawn_position`],
/// [`WorldSession::last_damage_cause`], and
/// [`crate::ui::screens::death_cause_line`] — so the demo proves the shipped
/// path instead of a parallel copy of it.
///
/// Five documented demo pins, not gameplay rules:
///
/// * the body is settled onto the world's real block top before the spot
///   search, for the same reason the Hostile stage does it: release frame
///   pacing can reach this stage while the spawn fall is still mid-settle, and
///   the spot rule floors the float feet, so a mid-fall y floors into the empty
///   cell above the standing cell and every candidate is rejected. On the debug
///   profile the body already rests there, so the snap is a no-op and every
///   pinned digest stays byte-identical;
/// * the yaw and pitch are pinned to `Player::new`'s defaults before the scan,
///   because `combat_skeleton_spot` builds its 16-angle lattice from the live
///   yaw while the Playing frames feed Wayland mouse-motion deltas into it. A
///   few thousandths of drift can move the whole lattice off the rare-bow
///   candidates that pass the one-in-sixteen gate. Live mouse-look must not
///   move them while the stage runs;
/// * the two surviving pinned hostiles are parked 20 blocks behind the camera
///   and re-pinned at the top of every observation, so a wander can never bring
///   one inside the 16-block aggro range and steal the damage or death cause
///   the stage is proving;
/// * one synthetic victim `Mob::new(Skeleton, ..)` is pushed at a spot whose
///   [`drop_roll`] lands the rare bow, with `home` fixed at that spot (the drop
///   roll key) even when the kill proof re-pins its body two blocks forward;
/// * the player is dropped to 1 hp from 18 through
///   [`crate::player::survival::SurvivalState::damage`] so the *next*
///   skeleton arrow is the killing blow and the death cause is unambiguous.
///
/// Every mismatch latches `stage=skeleton` with the named observed fact and ends
/// the run non-zero, so a partial proof can never be mistaken for a pass.
pub fn combat_skeleton_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    // Preflight: the two hostile survivors alone, at full health, alive, holding
    // the bow the Shot stage earned.
    let mobs = session.mobs().len();
    let zombies = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Zombie)
        .count();
    let skeletons = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Skeleton)
        .count();
    if mobs != 2 || zombies != 1 || skeletons != 1 {
        demo.abort(
            "skeleton",
            &format!("mobs={mobs} zombies={zombies} skeletons={skeletons}"),
        );
        return true;
    }
    if session.player.mode() != GameMode::Survival {
        demo.abort("skeleton", &format!("mode={:?}", session.player.mode()));
        return true;
    }
    let health = session.player.survival.health();
    if session.is_dead() || health != 20 {
        demo.abort(
            "skeleton",
            &format!("dead={} health={health}", session.is_dead()),
        );
        return true;
    }
    let bow_id = ItemId::Tool {
        kind: ToolKind::Bow,
        tier: ToolTier::Wood,
    };
    if session.player.selected_item().map(|item| item.id()) != Some(bow_id) {
        demo.abort("skeleton", "observed=bow_not_selected");
        return true;
    }

    // Settle the demo pose before the spot search, exactly like the Hostile
    // stage. A fresh world drops the body from terrain + 4 and physics lands it
    // on an integer block top, but release frame pacing can reach this stage
    // while the body is still mid-settle (debug has already landed by the same
    // frame count). The spot rule floors the float feet, so a mid-fall y floors
    // into the empty cell above the standing cell and every candidate is
    // rejected. Snapping onto the world's real block top first is a no-op on
    // the debug profile (the body already rests there), which is what keeps
    // every pinned digest and the 317-byte v9 witness byte-identical.
    let pose_before_settle = session.player.position;
    let Some(grounded_y) = combat_grounded_feet_y(session, pose_before_settle) else {
        demo.abort(
            "skeleton",
            &format!(
                "observed=ground player=({:.3},{:.3},{:.3})",
                pose_before_settle.x, pose_before_settle.y, pose_before_settle.z
            ),
        );
        return true;
    };
    if pose_before_settle.y != grounded_y {
        session.player.position.y = grounded_y;
        session.player.velocity = Vec3::ZERO;
        session.player.on_ground = true;
        session.sync_camera();
    }

    // Pin the yaw/pitch the whole stage is measured against, after the settle so
    // the spot scan, the forward axis and the victim pose all agree. These are
    // `Player::new`'s defaults (src/player/physics.rs:280-281); live mouse-look
    // must not move them while the stage runs, because the rare-bow candidates
    // only pass the one-in-sixteen gate at the canonical lattice.
    session.player.yaw = -std::f32::consts::FRAC_PI_4;
    session.player.pitch = -0.25;
    session.sync_camera();

    let pose = (
        session.player.position,
        session.player.yaw,
        session.player.pitch,
    );
    let items_before = combat_inventory_units_total(session);
    let player_feet = session.player.position;
    let player_eye = session.player.eye_position();
    let forward = Vec3::new(session.player.yaw.cos(), 0.0, session.player.yaw.sin());

    // Demo pin 1: snapshot both survivors, then park them 20 blocks behind the
    // camera. The loop below re-pins the parked snapshot before every
    // observation, so their wander can never walk them into aggro range.
    let survivors: Vec<CombatSkeletonSurvivor> = session
        .mobs()
        .iter()
        .map(|mob| CombatSkeletonSurvivor {
            kind: mob.kind,
            position: mob.position,
            home: mob.home,
            yaw: mob.yaw,
            health: mob.health,
        })
        .collect();
    let parked: Vec<CombatSkeletonSurvivor> = survivors
        .iter()
        .map(|survivor| {
            let behind = player_feet - forward * 20.0;
            CombatSkeletonSurvivor {
                position: behind,
                home: behind,
                yaw: 0.0,
                ..*survivor
            }
        })
        .collect();
    for (survivor, pin) in survivors.iter().zip(parked.iter()) {
        if !combat_skeleton_pin(session, survivor.home, pin) {
            demo.abort("skeleton", "observed=survivor");
            return true;
        }
    }

    let Some(spot) = combat_skeleton_spot(session, demo.seed) else {
        demo.abort("skeleton", "observed=spot");
        return true;
    };
    let victim_home = spot;
    let victim_yaw = (player_feet.x - spot.x).atan2(player_feet.z - spot.z);
    session
        .mobs_mut()
        .push(Mob::new(MobKind::Skeleton, spot, victim_yaw));
    {
        let victim = session
            .mobs_mut()
            .last_mut()
            .expect("the victim was just pushed");
        victim.home = victim_home;
        victim.health = MobKind::Skeleton.max_health();
        victim.state = MobAiState::Wander;
        victim.cooldown = 0.0;
        victim.aggro = 0.0;
        victim.stuck = 0.0;
        victim.stuck_anchor = spot;
        victim.reaggro = 0.0;
        victim.velocity = Vec3::ZERO;
        victim.knockback = Vec3::ZERO;
    }

    // 1. Fire proof: open the gate and alternate the production mob and arrow
    //    ticks until one shot is decided and one accepted player hit lands.
    session.set_hostile_ai_enabled(true);
    let mut fired = 0u32;
    let mut player_hits = 0u32;
    let mut damage = 0u16;
    let mut arc = false;
    let mut arrow_ok = false;
    let mut pinned_ok = true;
    for _ in 0..COMBAT_SKELETON_ABORT_TICKS {
        for pin in &parked {
            pinned_ok &= combat_skeleton_pin(session, pin.home, pin);
        }
        let mob_tick = session.tick_mobs(0.1, 1.0);
        if !mob_tick.shots.is_empty() {
            fired = fired.saturating_add(mob_tick.shots.len() as u32);
            if let Some(shot) = mob_tick.shots.first() {
                // The aim is gravity-compensated, so the direction points above
                // the straight line to the player eye.
                let straight = (player_eye - shot.origin).normalize_or_zero();
                let aimed = shot.dir.normalize_or_zero();
                arc |= aimed.y > straight.y + 1e-4;
            }
            arrow_ok |= session.arrows().iter().any(|arrow| {
                arrow.owner == ArrowOwner::Skeleton && arrow.damage == ARROW_DAMAGE_SKELETON
            });
        }
        let arrow_tick = session.tick_arrows(0.1, 1.0);
        player_hits = player_hits.saturating_add(arrow_tick.player_hits);
        damage = damage.saturating_add(arrow_tick.player_damage);
        if fired > 0 && player_hits >= 1 {
            break;
        }
    }
    let cause = session.last_damage_cause();
    if !pinned_ok
        || fired == 0
        || player_hits != 1
        || damage != ARROW_DAMAGE_SKELETON
        || !arc
        || !arrow_ok
        || cause != Some(DamageCause::SkeletonArrow)
    {
        demo.abort(
            "skeleton",
            &format!(
                "fired={fired} player_hits={player_hits} damage={damage} arc={arc} arrow={arrow_ok} cause={cause:?} pinned={pinned_ok}"
            ),
        );
        return true;
    }

    // 2. Death-cause pin: drop the player to 1 hp and let the next skeleton
    //    arrow kill. The applied damage of the killing blow is 1, so the death
    //    itself is the assertion here, not a second full-size hit.
    let health = session.player.survival.health();
    if health != 20 - ARROW_DAMAGE_SKELETON {
        demo.abort("skeleton", &format!("health={health}"));
        return true;
    }
    let dropped = session.player.survival.damage(health - 1);
    if dropped != health - 1 || session.player.survival.health() != 1 {
        demo.abort(
            "skeleton",
            &format!(
                "drop applied={dropped} health={}",
                session.player.survival.health()
            ),
        );
        return true;
    }
    let mut died = false;
    for _ in 0..COMBAT_SKELETON_ABORT_TICKS {
        for pin in &parked {
            pinned_ok &= combat_skeleton_pin(session, pin.home, pin);
        }
        session.tick_mobs(0.1, 1.0);
        session.tick_arrows(0.1, 1.0);
        if session.is_dead() {
            died = true;
            break;
        }
    }
    let cause = session.last_damage_cause();
    let cause_line = death_cause_line(cause);
    if !died
        || !pinned_ok
        || cause != Some(DamageCause::SkeletonArrow)
        || cause_line != "Cause: Skeleton arrow"
    {
        demo.abort(
            "skeleton",
            &format!("death dead={died} cause={cause:?} line={cause_line}"),
        );
        return true;
    }

    // 3. Respawn through the production seam, then prove every post-condition
    //    the Death screen and the HUD depend on. A respawn clears in-flight
    //    projectiles, so the empty-list assert is part of the contract.
    let spawn = WorldSession::spawn_position(demo.seed);
    session.respawn();
    let respawned = session.player.position;
    let items_after = combat_inventory_units_total(session);
    let alive = !session.is_dead();
    let respawn_health = session.player.survival.health();
    let cause_cleared = session.last_damage_cause().is_none();
    let flash = session.player_hurt_intensity();
    let arrows_empty = session.arrows().is_empty();
    if !alive
        || respawn_health != 20
        || respawned != spawn
        || !cause_cleared
        || flash != 0.0
        || items_after != items_before
        || !arrows_empty
    {
        demo.abort(
            "skeleton",
            &format!(
                "respawn alive={alive} health={respawn_health} spawn={} cause={cause_cleared} flash={flash} items={items_after}/{items_before} arrows={}",
                respawned == spawn,
                session.arrows().len()
            ),
        );
        return true;
    }
    session.player.position = pose.0;
    session.player.yaw = pose.1;
    session.player.pitch = pose.2;
    session.sync_camera();

    // 4. Kill proof: re-pin the victim's body two blocks forward (its `home`
    //    stays at the rare spot, the drop-roll key), give it 4 health so one
    //    full-charge arrow kills it, and let the real projectile sweep deliver
    //    the player-attributed drop table.
    let arrow_id = ItemId::Material(Material::Arrow);
    let bones_before = combat_inventory_units(session, ItemId::Material(Material::Bone));
    let arrows_before = combat_inventory_units(session, arrow_id);
    if bones_before != 0 || arrows_before != 2 {
        demo.abort(
            "skeleton",
            &format!("ammo bones={bones_before} arrows={arrows_before}"),
        );
        return true;
    }
    let victim_feet = Vec3::new(
        pose.0.x + forward.x * 2.0,
        pose.0.y,
        pose.0.z + forward.z * 2.0,
    );
    let Some(victim) = session
        .mobs_mut()
        .iter_mut()
        .find(|mob| mob.home.distance_squared(victim_home) <= 1e-6)
    else {
        demo.abort("skeleton", "observed=victim_missing");
        return true;
    };
    victim.position = victim_feet;
    victim.yaw = (pose.0.x - victim_feet.x).atan2(pose.0.z - victim_feet.z);
    victim.health = 4;
    victim.state = MobAiState::Wander;
    victim.cooldown = 0.0;
    victim.aggro = 0.0;
    victim.stuck = 0.0;
    victim.stuck_anchor = victim_feet;
    victim.reaggro = 0.0;
    victim.velocity = Vec3::ZERO;
    victim.knockback = Vec3::ZERO;
    victim.hurt = 0.0;
    victim.hurt_iframe = 0.0;
    victim.death = 0.0;

    let Some(bow_slot) = session
        .player
        .inventory()
        .slots()
        .iter()
        .position(|slot| slot.as_ref().map(|item| item.id()) == Some(bow_id))
    else {
        demo.abort("skeleton", "observed=bow_slot");
        return true;
    };
    if session.player.selected_item().map(|item| item.id()) != Some(bow_id)
        && (!session.player.select_slot(bow_slot)
            || session.player.selected_item().map(|item| item.id()) != Some(bow_id))
    {
        demo.abort("skeleton", &format!("held=slot{bow_slot}"));
        return true;
    }
    let origin = session.camera.position;
    let centre = victim_feet + Vec3::Y * (MobKind::Skeleton.height() * 0.5);
    let dir = (centre - origin).normalize_or_zero();
    match session.fire_bow(origin, dir, 1.0) {
        Ok(arrow) if arrow.damage == 4 => {}
        Ok(arrow) => {
            demo.abort("skeleton", &format!("arrow damage={}", arrow.damage));
            return true;
        }
        Err(error) => {
            demo.abort("skeleton", &format!("fire={error:?}"));
            return true;
        }
    }
    let arrows_after_fire = combat_inventory_units(session, arrow_id);
    if arrows_after_fire != 1 {
        demo.abort("skeleton", &format!("arrows={arrows_after_fire}"));
        return true;
    }
    let mut kills = 0u32;
    let mut lost = 0u32;
    for _ in 0..COMBAT_SKELETON_ABORT_TICKS {
        let tick = session.tick_arrows(0.1, 1.0);
        kills = kills.saturating_add(tick.kills);
        lost = lost.saturating_add(tick.drops.lost);
        if kills >= 1 {
            break;
        }
    }
    let bones_after = combat_inventory_units(session, ItemId::Material(Material::Bone));
    let arrows_after = combat_inventory_units(session, arrow_id);
    let bows: Vec<Option<u16>> = session
        .player
        .inventory()
        .slots()
        .iter()
        .filter_map(|slot| slot.as_ref())
        .filter(|item| item.id() == bow_id)
        .map(|item| item.durability())
        .collect();
    let mut bows_sorted = bows.clone();
    bows_sorted.sort();
    // Three player shots wear the earned bow by one point each: two in the Shot
    // stage (59 -> 57) and this kill shot (57 -> 56). The dropped bow enters at
    // full durability, so the pair is exactly `[56, 59]`.
    let worn_bow = ToolTier::Wood.max_durability().saturating_sub(3);
    let expected_bows = vec![Some(worn_bow), Some(ToolTier::Wood.max_durability())];
    if kills != 1
        || lost != 0
        || bones_after != 1
        || arrows_after != 3
        || bows_sorted != expected_bows
    {
        demo.abort(
            "skeleton",
            &format!(
                "kill kills={kills} lost={lost} bones={bones_after} arrows={arrows_after} bows={bows:?}"
            ),
        );
        return true;
    }

    // Demo pin 3: remove the synthetic victim by its `home` (never an index),
    // close the gate, and restore the two pinned survivors byte for byte so the
    // Save stage owns a world it expects.
    session
        .mobs_mut()
        .retain(|mob| mob.home.distance_squared(victim_home) > 1e-6);
    session.set_hostile_ai_enabled(false);
    for (survivor, pin) in survivors.iter().zip(parked.iter()) {
        if !combat_skeleton_pin(session, pin.home, survivor) {
            demo.abort("skeleton", "observed=restore");
            return true;
        }
    }
    session.player.position = pose.0;
    session.player.yaw = pose.1;
    session.player.pitch = pose.2;
    session.player.combat.reset();
    session.sync_camera();
    if session.mobs().len() != 2 {
        demo.abort("skeleton", &format!("mobs={}", session.mobs().len()));
        return true;
    }

    demo.skeleton_fired = fired;
    demo.skeleton_player_hits = player_hits;
    demo.skeleton_damage = damage;
    demo.skeleton_arc = arc;
    demo.skeleton_killed = kills;
    demo.skeleton_bones = bones_after.saturating_sub(bones_before);
    demo.skeleton_arrows = arrows_after.saturating_sub(arrows_after_fire);
    demo.skeleton_bow = bows_sorted.len() == 2;
    demo.skeleton_respawn = true;
    demo.skeleton_digest = Some(demo.skeleton_digest());
    demo.stage = CombatStage::Creative;
    false
}

/// The Creative stage: the slice's real-binary proof that the effective player
/// mode, not the stored save mode, decides every mob interaction.
///
/// Runs entirely in ONE call, like Fight/Drops/Hostile/Shot/Skeleton: every
/// number here is observable, and a redraw between two partial phases would let
/// the mob runtime move a record the next check describes. Every mutation goes
/// through a production seam: [`WorldSession::use_creative_rules`],
/// [`WorldSession::set_hostile_ai_enabled`], [`WorldSession::tick_mobs`],
/// [`WorldSession::tick_arrows`], [`WorldSession::apply_mob_attacks`],
/// [`WorldSession::apply_mob_shots`], [`attack_damage`],
/// [`WorldSession::attack_mob`], [`WorldSession::resolve_mob_kill`],
/// [`WorldSession::spawn_arrow`], [`crate::player::physics::Player::consume_item`]
/// and [`WorldSession::sync_camera`].
///
/// The proof has four measured parts:
///
/// 1. **Inertness.** With the hostile gate OPEN and a Zombie pinned two blocks
///    away plus a full-health synthetic Zombie at the spot,
///    [`COMBAT_CREATIVE_INERT_TICKS`] fixed steps must decide zero attacks,
///    zero shots, apply zero player damage, leave the player at 20 health with
///    no cause and no hurt flash, spawn no arrows, and never put a mob in
///    `Chase`. The direct authority seams are then probed with hand-built
///    [`MobAttack`]/[`MobShot`] slices and must also refuse.
/// 2. **One-shot melee.** A no-weapon swing resolves through [`attack_damage`]
///    to exactly 20 and kills a full-health Zombie in one [`WorldSession::attack_mob`],
///    whose player-attributed [`WorldSession::resolve_mob_kill`] grants exactly
///    one rotten flesh.
/// 3. **One-shot arrow.** A charge-0 player arrow (base damage 1, so the kill
///    cannot be explained by charge) kills a second full-health Zombie through
///    the real projectile sweep, granting exactly one more rotten flesh.
/// 4. **Witness conservation.** Both earned rotten-flesh units are consumed
///    again and the inventory total must return to its pre-stage value, which
///    is what keeps the measured 317-byte v9 save byte-identical.
///
/// Documented demo pins, not gameplay rules: the two survivors are parked 20
/// blocks behind the camera (position *and* leash anchor, so the synthetic
/// victim's `home` is a unique key) and the whole mob vector is restored
/// byte-for-byte from a snapshot before the next stage; the player pose is
/// restored afterwards, so the Save witness starts from the same pinned pose.
pub fn combat_creative_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    // Preflight: the two hostile survivors alone, at full health, alive, in a
    // Survival world whose LIVE player is about to take Creative rules.
    let mobs = session.mobs().len();
    let zombies = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Zombie)
        .count();
    let skeletons = session
        .mobs()
        .iter()
        .filter(|mob| mob.kind == MobKind::Skeleton)
        .count();
    if mobs != 2 || zombies != 1 || skeletons != 1 {
        demo.abort(
            "creative",
            &format!("mobs={mobs} zombies={zombies} skeletons={skeletons}"),
        );
        return true;
    }
    let mode = session.player.mode();
    if mode != GameMode::Survival || session.save_mode != GameMode::Survival {
        demo.abort(
            "creative",
            &format!("mode={mode:?} save_mode={:?}", session.save_mode),
        );
        return true;
    }
    let health = session.player.survival.health();
    if session.is_dead() || health != 20 {
        demo.abort(
            "creative",
            &format!("dead={} health={health}", session.is_dead()),
        );
        return true;
    }
    if !session.arrows().is_empty() {
        demo.abort("creative", &format!("arrows={}", session.arrows().len()));
        return true;
    }
    session.set_ambient_spawning(false);

    let pose = (
        session.player.position,
        session.player.yaw,
        session.player.pitch,
    );
    let items_before = combat_inventory_units_total(session);
    let flesh_id = ItemId::Material(Material::RottenFlesh);
    let flesh_before = combat_inventory_units(session, flesh_id);
    let player_feet = session.player.position;
    let forward = Vec3::new(session.player.yaw.cos(), 0.0, session.player.yaw.sin());

    // Demo pin 1: snapshot both survivors, then park them 20 blocks behind the
    // camera. The leash anchor moves with the body so the parked pair stays put
    // (MEM449: a 4-block home leash would otherwise walk them back toward the
    // player) and the synthetic victim's `home` stays a unique key. The whole
    // vector is restored from this snapshot at the end, so the mutation never
    // reaches the save or the next stage.
    let survivors: Vec<Mob> = session.mobs().to_vec();
    let parked = player_feet - forward * 20.0;
    for mob in session.mobs_mut() {
        mob.position = parked;
        mob.home = parked;
    }

    // The effective-versus-stored distinction this whole slice is built on.
    session.use_creative_rules();
    if session.player.mode() != GameMode::Creative || session.save_mode != GameMode::Survival {
        demo.abort(
            "creative",
            &format!(
                "effective={:?} stored={:?}",
                session.player.mode(),
                session.save_mode
            ),
        );
        return true;
    }

    // Spot selection: the exact bounded 8-angle, 2.0-block loop the Hostile
    // stage uses, so the two stages cannot disagree about a legal spot. The
    // loop order is fixed and no RNG or clock enters it.
    let feet_y = combat_spot_feet_y(player_feet.y);
    let eye = session.player.eye_position();
    let mut spot = None;
    for step in 0..8u32 {
        let angle = session.player.yaw + step as f32 * std::f32::consts::FRAC_PI_4;
        let candidate = Vec3::new(
            player_feet.x + angle.cos() * 2.0,
            feet_y,
            player_feet.z + angle.sin() * 2.0,
        );
        if combat_hostile_spot_ok(session, candidate, eye) {
            spot = Some(candidate);
            break;
        }
    }
    let Some(spot) = spot else {
        demo.abort("creative", "observed=spot");
        return true;
    };
    let victim_yaw = (player_feet.x - spot.x).atan2(player_feet.z - spot.z);

    // 1. Inertness window. One synthetic full-health Zombie at the verified
    //    spot; 40 open-gate fixed steps must decide nothing at all.
    session
        .mobs_mut()
        .push(Mob::new(MobKind::Zombie, spot, victim_yaw));
    session.set_hostile_ai_enabled(true);
    let mut attacks = 0u32;
    let mut shots = 0u32;
    let mut damage = 0u16;
    let mut chased = false;
    for _ in 0..COMBAT_CREATIVE_INERT_TICKS {
        // Re-pin every body before observing, keyed on its `home` (the
        // survivors share the parked anchor, the victim owns the spot).
        for mob in session.mobs_mut() {
            mob.position = if mob.home.distance_squared(spot) <= 1e-6 {
                spot
            } else {
                parked
            };
        }
        let tick = session.tick_mobs(0.1, 1.0);
        attacks = attacks.saturating_add(tick.attacks.len() as u32);
        shots = shots.saturating_add(tick.shots.len() as u32);
        chased |= session
            .mobs()
            .iter()
            .any(|mob| mob.state == MobAiState::Chase);
        let arrow_tick = session.tick_arrows(0.1, 1.0);
        damage = damage.saturating_add(arrow_tick.player_damage);
        chased |= session
            .mobs()
            .iter()
            .any(|mob| mob.state == MobAiState::Chase);
        if session.player.survival.health() != 20 || !session.arrows().is_empty() {
            break;
        }
    }
    let health = session.player.survival.health();
    let cause = session.last_damage_cause();
    let flash = session.player_hurt_intensity();
    let arrows_empty = session.arrows().is_empty();
    if attacks != 0
        || shots != 0
        || damage != 0
        || chased
        || health != 20
        || cause.is_some()
        || flash != 0.0
        || !arrows_empty
    {
        demo.abort(
            "creative",
            &format!(
                "inert attacks={attacks} shots={shots} damage={damage} chase={chased} health={health} cause={cause:?} flash={flash} arrows={}",
                session.arrows().len()
            ),
        );
        return true;
    }

    // 1b. Direct authority proof: even a hand-built decision list must be
    //     refused by the two single-authority seams. Defense in depth, exactly
    //     the contract `tick_mobs`'s closed target build relies on.
    let melee_authority = session.apply_mob_attacks(&[MobAttack {
        kind: MobKind::Zombie,
        from: spot,
    }]);
    let shot_authority = session.apply_mob_shots(&[MobShot {
        kind: MobKind::Skeleton,
        origin: session.player.eye_position(),
        dir: forward,
    }]);
    let health = session.player.survival.health();
    if melee_authority != 0 || shot_authority != 0 || !session.arrows().is_empty() || health != 20 {
        demo.abort(
            "creative",
            &format!(
                "authority attack={melee_authority} shots={shot_authority} arrows={} health={health}",
                session.arrows().len()
            ),
        );
        return true;
    }

    // 2. Melee one-shot: a fist (no selected item) resolves to the target's own
    //    health in Creative, so one swing kills the full-health victim.
    let melee_damage = attack_damage(None, session.player.mode(), 20);
    if melee_damage != 20 {
        demo.abort("creative", &format!("melee_damage={melee_damage}"));
        return true;
    }
    let Some(victim_index) = session
        .mobs()
        .iter()
        .position(|mob| mob.home.distance_squared(spot) <= 1e-6)
    else {
        demo.abort("creative", "observed=victim_missing");
        return true;
    };
    let Some(hit) = session.attack_mob(victim_index, melee_damage, session.camera.position) else {
        demo.abort("creative", "observed=melee_refused");
        return true;
    };
    if !hit.killed || hit.health_after != 0 || hit.damage != melee_damage {
        demo.abort(
            "creative",
            &format!(
                "melee killed={} health={} damage={}",
                hit.killed, hit.health_after, hit.damage
            ),
        );
        return true;
    }
    let melee_drops = session.resolve_mob_kill(victim_index, hit, MobKillCause::Player);
    if melee_drops.accepted != 1 || melee_drops.lost != 0 {
        demo.abort(
            "creative",
            &format!(
                "melee_drops accepted={} lost={}",
                melee_drops.accepted, melee_drops.lost
            ),
        );
        return true;
    }
    let flesh_after_melee = combat_inventory_units(session, flesh_id);
    if flesh_after_melee != flesh_before + 1 {
        demo.abort(
            "creative",
            &format!("melee_flesh={flesh_after_melee}/{flesh_before}"),
        );
        return true;
    }
    session
        .mobs_mut()
        .retain(|mob| mob.home.distance_squared(spot) > 1e-6);

    // 3. Arrow one-shot: a charge-0 arrow (base damage 1) still kills a
    //    full-health Zombie through the Creative hit rule, and the real
    //    projectile sweep delivers the player-attributed drop table.
    session
        .mobs_mut()
        .push(Mob::new(MobKind::Zombie, spot, victim_yaw));
    let arrow_victim_health = session
        .mobs()
        .iter()
        .find(|mob| mob.home.distance_squared(spot) <= 1e-6)
        .map(|mob| mob.health)
        .unwrap_or(0);
    if arrow_victim_health != MobKind::Zombie.max_health() {
        demo.abort("creative", &format!("victim_health={arrow_victim_health}"));
        return true;
    }
    let eye = session.player.eye_position();
    let chest = spot + Vec3::Y * (MobKind::Zombie.height() * 0.5);
    let Some(arrow) = Arrow::player(eye, (chest - eye).normalize_or_zero(), 0.0) else {
        demo.abort("creative", "observed=arrow");
        return true;
    };
    if arrow.damage != 1 {
        demo.abort("creative", &format!("arrow_base={}", arrow.damage));
        return true;
    }
    if session.spawn_arrow(arrow).is_none() {
        demo.abort("creative", "observed=arrow_spawn");
        return true;
    }
    let mut mob_hits = 0u32;
    let mut arrow_kills = 0u32;
    let mut arrow_drops = MobDropReport::default();
    for _ in 0..COMBAT_CREATIVE_ABORT_TICKS {
        let tick = session.tick_arrows(0.1, 1.0);
        mob_hits = mob_hits.saturating_add(tick.mob_hits);
        arrow_kills = arrow_kills.saturating_add(tick.kills);
        arrow_drops.accepted = arrow_drops.accepted.saturating_add(tick.drops.accepted);
        arrow_drops.lost = arrow_drops.lost.saturating_add(tick.drops.lost);
        if mob_hits >= 1 {
            break;
        }
    }
    let arrow_victim_after = session
        .mobs()
        .iter()
        .find(|mob| mob.home.distance_squared(spot) <= 1e-6)
        .map(|mob| mob.health);
    let arrow_damage = arrow_victim_health.saturating_sub(arrow_victim_after.unwrap_or(0));
    let flesh_after_arrow = combat_inventory_units(session, flesh_id);
    if mob_hits != 1
        || arrow_kills != 1
        || arrow_drops.accepted != 1
        || arrow_drops.lost != 0
        || arrow_victim_after != Some(0)
        || arrow_damage != MobKind::Zombie.max_health()
        || !session.arrows().is_empty()
        || flesh_after_arrow != flesh_before + 2
    {
        demo.abort(
            "creative",
            &format!(
                "arrow hits={mob_hits} kills={arrow_kills} drops={} lost={} health={arrow_victim_after:?} damage={arrow_damage} arrows={} flesh={flesh_after_arrow}/{flesh_before}",
                arrow_drops.accepted,
                arrow_drops.lost,
                session.arrows().len()
            ),
        );
        return true;
    }
    session
        .mobs_mut()
        .retain(|mob| mob.home.distance_squared(spot) > 1e-6);

    // 4. Conserve the save witness: both earned units are consumed again, so
    //    the inventory section (and therefore the measured 317-byte v9 layout)
    //    is byte-identical.
    if session.player.consume_item(flesh_id).is_none()
        || session.player.consume_item(flesh_id).is_none()
    {
        demo.abort("creative", &format!("consume flesh={flesh_after_arrow}"));
        return true;
    }
    let items_after = combat_inventory_units_total(session);
    let flesh_left = combat_inventory_units(session, flesh_id);
    if items_after != items_before || flesh_left != flesh_before {
        demo.abort(
            "creative",
            &format!(
                "inventory items={items_after}/{items_before} flesh={flesh_left}/{flesh_before}"
            ),
        );
        return true;
    }

    // Restore before the next stage: close the gate, put the stored mode back,
    // restore the survivors byte-for-byte, and re-pin the player pose the Save
    // witness expects.
    session.set_hostile_ai_enabled(false);
    session.player.set_mode(session.save_mode);
    *session.mobs_mut() = survivors;
    session.player.position = pose.0;
    session.player.yaw = pose.1;
    session.player.pitch = pose.2;
    session.player.combat.reset();
    session.sync_camera();
    let mobs = session.mobs().len();
    let health = session.player.survival.health();
    let cause = session.last_damage_cause();
    let items_restored = combat_inventory_units_total(session);
    if mobs != 2
        || session.player.mode() != session.save_mode
        || session.is_dead()
        || health != 20
        || cause.is_some()
        || items_restored != items_before
    {
        demo.abort(
            "creative",
            &format!(
                "restore mobs={mobs} mode={:?}/{:?} dead={} health={health} cause={cause:?} items={items_restored}/{items_before}",
                session.player.mode(),
                session.save_mode,
                session.is_dead()
            ),
        );
        return true;
    }

    demo.creative_inert = true;
    demo.creative_attacks = attacks;
    demo.creative_shots = shots;
    demo.creative_damage = damage;
    demo.creative_melee_damage = melee_damage;
    demo.creative_arrow_damage = arrow_damage;
    demo.creative_kills = 2;
    demo.creative_drops = melee_drops.accepted.saturating_add(arrow_drops.accepted);
    demo.creative_drops_lost = melee_drops.lost.saturating_add(arrow_drops.lost);
    demo.creative_inventory_kept = true;
    demo.creative_digest = Some(demo.creative_digest());
    demo.stage = CombatStage::Save;
    false
}

/// Pin the two surviving hostile records and write the world through the one
/// production save site, then read the file back and require exactly those two
/// records.
///
/// The Drops stage removes the Pig and the Cow, so the witness is the two
/// hostile kinds alone: Zombie at 11 health and Skeleton at 14, each at its own
/// kind-indexed quartet pose and pinned yaw. Everything happens in one call so
/// no Playing tick can run between the pose assignment and the write. Any
/// failure latches `stage=save` with the named observed value instead of leaving
/// a half-written world.
pub fn combat_save_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    let (positions, _) = combat_quartet(session.player.position, session.player.yaw, demo.seed);
    if session.mobs().len() != 2 {
        demo.abort("save", &format!("mobs={}", session.mobs().len()));
        return true;
    }
    let mut survivors = 0u32;
    for mob in session.mobs_mut() {
        if !matches!(mob.kind, MobKind::Zombie | MobKind::Skeleton) {
            demo.abort("save", &format!("kind={:?}", mob.kind));
            return true;
        }
        let Some(index) = MobKind::ALL.iter().position(|kind| *kind == mob.kind) else {
            demo.abort("save", &format!("kind={:?}", mob.kind));
            return true;
        };
        mob.position = positions[index];
        mob.yaw = COMBAT_PINNED_YAW[index];
        mob.health = COMBAT_PINNED_HEALTH[index];
        survivors += 1;
    }
    if survivors != 2 {
        demo.abort("save", &format!("survivors={survivors}"));
        return true;
    }
    if let Err(error) = save_current_world(session) {
        demo.abort("save", &format!("error={error}"));
        return true;
    }
    let loaded = match load_world_from_path(&session.save_path) {
        Ok(loaded) => loaded,
        Err(error) => {
            demo.abort("save", &format!("error={error}"));
            return true;
        }
    };
    if loaded.mobs.len() != 2 {
        demo.abort("save", &format!("records={}", loaded.mobs.len()));
        return true;
    }
    for record in &loaded.mobs {
        if !matches!(record.kind, MobKind::Zombie | MobKind::Skeleton) {
            demo.abort("save", &format!("records=kind={:?}", record.kind));
            return true;
        }
        let Some(index) = MobKind::ALL.iter().position(|kind| *kind == record.kind) else {
            demo.abort("save", &format!("records=kind={:?}", record.kind));
            return true;
        };
        if record.position != positions[index].to_array()
            || record.yaw != COMBAT_PINNED_YAW[index]
            || record.health != COMBAT_PINNED_HEALTH[index]
        {
            demo.abort("save", &format!("records=mismatch={index}"));
            return true;
        }
    }
    demo.saved = 2;
    demo.records = loaded.mobs.len() as u32;
    demo.digest = Some(demo.success_digest());
    demo.stage = CombatStage::Done;
    true
}

/// Loaded-save leg: read the save, recompute the quartet from the RESTORED
/// player pose and seed, and require the two pinned hostile records plus the
/// earned inventory. The live session is only checked for kinds and health,
/// because a Playing tick may already have run `tick_mobs` and moved the mobs.
pub fn combat_restore_stage(demo: &mut CombatDemo, session: &mut WorldSession) -> bool {
    debug_assert!(!demo.fresh, "the Restore stage is only for a loaded save");
    let loaded = match load_world_from_path(&session.save_path) {
        Ok(loaded) => loaded,
        Err(error) => {
            demo.abort("restore", &format!("error={error}"));
            return true;
        }
    };
    if loaded.mobs.len() != 2 {
        // A v6 save (or any save with an empty mob tail) lands here.
        demo.abort("restore", &format!("mobs={}", loaded.mobs.len()));
        return true;
    }
    let (positions, _) = combat_quartet(
        Vec3::from_array(loaded.player.position),
        loaded.player.yaw,
        loaded.seed,
    );
    let mut kinds_seen = 0u32;
    let mut positions_ok = true;
    let mut yaw_ok = true;
    let mut health_ok = true;
    let mut health: Vec<String> = Vec::with_capacity(2);
    for record in &loaded.mobs {
        // The Drops stage kills the two passive kinds, so the witness must be
        // exactly the two hostile ones — never "two records of any kind".
        if !matches!(record.kind, MobKind::Zombie | MobKind::Skeleton) {
            demo.abort("restore", &format!("kind={:?}", record.kind));
            return true;
        }
        let Some(index) = MobKind::ALL.iter().position(|kind| *kind == record.kind) else {
            demo.abort("restore", &format!("kind={:?}", record.kind));
            return true;
        };
        kinds_seen += 1;
        health.push(COMBAT_PINNED_HEALTH[index].to_string());
        positions_ok &= record.position == positions[index].to_array();
        // The Save stage pins the facing to COMBAT_PINNED_YAW, not to the
        // pose-derived turn, so the restore comparison uses the same pin.
        yaw_ok &= record.yaw == COMBAT_PINNED_YAW[index];
        health_ok &= record.health == COMBAT_PINNED_HEALTH[index];
    }
    if kinds_seen != 2 {
        demo.abort("restore", &format!("kinds={kinds_seen}"));
        return true;
    }
    if !positions_ok {
        demo.abort("restore", "positions=false");
        return true;
    }
    if !yaw_ok {
        demo.abort("restore", "yaw=false");
        return true;
    }
    if !health_ok {
        demo.abort("restore", "health=false");
        return true;
    }
    let live = session.mobs();
    if live.len() != 2 {
        demo.abort("restore", &format!("mobs={}", live.len()));
        return true;
    }
    for kind in [MobKind::Zombie, MobKind::Skeleton] {
        let Some(index) = MobKind::ALL.iter().position(|candidate| *candidate == kind) else {
            demo.abort("restore", &format!("live_kind={kind:?}"));
            return true;
        };
        let Some(mob) = live.iter().find(|mob| mob.kind == kind) else {
            demo.abort("restore", &format!("live_kind={kind:?}"));
            return true;
        };
        if mob.health != COMBAT_PINNED_HEALTH[index] {
            demo.abort("restore", &format!("live_health={}", mob.health));
            return true;
        }
    }

    // The earned inventory is part of the witness: the wood sword the player
    // crafted (a full 59 durability), the wood bow the Shot stage earned and
    // wore twice (57) plus the full 59 the Skeleton stage's rare drop delivered
    // (worn to 56 by the kill shot), the three arrows left (two from the grant
    // minus the Shot stage's two shots, then the skeleton's two dropped), one
    // raw porkchop, one raw beef, the pair's three leather units, and the
    // skeleton's one bone — exactly eight non-empty stacks, scanned
    // order-independently across all 36 slots.
    let mut stacks = 0u32;
    let mut sword_durability: Option<Option<u16>> = None;
    let mut bow_durabilities: Vec<Option<u16>> = Vec::new();
    let mut arrows = 0u32;
    let mut porkchop = 0u32;
    let mut beef = 0u32;
    let mut leather = 0u32;
    let mut bone = 0u32;
    for item in loaded
        .player
        .inventory
        .slots()
        .iter()
        .filter_map(|slot| slot.as_ref())
    {
        stacks += 1;
        match item.id() {
            ItemId::Tool {
                kind: ToolKind::Sword,
                tier: ToolTier::Wood,
            } => {
                if sword_durability.is_some() {
                    demo.abort("restore", "items=duplicate_sword");
                    return true;
                }
                sword_durability = Some(item.durability());
            }
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            } => {
                // Two bows are expected: the earned, worn one and the
                // skeleton's rare full-durability drop. A third is drift.
                if bow_durabilities.len() >= 2 {
                    demo.abort("restore", "items=duplicate_bow");
                    return true;
                }
                bow_durabilities.push(item.durability());
            }
            ItemId::Material(Material::Arrow) => arrows += u32::from(item.count()),
            ItemId::Material(Material::RawPorkchop) => porkchop += u32::from(item.count()),
            ItemId::Material(Material::RawBeef) => beef += u32::from(item.count()),
            ItemId::Material(Material::Leather) => leather += u32::from(item.count()),
            ItemId::Material(Material::Bone) => bone += u32::from(item.count()),
            other => {
                demo.abort("restore", &format!("items=unexpected={other:?}"));
                return true;
            }
        }
    }
    if stacks != 8 {
        demo.abort("restore", &format!("items={stacks}"));
        return true;
    }
    if sword_durability != Some(Some(ToolTier::Wood.max_durability())) {
        demo.abort("restore", &format!("items=sword={sword_durability:?}"));
        return true;
    }
    // Three player shots wear the earned bow by exactly one point each (two in
    // the Shot stage, one on the Skeleton stage's victim); the dropped bow
    // enters at full durability (MEM392 ordering).
    let worn_bow = ToolTier::Wood.max_durability().saturating_sub(3);
    bow_durabilities.sort();
    let expected_bows = vec![Some(worn_bow), Some(ToolTier::Wood.max_durability())];
    if bow_durabilities != expected_bows {
        demo.abort("restore", &format!("items=bows={bow_durabilities:?}"));
        return true;
    }
    if arrows != 3 || porkchop != 1 || beef != 1 || leather != 3 || bone != 1 {
        demo.abort(
            "restore",
            &format!(
                "items=arrows={arrows} porkchop={porkchop} beef={beef} leather={leather} bone={bone}"
            ),
        );
        return true;
    }

    demo.digest = Some(format!(
        "combat_demo: seed={} stage=restored mobs={} health={} yaw={} positions={} items={} ok=true",
        demo.seed,
        live.len(),
        health.join("/"),
        yaw_ok,
        positions_ok,
        stacks
    ));
    demo.stage = CombatStage::Done;
    true
}
