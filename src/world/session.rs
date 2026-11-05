//! The replaceable active world session: the CPU-side world state
//! (`WorldManager` + `Player` + `Camera`) plus the save path every save
//! site writes to. Holding the camera inside the session lets the swap
//! atomically replace view state too. This module is pure lib code — no
//! I/O, no winit/wgpu — so the session-boundary contracts are unit-testable
//! without a window or GPU (D008 layering: S03-T02 wires this into
//! GameApp the way S01's lifecycle contracts were wired in S02).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use glam::{IVec3, Vec3};

use crate::config::{ConfigRuntime, RulesBundle, SaveConfigMeta};
use crate::content::faces::BlockFaces;
use crate::content::{ContentRegistry, WorldgenRule};
use crate::mesh::quad::ChunkMesh;
use crate::player::camera::Camera;
use crate::player::combat::DamageCause;
use crate::player::container::Container;
use crate::player::inventory::{
    ChestSlots, InventoryError, Item, ItemId, Material, ToolKind, ToolTier, CHEST_SLOTS,
};
use crate::player::melee::MeleeHit;
use crate::player::physics::Player;
use crate::player::raycast::raycast_voxels;
use crate::player::survival::{
    GameMode, SurvivalContext, SurvivalRuntime, SurvivalState, SurvivalTick,
};
use crate::world::arrow::{
    arrow_hit_damage, segment_aabb, Arrow, ArrowOwner, ArrowState, ARROW_CAP,
};
use crate::world::block::BlockId;
use crate::world::chunk::{world_to_chunk_and_local, ChunkPos};
use crate::world::daycycle::GameClock;
use crate::world::farming::CropRuntime;
use crate::world::loot::{wheat_harvest, LootEntry};
use crate::world::manager::WorldManager;
use crate::world::mob::{Mob, MobHit, MobKind, MobRuntime, MobShot, MobTick};
use crate::world::mob_ai::{MobAttack, MobTarget};
use crate::world::mob_loot::{drop_roll, mob_drops, MobDropReport, MobKillCause};
use crate::world::persistence::{ChestMap, ChunkEditMap, LoadedWorld, WorldSaveData};
use crate::world::season::{season_of, Season};
use crate::world::spawning::{
    SpawnContext, SpawnReport, SpawnRuntime, SpawnWorld, SPAWN_MAX_RADIUS,
};
use crate::world::terrain::{find_spawn_column, ruin_loot_chest_at, terrain_height_at_seeded};

/// Maximum eye-to-block distance (blocks) at which a placed crafting table is
/// reachable. Shared with the placement/mining ray length so every world
/// interaction uses one reach budget.
pub const TABLE_INTERACT_REACH: f32 = 8.0;

/// Creator-tunable interaction reach.
///
/// Owned here beside [`TABLE_INTERACT_REACH`] so the mining.toml interaction
/// row does not force `player::tools` to import `world::session`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InteractionRules {
    /// Maximum eye-to-block distance for table interaction, in blocks.
    pub table_reach: f32,
}

impl Default for InteractionRules {
    fn default() -> Self {
        Self {
            table_reach: TABLE_INTERACT_REACH,
        }
    }
}

/// One-shot notice for a v9 world saved under the same config schema but a
/// different effective-value digest.
pub const CONFIG_CHANGED_NOTICE: &str = "config changed since this world was saved";

/// One-shot notice for a v9 world whose config schema differs from the
/// running one; every effective value may still look the same by name.
pub const CONFIG_SCHEMA_CHANGED_NOTICE: &str = "config schema changed since this world was saved";

/// Notice matrix for one saved-meta/current-meta pair.
///
/// A schema mismatch wins over a digest mismatch: the digest is only
/// comparable within one schema version, so the caller must be told the more
/// actionable fact first.
fn config_notice_for(saved: SaveConfigMeta, current: SaveConfigMeta) -> Option<String> {
    if saved.schema != current.schema {
        Some(CONFIG_SCHEMA_CHANGED_NOTICE.to_string())
    } else if saved.digest != current.digest {
        Some(CONFIG_CHANGED_NOTICE.to_string())
    } else {
        None
    }
}

/// Why [`WorldSession::fire_bow`] refused a shot.
///
/// Every variant is returned with **zero mutation**: the refusal matrix is
/// checked in full before the arrow list, the inventory, or the bow's
/// durability can change.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BowFireError {
    /// The player has zero health.
    Dead,
    /// The selected hotbar item is not a wood bow.
    NotHoldingBow,
    /// The 36 inventory slots hold no `Arrow` to spend.
    NoAmmo,
    /// [`ARROW_CAP`] projectiles are already resident (embedded ones count).
    ArrowCapReached,
    /// [`Arrow::player`] rejected the origin, direction, or charge.
    InvalidShot,
}

/// Counters for one [`WorldSession::tick_arrows`] call.
///
/// The entity-facing fields are filled by the entity sweep inside
/// [`WorldSession::tick_arrows`]: a body consumes the arrow before the block
/// test runs, so `mob_hits`/`player_hits` and `block_hits` are mutually
/// exclusive for a single arrow on a single substep.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ArrowTick {
    /// Arrows that embedded into a solid block this tick.
    pub block_hits: u32,
    /// Arrows that hit a mob this tick.
    pub mob_hits: u32,
    /// Arrows that hit the player this tick.
    pub player_hits: u32,
    /// Mob kills credited to a player-fired arrow this tick.
    pub kills: u32,
    /// Units delivered (or lost) by this tick's attributed mob kills.
    pub drops: MobDropReport,
    /// Damage applied to the player by skeleton arrows this tick.
    pub player_damage: u16,
    /// Arrows removed because their lifetime (flying or embedded) ran out.
    pub expired: u32,
}

/// The active world session GameApp holds (S03-T02). One always-present
/// session is swapped atomically at the `start_world_session` boundary.
pub struct WorldSession {
    pub world: WorldManager,
    pub player: Player,
    pub camera: Camera,
    /// The effective typed gameplay tuning this session simulates with.
    ///
    /// Installed by [`WorldSession::apply_rules`] at startup and after every
    /// accepted live reload, then read by the session's gameplay seams.
    /// Ephemeral: never serialized, so a save never pins a tuning value.
    gameplay_rules: RulesBundle,
    /// The session-owned game clock. Every save/load boundary restores it
    /// from persisted game time, never from the wall clock.
    pub clock: GameClock,
    /// Length of one season in game days, persisted with the world and
    /// consumed by the pure [`crate::world::season`] calendar. New worlds
    /// start at [`DEFAULT_SEASON_LENGTH_DAYS`]; the New Game row and the
    /// `--season-length` seam override it.
    pub season_length_days: u32,
    /// The mode a save writes. Deliberately separate from `player.mode()`:
    /// Edit Background applies Creative rules to the live player while the
    /// world's stored mode must survive the next save unchanged.
    pub save_mode: GameMode,
    /// Persisted chest contents keyed by absolute block position. Cloned only
    /// at save time; the frame loop never iterates it.
    pub chests: ChestMap,
    pub save_path: PathBuf,
    /// The immutable content registry this session was built with. Every save
    /// names its references through this registry, and the load path freezes
    /// the registry it resolved against here too. A live pack-stack change
    /// never replaces it (MEM512/D181); the next loaded world does.
    pub registry: Arc<ContentRegistry>,
    /// Config metadata this session writes with. Fresh sessions write the
    /// registry defaults; a loaded world inherits the running config after
    /// [`WorldSession::apply_config`], and the next save names it.
    save_config: SaveConfigMeta,
    /// Config metadata the loaded world carried, or `None` for fresh sessions
    /// and v1-v8 saves. Ephemeral: the pending notice and this record are
    /// consumed together by [`WorldSession::take_config_notice`].
    saved_config: Option<SaveConfigMeta>,
    /// Ephemeral changed-stack note computed once at load time and consumed by
    /// [`WorldSession::take_content_notice`]. Never persisted and never
    /// re-derived per frame.
    content_notice: Option<String>,
    /// Ephemeral crafting-station state: the position of the table whose
    /// interface is open, if any. Deliberately not persisted (`SAVE_VERSION`
    /// unchanged) and reset by both constructors, so a restart always starts
    /// with no table open.
    open_table: Option<IVec3>,
    /// Ephemeral chest-station state: the position of the chest whose
    /// interface is open, if any. Never persisted; chest *contents* live in
    /// [`WorldSession::chests`]. Mutually exclusive with `open_table`.
    open_chest: Option<IVec3>,
    /// Pure game-time survival rules and their fractional accumulators.
    /// Ephemeral: a load or respawn starts from a fresh runtime.
    survival_runtime: SurvivalRuntime,
    /// Ephemeral wheat growth progress. Only the visible stage block is
    /// persisted, as an ordinary chunk edit in `WorldManager::chunk_edits`;
    /// growth progress itself is never saved. Constructors start empty and
    /// [`WorldSession::session_from_save`] rebuilds candidates from the saved
    /// edits at progress zero.
    crops: CropRuntime,
    /// Set only for a fresh Survival session (and respawn): player physics is
    /// gated until the chunks containing the feet and four blocks below are
    /// resident. Ephemeral and never saved.
    spawn_settle: bool,
    /// Flat session-owned mob list (D131). Live records are persisted in the
    /// v7 save tail and restored by [`WorldSession::session_from_save`].
    /// Records are never dropped by simulation.
    mobs: Vec<Mob>,
    /// Ephemeral in-flight and embedded projectiles, owned for the lifetime of
    /// the session. **Never persisted**: the v7 save tail is mob-only and an
    /// arrow in flight is ephemeral exactly like a mob's knockback impulse, so
    /// a reload always starts with an empty list.
    arrows: Vec<Arrow>,
    /// Fixed-rate 20 Hz mob driver, seeded from the session's world seed.
    /// Ephemeral like `survival_runtime`.
    mob_runtime: MobRuntime,
    /// Ephemeral hostile-AI demo switch, never persisted. Enabled by default
    /// for every production session; the `--demo-combat` seam closes it at
    /// startup and reopens it only inside its dedicated `Hostile` stage.
    ///
    /// It stops target selection (and therefore acquisition and attacks) but
    /// deliberately does **not** stop wandering, the distance cull, or the
    /// ambient gate. It is explicitly **not** the Creative rule: Creative
    /// inertness is a game-mode authority owned by S09, while this switch is
    /// demo sequencing state.
    hostile_ai: bool,
    /// Deterministic mob population layer (M009/S05): spawn attempts, the
    /// global and per-region caps, the distance cull, and daylight burning.
    /// Ephemeral like `mob_runtime` — a reload restarts the attempt sequence,
    /// so a save can never carry a spawn cursor.
    spawn_runtime: SpawnRuntime,
    /// Cached deterministic world-spawn feet position ([`Self::spawn_position`]),
    /// computed once per construction by the same pure land search a fresh
    /// spawn already pays for.
    ///
    /// The spawner protects this anchor with its own safe radius, so it must
    /// stay the *world* spawn: it deliberately does not follow the player, the
    /// Edit Background anchor, or a saved player position.
    world_spawn: Vec3,
    /// The single ambient cause writer for the last player damage event.
    ///
    /// Ephemeral and **never persisted** (the v7 wire format has no cause
    /// byte): a loaded dead save therefore has no cause and the Death screen
    /// renders "Unknown". Written in exactly three places —
    /// [`WorldSession::apply_mob_attacks`] (always `Zombie`, the mob-melee
    /// seam), [`WorldSession::tick_survival`] (the survival rules' own
    /// `Fall`/`Drowning`/`Starvation`), and [`WorldSession::tick_arrows`]
    /// (`SkeletonArrow`, the projectile seam) — and cleared by
    /// [`WorldSession::respawn`]. Three writers, never three authorities: the
    /// last one is a different damage path, not a second writer for the same
    /// one.
    last_damage_cause: Option<DamageCause>,
}

impl WorldSession {
    /// A fresh world, exactly mirroring main()'s fresh-world branch:
    /// render distance 16, the given seed, the requested season length, day 0
    /// at the default time of day, no chunk edits, and the player spawned 4
    /// blocks above the deterministic land-search column (D071: the old fixed
    /// (0, 0) column can be open ocean after the biome rewrite).
    pub fn new_session(seed: u32, save_path: PathBuf, season_length_days: u32) -> WorldSession {
        // Every pre-M008 CLI demo and test keeps the Creative/flying default.
        Self::new_session_with_mode(seed, save_path, season_length_days, GameMode::Creative)
    }

    /// A fresh world whose persisted and effective mode is `mode`.
    ///
    /// Survival spawns a walking player with full vitals; Creative inherits
    /// [`Player::new`]'s flying default. Every other field mirrors
    /// [`WorldSession::new_session`], so the New Game mode row (T05) can pick a
    /// mode without a second spawn/seed path.
    pub fn new_session_with_mode(
        seed: u32,
        save_path: PathBuf,
        season_length_days: u32,
        mode: GameMode,
    ) -> WorldSession {
        Self::new_session_with_mode_and_registry(
            seed,
            save_path,
            season_length_days,
            mode,
            ContentRegistry::builtin_arc(),
        )
    }

    /// A fresh world whose persisted and effective mode is `mode`, resolved
    /// against `registry`.
    ///
    /// The registry is frozen for the session's lifetime (MEM512/D181).
    /// Production callers pass the registry built from the launch pack stack;
    /// [`WorldSession::new_session_with_mode`] is the built-in wrapper for
    /// every existing test and demo.
    pub fn new_session_with_mode_and_registry(
        seed: u32,
        save_path: PathBuf,
        season_length_days: u32,
        mode: GameMode,
        registry: Arc<ContentRegistry>,
    ) -> WorldSession {
        let mut player = Player::new_with_mode(Self::spawn_position(seed), mode);
        // The player starts on the deterministic world spawn, so the cached
        // anchor is that same position: no second land search.
        let world_spawn = player.position;
        if mode == GameMode::Survival {
            // The deterministic spawn is four blocks above the surface, so the
            // first dry landing is discarded instead of costing health while
            // the spawn chunks stream in.
            player.arm_spawn_immunity();
        }
        // Build the face table from the registry *before* the registry moves
        // into the session, so mesh workers never hold the registry itself.
        let faces = Arc::new(BlockFaces::from_registry(&registry));
        let rules: Arc<[WorldgenRule]> = Arc::from(registry.worldgen_rules());
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            player,
            camera: Camera::new(Vec3::ZERO),
            gameplay_rules: RulesBundle::default(),
            clock: GameClock::new(),
            season_length_days,
            save_mode: mode,
            chests: ChestMap::default(),
            save_path,
            registry,
            save_config: ConfigRuntime::defaults().meta(),
            saved_config: None,
            content_notice: None,
            open_table: None,
            open_chest: None,
            survival_runtime: SurvivalRuntime::default(),
            crops: CropRuntime::default(),
            spawn_settle: mode == GameMode::Survival,
            mobs: Vec::new(),
            arrows: Vec::new(),
            mob_runtime: MobRuntime::new(seed),
            hostile_ai: true,
            spawn_runtime: SpawnRuntime::new(),
            world_spawn,
            // A fresh session has taken no damage: no cause is recorded, so a
            // Death screen can never name a cause the player did not earn.
            last_damage_cause: None,
        };
        session.world.set_block_faces(faces);
        session.world.set_worldgen_rules(rules);
        session.sync_camera();
        session
    }

    /// The deterministic fresh-world spawn: the land-search column center at
    /// terrain height + 4 (D071). Shared by every fresh-session constructor
    /// and [`WorldSession::respawn`], so the +4 rule has one source of truth.
    pub fn spawn_position(seed: u32) -> Vec3 {
        let (spawn_x, spawn_z) = find_spawn_column(seed);
        let spawn_y = (terrain_height_at_seeded(spawn_x, spawn_z, seed) + 4) as f32;
        Vec3::new(spawn_x as f32 + 0.5, spawn_y, spawn_z as f32 + 0.5)
    }

    /// A fresh world starting at a caller-supplied position and clock.
    ///
    /// Used by M012's Edit Background: the editor starts exactly at the menu
    /// scene's anchor (feet below the saved eye) and at the scene's clock,
    /// instead of the seed's deterministic spawn column. Every other field
    /// mirrors [`WorldSession::new_session`]; the caller applies settings FOV
    /// and render distance at the session-swap boundary like any other swap.
    pub fn new_at(
        seed: u32,
        save_path: PathBuf,
        season_length_days: u32,
        position: Vec3,
        clock: GameClock,
    ) -> WorldSession {
        Self::new_at_with_registry(
            seed,
            save_path,
            season_length_days,
            position,
            clock,
            ContentRegistry::builtin_arc(),
        )
    }

    /// A fresh world starting at a caller-supplied position and clock, resolved
    /// against `registry`.
    ///
    /// [`WorldSession::new_at`] is the built-in wrapper; production callers
    /// pass the launch pack registry so the editor save names modded content.
    pub fn new_at_with_registry(
        seed: u32,
        save_path: PathBuf,
        season_length_days: u32,
        position: Vec3,
        clock: GameClock,
        registry: Arc<ContentRegistry>,
    ) -> WorldSession {
        // Build the face table from the registry *before* the registry moves
        // into the session, so mesh workers never hold the registry itself.
        let faces = Arc::new(BlockFaces::from_registry(&registry));
        let rules: Arc<[WorldgenRule]> = Arc::from(registry.worldgen_rules());
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            player: Player::new(position),
            camera: Camera::new(Vec3::ZERO),
            gameplay_rules: RulesBundle::default(),
            clock,
            season_length_days,
            // Edit Background is a Creative-rules editor; a stored world mode
            // only enters through `session_from_save`.
            save_mode: GameMode::Creative,
            chests: ChestMap::default(),
            save_path,
            registry,
            save_config: ConfigRuntime::defaults().meta(),
            saved_config: None,
            content_notice: None,
            open_table: None,
            open_chest: None,
            survival_runtime: SurvivalRuntime::default(),
            crops: CropRuntime::default(),
            // Edit Background is a Creative-rules editor and is never gated.
            spawn_settle: false,
            mobs: Vec::new(),
            arrows: Vec::new(),
            mob_runtime: MobRuntime::new(seed),
            hostile_ai: true,
            spawn_runtime: SpawnRuntime::new(),
            // The editor anchor is not the world spawn: the safe radius must
            // protect the seed's deterministic spawn pad, so a load pays the
            // same single pure land search a fresh session already pays.
            world_spawn: Self::spawn_position(seed),
            last_damage_cause: None,
        };
        session.world.set_block_faces(faces);
        session.world.set_worldgen_rules(rules);
        session.sync_camera();
        session
    }

    /// Reconstruct a session from persisted save data: player
    /// position/yaw/pitch copied, the legacy fly byte normalized through the
    /// stored mode, the checked vitals and chest records restored, the
    /// validated inventory aggregate and both crafting grids restored,
    /// `selected_slot` clamped to the 9-slot hotbar, the clock restored to
    /// the saved day index and time of day, the saved season length carried
    /// through, the stored mode kept separately in `save_mode`, and the world
    /// rebuilt with the saved render distance, seed, and chunk edits.
    ///
    /// `Player::with_inventory` and `restore_selected_slot` are the only
    /// restore entry points, so a save can never publish an out-of-range
    /// selection or a grid stored under the wrong kind. `set_mode` runs after
    /// the raw fly assignment, so a Survival record cannot restore flight.
    pub fn session_from_save(data: WorldSaveData, save_path: PathBuf) -> WorldSession {
        Self::session_from_save_with_registry(
            data,
            save_path,
            ContentRegistry::builtin_arc(),
            None,
            None,
        )
    }

    /// Rebuild a session from a registry-aware load, preserving the placeholder
    /// registry and the one-shot changed-stack notice.
    ///
    /// The notice is computed by [`LoadedWorld::content_notice`] before the
    /// loaded world is destructured, then stored ephemerally: it is presented
    /// once by the UI and never persisted.
    pub fn from_loaded_world(loaded: LoadedWorld, save_path: PathBuf) -> WorldSession {
        let content_notice = loaded.content_notice();
        let saved_config = loaded.saved_config;
        Self::session_from_save_with_registry(
            loaded.data,
            save_path,
            Arc::new(loaded.registry),
            content_notice,
            saved_config,
        )
    }

    /// Takes the pending changed-stack notice, if any.
    ///
    /// At most one consumer sees it; the notice is never re-derived and never
    /// persisted.
    pub fn take_content_notice(&mut self) -> Option<String> {
        self.content_notice.take()
    }

    fn session_from_save_with_registry(
        data: WorldSaveData,
        save_path: PathBuf,
        registry: Arc<ContentRegistry>,
        content_notice: Option<String>,
        saved_config: Option<SaveConfigMeta>,
    ) -> WorldSession {
        let WorldSaveData {
            seed,
            time_of_day,
            day_index,
            season_length_days,
            mode,
            player: saved,
            chests,
            mobs: saved_mobs,
            chunk_edits,
        } = data;

        // Both calendar fields are persisted world state: the clock resumes
        // at the exact day/time and the season length survives the reload.
        let clock = GameClock::from_day_and_time(day_index, time_of_day);
        let mut player = Player::with_inventory(
            Vec3::from_array(saved.position),
            saved.inventory,
            saved.player_grid,
            saved.table_grid,
        );
        player.yaw = saved.yaw;
        player.pitch = saved.pitch;
        // Normalise the legacy fly byte through the stored mode before
        // `set_mode`, so a Survival save whose raw byte says flying still
        // restores a walking player. `set_mode` then enforces the rule again.
        player.fly_mode = saved.fly_mode && mode.allows_fly();
        player.set_mode(mode);
        player.survival = saved.survival;
        player.restore_selected_slot(saved.selected_slot as usize);
        // Rebuild growth candidates from the persisted stage edits *before*
        // the edit map is moved into the world manager. Progress restarts at
        // zero: only the visible stage survives a save.
        let crops = CropRuntime::from_edits(&chunk_edits);
        // Restore the live mob records from the v7 tail. Each record re-anchors
        // `home` at the saved feet position and keeps the saved health, so
        // damage survives a reload; every ephemeral runtime field (velocity,
        // AI state, hurt/cooldown timers, walk phase) restarts at its neutral
        // value.
        let mobs: Vec<Mob> = saved_mobs
            .into_iter()
            .map(|record| {
                Mob::restored(
                    record.kind,
                    Vec3::from_array(record.position),
                    record.yaw,
                    record.health,
                )
            })
            .collect();
        // S03 appends placeholders while decoding, so the table must be built
        // from the already-augmented registry (a known pack block still maps
        // to its authored layer; an unresolved name maps to the checker).
        let faces = Arc::new(BlockFaces::from_registry(&registry));
        let rules: Arc<[WorldgenRule]> = Arc::from(registry.worldgen_rules());
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(
                saved.render_distance as i32,
                seed,
                chunk_edits,
            ),
            player,
            camera: Camera::new(Vec3::ZERO),
            gameplay_rules: RulesBundle::default(),
            clock,
            season_length_days,
            save_mode: mode,
            chests,
            save_path,
            registry,
            // A loaded session first presents its saved config through the
            // one-shot notice; `apply_config` then installs the running meta
            // the next save writes.
            save_config: ConfigRuntime::defaults().meta(),
            saved_config,
            content_notice,
            open_table: None,
            open_chest: None,
            survival_runtime: SurvivalRuntime::default(),
            crops,
            // A restored player is never spawn-gated: the visible world state
            // is the save's, and a mid-air fall must still hurt.
            spawn_settle: false,
            mobs,
            // Projectiles are never persisted: a reload starts in flight-free
            // air, exactly like a fresh session.
            arrows: Vec::new(),
            // The fixed-rate driver deliberately restarts at tick 0: a reload
            // is a deterministic restart of the AI clock, not a continuation
            // of the pre-save tick index.
            mob_runtime: MobRuntime::new(seed),
            hostile_ai: true,
            // The population layer restarts too (ephemeral gate, accumulators,
            // attempt cursor, report), so a reload never resumes a spawn
            // sequence or inherits a closed demo gate.
            spawn_runtime: SpawnRuntime::new(),
            // A saved player position is not the world spawn: the safe radius
            // must protect the seed's deterministic spawn pad, exactly like
            // `new_at`. One pure land search at construction is the cost.
            world_spawn: Self::spawn_position(seed),
            // The cause is deliberately NOT restored: it has no wire byte, so
            // a loaded dead save starts causeless and renders "Unknown".
            last_damage_cause: None,
        };
        session.world.set_block_faces(faces);
        session.world.set_worldgen_rules(rules);
        session.sync_camera();
        session
    }

    /// Applies Creative movement rules to the live player without changing the
    /// mode a save writes. Edit Background uses this so editing a Survival
    /// world cannot silently rewrite that world to Creative on the next save.
    pub fn use_creative_rules(&mut self) {
        self.player.set_mode(GameMode::Creative);
    }

    /// Whether the session's player has reached zero health.
    ///
    /// A dead player is inert for survival rules but remains a fully valid
    /// session; the Death screen (T04/T05) owns what the player sees next.
    pub fn is_dead(&self) -> bool {
        self.player.survival.health() == 0
    }

    /// Whether player physics may run this frame.
    ///
    /// Creative and restored sessions are never gated. A fresh Survival
    /// session (or respawn) waits until both the chunk containing the feet and
    /// the chunk containing four blocks below it are resident, so the +4 spawn
    /// drop cannot fall through unloaded (Air) chunks. The flag clears once
    /// and every later call reports ready.
    pub fn settle_spawn(&mut self) -> bool {
        if !self.spawn_settle {
            return true;
        }
        let feet_x = self.player.position.x.floor() as i32;
        let feet_y = self.player.position.y.floor() as i32;
        let feet_z = self.player.position.z.floor() as i32;
        let support_y = (self.player.position.y - 4.0).floor() as i32;
        let (feet_chunk, _) = world_to_chunk_and_local(feet_x, feet_y, feet_z);
        let (support_chunk, _) = world_to_chunk_and_local(feet_x, support_y, feet_z);
        if self.world.is_resident(feet_chunk) && self.world.is_resident(support_chunk) {
            self.spawn_settle = false;
            true
        } else {
            false
        }
    }

    /// Advances the pure survival rules by one frame of game time.
    ///
    /// `dt` is clamped to `0..=0.1` exactly like [`Player::update`], then
    /// scaled by `time_scale` (negative scales become zero). The player's
    /// one-shot landing distance is consumed exactly once and handed to
    /// [`SurvivalRuntime`], which owns the defensive `MAX_TICK_GAME_SECS`
    /// clamp. Creative and dead players are inert.
    pub fn tick_survival(
        &mut self,
        dt: f32,
        time_scale: f32,
        head_submerged: bool,
    ) -> SurvivalTick {
        let dt = dt.clamp(0.0, 0.1);
        let game_dt = dt * time_scale.max(0.0);
        let ctx = SurvivalContext {
            mode: self.player.mode(),
            head_submerged,
            fall_distance: self.player.take_landing_fall(),
        };
        // Copy the Copy rule slice before the mutable vitals borrow.
        let survival_rules = self.gameplay_rules.survival;
        let tick = self.survival_runtime.tick_with(
            &mut self.player.survival,
            ctx,
            game_dt,
            &survival_rules,
        );
        // The survival rules are the ONLY writers of the `Fall`/`Drowning`/
        // `Starvation` causes (the mob-melee seam is the only writer of
        // `Zombie`). A tick that applied no damage never touches the tag, so an
        // idle tick leaves the last real cause intact.
        if tick.damage > 0 {
            if let Some(cause) = tick.cause {
                self.last_damage_cause = Some(cause);
            }
        }
        tick
    }

    /// The session's flat mob list (D131). Renderer and diagnostics read it.
    #[inline]
    pub fn mobs(&self) -> &[Mob] {
        &self.mobs
    }

    /// Mutable access for spawning and save restore. Simulation reaps only
    /// finished corpses through [`WorldSession::retain_live_mobs`].
    #[inline]
    pub fn mobs_mut(&mut self) -> &mut Vec<Mob> {
        &mut self.mobs
    }

    /// Removes every mob record that is finished, returning how many records
    /// were dropped.
    ///
    /// This is still THE one removal site for mob records. Two kinds of record
    /// leave here:
    ///
    /// * a finished corpse — dead (`health == 0`) with the collapse timer
    ///   fully drained ([`Mob::death_finished`]); and
    /// * a record the spawner marked with [`Mob::despawn_pending`] because it
    ///   drifted past [`crate::world::spawning::DESPAWN_RADIUS`]. The spawner
    ///   only ever *marks*; no other code removes a record, so a mob can never
    ///   vanish without passing through this function.
    ///
    /// Because [`Mob::death`] decays only while the mob's feet chunk is
    /// resident, a mid-fade corpse in an unloaded chunk freezes and can
    /// legally be saved as a health-0 record; it vanishes on the first tick
    /// after its chunk reloads. Drops are granted by the killer, never by the
    /// reaper.
    pub fn retain_live_mobs(&mut self) -> usize {
        let before = self.mobs.len();
        self.mobs
            .retain(|mob| !mob.death_finished() && !mob.despawn_pending);
        before - self.mobs.len()
    }

    /// Whether the chunk containing the mob's feet is resident. A mob in an
    /// unloaded chunk stays in the list but is frozen until it streams in.
    pub fn mob_is_resident(&self, mob: &Mob) -> bool {
        position_is_resident(&self.world, mob.position)
    }

    /// Every mob whose feet chunk is currently resident. Ordering matches
    /// [`WorldSession::mobs`], so render batch and entity list stay aligned.
    pub fn resident_mobs(&self) -> impl Iterator<Item = &Mob> + '_ {
        self.mobs.iter().filter(|mob| self.mob_is_resident(mob))
    }

    // -----------------------------------------------------------------------
    // Projectiles (S07)
    // -----------------------------------------------------------------------

    /// The session-owned projectile list: flying and embedded arrows alike.
    ///
    /// Ordering is spawn order. The renderer batches this list directly, and
    /// embedded arrows stay in it until their embed timer drains.
    #[inline]
    pub fn arrows(&self) -> &[Arrow] {
        &self.arrows
    }

    /// Whether the chunk containing the arrow's floor cell is resident.
    ///
    /// The exact gate [`WorldSession::mob_is_resident`] uses: an arrow whose
    /// position is in an unloaded chunk is frozen in place rather than
    /// colliding with the world manager's implicit Air, so it can never tunnel
    /// through terrain that simply has not streamed in yet.
    pub fn arrow_is_resident(&self, arrow: &Arrow) -> bool {
        position_is_resident(&self.world, arrow.position)
    }

    /// Pushes an already-built arrow into the session list, returning its
    /// index.
    ///
    /// The shared spawn seam for both the player's bow and skeleton arrows
    /// (S08): [`WorldSession::apply_mob_shots`] funnels every decided shot
    /// through here rather than pushing into the list itself. It refuses (and
    /// mutates nothing) when the arrow is already [`Arrow::finished`] — a
    /// lifetime already spent, or non-finite position/velocity — or when the
    /// list is at [`ARROW_CAP`]. Embedded arrows count toward the cap until
    /// they expire, so the cap bounds both motion and render work.
    pub fn spawn_arrow(&mut self, arrow: Arrow) -> Option<usize> {
        if arrow.finished() || self.arrows.len() >= ARROW_CAP {
            return None;
        }
        self.arrows.push(arrow);
        Some(self.arrows.len() - 1)
    }

    /// Fires one arrow from the player's wood bow, returning the spawned
    /// arrow.
    ///
    /// Every guard runs **before** the first mutation, so each refusal leaves
    /// the arrow list, the inventory, and the bow's durability untouched. The
    /// checks are ordered from cheapest/most authoritative to most work:
    ///
    /// 1. [`WorldSession::is_dead`] → [`BowFireError::Dead`];
    /// 2. the selected item must be a wood bow →
    ///    [`BowFireError::NotHoldingBow`] (there is deliberately no game-mode
    ///    gate: a Creative player may fire, and S09 owns Creative damage);
    /// 3. [`Arrow::player`] must accept the origin/direction/charge →
    ///    [`BowFireError::InvalidShot`];
    /// 4. the list must be below [`ARROW_CAP`] →
    ///    [`BowFireError::ArrowCapReached`] (a refused shot spends no ammo and
    ///    no wear);
    /// 5. exactly one `Arrow` must be consumed by identity from the 36 slots
    ///    → [`BowFireError::NoAmmo`].
    ///
    /// Only then is the arrow pushed and the bow worn exactly once. The wear
    /// result is ignored (MEM392): the shot is already in flight, so a bow
    /// that breaks on its last durability point still fired. Ammo may come
    /// from any slot, never the cursor or either crafting grid.
    pub fn fire_bow(
        &mut self,
        origin: Vec3,
        dir: Vec3,
        charge: f32,
    ) -> Result<Arrow, BowFireError> {
        if self.is_dead() {
            return Err(BowFireError::Dead);
        }
        let holding_bow = matches!(
            self.player.selected_item().map(|item| item.id()),
            Some(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            })
        );
        if !holding_bow {
            return Err(BowFireError::NotHoldingBow);
        }
        let arrow = Arrow::player_with_rules(origin, dir, charge, &self.gameplay_rules.arrow)
            .ok_or(BowFireError::InvalidShot)?;
        if self.arrows.len() >= ARROW_CAP {
            return Err(BowFireError::ArrowCapReached);
        }
        if self
            .player
            .consume_item(ItemId::Material(Material::Arrow))
            .is_none()
        {
            return Err(BowFireError::NoAmmo);
        }
        self.arrows.push(arrow);
        // Mutate-then-wear (MEM392): the arrow exists now, so a bow that
        // breaks on this point of wear must not roll the shot back.
        let _ = self.player.wear_selected_tool();
        Ok(arrow)
    }

    /// Advances every projectile by one frame of game time.
    ///
    /// Exactly the [`WorldSession::tick_mobs`] clamp contract: `dt` is clamped
    /// to `0.0..=0.1`, then scaled by `time_scale` (negative becomes zero), and
    /// non-finite/non-positive game time is a no-op.
    ///
    /// The list is taken out with [`std::mem::take`] so the world and the
    /// entity lists stay independently borrowable while arrows are mutated.
    /// Per arrow:
    ///
    /// * non-resident → kept unchanged (frozen at the chunk boundary, never
    ///   collided against unloaded Air);
    /// * `Embedded` → one `advance(game_dt)`, draining the embed timer;
    /// * `Flying` → [`Arrow::substeps`] substeps of `game_dt / substeps`. A
    ///   32 b/s arrow moves 3.2 blocks in a 0.1 s frame, so point sampling
    ///   would tunnel through a one-block wall; 0.25-block substeps cannot.
    ///
    /// **Substep order: move, then entities, then blocks.** Each substep
    /// saves `before`, advances, restores the position and freezes when the
    /// destination chunk is not resident, sweeps every resident live mob (and
    /// the player for a skeleton arrow), and only tests the destination block
    /// when no body was hit — "hit the body, not the wall". A body consumes
    /// the arrow whether or not the target accepted the damage: an
    /// i-frame-refused arrow still hit the body and is spent.
    ///
    /// Two exclusions are deliberate:
    ///
    /// * a mob whose AABB **contains `before`** is skipped, so an arrow can
    ///   never hit the body it starts inside. This stops a shooter from
    ///   self-hitting through its own target and is the rule S08 relies on;
    /// * a player-owned arrow never tests the player, so only skeletons hurt
    ///   the player with projectiles.
    ///
    /// Mobs killed by a player-fired arrow are resolved after the arrow list
    /// is restored: [`WorldSession::resolve_mob_kill`] needs `&mut self` for
    /// the inventory grant, so it cannot run while `&mut self.mobs` is held.
    /// A player-fired arrow is player-attributed exactly like a melee swing, so
    /// [`crate::player::physics::Player::collect_items`] and its overflow
    /// notice are reused unchanged. A skeleton arrow killing a mob grants
    /// nothing: only a player earns the table.
    ///
    /// Skeleton-arrow player damage goes through
    /// [`crate::player::combat::PlayerCombat::take_hit`], the session's only
    /// player-damage authority, so i-frames, the hurt flash, the knockback
    /// impulse and the Creative/Survival gate all come from S06. On an applied
    /// hit this method is also the single writer of the
    /// [`DamageCause::SkeletonArrow`] cause tag.
    ///
    /// **Creative one-shot rule.** The effective mode is read once per tick
    /// (`player.mode()`, not the stored `save_mode`) and applied only to
    /// player-owned arrows through [`arrow_hit_damage`]: a Creative player's
    /// arrow lands the mob's remaining health instead of its charge damage, so
    /// every surviving mob dies in one hit. Skeleton arrows are deliberately
    /// outside the rule — the entity sweep does not filter by owner, so an
    /// unscoped rule would one-shot a skeleton's friendly fire in a Creative
    /// session — and keep their pinned 2 damage. The kill still flows through
    /// [`WorldSession::resolve_mob_kill`] exactly like a melee kill, so the
    /// pinned drop table is granted unchanged.
    ///
    /// Finished arrows (spent lifetime, or non-finite from a corrupt advance)
    /// are dropped and counted into [`ArrowTick::expired`]. Embedded arrows
    /// still count toward [`ARROW_CAP`] until they expire.
    pub fn tick_arrows(&mut self, dt: f32, time_scale: f32) -> ArrowTick {
        let dt = dt.clamp(0.0, 0.1);
        let game_dt = dt * time_scale.max(0.0);
        let mut report = ArrowTick::default();
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return report;
        }

        let arrows = std::mem::take(&mut self.arrows);
        let mut kept = Vec::with_capacity(arrows.len());
        // Copy the Copy rule slices before the split mutable borrows below.
        let arrow_rules = self.gameplay_rules.arrow;
        let combat_rules = self.gameplay_rules.combat;
        // Kills are buffered, not granted, while the mob list is mutably
        // borrowed; `resolve_mob_kill` needs `&mut self` for the drop grant.
        let mut pending_kills: Vec<(usize, MobHit)> = Vec::new();

        // Read the effective mode once: the rule is a hit-time property, not
        // something stored on the arrow, and `save_mode` may differ under Edit
        // Background. Scoped to player arrows at the call site.
        let creative_player = self.player.mode() == GameMode::Creative;

        // Split-borrow shape, exactly like `tick_mobs`: terrain is immutable
        // while the entity lists and the player's combat/vitals are mutable.
        // `last_damage_cause` is a disjoint field and stays writable here.
        let world = &self.world;
        let mobs = &mut self.mobs;
        let player = &mut self.player;

        for mut arrow in arrows {
            if !position_is_resident(world, arrow.position) {
                kept.push(arrow);
                continue;
            }
            let mut consumed = false;
            match arrow.state {
                ArrowState::Embedded => arrow.advance_with_rules(game_dt, &arrow_rules),
                ArrowState::Flying => {
                    let substeps = arrow.substeps(game_dt);
                    let sub_dt = game_dt / substeps as f32;
                    for _ in 0..substeps {
                        let before = arrow.position;
                        arrow.advance_with_rules(sub_dt, &arrow_rules);
                        if !position_is_resident(world, arrow.position) {
                            // The next cell lives in a chunk that has not
                            // streamed in: restore the last known-good point
                            // and freeze instead of colliding with Air.
                            arrow.position = before;
                            break;
                        }
                        let delta = arrow.position - before;

                        // 1. Entity sweep. A body consumes the arrow, so the
                        //    block test below is skipped entirely.
                        if let Some(index) = nearest_arrow_mob(mobs, world, before, delta) {
                            let damage = arrow_hit_damage(
                                arrow.damage,
                                creative_player && arrow.owner == ArrowOwner::Player,
                                mobs[index].health,
                            );
                            let hit = mobs[index].take_hit(damage, before);
                            report.mob_hits += 1;
                            // Only a player's arrow earns the drop table; the
                            // hit itself lands for either owner.
                            if arrow.owner == ArrowOwner::Player {
                                if let Some(hit) = hit {
                                    if hit.killed {
                                        pending_kills.push((index, hit));
                                    }
                                }
                            }
                            consumed = true;
                            break;
                        }

                        // 2. Player sweep, skeleton arrows only.
                        if arrow.owner == ArrowOwner::Skeleton {
                            let target = player.aabb();
                            if segment_aabb(before, delta, target.min, target.max).is_some() {
                                // Read the player's own state before the
                                // disjoint mutable borrows below.
                                let mode = player.mode();
                                let feet = player.position;
                                let applied = player.combat.take_hit_with_rules(
                                    &mut player.survival,
                                    arrow.damage,
                                    mode,
                                    DamageCause::SkeletonArrow,
                                    feet,
                                    before,
                                    &combat_rules,
                                );
                                if let Some(applied) = applied {
                                    self.last_damage_cause = Some(DamageCause::SkeletonArrow);
                                    report.player_damage =
                                        report.player_damage.saturating_add(applied.damage);
                                    report.player_hits += 1;
                                }
                                // A refused hit (Creative, dead, i-frame, zero
                                // damage) still spends the arrow.
                                consumed = true;
                                break;
                            }
                        }

                        // 3. Block test at the destination only.
                        let cell = arrow.position.floor();
                        let block = world.get_block(cell.x as i32, cell.y as i32, cell.z as i32);
                        if block.is_solid() {
                            arrow.embed_with_rules(&arrow_rules);
                            report.block_hits += 1;
                            break;
                        }
                    }
                }
            }
            if consumed {
                continue;
            }
            if arrow.finished() {
                report.expired += 1;
            } else {
                kept.push(arrow);
            }
        }
        self.arrows = kept;

        // Borrow-safe kill attribution: the entity sweep is over, so each
        // buffered kill can be granted through the single loot seam.
        for (index, hit) in pending_kills {
            let grant = self.resolve_mob_kill(index, hit, MobKillCause::Player);
            report.drops.accepted = report.drops.accepted.saturating_add(grant.accepted);
            report.drops.lost = report.drops.lost.saturating_add(grant.lost);
            report.kills += 1;
        }
        report
    }

    /// The nearest resident, *live* mob the crosshair ray enters within
    /// `reach`.
    ///
    /// This is the one melee hit-test seam: it walks the live mob list once
    /// through [`crate::player::melee::nearest_mob_hit`] and applies the exact
    /// same residency gate as rendering and the fixed-rate runtime, so a mob
    /// in an unloaded chunk can never be hit through terrain that is not
    /// loaded. A dead record (`health == 0`) is skipped too, so a collapsing
    /// corpse can never be selected and never suppresses mining. No candidate
    /// `Vec` is allocated.
    pub fn melee_candidate(&self, origin: Vec3, dir: Vec3, reach: f32) -> Option<MeleeHit> {
        crate::player::melee::nearest_mob_hit(
            origin,
            dir,
            reach,
            self.mobs
                .iter()
                .enumerate()
                .filter(|(_, mob)| mob.health > 0 && self.mob_is_resident(mob)),
        )
    }

    /// Applies one melee hit to mob `index` and reports the outcome.
    ///
    /// Every guard lives in one place: a missing index and a non-resident mob
    /// are refused here, while a dead record (`health == 0`), an open i-frame
    /// window, and zero damage are refused by [`Mob::take_hit`]. A hit never
    /// removes or reorders a record: a lethal hit arms the collapse and leaves
    /// the record in [`WorldSession::mobs`] until [`WorldSession::retain_live_mobs`]
    /// reaps it after the fade.
    pub fn attack_mob(&mut self, index: usize, damage: u16, from: Vec3) -> Option<MobHit> {
        let mob = self.mobs.get(index)?;
        if !self.mob_is_resident(mob) {
            return None;
        }
        // The immutable borrow ends here; the index is re-checked against the
        // same list, which neither hit test nor this call can change shape.
        self.mobs.get_mut(index)?.take_hit(damage, from)
    }

    /// The single player-attributed grant seam for a proven mob kill.
    ///
    /// Attribution is explicit: [`MobKillCause::Environment`] and any hit that
    /// did not kill resolve the default zero report and mutate nothing. On a
    /// player kill the mob's pinned table is resolved through
    /// [`mob_drop_items`] — including the rare-entry gate keyed on the world
    /// seed, the kind and the mob's quantised home cell — and delivered
    /// best-effort with [`Player::collect_items`], so a full inventory loses
    /// only the overflow and the report says exactly how many units landed.
    ///
    /// The mob record is never removed, reordered, or mutated here: the lethal
    /// hit already armed the collapse, and [`WorldSession::retain_live_mobs`]
    /// owns removal after the fade. Calling this twice for the same corpse is
    /// therefore not idempotent by index; callers grant exactly once per hit
    /// (the melee tick reports a single `Swing` per cooldown).
    pub fn resolve_mob_kill(
        &mut self,
        index: usize,
        hit: MobHit,
        cause: MobKillCause,
    ) -> MobDropReport {
        if cause != MobKillCause::Player || !hit.killed {
            return MobDropReport::default();
        }
        // The index may be stale (the record streamed out or was reaped since
        // the swing was armed): a missing or still-live record grants nothing.
        let Some(mob) = self.mobs.get(index) else {
            return MobDropReport::default();
        };
        if mob.health != 0 {
            return MobDropReport::default();
        }
        let kind = mob.kind;
        // The rare gate is keyed on the persisted home, so a reloaded mob
        // rolls the same drops; no tick or kill order enters the draw.
        let home_cell = IVec3::new(
            mob.home.x.floor() as i32,
            mob.home.y.floor() as i32,
            mob.home.z.floor() as i32,
        );
        let Some(items) = mob_drop_items(kind, self.world.seed(), home_cell) else {
            return MobDropReport::default();
        };
        let outcome = self.player.collect_items(&items);
        MobDropReport::new(outcome.accepted, outcome.lost)
    }

    /// Whether the fresh Survival spawn gate is still waiting for chunks.
    /// T05 uses this to tell a fresh `--demo-combat` session from a loaded
    /// one without exposing the mutable flag.
    #[inline]
    pub fn spawn_settle_pending(&self) -> bool {
        self.spawn_settle
    }

    /// The cached deterministic world-spawn feet position (D071), the anchor
    /// the population layer protects with its safe radius.
    #[inline]
    pub fn world_spawn(&self) -> Vec3 {
        self.world_spawn
    }

    /// Whether every chunk column that could hold a spawn candidate is
    /// resident and lit.
    ///
    /// [`SpawnWorld::column_ready`] is a mandatory gate before a spawn attempt
    /// may read light: a not-yet-streamed column silently refuses the attempt.
    /// A scripted proof that pins exact population counters therefore cannot
    /// start its attempt sequence while the set of ready columns is still
    /// growing — the same seed would place different mobs on a fast (release)
    /// and a slow (debug) streamer. This square covers the full
    /// `SAFE_RADIUS..=SPAWN_MAX_RADIUS` ring plus one block of rounding slack on
    /// every side.
    pub fn spawn_ring_ready(&self) -> bool {
        let reach = SPAWN_MAX_RADIUS + 1.0;
        let (min_chunk, _) = world_to_chunk_and_local(
            (self.player.position.x - reach).floor() as i32,
            0,
            (self.player.position.z - reach).floor() as i32,
        );
        let (max_chunk, _) = world_to_chunk_and_local(
            (self.player.position.x + reach).floor() as i32,
            0,
            (self.player.position.z + reach).floor() as i32,
        );
        (min_chunk.x..=max_chunk.x)
            .all(|cx| (min_chunk.z..=max_chunk.z).all(|cz| self.world.column_ready(cx, cz)))
    }

    /// The ambient cause of the last player damage event, or `None` when the
    /// player has taken no damage since construction or respawn.
    ///
    /// Ephemeral: never persisted, so a restored session always starts `None`
    /// even if it was saved dead. The Death screen renders
    /// [`crate::player::combat::DamageCause::label`] when this is `Some` and
    /// "Unknown" otherwise.
    #[inline]
    pub fn last_damage_cause(&self) -> Option<DamageCause> {
        self.last_damage_cause
    }

    /// The player's current red damage-flash intensity in `0.0..=1.0`.
    ///
    /// Delegates to the pure [`crate::player::combat::PlayerCombat`] carrier,
    /// so the HUD and the demo read the same number the physics integration
    /// uses. Exactly `0.0` when no flash is active.
    #[inline]
    pub fn player_hurt_intensity(&self) -> f32 {
        self.player
            .combat
            .hurt_intensity_with_rules(&self.gameplay_rules.combat)
    }

    /// Whether the population layer may create mobs and burn them in daylight.
    ///
    /// The distance cull is **not** covered by this gate: a record that drifts
    /// out of range is still culled, so the gate can never move a deterministic
    /// demo's mob list. This is an ephemeral demo switch (the scripted
    /// `--demo-combat` seam flips it off around its pinned stages), **not** a
    /// game-mode rule: ambient spawning behaves identically in Creative and
    /// Survival, and making Creative inert is S09's rule, not this gate's.
    #[inline]
    pub fn ambient_spawning(&self) -> bool {
        self.spawn_runtime.enabled()
    }

    /// Enables or disables mob creation and daylight burning.
    ///
    /// Mirrors how `--demo-combat` pins the clock and the world mode: the demo
    /// seam owns this switch for the life of its scripted run, while production
    /// sessions leave it enabled for their whole life. Disabling clears the
    /// partial attempt/burn intervals, so re-enabling starts a fresh window
    /// instead of firing on the next frame.
    pub fn set_ambient_spawning(&mut self, enabled: bool) {
        self.spawn_runtime.set_enabled(enabled);
    }

    /// Whether hostile steering may select the player as a target.
    ///
    /// Enabled for every production session. The `--demo-combat` seam closes
    /// it at startup and reopens it only inside its dedicated `Hostile` stage,
    /// so the pinned quartet stages keep their pre-S06 behaviour. The switch
    /// affects target selection (and therefore attacks) only; the distance
    /// cull and the ambient population gate are independent.
    #[inline]
    pub fn hostile_ai_enabled(&self) -> bool {
        self.hostile_ai
    }

    /// Enables or disables hostile target selection for this session.
    ///
    /// Ephemeral demo sequencing state, never persisted. Closing it makes
    /// `tick_mobs` build no [`MobTarget`], so no hostile can acquire, chase,
    /// or attack; a chase already in progress is dropped on the next step.
    /// It is deliberately not the Creative rule: S09 owns mode inertness.
    pub fn set_hostile_ai_enabled(&mut self, enabled: bool) {
        self.hostile_ai = enabled;
    }

    /// Cumulative population counters since construction or
    /// [`WorldSession::reset_spawn_report`]: spawned, despawn-marked, burned, and
    /// cap-blocked records.
    #[inline]
    pub fn spawn_report(&self) -> SpawnReport {
        self.spawn_runtime.report()
    }

    /// Zeroes the cumulative spawn report without touching the gate or the
    /// deterministic attempt sequence, so a demo stage can re-measure a window.
    pub fn reset_spawn_report(&mut self) {
        self.spawn_runtime.reset_report();
    }

    /// Resets the fixed-rate mob runtime's step accumulator and tick index.
    ///
    /// The frame loop owns the production accumulator, so a scripted stage that
    /// pins exact fixed-step outcomes would otherwise inherit a fractional
    /// leftover (and a timing-dependent tick index) from however many frames
    /// ran before it. Resetting here gives the stage the same starting state on
    /// every host and frame rate; the mob list itself is untouched, and the
    /// caller owns any record pinning it needs.
    pub fn reset_mob_runtime(&mut self) {
        self.mob_runtime = MobRuntime::new(self.world.seed());
    }

    /// Advances the fixed-rate 20 Hz mob runtime AND the deterministic
    /// population layer by one frame of game time.
    ///
    /// Exactly the [`WorldSession::tick_survival`] clamp contract: `dt` is
    /// clamped to `0..=0.1`, then scaled by `time_scale` (negative becomes
    /// zero), and non-finite/non-positive game time is a no-op — a paused or
    /// zero-scale frame therefore runs neither the AI nor a spawn attempt.
    /// Residency is resolved per mob from the live world manager, so a mob in
    /// an unloaded chunk neither moves nor decays.
    ///
    /// This is the **single** spawn site: [`SpawnRuntime::tick`] receives the
    /// already-clamped, already-scaled `game_dt` plus the session's clock, seed,
    /// player and cached world-spawn anchor, and is the only code that creates
    /// mob records or marks one for culling. Finished corpses and
    /// spawner-marked records are both dropped at the end through
    /// [`WorldSession::retain_live_mobs`], the one removal site.
    pub fn tick_mobs(&mut self, dt: f32, time_scale: f32) -> MobTick {
        let dt = dt.clamp(0.0, 0.1);
        let game_dt = dt * time_scale.max(0.0);
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return MobTick::default();
        }

        // The FIRST of the two Creative authority sites (the second is
        // `apply_mob_attacks`, S06-T04): a non-Survival player, the closed
        // demo gate, or a dead player builds no target at all, so no hostile
        // can acquire and no attack can be decided. The target is built once
        // per call, not per fixed step, because the player does not move
        // between the steps of one frame.
        let target = (self.player.mode() == GameMode::Survival
            && self.hostile_ai
            && !self.is_dead())
        .then(|| MobTarget {
            feet: self.player.position,
            eye: self.player.eye_position(),
        });
        let report = {
            let world = &self.world;
            let runtime = &mut self.mob_runtime;
            let mobs = &mut self.mobs;
            runtime.tick(
                game_dt,
                mobs,
                |position| {
                    let (chunk, _) = world_to_chunk_and_local(
                        position.x.floor() as i32,
                        position.y.floor() as i32,
                        position.z.floor() as i32,
                    );
                    world.is_resident(chunk)
                },
                target,
                world,
            )
        };
        // Fixed order: move -> age the player's combat timers by the steps that
        // actually executed -> apply the melee damage those steps decided ->
        // spawn the arrows those steps decided -> spawn/cull. A zero-step or
        // paused frame returns above, so it can neither age the i-frame window,
        // damage the player, nor emit a shot.
        self.player
            .combat
            .advance_with_rules(report.ai_steps, &self.gameplay_rules.combat);
        self.apply_mob_attacks(&report.attacks);
        self.apply_mob_shots(&report.shots);
        {
            // Same split-borrow shape as the AI driver above: `world` is
            // immutable while the runtime and the mob list are mutable.
            let ctx = SpawnContext {
                seed: self.world.seed(),
                day_index: self.clock.day_index,
                time_of_day: self.clock.time_of_day,
                player: self.player.position,
                world_spawn: self.world_spawn,
            };
            let world = &self.world;
            let runtime = &mut self.spawn_runtime;
            let mobs = &mut self.mobs;
            runtime.tick(game_dt, &ctx, world, mobs);
        }
        self.retain_live_mobs();
        report
    }

    /// Applies the melee attacks decided by [`WorldSession::tick_mobs`].
    ///
    /// This is the **single** place a mob may change the player's health, and
    /// the second of exactly two Creative authority sites (the first is the
    /// target build inside `tick_mobs`; S09 pins both). A non-Survival player
    /// returns `0` immediately and records no cause, so Creative can never be
    /// hurt by a mob even if an attack list were produced some other way.
    ///
    /// Attacks are consumed in order and **at most one lands per call**: the
    /// first accepted hit opens the player's 0.5 s i-frame window, so every
    /// later attack in the same slice is refused by
    /// [`crate::player::combat::PlayerCombat::take_hit`]. That is exactly the
    /// "one hit per i-frame window, no matter how many zombies are adjacent"
    /// rule. Returns the damage actually applied (`0` when nothing landed).
    ///
    /// The cause is the literal [`DamageCause::Zombie`] because this seam is
    /// mob melee only: [`MobKind::attack_profile`] already returns `None` for
    /// every kind without a melee attack, so a Skeleton attack applies
    /// nothing. Skeleton arrows are a disjoint payload applied by
    /// [`WorldSession::apply_mob_shots`], never through this seam.
    pub fn apply_mob_attacks(&mut self, attacks: &[MobAttack]) -> u16 {
        if self.player.mode() != GameMode::Survival {
            return 0;
        }
        for attack in attacks {
            let Some(profile) = attack.kind.attack_profile() else {
                continue;
            };
            // Read the player's own state BEFORE the disjoint mutable borrows
            // of `combat` and `survival` below.
            let mode = self.player.mode();
            let feet = self.player.position;
            let applied = self.player.combat.take_hit_with_rules(
                &mut self.player.survival,
                profile.damage,
                mode,
                DamageCause::Zombie,
                feet,
                attack.from,
                &self.gameplay_rules.combat,
            );
            if let Some(applied) = applied {
                self.last_damage_cause = Some(DamageCause::Zombie);
                return applied.damage;
            }
        }
        0
    }

    /// Turns the ranged shot decisions of [`WorldSession::tick_mobs`] into
    /// real skeleton arrows through the shared spawn seam.
    ///
    /// This is the **single** place a skeleton shot enters the projectile
    /// list; the AI emits geometry only ([`MobShot`]) and this method owns the
    /// conversion to [`Arrow::skeleton`], so the 28 b/s / 2 damage pair and
    /// every ballistics rule stay in the projectile domain. Returns the number
    /// of arrows actually pushed.
    ///
    /// The Creative guard mirrors [`WorldSession::apply_mob_attacks`], the
    /// second of the two Creative authority sites: a non-Survival player
    /// spawns nothing and the return is `0`. That is defense in depth — the
    /// closed target build in `tick_mobs` already emits no shots.
    ///
    /// Refusals are inherited, never re-implemented: a shot whose geometry
    /// [`Arrow::skeleton`] rejects, a finished arrow, and the [`ARROW_CAP`]
    /// ceiling all simply do not count. One refused shot never stops the
    /// remaining shots in the same slice from being applied.
    pub fn apply_mob_shots(&mut self, shots: &[MobShot]) -> u32 {
        if self.player.mode() != GameMode::Survival {
            return 0;
        }
        let mut spawned: u32 = 0;
        for shot in shots {
            let Some(arrow) =
                Arrow::skeleton_with_rules(shot.origin, shot.dir, &self.gameplay_rules.arrow)
            else {
                continue;
            };
            if self.spawn_arrow(arrow).is_some() {
                spawned = spawned.saturating_add(1);
            }
        }
        spawned
    }

    /// Tills a live `Grass` or `Dirt` block into `Farmland` when a Hoe is
    /// selected.
    ///
    /// The caller owns the top-face rule (the session has no raycast normal),
    /// but every other guard is re-checked against live state: the selected
    /// item must be a Hoe of any tier, the block must still be soil, and the
    /// block must be within [`TABLE_INTERACT_REACH`]. The edit is applied only
    /// when it can produce remesh work; the tool is worn exactly once after
    /// that proven edit in every profile (the call is not wrapped in a
    /// `debug_assert!`, so release builds pay it too), so a refusal never
    /// mutates the world or the inventory. Returns the remesh work the caller
    /// uploads.
    pub fn till_at(&mut self, pos: IVec3) -> Option<Vec<(ChunkPos, ChunkMesh)>> {
        let selected = self.player.selected_item()?;
        match selected.id() {
            ItemId::Tool {
                kind: ToolKind::Hoe,
                ..
            } => {}
            _ => return None,
        }
        let live = self.world.get_block(pos.x, pos.y, pos.z);
        if !matches!(live, BlockId::Grass | BlockId::Dirt) {
            return None;
        }
        if !self.station_within_reach(pos) || !self.edit_will_remesh(pos) {
            return None;
        }

        let remeshed = self
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Farmland);
        debug_assert!(
            !remeshed.is_empty(),
            "edit_will_remesh proved the chunk and its face neighbours resident"
        );
        // The edit is visible; spend exactly one durability point. The wear is
        // unconditional (release builds pay it too, unlike the old
        // `debug_assert!` wrapper); only the invariant check is compiled out of
        // release. The selected-slot check above guarantees the Hoe is still
        // there, so a failure here is a state bug rather than a user error.
        let worn = self.player.wear_selected_tool();
        debug_assert!(
            worn.is_ok(),
            "the selected-slot guard proves the Hoe is still held"
        );
        Some(remeshed)
    }

    /// Plants `WheatStage0` on live `Farmland` when `WheatSeeds` are selected.
    ///
    /// Every guard is re-checked against live state: the selected item must
    /// be seeds, the target cell must be `Air`, the support block directly
    /// below must still be `Farmland`, the cell must be within
    /// [`TABLE_INTERACT_REACH`], and the player's AABB must not overlap the
    /// cell (no planting inside yourself). The edit is applied only when it
    /// can produce remesh work; exactly one seed is consumed in every profile
    /// (the call is not wrapped in a `debug_assert!`, so release builds pay it
    /// too) and the crop candidate is tracked after that proven edit, so a
    /// refusal never mutates the world or the inventory. Farming is
    /// mode-independent.
    pub fn plant_wheat_at(&mut self, pos: IVec3) -> Option<Vec<(ChunkPos, ChunkMesh)>> {
        let selected = self.player.selected_item()?;
        if selected.id() != ItemId::Material(Material::WheatSeeds) {
            return None;
        }
        if self.world.get_block(pos.x, pos.y, pos.z) != BlockId::Air {
            return None;
        }
        let below = pos - IVec3::Y;
        if self.world.get_block(below.x, below.y, below.z) != BlockId::Farmland {
            return None;
        }
        if !self.station_within_reach(pos)
            || self.player.aabb().intersects_block(pos.x, pos.y, pos.z)
            || !self.edit_will_remesh(pos)
        {
            return None;
        }

        let remeshed = self
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::WheatStage0);
        debug_assert!(
            !remeshed.is_empty(),
            "edit_will_remesh proved the chunk and its face neighbours resident"
        );
        // The edit is visible; consume exactly one seed and track the new
        // crop. The consume is unconditional (release builds pay it too,
        // unlike the old `debug_assert!` wrapper); only the invariant check is
        // compiled out of release. The selected-slot check above guarantees
        // seeds are still held.
        let consumed = self.player.consume_selected();
        debug_assert!(
            consumed.is_some(),
            "the selected-slot guard proves seeds are still held"
        );
        self.crops.insert(pos);
        Some(remeshed)
    }

    /// Advances every tracked crop by one frame of game time.
    ///
    /// Mirrors [`Self::tick_survival`]: `dt` is clamped to `0..=0.1`, then
    /// scaled by `time_scale` (negative scales become zero), and non-finite or
    /// non-positive game time is a no-op. Winter pauses growth while keeping
    /// each crop's banked progress. The pure runtime owns residency, support,
    /// maturity, and deterministic ordering; this seam applies each resulting
    /// stage only when the live block still equals the stage the runtime saw,
    /// so a stale advance can never overwrite a replaced crop. Returns the
    /// remesh work the caller uploads.
    pub fn tick_crops(&mut self, dt: f32, time_scale: f32) -> Vec<(ChunkPos, ChunkMesh)> {
        let dt = dt.clamp(0.0, 0.1);
        let game_dt = dt * time_scale.max(0.0);
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return Vec::new();
        }

        let paused = self.season() == Season::Winter;
        let advances = self.crops.tick(
            game_dt,
            paused,
            |p| {
                let (chunk, _) = world_to_chunk_and_local(p.x, p.y, p.z);
                self.world.is_resident(chunk)
            },
            |p| self.world.get_block(p.x, p.y, p.z),
        );

        let mut remeshed = Vec::new();
        for advance in advances {
            if self
                .world
                .get_block(advance.pos.x, advance.pos.y, advance.pos.z)
                != advance.from
            {
                continue;
            }
            remeshed.extend(self.world.set_block_immediate(
                advance.pos.x,
                advance.pos.y,
                advance.pos.z,
                advance.to,
            ));
        }
        remeshed
    }

    /// The validated items a broken wheat block yields, or `None` when the
    /// block is not a wheat stage.
    ///
    /// The deterministic world/local harvest table is the only source of
    /// truth; this seam maps it into inventory-validated [`Item`]s and fails
    /// closed on a malformed table rather than inventing partial loot.
    pub fn wheat_harvest_items(&self, block: BlockId) -> Option<Vec<Item>> {
        loot_items(wheat_harvest(block)?)
    }

    /// Forgets a crop candidate, e.g. after its block was broken or replaced.
    pub fn remove_crop(&mut self, pos: IVec3) {
        self.crops.remove(pos);
    }

    /// Restores the player to the deterministic spawn with full vitals.
    ///
    /// Only survival-transient state is reset: position/velocity/on-ground,
    /// vitals, spawn immunity, the runtime accumulators, the settle gate, and
    /// both open stations. The durable inventory aggregate (slots, cursor,
    /// grids), selected slot, clock, chest records, and `save_mode` are
    /// deliberately untouched.
    ///
    /// Every mob also drops its hostile-chase bookkeeping. Without this a
    /// zombie that was chasing the old position would keep its `Chase` state
    /// and aggro memory across the respawn and re-attack the player at the
    /// world spawn before they can move. Positions, health, yaw, and the
    /// attack cooldown are left alone, so the body and its cadence survive.
    pub fn respawn(&mut self) {
        self.player.position = Self::spawn_position(self.world.seed());
        self.player.velocity = Vec3::ZERO;
        self.player.on_ground = false;
        self.player.survival = SurvivalState::full();
        self.player.arm_spawn_immunity();
        // The player respawns unhurt: the flash, the i-frame window, and the
        // knockback impulse all clear, and the ambient cause is forgotten so a
        // new death cannot inherit the previous one's label.
        self.player.combat.reset();
        self.last_damage_cause = None;
        self.survival_runtime.reset();
        self.spawn_settle = true;
        // Projectiles are ephemeral session-owned state: a respawn starts in
        // flight-free air, never inheriting a shot fired before death.
        self.arrows.clear();
        for mob in &mut self.mobs {
            mob.reset_hostile_tracking();
        }
        self.close_table();
        self.close_chest();
        self.sync_camera();
    }

    /// The live season of this session, derived from its persisted calendar.
    ///
    /// The Playing frame loop calls this once per frame so a day rollover starts
    /// the world manager's bounded seasonal rollover wave. Pure function of
    /// `(day_index, season_length_days, seed)` — never the wall clock — and the
    /// day-boundary acceptance test drives this exact method, so the seam the
    /// test proves is the seam production runs.
    pub fn season(&self) -> Season {
        season_of(
            self.clock.day_index,
            self.season_length_days,
            self.world.seed(),
        )
    }

    /// Sync the camera from the player: eye position plus yaw/pitch. Called
    /// by both constructors and reused by S03-T02/T03 at the swap boundary.
    pub fn sync_camera(&mut self) {
        self.camera.position = self.player.eye_position();
        self.camera.yaw = self.player.yaw;
        self.camera.pitch = self.player.pitch;
    }

    /// Applies effective gameplay tuning to this session.
    ///
    /// Movement rules go to the player, projection planes to the camera, and
    /// the live day length to the session clock. The camera
    /// position/orientation, the world, and the clock's calendar
    /// (`day_index`/`time_of_day`/sub-tick remainder) are deliberately
    /// untouched, so a caller can re-apply rules mid-frame without moving the
    /// view or jumping time. S01 calls this once at startup; S02 re-applies
    /// after a live config reload.
    pub fn apply_rules(&mut self, rules: &RulesBundle) {
        self.gameplay_rules = *rules;
        self.spawn_runtime.set_rules(rules.spawning);
        self.clock.set_day_seconds(rules.clock.day_seconds);
        self.player.movement = rules.movement;
        self.camera.z_near = rules.camera.z_near;
        self.camera.z_far = rules.camera.z_far;
    }

    /// The effective gameplay tuning this session simulates with.
    ///
    /// `GameApp`-owned transient state (melee/bow/eating) reads the same
    /// bundle the session's internal seams use, so one live reload changes
    /// both halves of the frame at once.
    #[inline]
    pub fn rules(&self) -> RulesBundle {
        self.gameplay_rules
    }

    /// Applies live tuning and records the config meta the next save writes.
    ///
    /// S02's live reload replaces `GameApp.config` and re-enters the session
    /// install path, so this is the single writer that keeps the session's
    /// save metadata in step with the running values. The saved-world notice
    /// is deliberately unchanged here: it is load-scoped, not reload-scoped.
    pub fn apply_config(&mut self, rules: &RulesBundle, meta: SaveConfigMeta) {
        self.apply_rules(rules);
        self.save_config = meta;
    }

    /// The config metadata this session will attach to its next save.
    pub const fn save_config(&self) -> SaveConfigMeta {
        self.save_config
    }

    /// The non-blocking note describing how this session's loaded config
    /// differs from `current`, or `None` when they match or the world carries
    /// no config record (v1-v8).
    ///
    /// Pure read: unlike [`Self::take_config_notice`] it does not consume the
    /// record, so a caller that only wants to display the note must use
    /// `take_config_notice` to preserve the one-shot contract.
    pub fn config_notice(&self, current: SaveConfigMeta) -> Option<String> {
        self.saved_config
            .and_then(|saved| config_notice_for(saved, current))
    }

    /// Takes the pending config notice, if any.
    ///
    /// The loaded record is consumed even when the metas match, so a notice is
    /// presented at most once per session and a later tuning edit can never
    /// re-warn after the player has already been told (or told nothing
    /// because the config matched).
    pub fn take_config_notice(&mut self, current: SaveConfigMeta) -> Option<String> {
        let saved = self.saved_config.take()?;
        config_notice_for(saved, current)
    }

    /// The position of the crafting table whose interface is open, if any.
    ///
    /// Read-only accessor: validity is re-established by
    /// [`Self::sync_open_table`] and [`Self::open_table_at`], never by the
    /// reader. The state is ephemeral and never part of a save.
    pub const fn open_table_position(&self) -> Option<IVec3> {
        self.open_table
    }

    /// The position of the chest whose interface is open, if any.
    ///
    /// Ephemeral and never saved. Validity is re-established by
    /// [`Self::sync_open_chest`] and [`Self::open_chest_at`], never by the
    /// reader.
    pub const fn open_chest_position(&self) -> Option<IVec3> {
        self.open_chest
    }

    /// The contents of the open chest, if one is open and has a record.
    ///
    /// A generated LootChest materializes its deterministic record on first
    /// open, so it has contents here; a plain chest that has never been
    /// touched has no record, so this is `None` rather than an empty
    /// container and the caller treats both as empty.
    pub fn open_chest_contents(&self) -> Option<&Container> {
        self.open_chest.and_then(|pos| self.chests.get(&pos))
    }

    /// The persisted contents at `pos`, or `None` when no record exists.
    ///
    /// This is the record-lifecycle read: `None` means the chest was never
    /// touched (or is not a generated ruin chest), `Some(Container::empty())`
    /// means it was emptied. A generated LootChest gets a record on its first
    /// open, so it reports `Some` from then on.
    pub fn chest_at(&self, pos: IVec3) -> Option<&Container> {
        self.chests.get(&pos)
    }

    /// Everything a broken chest drops: its contents in slot order followed
    /// by exactly one chest item.
    ///
    /// When no record exists, a generated LootChest derives the same contents
    /// first open would materialize, so the caller's all-or-nothing insertion
    /// preflight sees the loot that would otherwise be lost. A missing or
    /// empty record on any other block still yields the chest item alone.
    pub fn chest_break_items(&self, pos: IVec3) -> Vec<Item> {
        let mut items = match self.chests.get(&pos) {
            Some(record) => record.contents(),
            None => self.derived_loot_items(pos).unwrap_or_default(),
        };
        items.push(
            Item::new(ItemId::Block(BlockId::Chest), 1)
                .expect("one chest block is a valid single-item stack"),
        );
        items
    }

    /// Opens the crafting table at `pos` only when the *live* world block
    /// there is a crafting table and the player's eye is within
    /// [`TABLE_INTERACT_REACH`].
    ///
    /// `pos` is treated as untrusted (it comes from a raycast or any caller),
    /// so the world is re-read at interaction time. A missing/wrong/replaced/
    /// out-of-reach coordinate returns `false` and mutates nothing: a failed
    /// interaction must not silently close a table that is already open.
    pub fn open_table_at(&mut self, pos: IVec3) -> bool {
        if !self
            .world
            .get_block(pos.x, pos.y, pos.z)
            .is_table_interactable()
        {
            return false;
        }
        if !self.station_within_reach(pos) {
            return false;
        }
        self.open_table = Some(pos);
        self.open_chest = None;
        true
    }

    /// Raycasts one bounded step from the session camera and opens the targeted
    /// crafting table.
    ///
    /// Returns the opened position, or `None` when the ray misses, hits a
    /// non-table block, or the hit is out of reach. The caller then falls
    /// through to its normal placement/interaction path, so a non-table click
    /// behaves exactly as it did before table stations existed.
    pub fn try_open_targeted_table(&mut self) -> Option<IVec3> {
        let hit = raycast_voxels(
            self.camera.position,
            self.camera.forward(),
            TABLE_INTERACT_REACH,
            |x, y, z| self.world.get_block(x, y, z),
        )?;
        if self.open_table_at(hit.block_pos) {
            Some(hit.block_pos)
        } else {
            None
        }
    }

    /// Re-validates the open table against the current world and reach,
    /// closing it when the block was removed/replaced or the player walked
    /// away. Returns the still-open position. One bounded block lookup.
    pub fn sync_open_table(&mut self) -> Option<IVec3> {
        let pos = self.open_table?;
        if self
            .world
            .get_block(pos.x, pos.y, pos.z)
            .is_table_interactable()
            && self.station_within_reach(pos)
        {
            Some(pos)
        } else {
            self.open_table = None;
            None
        }
    }

    /// Closes the open table, if any. Idempotent.
    pub fn close_table(&mut self) {
        self.open_table = None;
    }

    /// Opens the chest at `pos` only when the *live* world block there is a
    /// container and the player's eye is within [`TABLE_INTERACT_REACH`].
    ///
    /// `pos` is untrusted, so the world is re-read at interaction time. A
    /// generated LootChest materializes its deterministic record on first
    /// open (a repeat open reuses it); a plain Chest still creates no record
    /// until a player transfer touches it. A LootChest outside a generated
    /// ruin opens empty rather than guessing a neighbouring table. A
    /// missing/wrong/replaced/out-of-reach coordinate returns `false` and
    /// mutates nothing, so a failed interaction cannot close a station that
    /// is already open. A successful open clears any open crafting table.
    pub fn open_chest_at(&mut self, pos: IVec3) -> bool {
        if !self
            .world
            .get_block(pos.x, pos.y, pos.z)
            .is_container_interactable()
        {
            return false;
        }
        if !self.station_within_reach(pos) {
            return false;
        }
        self.materialize_loot_chest(pos);
        self.open_chest = Some(pos);
        self.open_table = None;
        true
    }

    /// Raycasts one bounded step from the session camera and opens the targeted
    /// chest.
    ///
    /// Returns the opened position, or `None` when the ray misses, hits a
    /// non-container block, or the hit is out of reach. The caller then falls
    /// through to its normal placement/interaction path.
    pub fn try_open_targeted_chest(&mut self) -> Option<IVec3> {
        let hit = raycast_voxels(
            self.camera.position,
            self.camera.forward(),
            TABLE_INTERACT_REACH,
            |x, y, z| self.world.get_block(x, y, z),
        )?;
        if self.open_chest_at(hit.block_pos) {
            Some(hit.block_pos)
        } else {
            None
        }
    }

    /// Re-validates the open chest against the current world and reach,
    /// closing it when the block was removed/replaced or the player walked
    /// away. Returns the still-open position. One bounded block lookup.
    pub fn sync_open_chest(&mut self) -> Option<IVec3> {
        let pos = self.open_chest?;
        if self
            .world
            .get_block(pos.x, pos.y, pos.z)
            .is_container_interactable()
            && self.station_within_reach(pos)
        {
            Some(pos)
        } else {
            self.open_chest = None;
            None
        }
    }

    /// Closes the open chest, if any. Idempotent.
    pub fn close_chest(&mut self) {
        self.open_chest = None;
    }

    /// Closes the open chest only when it is the chest at `pos`.
    ///
    /// Used by the break path so removing a different block cannot close the
    /// station the player is using.
    pub fn close_chest_at(&mut self, pos: IVec3) {
        if self.open_chest == Some(pos) {
            self.open_chest = None;
        }
    }

    /// Applies one whole-stack click to the open chest's slot `index`.
    ///
    /// The station is re-validated first: losing the live block or walking
    /// out of reach closes it and reports [`ChestCommandError::NotOpen`]. On a
    /// successful click, the candidate record is written only when it differs
    /// from the pre-click record or the shared cursor changed. A no-op click
    /// therefore creates no record and does not dirty the save; an existing
    /// all-empty record is retained rather than deleted, because a touched
    /// empty chest must stay distinguishable from an untouched one.
    pub fn click_open_chest_slot(&mut self, index: usize) -> Result<(), ChestCommandError> {
        let pos = self.sync_open_chest().ok_or(ChestCommandError::NotOpen)?;
        let previous = self.chests.get(&pos).cloned();
        let previous_cursor = self.player.inventory().cursor();
        let mut candidate = previous.clone().unwrap_or_else(Container::empty);
        self.player
            .click_container_slot(&mut candidate, index)
            .map_err(ChestCommandError::Slot)?;

        // An absent record and an all-empty candidate are the same observable
        // state: a no-op click must not create a record. Any existing record
        // (including a touched-empty one) is compared by value.
        let record_changed = match &previous {
            Some(record) => record != &candidate,
            None => !candidate.is_empty(),
        };
        let cursor_changed = self.player.inventory().cursor() != previous_cursor;
        if record_changed || cursor_changed {
            self.chests.insert(pos, candidate);
            self.world.mark_unsaved();
        }
        Ok(())
    }

    /// Reach check against the block's axis-aligned box: the player may
    /// interact when the nearest point of the station block is within
    /// [`TABLE_INTERACT_REACH`] of the eye. Shared by the table and chest
    /// stations so both use one reach budget.
    fn station_within_reach(&self, pos: IVec3) -> bool {
        let eye = self.player.eye_position();
        let min = pos.as_vec3();
        let closest = eye.clamp(min, min + Vec3::ONE);
        eye.distance_squared(closest) <= TABLE_INTERACT_REACH * TABLE_INTERACT_REACH
    }

    /// Whether a block edit at `pos` can produce remesh work right now.
    ///
    /// [`WorldManager::set_block_immediate`] edits a resident chunk even when
    /// its face neighbours are missing, but then returns no remesh work. The
    /// till/plant seams check this first so a refusal is never a silent world
    /// mutation: the edit, the tool wear / seed consume, and the returned
    /// remesh work always agree.
    fn edit_will_remesh(&self, pos: IVec3) -> bool {
        use crate::world::manager::{MAX_CHUNK_Y, MIN_CHUNK_Y};

        let (chunk, _) = world_to_chunk_and_local(pos.x, pos.y, pos.z);
        if !self.world.is_resident(chunk) {
            return false;
        }
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let ny = chunk.y + dy;
            if !(MIN_CHUNK_Y..=MAX_CHUNK_Y).contains(&ny) {
                continue;
            }
            if !self
                .world
                .is_resident(ChunkPos::new(chunk.x + dx, ny, chunk.z + dz))
            {
                return false;
            }
        }
        true
    }

    /// The deterministic loot a generated ruin chest at `pos` would hold, in
    /// slot order, or `None` when `pos` is not a live generated LootChest.
    ///
    /// The live-block check is what keeps a player-placed plain Chest (or a
    /// LootChest placed outside a ruin) from ever deriving ruin loot.
    fn derived_loot_items(&self, pos: IVec3) -> Option<Vec<Item>> {
        if self.world.get_block(pos.x, pos.y, pos.z) != BlockId::LootChest {
            return None;
        }
        let entries = ruin_loot_chest_at(pos, self.world.seed())?;
        loot_items(&entries)
    }

    /// Gives a generated LootChest its deterministic record on first open.
    ///
    /// A repeat open reuses the existing record, and a non-LootChest or a
    /// LootChest outside a generated ruin opens empty. Opening deliberately
    /// does not mark the world unsaved: materializing generated contents is
    /// not a player content change, and a plain chest still creates no record
    /// until a transfer touches it.
    fn materialize_loot_chest(&mut self, pos: IVec3) {
        if self.chests.contains_key(&pos) {
            return;
        }
        let Some(items) = self.derived_loot_items(pos) else {
            return;
        };
        let mut slots: ChestSlots = [None; CHEST_SLOTS];
        for (slot, item) in slots.iter_mut().zip(items) {
            *slot = Some(item);
        }
        self.chests.insert(pos, Container::from_slots(slots));
    }
}

/// Whether the chunk containing `position` is resident.
///
/// The one residency gate shared by mobs, arrows, the AI driver and the
/// renderer: a position's floor cell decides the chunk, so an entity whose
/// feet are exactly on a chunk boundary is attributed to the upper chunk.
fn position_is_resident(world: &WorldManager, position: Vec3) -> bool {
    let (chunk, _) = world_to_chunk_and_local(
        position.x.floor() as i32,
        position.y.floor() as i32,
        position.z.floor() as i32,
    );
    world.is_resident(chunk)
}

/// The nearest resident, live mob whose AABB the swept segment enters.
///
/// Returns the mob's index, so the caller mutates exactly one record. Two
/// exclusion rules keep projectile collision honest:
///
/// * the mob's feet chunk must be resident — the same gate the AI and the
///   renderer use, so an arrow can never damage a mob that has not streamed
///   in;
/// * a mob whose AABB **contains `origin`** is skipped. Without this, an arrow
///   that starts inside a body (e.g. a shot fired from inside the target at
///   point-blank range) would land a zero-distance hit. The arrow must enter
///   the body from outside, which is the rule S08 relies on.
///
/// The smallest segment parameter wins, so when two bodies overlap the closer
/// one takes the arrow. Non-finite geometry is rejected by
/// [`segment_aabb`], which yields no hit at all.
fn nearest_arrow_mob(
    mobs: &[Mob],
    world: &WorldManager,
    origin: Vec3,
    delta: Vec3,
) -> Option<usize> {
    let mut nearest: Option<(usize, f32)> = None;
    for (index, mob) in mobs.iter().enumerate() {
        if mob.health == 0 || !position_is_resident(world, mob.position) {
            continue;
        }
        let (min, max) = mob.aabb();
        if origin.cmpge(min).all() && origin.cmple(max).all() {
            // The arrow starts inside this body: never a hit on it.
            continue;
        }
        let Some(t) = segment_aabb(origin, delta, min, max) else {
            continue;
        };
        if nearest.is_none_or(|(_, best)| t < best) {
            nearest = Some((index, t));
        }
    }
    nearest.map(|(index, _)| index)
}

/// Maps a world-local loot table onto validated inventory items.
///
/// Fails closed on malformed data instead of inventing partial loot: an empty
/// table, more entries than a chest has slots, a zero count, or a range the
/// generator failed to roll all reject the whole batch. The M008 table is
/// guaranteed non-empty with degenerate ranges, so any rejection means the
/// world/local vocabulary drifted.
fn loot_items(entries: &[LootEntry]) -> Option<Vec<Item>> {
    if entries.is_empty() || entries.len() > CHEST_SLOTS {
        return None;
    }
    entries
        .iter()
        .map(|entry| {
            if entry.min_count == 0 || entry.min_count != entry.max_count {
                return None;
            }
            Item::new(ItemId::from(entry.item), entry.min_count).ok()
        })
        .collect()
}

/// Maps one mob kind's pinned drop table onto validated inventory items.
///
/// An empty table is valid and yields `Some(vec![])`: a hostile kind whose
/// slice has not landed yet legitimately drops nothing. Each entry is filtered
/// through [`crate::world::mob_loot::MobDrop::rolls`] against one
/// [`crate::world::mob_loot::drop_roll`] value drawn from
/// `(seed, kind, home_cell)`, so the result is deterministic and the same
/// corpse always resolves the same batch. A zero count, a count above the
/// identity's stack limit, or an `Item::new` failure rejects the whole batch
/// instead of delivering a partial one, so a drifted table can never invent or
/// silently truncate a drop. Entries pass through the T01
/// `From<MobDropItem> for ItemId` bridge, which is the only world -> player
/// identity mapping.
///
/// `#[doc(hidden)] pub` is a narrowly scoped allowance, not supported API: the
/// bin crate's bounded `--demo-combat` Drops self-check builds the same batch a
/// kill delivers so it can prove the overflow accounting one level below the
/// [`crate::player::physics::Player::collect_items`] forwarder. No gameplay
/// caller reads this; production delivery always goes through
/// [`WorldSession::resolve_mob_kill`].
#[doc(hidden)]
pub fn mob_drop_items(kind: MobKind, seed: u32, home_cell: IVec3) -> Option<Vec<Item>> {
    let roll = drop_roll(seed, kind, home_cell);
    mob_drops(kind)
        .iter()
        .filter(|drop| drop.rolls(roll))
        .map(|drop| {
            let id = ItemId::from(drop.item);
            if drop.count == 0 || drop.count > id.stack_limit() {
                return None;
            }
            Item::new(id, drop.count).ok()
        })
        .collect()
}

/// Why a chest-slot command was rejected.
///
/// [`ChestCommandError::NotOpen`] means the station disappeared (block
/// removed/replaced or out of reach) before the click could be applied;
/// [`ChestCommandError::Slot`] forwards the domain transfer error, whose
/// guarantee is that the chest record and cursor are unchanged. Neither
/// variant creates a chest record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChestCommandError {
    NotOpen,
    Slot(InventoryError),
}

/// A time-derived random seed for blank-seed creates. Uses the wall clock's
/// nanosecond component — no `rand` dependency. Determinism tests inject
/// the seed instead; S03-T03/T04 wire blank-seed semantics through
/// `catalog::resolve_seed_or(seed_form_value, random_seed())`.
pub fn random_seed() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::bow::BowRules;
    use crate::player::camera::CameraRules;
    use crate::player::combat::CombatRules;
    use crate::player::container::Container;
    use crate::player::inventory::{
        InventoryError, Item, ItemId, Material, ToolKind, ToolTier, CHEST_SLOTS,
    };
    use crate::player::melee::attack_damage;
    use crate::player::melee::MeleeRules;
    use crate::player::physics::{InputState, MovementRules};
    use crate::player::survival::SurvivalRules;
    use crate::player::survival::{GameMode, SurvivalState, SurvivalTick, FULL_AIR};
    use crate::world::arrow::ArrowRules;
    use crate::world::block::BlockId;
    use crate::world::daycycle::{ClockRules, DEFAULT_TIME_OF_DAY};
    use crate::world::mob::{MobAiState, MobKind, MOB_DEATH_SECS, MOB_TICK_SECS};
    use crate::world::persistence::SavedMob;
    use crate::world::season::DEFAULT_SEASON_LENGTH_DAYS;
    use crate::world::spawning::{SpawnRules, DESPAWN_RADIUS, SAFE_RADIUS};

    #[test]
    fn interaction_rules_default_matches_constants() {
        assert_eq!(
            InteractionRules::default().table_reach,
            TABLE_INTERACT_REACH
        );
    }

    fn make_save(seed: u32) -> WorldSaveData {
        WorldSaveData {
            seed,
            time_of_day: crate::world::daycycle::DEFAULT_TIME_OF_DAY,
            day_index: 0,
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            mode: GameMode::Creative,
            player: crate::world::persistence::SavedPlayerState {
                position: [1.5, 20.5, 2.5],
                yaw: 0.75,
                pitch: -0.15,
                fly_mode: false,
                selected_slot: 3,
                render_distance: 12,
                inventory: crate::player::inventory::Inventory::empty(),
                player_grid: crate::player::inventory::CraftingGrid::new(
                    crate::player::inventory::GridKind::Player,
                ),
                table_grid: crate::player::inventory::CraftingGrid::new(
                    crate::player::inventory::GridKind::Table,
                ),
                survival: SurvivalState::full(),
            },
            chests: crate::world::persistence::ChestMap::default(),
            mobs: Vec::new(),
            chunk_edits: ChunkEditMap::default(),
        }
    }

    fn chest_with_items() -> Container {
        let mut chest = [None; crate::player::inventory::CHEST_SLOTS];
        chest[0] =
            Some(Item::new(ItemId::Block(crate::world::block::BlockId::Stone), 7).expect("stone"));
        chest[13] = Some(Item::new(ItemId::Material(Material::Apple), 1).expect("apple"));
        chest[26] = Some(Item::new(ItemId::Material(Material::Bread), 2).expect("bread"));
        Container::from_slots(chest)
    }

    #[test]
    fn new_session_defaults_to_creative_with_no_chests() {
        let session = WorldSession::new_session(
            17u32,
            PathBuf::from("/tmp/fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(session.save_mode, GameMode::Creative);
        assert_eq!(session.player.mode(), GameMode::Creative);
        assert!(session.player.can_fly());
        assert!(session.chests.is_empty());
    }

    #[test]
    fn apply_rules_sets_player_and_camera() {
        let mut session = WorldSession::new_session(
            21u32,
            PathBuf::from("/tmp/apply-rules.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let camera_position = session.camera.position;
        let camera_yaw = session.camera.yaw;
        let camera_pitch = session.camera.pitch;

        let rules = RulesBundle {
            movement: MovementRules {
                walk_speed: 9.0,
                jump_velocity: 12.0,
                ..MovementRules::default()
            },
            camera: CameraRules {
                z_near: 0.25,
                z_far: 512.0,
            },
            ..RulesBundle::default()
        };
        session.apply_rules(&rules);

        assert_eq!(session.player.movement, rules.movement);
        assert_eq!(session.camera.z_near, 0.25);
        assert_eq!(session.camera.z_far, 512.0);
        assert_eq!(session.camera.position, camera_position);
        assert_eq!(session.camera.yaw, camera_yaw);
        assert_eq!(session.camera.pitch, camera_pitch);
    }

    fn config_meta(schema: u16, digest: u64) -> SaveConfigMeta {
        SaveConfigMeta { schema, digest }
    }

    #[test]
    fn config_notice_matrix_and_one_shot() {
        let current = config_meta(1, 0xAAAA);
        let mut session = WorldSession::new_session(
            22u32,
            PathBuf::from("/tmp/config-notice.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );

        // Pin the exact player-facing strings, not just the enum of outcomes.
        assert_eq!(
            CONFIG_CHANGED_NOTICE,
            "config changed since this world was saved"
        );
        assert_eq!(
            CONFIG_SCHEMA_CHANGED_NOTICE,
            "config schema changed since this world was saved"
        );

        // Row 1: saved None (fresh session or v1-v8 payload) is silent.
        assert_eq!(session.config_notice(current), None);
        assert_eq!(session.take_config_notice(current), None);

        // Row 2: same schema and digest is silent, but `take` still consumes.
        session.saved_config = Some(current);
        assert_eq!(session.config_notice(current), None);
        assert_eq!(session.take_config_notice(current), None);
        assert!(session.saved_config.is_none());
        // A silent matching take must not leave a record that a later tuning
        // edit could turn into a warning.
        assert_eq!(session.take_config_notice(config_meta(2, 0xBBBB)), None);

        // Row 3: same schema, different digest -> changed-config notice once.
        session.saved_config = Some(config_meta(1, 0xBBBB));
        assert_eq!(
            session.config_notice(current),
            Some(CONFIG_CHANGED_NOTICE.to_string())
        );
        assert_eq!(
            session.take_config_notice(current),
            Some(CONFIG_CHANGED_NOTICE.to_string())
        );
        assert_eq!(session.take_config_notice(current), None);

        // Row 4: different schema (any digest) -> schema notice, and the
        // schema mismatch wins even when the digest looks unchanged.
        session.saved_config = Some(config_meta(0, 0xAAAA));
        assert_eq!(
            session.config_notice(config_meta(1, 0xAAAA)),
            Some(CONFIG_SCHEMA_CHANGED_NOTICE.to_string())
        );
        assert_eq!(
            session.take_config_notice(config_meta(1, 0xAAAA)),
            Some(CONFIG_SCHEMA_CHANGED_NOTICE.to_string())
        );
        assert_eq!(session.take_config_notice(config_meta(1, 0xAAAA)), None);
    }

    #[test]
    fn from_loaded_world_carries_saved_config() {
        let saved = config_meta(1, 0x1234);
        let current = config_meta(1, 0x5678);
        let mut session = WorldSession::from_loaded_world(
            LoadedWorld {
                data: make_save(23u32),
                registry: ContentRegistry::builtin().clone(),
                saved_content: None,
                saved_config: Some(saved),
                unresolved: 0,
            },
            PathBuf::from("/tmp/v9-config.mcvx"),
        );

        assert_eq!(
            session.config_notice(current),
            Some(CONFIG_CHANGED_NOTICE.to_string())
        );
        // The loaded world's record is a load-time fact, not the meta a fresh
        // save would write; `apply_config` installs the running meta.
        assert_eq!(session.save_config(), ConfigRuntime::defaults().meta());
        assert_eq!(
            session.take_config_notice(current),
            Some(CONFIG_CHANGED_NOTICE.to_string())
        );
        assert_eq!(session.take_config_notice(current), None);
    }

    #[test]
    fn apply_config_sets_rules_and_save_meta() {
        let mut session = WorldSession::new_session(
            24u32,
            PathBuf::from("/tmp/apply-config.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let meta = config_meta(4, 0xDEAD_BEEF);
        let rules = RulesBundle {
            movement: MovementRules {
                walk_speed: 9.0,
                jump_velocity: 12.0,
                ..MovementRules::default()
            },
            camera: CameraRules {
                z_near: 0.25,
                z_far: 512.0,
            },
            ..RulesBundle::default()
        };

        session.apply_config(&rules, meta);

        assert_eq!(session.player.movement, rules.movement);
        assert_eq!(session.camera.z_near, 0.25);
        assert_eq!(session.camera.z_far, 512.0);
        assert_eq!(session.save_config(), meta);
    }

    #[test]
    fn apply_rules_installs_live_day_length_without_moving_the_calendar() {
        let mut session = WorldSession::new_session(
            30u32,
            PathBuf::from("/tmp/live-day-length.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let rules = RulesBundle {
            clock: ClockRules { day_seconds: 600.0 },
            ..RulesBundle::default()
        };
        session.apply_rules(&rules);

        assert_eq!(session.rules().clock.day_seconds, 600.0);
        assert_eq!(session.clock.day_seconds(), 600.0);
        // Installing the rate mid-day must not move the calendar.
        assert_eq!(session.clock.day_index, 0);
        assert_eq!(session.clock.time_of_day, DEFAULT_TIME_OF_DAY);

        // 600 real seconds is one full 24,000-tick day at the tuned rate, so
        // from noon the clock lands on noon one day later.
        session.clock.advance_secs(600.0, 1.0);
        assert_eq!(session.clock.day_index, 1);
        assert_eq!(session.clock.time_of_day, DEFAULT_TIME_OF_DAY);
    }

    #[test]
    fn fresh_session_reports_default_gameplay_rules() {
        let session = WorldSession::new_session(
            26u32,
            PathBuf::from("/tmp/fresh-rules.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(session.rules(), RulesBundle::default());
    }

    #[test]
    fn apply_rules_drives_survival_and_combat_seams() {
        let mut session = survival_session(27);
        let rules = RulesBundle {
            survival: SurvivalRules {
                hunger_drain_secs: 1.0,
                ..SurvivalRules::default()
            },
            combat: CombatRules {
                iframe_secs: 0.0,
                hurt_flash_secs: 2.0,
                ..CombatRules::default()
            },
            ..RulesBundle::default()
        };
        session.apply_rules(&rules);
        assert_eq!(session.rules(), rules);
        assert_eq!(session.rules().survival, rules.survival);
        assert_eq!(session.rules().combat, rules.combat);

        // One game second of the tuned 1 s drain costs exactly one hunger
        // point; the 30 s default would cost none.
        let hunger = session.player.survival.hunger();
        tick_game_seconds(&mut session, 1, false);
        assert_eq!(session.player.survival.hunger(), hunger - 1);

        // A tuned zero i-frame lets the next hit land immediately; the default
        // 0.5 s window would refuse it.
        let feet = session.player.position;
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            3
        );
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            3,
            "the stored zero iframe must not refuse the second hit"
        );
        assert_eq!(session.player.survival.health(), 14);
    }

    #[test]
    fn apply_rules_reach_selects_a_farther_melee_candidate() {
        let mut session = survival_hostile_session(28, MobKind::Zombie);
        let origin = Vec3::new(8.5, 5.2, 8.5);
        let dir = Vec3::Z;
        assert!(
            session.melee_candidate(origin, dir, 1.0).is_none(),
            "the mob is out of the default reach"
        );

        let rules = RulesBundle {
            melee: MeleeRules {
                reach: 3.0,
                ..MeleeRules::default()
            },
            ..RulesBundle::default()
        };
        session.apply_rules(&rules);
        assert_eq!(session.rules().melee.reach, 3.0);
        assert!(
            session
                .melee_candidate(origin, dir, session.rules().melee.reach)
                .is_some(),
            "the stored tuned reach reaches the mob"
        );
    }

    #[test]
    fn apply_rules_drives_bow_and_arrow_spawn_stats() {
        let mut session = arrow_session(29);
        give_bow(&mut session);
        give_arrows(&mut session, 1);
        let rules = RulesBundle {
            bow: BowRules {
                min_charge_secs: 0.1,
                full_charge_secs: 0.2,
            },
            arrow: ArrowRules {
                speed_max: 64.0,
                damage_max: 9,
                lifetime_secs: 12.0,
                ..ArrowRules::default()
            },
            ..RulesBundle::default()
        };
        session.apply_rules(&rules);

        let arrow = session
            .fire_bow(shot_origin(), Vec3::new(0.0, 0.0, 1.0), 1.0)
            .expect("a full-draw shot with ammo");
        assert_eq!(arrow.damage, 9);
        assert!(
            (arrow.velocity.length() - 64.0).abs() < 1e-3,
            "tuned full-draw speed was {}",
            arrow.velocity.length()
        );
        assert!((arrow.life - 12.0).abs() < 1e-6);
    }

    #[test]
    fn fresh_session_writes_defaults_meta() {
        let defaults = ConfigRuntime::defaults().meta();
        let registry = ContentRegistry::builtin_arc();

        let session = WorldSession::new_session_with_mode_and_registry(
            25u32,
            PathBuf::from("/tmp/fresh-meta.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
            Arc::clone(&registry),
        );
        assert_eq!(session.save_config(), defaults);
        assert!(session.saved_config.is_none());
        assert_eq!(session.config_notice(defaults), None);

        let editor = WorldSession::new_at_with_registry(
            25u32,
            PathBuf::from("/tmp/editor-meta.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(1.0, 20.0, 2.0),
            GameClock::new(),
            registry,
        );
        assert_eq!(editor.save_config(), defaults);
        assert!(editor.saved_config.is_none());
        assert_eq!(editor.config_notice(defaults), None);
    }

    #[test]
    fn new_session_with_mode_survival_walks_with_full_vitals() {
        let session = WorldSession::new_session_with_mode(
            18u32,
            PathBuf::from("/tmp/survival.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        assert_eq!(session.save_mode, GameMode::Survival);
        assert_eq!(session.player.mode(), GameMode::Survival);
        assert_eq!(session.player.survival, SurvivalState::full());
        assert!(!session.player.fly_mode);
        assert!(!session.player.can_fly());
        assert!(session.chests.is_empty());
    }

    #[test]
    fn new_session_with_mode_creative_flies_with_full_vitals() {
        let session = WorldSession::new_session_with_mode(
            19u32,
            PathBuf::from("/tmp/creative.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
        );
        assert_eq!(session.save_mode, GameMode::Creative);
        assert_eq!(session.player.mode(), GameMode::Creative);
        assert_eq!(session.player.survival, SurvivalState::full());
        assert!(session.player.fly_mode);
        assert!(session.player.can_fly());
        assert!(session.chests.is_empty());
    }

    #[test]
    fn new_at_defaults_to_creative_with_no_chests() {
        let session = WorldSession::new_at(
            20u32,
            PathBuf::from("/tmp/background.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(1.5, 20.0, 2.5),
            GameClock::new(),
        );
        assert_eq!(session.save_mode, GameMode::Creative);
        assert_eq!(session.player.mode(), GameMode::Creative);
        assert!(session.chests.is_empty());
    }

    #[test]
    fn new_session_spawns_player_on_the_searched_land_column() {
        // Seed 12345's (0, 0) column is open ocean after the T03 biome rewrite,
        // so this seed proves the session really moved instead of spawning on
        // the seabed.
        let seed = 12345u32;
        let session = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let (spawn_x, spawn_z) = find_spawn_column(seed);
        let expected_y = (terrain_height_at_seeded(spawn_x, spawn_z, seed) + 4) as f32;
        assert_eq!(
            session.player.position,
            Vec3::new(spawn_x as f32 + 0.5, expected_y, spawn_z as f32 + 0.5)
        );
        assert!(
            terrain_height_at_seeded(0, 0, seed) < crate::world::terrain::SEA_LEVEL,
            "test premise: seed {seed} must have an ocean at (0, 0)"
        );
        assert!(
            (spawn_x, spawn_z) != (0, 0),
            "the session must spawn away from the ocean column"
        );
        let h = terrain_height_at_seeded(spawn_x, spawn_z, seed);
        assert!(h >= crate::world::terrain::SPAWN_MIN_HEIGHT);
        assert_ne!(
            crate::world::terrain::biome_at_seeded(spawn_x, spawn_z, seed),
            crate::world::terrain::Biome::Ocean
        );
    }

    #[test]
    fn new_session_preserves_the_seed() {
        let seed = 777u32;
        let session = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(session.world.seed(), seed);
    }

    #[test]
    fn new_session_is_deterministic_for_the_same_seed() {
        let seed = 4242u32;
        let a = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/a.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let b = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/b.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(a.player.position, b.player.position);
        assert_eq!(a.camera.position, b.camera.position);
        assert_eq!(a.camera.yaw, b.camera.yaw);
        assert_eq!(a.camera.pitch, b.camera.pitch);
        assert_eq!(a.world.seed(), b.world.seed());
    }

    #[test]
    fn new_session_syncs_the_camera_from_the_player() {
        let session = WorldSession::new_session(
            9u32,
            PathBuf::from("/tmp/fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(session.camera.position, session.player.eye_position());
        assert_eq!(session.camera.yaw, session.player.yaw);
        assert_eq!(session.camera.pitch, session.player.pitch);
    }

    #[test]
    fn new_at_uses_the_supplied_position_and_clock() {
        let clock = GameClock::from_day_and_time(3, 4_321);
        let position = Vec3::new(10.5, 80.0, -4.5);
        let session = WorldSession::new_at(7u32, PathBuf::from("/tmp/bg.mcvx"), 2, position, clock);
        assert_eq!(session.player.position, position);
        assert_eq!(session.camera.position, session.player.eye_position());
        assert_eq!(session.clock.day_index, 3);
        assert_eq!(session.clock.time_of_day, 4_321);
        assert_eq!(session.season_length_days, 2);
        assert_eq!(session.world.seed(), 7);
        assert_eq!(session.save_path, PathBuf::from("/tmp/bg.mcvx"));
    }

    #[test]
    fn new_session_passes_the_save_path_through_unchanged() {
        let path = PathBuf::from("saves/alpha.mcvx");
        let session = WorldSession::new_session(1u32, path.clone(), DEFAULT_SEASON_LENGTH_DAYS);
        assert_eq!(session.save_path, path);
    }

    #[test]
    fn session_from_save_round_trips_player_state() {
        let data = make_save(31337u32);
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.player.position, Vec3::new(1.5, 20.5, 2.5));
        assert_eq!(session.player.yaw, 0.75);
        assert_eq!(session.player.pitch, -0.15);
        assert!(!session.player.fly_mode);
        assert_eq!(session.player.selected_slot(), 3);
    }

    #[test]
    fn session_from_save_restores_survival_vitals_and_chests_without_flight() {
        let chest = chest_with_items();
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(4, -2, 7), chest.clone());
        chests.insert(IVec3::new(-1, 0, 3), Container::empty());

        let mut data = make_save(88u32);
        data.mode = GameMode::Survival;
        data.player.fly_mode = true; // stale/hostile byte: Survival must walk
        data.player.survival = SurvivalState::new(7, 3, 12).expect("in-range vitals");
        data.chests = chests.clone();

        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/survival.mcvx"));
        assert_eq!(session.save_mode, GameMode::Survival);
        assert_eq!(session.player.mode(), GameMode::Survival);
        assert_eq!(
            session.player.survival,
            SurvivalState::new(7, 3, 12).expect("in-range vitals")
        );
        assert_eq!(session.chests, chests);
        assert_eq!(session.chests[&IVec3::new(4, -2, 7)], chest);
        assert_eq!(session.chests[&IVec3::new(-1, 0, 3)], Container::empty());
        assert!(
            !session.player.fly_mode,
            "a Survival save cannot restore a flying player"
        );
        assert!(!session.player.can_fly());
    }

    #[test]
    fn session_from_save_creative_preserves_a_saved_fly_flag() {
        let mut data = make_save(89u32);
        data.mode = GameMode::Creative;
        data.player.fly_mode = true;

        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/creative.mcvx"));
        assert_eq!(session.save_mode, GameMode::Creative);
        assert_eq!(session.player.mode(), GameMode::Creative);
        assert!(session.player.fly_mode);
        assert!(session.player.can_fly());
    }

    #[test]
    fn use_creative_rules_keeps_the_stored_save_mode() {
        let mut session = WorldSession::new_session_with_mode(
            90u32,
            PathBuf::from("/tmp/edit.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        assert_eq!(session.save_mode, GameMode::Survival);

        session.use_creative_rules();

        assert_eq!(session.player.mode(), GameMode::Creative);
        assert_eq!(
            session.save_mode,
            GameMode::Survival,
            "Edit Background must not rewrite the stored world mode"
        );
        assert!(
            !session.player.fly_mode,
            "Creative rules do not re-enable a toggle Survival turned off"
        );
    }

    #[test]
    fn v6_bytes_roundtrip_preserves_mode_vitals_and_chests() {
        use crate::world::persistence::{load_world_from_bytes, save_world_to_bytes};

        let chest = chest_with_items();
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(3, 40, -2), chest);
        chests.insert(IVec3::new(-1, 0, 3), Container::empty());

        let vitals = SurvivalState::new(7, 3, 12).expect("in-range vitals");
        let mut data = make_save(60606u32);
        data.mode = GameMode::Survival;
        data.player.fly_mode = true; // the codec must canonicalise this away
        data.player.survival = vitals;
        data.chests = chests.clone();

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v6 encodes");
        let loaded = load_world_from_bytes(&bytes).expect("v6 decodes");
        assert_eq!(loaded.mode, GameMode::Survival);
        assert_eq!(loaded.player.survival, vitals);
        assert_eq!(loaded.chests, chests);

        let session = WorldSession::session_from_save(loaded, PathBuf::from("/tmp/roundtrip.mcvx"));
        assert_eq!(session.save_mode, GameMode::Survival);
        assert_eq!(session.player.mode(), GameMode::Survival);
        assert_eq!(session.player.survival, vitals);
        assert_eq!(session.chests, chests);
        assert!(!session.player.fly_mode);
        assert!(!session.player.can_fly());
    }

    #[test]
    fn session_from_save_clamps_selected_slot_above_the_hotbar() {
        let mut data = make_save(5u32);
        data.player.selected_slot = 200;
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.player.selected_slot(), 8);
    }

    #[test]
    fn session_from_save_clamps_render_distance_through_the_world_manager() {
        let mut data = make_save(5u32);
        data.player.render_distance = 0; // below the 4..36 floor
        let session =
            WorldSession::session_from_save(data.clone(), PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.world.render_distance(), 4);

        let mut data = make_save(5u32);
        data.player.render_distance = 900; // above the 4..36 ceiling
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.world.render_distance(), 36);
    }

    #[test]
    fn session_from_save_restores_seed_and_chunk_edits() {
        use crate::world::chunk::ChunkPos;
        use crate::world::persistence::SavedPlayerState;
        use rustc_hash::FxHashMap;

        let seed = 8181u32;
        let mut edits = ChunkEditMap::default();
        let mut inner: FxHashMap<u16, crate::world::block::BlockId> = FxHashMap::default();
        inner.insert(5u16, crate::world::block::BlockId::Stone);
        edits.insert(ChunkPos { x: 1, y: 0, z: -2 }, inner);

        let data = WorldSaveData {
            seed,
            time_of_day: crate::world::daycycle::DEFAULT_TIME_OF_DAY,
            day_index: 0,
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            mode: GameMode::Creative,
            player: SavedPlayerState {
                position: [0.0, 8.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                fly_mode: true,
                selected_slot: 0,
                render_distance: 16,
                inventory: crate::player::inventory::Inventory::empty(),
                player_grid: crate::player::inventory::CraftingGrid::new(
                    crate::player::inventory::GridKind::Player,
                ),
                table_grid: crate::player::inventory::CraftingGrid::new(
                    crate::player::inventory::GridKind::Table,
                ),
                survival: SurvivalState::full(),
            },
            chests: crate::world::persistence::ChestMap::default(),
            mobs: Vec::new(),
            chunk_edits: edits.clone(),
        };
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.world.seed(), seed);
        assert_eq!(session.world.chunk_edits(), &edits);
    }

    #[test]
    fn session_from_save_restores_hotbar_stacks_exactly() {
        use crate::player::inventory::{Hotbar, ItemStack};
        use crate::world::block::BlockId;

        let hotbar = Hotbar::from_slots([
            Some(ItemStack::new(BlockId::CoalOre, 3).unwrap()),
            Some(ItemStack::new(BlockId::IronOre, 2).unwrap()),
            None,
            None,
            Some(ItemStack::new(BlockId::Stone, 999).unwrap()),
            None,
            None,
            None,
            Some(ItemStack::new(BlockId::Brick, 1).unwrap()),
        ]);
        let mut data = make_save(11u32);
        data.player.inventory = crate::player::inventory::Inventory::from_legacy_hotbar(&hotbar);

        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        for slot in 0..crate::player::inventory::HOTBAR_SLOTS {
            assert_eq!(session.player.hotbar_stack(slot), hotbar.stack(slot));
        }
        assert!(
            session.player.inventory().main(0).is_none(),
            "legacy saves default main storage to empty"
        );
        assert!(session.player.inventory().cursor().is_none());
        assert!(session.player.player_grid().is_empty());
        assert!(session.player.table_grid().is_empty());
    }

    #[test]
    fn session_from_save_preserves_tool_durability_cursor_main_slots_and_grids() {
        use crate::player::inventory::{
            CraftingGrid, GridKind, Inventory, Item, ItemId, Material, ToolKind, ToolTier,
        };
        use crate::world::block::BlockId;

        let mut slots = [None; crate::player::inventory::INVENTORY_SLOTS];
        slots[2] = Some(Item::new(ItemId::Block(BlockId::Stone), 7).expect("stone"));
        slots[crate::player::inventory::HOTBAR_SLOTS + 5] =
            Some(Item::new(ItemId::Material(Material::Stick), 4).expect("sticks"));
        let pickaxe =
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 137).expect("tool");
        let inventory = Inventory::from_parts(slots, Some(pickaxe));

        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(
                3,
                Some(Item::new(ItemId::Material(Material::Stick), 1).expect("stick")),
            )
            .expect("player grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(
                15,
                Some(Item::new(ItemId::Block(BlockId::IronOre), 2).expect("ore")),
            )
            .expect("4x4 table grid cell above index 8");

        let mut data = make_save(21u32);
        data.player.selected_slot = 2;
        data.player.inventory = inventory.clone();
        data.player.player_grid = player_grid.clone();
        data.player.table_grid = table_grid.clone();

        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.player.selected_slot(), 2);
        assert_eq!(session.player.inventory(), &inventory);
        assert_eq!(
            session
                .player
                .inventory()
                .cursor()
                .and_then(|item| item.durability()),
            Some(137),
            "cursor tool durability round-trips through the session"
        );
        assert_eq!(
            session.player.inventory().main(5).map(|item| item.count()),
            Some(4)
        );
        assert_eq!(session.player.player_grid(), &player_grid);
        assert_eq!(session.player.table_grid(), &table_grid);
    }

    #[test]
    fn session_from_save_defaults_mismatched_grid_kinds() {
        use crate::player::inventory::{CraftingGrid, GridKind, Inventory, Item, ItemId};
        use crate::world::block::BlockId;

        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(
                0,
                Some(Item::new(ItemId::Block(BlockId::Stone), 1).expect("stone")),
            )
            .expect("table grid cell");
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(
                0,
                Some(Item::new(ItemId::Block(BlockId::Dirt), 1).expect("dirt")),
            )
            .expect("player grid cell");

        let mut data = make_save(22u32);
        data.player.inventory = Inventory::empty();
        data.player.player_grid = table_grid;
        data.player.table_grid = player_grid;

        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(
            session.player.player_grid().kind(),
            GridKind::Player,
            "a 4x4 grid in the player position defaults to an empty 2x2 grid"
        );
        assert!(session.player.player_grid().is_empty());
        assert_eq!(session.player.table_grid().kind(), GridKind::Table);
        assert!(session.player.table_grid().is_empty());
    }

    #[test]
    fn new_session_starts_with_an_empty_inventory_and_grids() {
        let session = WorldSession::new_session(
            4u32,
            PathBuf::from("/tmp/fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert!(session
            .player
            .inventory()
            .slots()
            .iter()
            .all(Option::is_none));
        assert!(session.player.inventory().cursor().is_none());
        assert!(session.player.player_grid().is_empty());
        assert!(session.player.table_grid().is_empty());
        assert_eq!(session.player.selected_slot(), 0);
    }

    #[test]
    fn session_from_a_hand_built_v3_save_migrates_the_hotbar_and_defaults_new_state() {
        use crate::player::inventory::{Hotbar, ItemStack};
        use crate::world::block::BlockId;
        use crate::world::persistence::{load_world_from_bytes, SAVE_MAGIC, TIME_OF_DAY_VERSION};

        let hotbar = Hotbar::from_slots([
            Some(ItemStack::new(BlockId::Stone, 12).expect("stone")),
            None,
            None,
            Some(ItemStack::new(BlockId::CoalOre, 1).expect("coal")),
            None,
            None,
            None,
            None,
            None,
        ]);

        // A real version-3 payload: header, 27-byte hotbar, time, no chunks.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SAVE_MAGIC);
        bytes.extend_from_slice(&TIME_OF_DAY_VERSION.to_le_bytes());
        bytes.extend_from_slice(&2024u32.to_le_bytes());
        for c in [1.0f32, 64.0, -2.0] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&0.5f32.to_le_bytes());
        bytes.extend_from_slice(&(-0.25f32).to_le_bytes());
        bytes.push(1); // fly_mode
        bytes.push(4); // selected_slot
        bytes.extend_from_slice(&16u16.to_le_bytes());
        for stack in hotbar.stacks() {
            match stack {
                Some(stack) => {
                    bytes.push(stack.block.to_raw() as u8);
                    bytes.extend_from_slice(&stack.count.to_le_bytes());
                }
                None => {
                    bytes.push(0);
                    bytes.extend_from_slice(&0u16.to_le_bytes());
                }
            }
        }
        bytes.extend_from_slice(&4321u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // chunk_count

        let data = load_world_from_bytes(&bytes).expect("v3 save must migrate");
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/legacy.mcvx"));

        assert_eq!(session.player.selected_slot(), 4);
        for slot in 0..crate::player::inventory::HOTBAR_SLOTS {
            assert_eq!(session.player.hotbar_stack(slot), hotbar.stack(slot));
        }
        assert!(session.player.inventory().main(0).is_none());
        assert!(session.player.inventory().cursor().is_none());
        assert!(session.player.player_grid().is_empty());
        assert!(session.player.table_grid().is_empty());
        assert_eq!(session.clock.time_of_day, 4321);
    }

    #[test]
    fn session_from_save_syncs_the_camera_from_the_player() {
        let data = make_save(5u32);
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.camera.position, session.player.eye_position());
        assert_eq!(session.camera.yaw, session.player.yaw);
        assert_eq!(session.camera.pitch, session.player.pitch);
    }

    #[test]
    fn new_session_starts_at_default_time_of_day() {
        let session = WorldSession::new_session(
            4u32,
            PathBuf::from("/tmp/fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(
            session.clock.time_of_day,
            crate::world::daycycle::DEFAULT_TIME_OF_DAY
        );
    }

    #[test]
    fn session_from_save_restores_a_non_default_time_of_day_exactly() {
        let mut data = make_save(77u32);
        data.time_of_day = 12_345;
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(session.clock.time_of_day, 12_345);
    }

    #[test]
    fn new_session_starts_on_day_zero_with_the_requested_season_length() {
        let session = WorldSession::new_session(4u32, PathBuf::from("/tmp/fresh.mcvx"), 8);
        assert_eq!(session.clock.day_index, 0);
        assert_eq!(session.season_length_days, 8);
    }

    #[test]
    fn session_from_save_restores_the_day_index_and_season_length_exactly() {
        let mut data = make_save(77u32);
        data.time_of_day = 12_345;
        data.day_index = 21;
        data.season_length_days = 4;
        let session = WorldSession::session_from_save(data, PathBuf::from("/tmp/keep.mcvx"));
        assert_eq!(
            session.clock.day_index, 21,
            "the saved day index must survive a reload"
        );
        assert_eq!(session.clock.time_of_day, 12_345);
        assert_eq!(session.season_length_days, 4);
    }

    #[test]
    fn session_from_save_passes_the_save_path_through_unchanged() {
        let path = PathBuf::from("saves/beta.mcvx");
        let session = WorldSession::session_from_save(make_save(2u32), path.clone());
        assert_eq!(session.save_path, path);
    }

    /// A session with a deterministic all-air chunk at the origin, so table
    /// placement and raycasts never depend on generated terrain.
    fn table_session() -> WorldSession {
        use crate::world::block::BlockId;
        use crate::world::chunk::{Chunk, ChunkPos};

        let mut session = WorldSession::new_session(
            31u32,
            PathBuf::from("/tmp/table.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        session
    }

    /// Aim the player's eyes straight down from three blocks above `pos`.
    fn aim_down_at(session: &mut WorldSession, pos: IVec3) {
        session.player.position =
            Vec3::new(pos.x as f32 + 0.5, pos.y as f32 + 3.0, pos.z as f32 + 0.5);
        session.player.yaw = 0.0;
        session.player.pitch = -std::f32::consts::FRAC_PI_2;
        session.sync_camera();
    }

    #[test]
    fn targeting_a_placed_table_opens_it() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::CraftingTable);
        aim_down_at(&mut session, pos);
        assert_eq!(session.open_table_position(), None);

        assert_eq!(session.try_open_targeted_table(), Some(pos));
        assert_eq!(session.open_table_position(), Some(pos));
        // A still-valid table survives re-validation.
        assert_eq!(session.sync_open_table(), Some(pos));
        assert_eq!(session.open_table_position(), Some(pos));
    }

    #[test]
    fn targeting_a_non_table_block_does_not_open() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Stone);
        aim_down_at(&mut session, pos);

        assert_eq!(session.try_open_targeted_table(), None);
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn targeting_air_does_not_open() {
        let mut session = table_session();
        aim_down_at(&mut session, IVec3::new(0, 5, 0));

        assert_eq!(session.try_open_targeted_table(), None);
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn a_replaced_table_closes_the_open_station() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::CraftingTable);
        aim_down_at(&mut session, pos);
        assert_eq!(session.try_open_targeted_table(), Some(pos));

        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Stone);

        assert_eq!(session.sync_open_table(), None);
        assert_eq!(session.open_table_position(), None);
        assert!(session.try_open_targeted_table().is_none());
    }

    #[test]
    fn a_removed_table_closes_the_open_station() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::CraftingTable);
        aim_down_at(&mut session, pos);
        assert_eq!(session.try_open_targeted_table(), Some(pos));

        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Air);

        assert_eq!(session.sync_open_table(), None);
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn walking_out_of_reach_closes_the_open_station() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::CraftingTable);
        aim_down_at(&mut session, pos);
        assert_eq!(session.try_open_targeted_table(), Some(pos));

        session.player.position = Vec3::new(20.5, 8.0, 0.5);
        session.sync_camera();

        assert_eq!(session.sync_open_table(), None);
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn an_out_of_reach_table_is_rejected_even_when_valid() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let far = IVec3::new(20, 5, 0);
        session
            .world
            .set_block_immediate(far.x, far.y, far.z, BlockId::CraftingTable);
        aim_down_at(&mut session, IVec3::new(0, 5, 0));

        assert!(
            !session.open_table_at(far),
            "a table beyond the reach budget must not open"
        );
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn missing_chunk_and_air_coordinates_are_rejected() {
        let mut session = table_session();

        assert!(
            !session.open_table_at(IVec3::new(0, 5, 0)),
            "air is not a table"
        );
        assert!(
            !session.open_table_at(IVec3::new(500, 5, 500)),
            "unloaded chunk"
        );
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn a_rejected_target_does_not_close_an_already_open_table() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let open = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(open.x, open.y, open.z, BlockId::CraftingTable);
        aim_down_at(&mut session, open);
        assert_eq!(session.try_open_targeted_table(), Some(open));

        // Failed interactions are side-effect free: they must not silently
        // close a table that is still valid.
        assert!(!session.open_table_at(IVec3::new(0, 6, 0)));
        assert!(!session.open_table_at(IVec3::new(500, 5, 500)));
        assert_eq!(session.open_table_position(), Some(open));
    }

    #[test]
    fn close_table_clears_the_open_state_idempotently() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::CraftingTable);
        aim_down_at(&mut session, pos);
        assert_eq!(session.try_open_targeted_table(), Some(pos));

        session.close_table();
        assert_eq!(session.open_table_position(), None);
        session.close_table();
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn table_open_state_is_never_restored_from_a_save() {
        use crate::world::block::BlockId;

        let mut session = table_session();
        let pos = IVec3::new(0, 5, 0);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::CraftingTable);
        aim_down_at(&mut session, pos);
        assert!(session.try_open_targeted_table().is_some());

        // Ephemeral by construction: the save schema has no table-open field,
        // and both constructors start closed.
        let restored =
            WorldSession::session_from_save(make_save(31u32), PathBuf::from("/tmp/table.mcvx"));
        assert_eq!(restored.open_table_position(), None);
        assert_eq!(
            WorldSession::new_session(
                31u32,
                PathBuf::from("/tmp/fresh.mcvx"),
                DEFAULT_SEASON_LENGTH_DAYS
            )
            .open_table_position(),
            None
        );
    }

    // -----------------------------------------------------------------------
    // Chest station
    // -----------------------------------------------------------------------

    /// A session with a deterministic all-air chunk at the origin, so chest
    /// placement and raycasts never depend on generated terrain.
    fn chest_session() -> WorldSession {
        use crate::world::block::BlockId;
        use crate::world::chunk::{Chunk, ChunkPos};

        let mut session = WorldSession::new_session(
            37u32,
            PathBuf::from("/tmp/chest.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        session
    }

    /// Places a block and clears the setup-time dirty flag, so tests can
    /// attribute `has_unsaved_changes()` to the chest command under test.
    fn place_block(session: &mut WorldSession, pos: IVec3, block: crate::world::block::BlockId) {
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, block);
        session.world.mark_saved();
    }

    fn stone(count: u16) -> Item {
        Item::new(ItemId::Block(BlockId::Stone), count).expect("valid stone stack")
    }

    fn chest_item() -> Item {
        Item::new(ItemId::Block(BlockId::Chest), 1).expect("valid chest item")
    }

    /// Inserts one stack into the empty inventory and picks it up, leaving it
    /// on the shared cursor for a chest click.
    fn give_to_cursor(session: &mut WorldSession, item: Item) {
        session
            .player
            .insert_items(&[item])
            .expect("empty inventory accepts one stack");
        let slot = session
            .player
            .inventory()
            .slots()
            .iter()
            .position(|stack| stack.is_some())
            .expect("the inserted stack is present");
        session
            .player
            .click_inventory_slot(slot)
            .expect("pick the inserted stack into the cursor");
        assert_eq!(session.player.inventory().cursor(), Some(item));
    }

    #[test]
    fn every_constructor_starts_with_no_open_chest() {
        let fresh = WorldSession::new_session(
            41u32,
            PathBuf::from("/tmp/chest-fresh.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert_eq!(fresh.open_chest_position(), None);

        let survival = WorldSession::new_session_with_mode(
            42u32,
            PathBuf::from("/tmp/chest-survival.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        assert_eq!(survival.open_chest_position(), None);

        let editor = WorldSession::new_at(
            43u32,
            PathBuf::from("/tmp/chest-editor.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(0.5, 40.0, 0.5),
            GameClock::new(),
        );
        assert_eq!(editor.open_chest_position(), None);

        let restored =
            WorldSession::session_from_save(make_save(44u32), PathBuf::from("/tmp/chest.mcvx"));
        assert_eq!(restored.open_chest_position(), None);
    }

    #[test]
    fn targeting_a_placed_chest_opens_it_without_creating_a_record() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);

        assert_eq!(session.try_open_targeted_chest(), Some(pos));
        assert_eq!(session.open_chest_position(), Some(pos));
        assert_eq!(session.sync_open_chest(), Some(pos));
        assert!(session.chests.is_empty(), "opening alone creates no record");
        assert!(!session.world.has_unsaved_changes());
        assert_eq!(session.open_chest_contents(), None);
    }

    #[test]
    fn a_loot_chest_is_container_interactable() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::LootChest);
        aim_down_at(&mut session, pos);

        assert!(session.open_chest_at(pos));
        assert_eq!(session.open_chest_position(), Some(pos));
        assert!(session.chests.is_empty());
    }

    #[test]
    fn a_non_container_target_does_not_open_a_chest() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Stone);
        aim_down_at(&mut session, pos);

        assert_eq!(session.try_open_targeted_chest(), None);
        assert_eq!(session.open_chest_position(), None);
    }

    #[test]
    fn chest_and_table_opens_are_mutually_exclusive() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let chest_pos = IVec3::new(0, 5, 0);
        let table_pos = IVec3::new(1, 5, 0);
        place_block(&mut session, chest_pos, BlockId::Chest);
        place_block(&mut session, table_pos, BlockId::CraftingTable);
        aim_down_at(&mut session, chest_pos);

        assert!(session.open_chest_at(chest_pos));
        assert!(session.open_table_at(table_pos));
        assert_eq!(session.open_table_position(), Some(table_pos));
        assert_eq!(
            session.open_chest_position(),
            None,
            "opening a table closes the chest station"
        );

        assert!(session.open_chest_at(chest_pos));
        assert_eq!(session.open_chest_position(), Some(chest_pos));
        assert_eq!(
            session.open_table_position(),
            None,
            "opening a chest closes the table station"
        );
    }

    #[test]
    fn a_rejected_chest_open_mutates_nothing() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let open = IVec3::new(0, 5, 0);
        let far = IVec3::new(20, 5, 0);
        place_block(&mut session, open, BlockId::Chest);
        place_block(&mut session, far, BlockId::Chest);
        aim_down_at(&mut session, open);
        assert!(session.open_chest_at(open));

        assert!(
            !session.open_chest_at(IVec3::new(0, 6, 0)),
            "air is not a chest"
        );
        assert!(
            !session.open_chest_at(IVec3::new(500, 5, 500)),
            "unloaded chunk"
        );
        assert!(!session.open_chest_at(far), "beyond the reach budget");
        assert!(!session.open_table_at(open), "a chest is not a table");
        assert!(
            !session.open_table_at(IVec3::new(0, 6, 0)),
            "a rejected table open is side-effect free"
        );

        assert_eq!(session.open_chest_position(), Some(open));
        assert_eq!(session.open_table_position(), None);
        assert!(session.chests.is_empty());
        assert!(!session.world.has_unsaved_changes());
    }

    #[test]
    fn the_first_transfer_creates_a_record_and_dirties_the_save() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);
        assert!(session.open_chest_at(pos));

        give_to_cursor(&mut session, stone(4));
        assert!(session.chests.is_empty());
        assert!(!session.world.has_unsaved_changes());

        session
            .click_open_chest_slot(5)
            .expect("cursor into the chest");

        let record = session
            .chest_at(pos)
            .expect("first transfer creates a record");
        assert_eq!(record.slot(5), Some(stone(4)));
        assert_eq!(session.player.inventory().cursor(), None);
        assert_eq!(
            session
                .open_chest_contents()
                .and_then(|chest| chest.slot(5)),
            Some(stone(4))
        );
        assert!(session.world.has_unsaved_changes());
    }

    #[test]
    fn no_op_and_failed_transfers_create_no_record() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);
        assert!(session.open_chest_at(pos));

        // Empty cursor into an empty slot is a successful inert no-op.
        session
            .click_open_chest_slot(0)
            .expect("empty/empty click is a no-op");
        assert!(session.chests.is_empty(), "a no-op click creates no record");
        assert!(!session.world.has_unsaved_changes());

        // A rejected index is a failed transfer and must not create one.
        assert_eq!(
            session.click_open_chest_slot(CHEST_SLOTS),
            Err(ChestCommandError::Slot(InventoryError::OutOfBounds))
        );
        assert!(
            session.chests.is_empty(),
            "a failed click creates no record"
        );
        assert!(!session.world.has_unsaved_changes());
    }

    #[test]
    fn taking_the_last_item_retains_a_present_empty_record() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);
        let mut slots = [None; CHEST_SLOTS];
        slots[0] = Some(stone(1));
        session.chests.insert(pos, Container::from_slots(slots));
        assert!(session.open_chest_at(pos));

        session
            .click_open_chest_slot(0)
            .expect("chest into the cursor");

        assert_eq!(session.player.inventory().cursor(), Some(stone(1)));
        assert_eq!(
            session.chest_at(pos),
            Some(&Container::empty()),
            "a touched-empty chest keeps its record"
        );
        assert!(session.world.has_unsaved_changes());
    }

    #[test]
    fn a_no_op_click_retains_an_existing_all_empty_record() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);
        session.chests.insert(pos, Container::empty());
        assert!(session.open_chest_at(pos));

        session
            .click_open_chest_slot(3)
            .expect("empty/empty click is a no-op");

        assert_eq!(session.chest_at(pos), Some(&Container::empty()));
        assert!(!session.world.has_unsaved_changes());
    }

    #[test]
    fn removing_or_replacing_the_block_closes_the_station() {
        use crate::world::block::BlockId;

        for replacement in [BlockId::Air, BlockId::Stone] {
            let mut session = chest_session();
            let pos = IVec3::new(0, 5, 0);
            place_block(&mut session, pos, BlockId::Chest);
            aim_down_at(&mut session, pos);
            assert!(session.open_chest_at(pos));

            place_block(&mut session, pos, replacement);

            assert_eq!(session.sync_open_chest(), None);
            assert_eq!(session.open_chest_position(), None);
            assert_eq!(
                session.click_open_chest_slot(0),
                Err(ChestCommandError::NotOpen)
            );
            assert!(session.chests.is_empty());
        }
    }

    #[test]
    fn walking_out_of_reach_closes_the_chest_station() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);
        assert!(session.open_chest_at(pos));

        session.player.position = Vec3::new(20.5, 8.0, 0.5);
        session.sync_camera();

        assert_eq!(session.sync_open_chest(), None);
        assert_eq!(session.open_chest_position(), None);
        assert_eq!(
            session.click_open_chest_slot(0),
            Err(ChestCommandError::NotOpen)
        );
    }

    #[test]
    fn close_chest_helpers_are_idempotent_and_position_guarded() {
        use crate::world::block::BlockId;

        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::Chest);
        aim_down_at(&mut session, pos);
        assert!(session.open_chest_at(pos));

        session.close_chest_at(IVec3::new(1, 5, 0));
        assert_eq!(
            session.open_chest_position(),
            Some(pos),
            "a different position must not close the station"
        );
        session.close_chest_at(pos);
        assert_eq!(session.open_chest_position(), None);

        session.close_chest();
        session.close_chest();
        assert_eq!(session.open_chest_position(), None);
    }

    #[test]
    fn chest_break_items_are_contents_plus_exactly_one_chest() {
        let mut session = chest_session();
        let pos = IVec3::new(4, 40, -2);

        assert_eq!(session.chest_break_items(pos), vec![chest_item()]);

        session.chests.insert(pos, Container::empty());
        assert_eq!(session.chest_break_items(pos), vec![chest_item()]);

        let contents = chest_with_items();
        let expected = [
            contents.slot(0).expect("slot 0"),
            contents.slot(13).expect("slot 13"),
            contents.slot(26).expect("slot 26"),
            chest_item(),
        ];
        session.chests.insert(pos, contents);
        assert_eq!(session.chest_break_items(pos), expected);
        assert_eq!(
            session
                .chest_break_items(pos)
                .iter()
                .filter(|item| item.id() == ItemId::Block(BlockId::Chest))
                .count(),
            1,
            "the chest drops exactly once"
        );
    }

    // -----------------------------------------------------------------------
    // Ruin LootChest materialization and unopened-break spill
    // -----------------------------------------------------------------------

    /// Every generated LootChest in a bounded chunk region (radius, cy 0..=2,
    /// Summer), scanned through the same `generate_chunk_seeded` seam
    /// worldgen uses.
    fn generated_loot_chests(seed: u32, radius: i32) -> Vec<IVec3> {
        use crate::world::chunk::{ChunkPos, CHUNK_SIZE};
        use crate::world::terrain::generate_chunk_seeded;

        let mut chests = Vec::new();
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for cy in 0..=2 {
                    let chunk =
                        generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                if chunk.get(lx, ly, lz) == BlockId::LootChest {
                                    chests.push(IVec3::new(
                                        cx * CHUNK_SIZE as i32 + lx as i32,
                                        cy * CHUNK_SIZE as i32 + ly as i32,
                                        cz * CHUNK_SIZE as i32 + lz as i32,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        chests
    }

    /// The first generated LootChest in the region; fails loudly rather than
    /// letting the test pass vacuously.
    fn generated_loot_chest(seed: u32, radius: i32) -> IVec3 {
        let chests = generated_loot_chests(seed, radius);
        assert!(
            !chests.is_empty(),
            "no generated LootChest in the {radius}-chunk region for seed {seed}"
        );
        chests[0]
    }

    /// Inserts the generated chunk that owns `pos`, so the live world holds
    /// the same LootChest the seed's worldgen produces.
    fn insert_generated_chunk(
        session: &mut WorldSession,
        pos: IVec3,
    ) -> crate::world::chunk::ChunkPos {
        use crate::world::terrain::generate_chunk_seeded;

        let (chunk_pos, _) = world_to_chunk_and_local(pos.x, pos.y, pos.z);
        let chunk = generate_chunk_seeded(chunk_pos, session.world.seed(), Season::Summer);
        session.world.insert_chunk_for_test(chunk_pos, chunk);
        chunk_pos
    }

    /// The validated inventory items a generated chest's table yields, in
    /// slot order.
    fn loot_chest_items(pos: IVec3, seed: u32) -> Vec<Item> {
        ruin_loot_chest_at(pos, seed)
            .expect("a generated LootChest must resolve its own table")
            .iter()
            .map(|entry| {
                Item::new(ItemId::from(entry.item), entry.min_count).expect("valid loot stack")
            })
            .collect()
    }

    /// Takes every non-empty chest stack through the real click seam and puts
    /// each on an empty inventory slot.
    fn take_all_from_open_chest(session: &mut WorldSession) {
        for index in 0..CHEST_SLOTS {
            let occupied = session
                .open_chest_contents()
                .and_then(|chest| chest.slot(index))
                .is_some();
            if !occupied {
                continue;
            }
            session
                .click_open_chest_slot(index)
                .expect("chest slot into the cursor");
            let empty = (0..crate::player::inventory::INVENTORY_SLOTS)
                .find(|&slot| session.player.inventory().slot(slot).is_none())
                .expect("an empty inventory slot accepts the stack");
            session
                .player
                .click_inventory_slot(empty)
                .expect("cursor into the inventory");
        }
        assert!(
            session.player.inventory().cursor().is_none(),
            "every taken stack was deposited"
        );
    }

    #[test]
    fn loot_items_fails_closed_on_malformed_tables() {
        use crate::world::loot::{LootEntry, LootItem};

        let valid = LootEntry {
            item: LootItem::Wheat,
            min_count: 2,
            max_count: 2,
        };
        assert_eq!(loot_items(&[]), None, "an empty table is malformed");
        let oversized = [valid; CHEST_SLOTS + 1];
        assert!(
            loot_items(&oversized).is_none(),
            "more entries than chest slots is malformed"
        );
        assert!(
            loot_items(&[LootEntry {
                min_count: 0,
                max_count: 0,
                ..valid
            }])
            .is_none(),
            "a zero count is malformed"
        );
        assert!(
            loot_items(&[LootEntry {
                min_count: 1,
                max_count: 2,
                ..valid
            }])
            .is_none(),
            "an unrolled range is malformed"
        );
        assert_eq!(
            loot_items(&[valid]),
            Some(vec![
                Item::new(ItemId::from(valid.item), 2).expect("a valid wheat stack")
            ]),
            "a well-formed table maps in slot order"
        );
    }

    #[test]
    fn a_generated_ruin_loot_chest_materializes_deterministic_contents_on_first_open() {
        let seed = 37u32;
        let mut session = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/loot-first-open.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let pos = generated_loot_chest(seed, 3);
        insert_generated_chunk(&mut session, pos);
        aim_down_at(&mut session, pos);
        assert_eq!(
            session.world.get_block(pos.x, pos.y, pos.z),
            BlockId::LootChest
        );
        assert!(
            session.chest_at(pos).is_none(),
            "no record before the first open"
        );
        assert!(!session.world.has_unsaved_changes());

        assert!(session.open_chest_at(pos));

        let expected = loot_chest_items(pos, seed);
        assert_eq!(
            session.chest_at(pos).map(Container::contents),
            Some(expected.clone()),
            "first open materializes the deterministic table"
        );
        assert_eq!(
            session.open_chest_contents().map(Container::contents),
            Some(expected)
        );
        assert_eq!(session.chests.len(), 1, "exactly one record");
        assert!(
            !session.world.has_unsaved_changes(),
            "materializing generated loot is not a player content change"
        );
    }

    #[test]
    fn emptying_a_ruin_loot_chest_persists_a_present_empty_record_across_save_and_reload() {
        use crate::world::persistence::{
            load_world_from_path, save_world_to_path, SavedPlayerState,
        };

        let seed = 37u32;
        let mut session = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/loot-empty.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let pos = generated_loot_chest(seed, 3);
        insert_generated_chunk(&mut session, pos);
        aim_down_at(&mut session, pos);
        assert!(session.open_chest_at(pos));
        take_all_from_open_chest(&mut session);

        assert_eq!(
            session.chest_at(pos),
            Some(&Container::empty()),
            "a looted chest keeps a present-empty record"
        );
        assert!(session.world.has_unsaved_changes());

        let data = WorldSaveData {
            seed: session.world.seed(),
            time_of_day: session.clock.time_of_day,
            day_index: session.clock.day_index,
            season_length_days: session.season_length_days,
            mode: session.save_mode,
            player: SavedPlayerState {
                position: session.player.position.to_array(),
                yaw: session.player.yaw,
                pitch: session.player.pitch,
                fly_mode: session.player.fly_mode,
                selected_slot: session.player.selected_slot() as u8,
                render_distance: session.world.render_distance() as u16,
                inventory: session.player.inventory().clone(),
                player_grid: session.player.player_grid().clone(),
                table_grid: session.player.table_grid().clone(),
                survival: session.player.survival,
            },
            chests: session.chests.clone(),
            mobs: Vec::new(),
            chunk_edits: session.world.chunk_edits().clone(),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loot-empty.mcvx");
        save_world_to_path(&path, &data).expect("the save writes");
        let loaded = load_world_from_path(&path).expect("the save loads");
        let mut restored = WorldSession::session_from_save(loaded, path);
        insert_generated_chunk(&mut restored, pos);

        assert_eq!(
            restored.chest_at(pos),
            Some(&Container::empty()),
            "reload must not re-roll a looted chest"
        );
        assert!(restored.open_chest_at(pos));
        assert_eq!(
            restored.open_chest_contents(),
            Some(&Container::empty()),
            "the reloaded chest stays present-empty"
        );
    }

    #[test]
    fn an_arbitrary_loot_chest_opens_empty_without_a_record() {
        let mut session = chest_session();
        let pos = IVec3::new(0, 5, 0);
        place_block(&mut session, pos, BlockId::LootChest);
        aim_down_at(&mut session, pos);

        assert!(session.open_chest_at(pos));
        assert!(
            session.chests.is_empty(),
            "a LootChest outside a generated ruin opens empty"
        );
        assert_eq!(session.open_chest_contents(), None);
    }

    #[test]
    fn an_unopened_ruin_loot_chest_derives_break_items_without_mutating() {
        let seed = 37u32;
        let mut session = WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/loot-break-items.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let pos = generated_loot_chest(seed, 3);
        insert_generated_chunk(&mut session, pos);
        assert!(session.chests.is_empty());

        let mut expected = loot_chest_items(pos, seed);
        expected.push(chest_item());

        assert_eq!(
            session.chest_break_items(pos),
            expected,
            "an unopened chest derives the same loot first open would materialize"
        );
        assert!(
            session.chests.is_empty(),
            "deriving break items creates no record"
        );
        assert!(!session.world.has_unsaved_changes());
    }

    // -----------------------------------------------------------------------
    // Survival session loop: spawn settle, tick, death, and respawn
    // -----------------------------------------------------------------------

    fn survival_session(seed: u32) -> WorldSession {
        WorldSession::new_session_with_mode(
            seed,
            PathBuf::from("/tmp/session-survival.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        )
    }

    /// Ticks `seconds` game-seconds through the session seam, one second per
    /// call (raw dt 0.1 x time_scale 10).
    fn tick_game_seconds(
        session: &mut WorldSession,
        seconds: u32,
        head_submerged: bool,
    ) -> SurvivalTick {
        let mut total = SurvivalTick::default();
        for _ in 0..seconds {
            let tick = session.tick_survival(0.1, 10.0, head_submerged);
            total.damage = total.damage.saturating_add(tick.damage);
            total.healed = total.healed.saturating_add(tick.healed);
        }
        total
    }

    /// Runs the real walk-mode physics against the session's live world.
    fn run_physics(session: &mut WorldSession, frames: usize) {
        let input = InputState::default();
        for _ in 0..frames {
            session.player.update(1.0 / 60.0, &input, |x, y, z| {
                session.world.get_block(x, y, z)
            });
        }
    }

    /// Lays a 3x3 Stone floor at `y` under the (16.5, *, 16.5) scratch column.
    fn lay_scratch_floor(session: &mut WorldSession, y: i32) {
        for x in 15..=17 {
            for z in 15..=17 {
                session.world.set_block_immediate(x, y, z, BlockId::Stone);
            }
        }
    }

    #[test]
    fn spawn_position_matches_the_constructor_and_the_pinned_formula() {
        let seed = 12345u32;
        let (spawn_x, spawn_z) = find_spawn_column(seed);
        let pinned = Vec3::new(
            spawn_x as f32 + 0.5,
            (terrain_height_at_seeded(spawn_x, spawn_z, seed) + 4) as f32,
            spawn_z as f32 + 0.5,
        );
        assert_eq!(WorldSession::spawn_position(seed), pinned);

        let session = WorldSession::new_session_with_mode(
            seed,
            PathBuf::from("/tmp/spawn.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        assert_eq!(session.player.position, pinned);
        assert_eq!(session.camera.position, session.player.eye_position());
        assert!(session.spawn_settle);
    }

    #[test]
    fn survival_spawn_waits_for_the_feet_and_support_chunks() {
        use crate::world::chunk::{Chunk, ChunkPos};

        let mut session = survival_session(18);
        // Feet at y=34 and support at y=30 straddle the y=32 chunk boundary.
        session.player.position = Vec3::new(0.5, 34.0, 0.5);
        assert!(session.spawn_settle);
        assert!(!session.settle_spawn(), "no chunks are resident yet");

        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 1, 0), Chunk::new_uniform(BlockId::Air));
        assert!(
            !session.settle_spawn(),
            "the feet chunk alone is not enough"
        );

        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Air));
        assert!(session.settle_spawn());
        assert!(!session.spawn_settle, "the gate clears after settling");
        assert!(
            session.settle_spawn(),
            "a settled session is never gated again"
        );

        let mut creative = WorldSession::new_session_with_mode(
            19u32,
            PathBuf::from("/tmp/settle-creative.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
        );
        assert!(creative.settle_spawn(), "Creative is never spawn-gated");
        assert!(!creative.spawn_settle);

        let mut restored = WorldSession::session_from_save(
            make_save(20u32),
            PathBuf::from("/tmp/settle-restored.mcvx"),
        );
        assert!(restored.settle_spawn(), "a restored session is never gated");
    }

    #[test]
    fn respawn_restores_spawn_and_vitals_while_preserving_inventory_and_selection() {
        use crate::world::chunk::{Chunk, ChunkPos};

        let seed = 77u32;
        let mut session = survival_session(seed);
        session
            .player
            .insert_items(&[
                Item::new(ItemId::Block(BlockId::Stone), 7).expect("stone"),
                Item::new(ItemId::Material(Material::Apple), 2).expect("apple"),
            ])
            .expect("the empty inventory accepts the stacks");
        assert!(session.player.select_slot(4));
        let inventory = session.player.inventory().clone();
        let selected = session.player.selected_slot();

        // Open a chest station so respawn has something to close.
        session.player.position = Vec3::new(16.5, 40.0, 16.5);
        session.sync_camera();
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 1, 0), Chunk::new_uniform(BlockId::Air));
        session
            .world
            .set_block_immediate(16, 40, 17, BlockId::Chest);
        assert!(session.open_chest_at(IVec3::new(16, 40, 17)));
        assert_eq!(session.open_chest_position(), Some(IVec3::new(16, 40, 17)));

        // Transient state that respawn must overwrite.
        session.player.survival = SurvivalState::new(0, 3, 12).expect("in-range vitals");
        session.player.velocity = Vec3::new(1.0, 5.0, -2.0);
        session.player.on_ground = true;

        session.respawn();

        assert_eq!(session.player.position, WorldSession::spawn_position(seed));
        assert_eq!(session.player.velocity, Vec3::ZERO);
        assert!(!session.player.on_ground);
        assert_eq!(session.player.survival, SurvivalState::full());
        assert!(session.spawn_settle);
        assert_eq!(session.camera.position, session.player.eye_position());
        assert!(!session.player.can_fly());
        assert_eq!(session.open_chest_position(), None);
        assert_eq!(session.player.inventory(), &inventory);
        assert_eq!(session.player.selected_slot(), selected);

        // Respawn re-arms the one-shot first-contact immunity: a 5-block drop
        // onto a fresh scratch floor would otherwise cost 2 health.
        lay_scratch_floor(&mut session, 36);
        session.player.position = Vec3::new(16.5, 42.0, 16.5);
        session.sync_camera();
        assert!(session.settle_spawn());
        run_physics(&mut session, 60);
        assert!(session.player.on_ground, "the respawned player landed");
        assert_eq!(session.tick_survival(0.1, 10.0, false).damage, 0);
        assert_eq!(session.player.survival.health(), 20);
    }

    #[test]
    fn tick_survival_scales_dt_by_time_scale_and_clamps_raw_dt() {
        let mut session = survival_session(22);
        let tick = session.tick_survival(0.1, 10.0, true);
        assert_eq!(tick, SurvivalTick::default());
        assert_eq!(session.player.survival.air(), FULL_AIR - 20);

        // Raw dt is clamped to 0.1 before scaling: 0.2 x 1 is still 0.1.
        let air = session.player.survival.air();
        session.tick_survival(0.2, 1.0, true);
        assert_eq!(session.player.survival.air(), air - 2);

        // A negative scale cannot run time backwards.
        let air = session.player.survival.air();
        session.tick_survival(0.1, -5.0, true);
        assert_eq!(session.player.survival.air(), air);
    }

    #[test]
    fn tick_survival_drowns_after_air_runs_out_and_refills_on_the_surface() {
        let mut session = survival_session(23);
        tick_game_seconds(&mut session, 15, true);
        assert_eq!(session.player.survival.air(), 0);
        assert_eq!(session.player.survival.health(), 20);
        assert!(!session.is_dead());

        tick_game_seconds(&mut session, 10, true);
        assert_eq!(session.player.survival.health(), 0);
        assert!(session.is_dead());

        // A zero-health session ticks inert.
        let after_death = session.player.survival;
        assert_eq!(
            session.tick_survival(0.1, 10.0, true),
            SurvivalTick::default()
        );
        assert_eq!(session.player.survival, after_death);

        // Surfacing refills to FULL_AIR, not the wire ceiling.
        let mut surfaced = survival_session(24);
        tick_game_seconds(&mut surfaced, 5, true);
        assert_eq!(surfaced.player.survival.air(), FULL_AIR - 100);
        surfaced.tick_survival(0.1, 10.0, false);
        assert_eq!(surfaced.player.survival.air(), FULL_AIR);
    }

    #[test]
    fn tick_survival_starves_eighty_game_seconds_after_hunger_is_empty() {
        let mut session = survival_session(29);
        session.player.survival = SurvivalState::new(20, 0, FULL_AIR).expect("in-range vitals");

        tick_game_seconds(&mut session, 79, false);
        assert_eq!(session.player.survival.health(), 1);
        assert!(!session.is_dead());

        tick_game_seconds(&mut session, 1, false);
        assert_eq!(session.player.survival.health(), 0);
        assert!(session.is_dead());
    }

    #[test]
    fn tick_survival_regenerates_only_at_hunger_eighteen_and_spends_hunger() {
        let mut below = survival_session(31);
        below.player.survival = SurvivalState::new(10, 17, FULL_AIR).expect("in-range vitals");
        let tick = tick_game_seconds(&mut below, 4, false);
        assert_eq!(tick.healed, 0);
        assert_eq!(below.player.survival.health(), 10);
        assert_eq!(below.player.survival.hunger(), 17);

        let mut session = survival_session(32);
        session.player.survival = SurvivalState::new(10, 18, FULL_AIR).expect("in-range vitals");
        let tick = tick_game_seconds(&mut session, 4, false);
        assert_eq!(tick.healed, 1);
        assert_eq!(session.player.survival.health(), 11);
        assert_eq!(session.player.survival.hunger(), 17);

        // One heal drops hunger below the threshold, stopping regeneration.
        let tick = tick_game_seconds(&mut session, 4, false);
        assert_eq!(tick.healed, 0);
        assert_eq!(session.player.survival.health(), 11);
        assert_eq!(session.player.survival.hunger(), 17);
    }

    #[test]
    fn creative_sessions_never_lose_vitals_from_falls_or_submersion() {
        use crate::world::chunk::{Chunk, ChunkPos};

        let mut session = WorldSession::new_session_with_mode(
            33u32,
            PathBuf::from("/tmp/session-creative.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
        );
        session.player.survival = SurvivalState::new(5, 3, 12).expect("in-range vitals");
        assert!(
            session.player.toggle_fly(),
            "a Creative player may walk under Creative rules"
        );
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 1, 0), Chunk::new_uniform(BlockId::Air));
        lay_scratch_floor(&mut session, 36);
        session.player.position = Vec3::new(16.5, 42.0, 16.5);
        session.sync_camera();
        let before = session.player.survival;

        run_physics(&mut session, 90);
        assert!(session.player.on_ground, "the Creative body still lands");
        let tick = session.tick_survival(0.1, 10.0, true);
        assert_eq!(tick, SurvivalTick::default());
        assert_eq!(session.player.survival, before);
    }

    #[test]
    fn the_landing_latch_is_consumed_exactly_once() {
        use crate::world::chunk::{Chunk, ChunkPos};

        let mut session = survival_session(71);
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 1, 0), Chunk::new_uniform(BlockId::Air));
        lay_scratch_floor(&mut session, 36);

        // Spend the one-shot spawn immunity on the safe 3-block spawn drop.
        session.player.position = Vec3::new(16.5, 40.0, 16.5);
        session.sync_camera();
        assert!(session.settle_spawn());
        run_physics(&mut session, 60);
        assert!(session.player.on_ground);
        assert_eq!(session.tick_survival(0.1, 10.0, false).damage, 0);

        // A later 5-block dry landing applies damage once...
        session.player.position = Vec3::new(16.5, 42.0, 16.5);
        session.player.velocity = Vec3::ZERO;
        session.sync_camera();
        run_physics(&mut session, 60);
        assert!(session.player.on_ground);
        let first = session.tick_survival(0.1, 10.0, false);
        assert_eq!(first.damage, 2);
        assert_eq!(session.player.survival.health(), 18);

        // ...and the cleared latch never damages a second time.
        let second = session.tick_survival(0.1, 10.0, false);
        assert_eq!(second.damage, 0);
        assert_eq!(session.player.survival.health(), 18);
    }

    #[test]
    fn lethal_fall_kills_then_respawn_restores_spawn_vitals_inventory_and_selection() {
        use crate::world::chunk::{Chunk, ChunkPos};

        let seed = 61u32;
        let mut session = survival_session(seed);
        session
            .player
            .insert_items(&[
                Item::new(ItemId::Block(BlockId::Stone), 5).expect("stone"),
                Item::new(ItemId::Material(Material::Bread), 3).expect("bread"),
            ])
            .expect("the empty inventory accepts the stacks");
        assert!(session.player.select_slot(4));
        let inventory = session.player.inventory().clone();
        let selected = session.player.selected_slot();

        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 1, 0), Chunk::new_uniform(BlockId::Air));
        lay_scratch_floor(&mut session, 36);
        session.player.position = Vec3::new(16.5, 40.0, 16.5);
        session.sync_camera();
        assert!(session.settle_spawn(), "the scratch chunk is resident");

        // First landing: spawn immunity discards the 3-block drop.
        run_physics(&mut session, 60);
        assert!(session.player.on_ground, "the player landed on the floor");
        assert_eq!(session.player.take_landing_fall(), 0.0);
        assert_eq!(session.player.survival.health(), 20);

        // Teleport high above the same floor and fall until the drop kills.
        session.player.position = Vec3::new(16.5, 300.0, 16.5);
        session.player.velocity = Vec3::ZERO;
        session.sync_camera();
        for _ in 0..900 {
            session
                .player
                .update(1.0 / 60.0, &InputState::default(), |x, y, z| {
                    session.world.get_block(x, y, z)
                });
            session.tick_survival(1.0 / 60.0, 1.0, false);
            if session.is_dead() {
                break;
            }
        }
        assert!(session.is_dead(), "a fall from y=300 must be lethal");
        assert_eq!(session.player.survival.health(), 0);

        session.respawn();

        assert_eq!(session.player.position, WorldSession::spawn_position(seed));
        assert_eq!(session.player.survival, SurvivalState::full());
        assert_eq!(session.player.velocity, Vec3::ZERO);
        assert!(session.spawn_settle);
        assert_eq!(session.player.inventory(), &inventory);
        assert_eq!(session.player.selected_slot(), selected);
        assert!(!session.player.can_fly());
    }

    // -----------------------------------------------------------------------
    // Hoe and wheat farming seams
    // -----------------------------------------------------------------------

    /// Inserts an all-air chunk plus its six face neighbours, so
    /// `set_block_immediate` returns remesh work for edits inside `chunk`.
    fn insert_air_chunk_with_face_neighbors(session: &mut WorldSession, chunk: ChunkPos) {
        use crate::world::chunk::Chunk;

        session
            .world
            .insert_chunk_for_test(chunk, Chunk::new_uniform(BlockId::Air));
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let neighbor = ChunkPos::new(chunk.x + dx, chunk.y + dy, chunk.z + dz);
            session
                .world
                .insert_chunk_for_test(neighbor, Chunk::new_uniform(BlockId::Air));
        }
    }

    /// A Creative session with a deterministic all-air origin chunk and faces.
    fn farm_session(seed: u32, season_length_days: u32) -> WorldSession {
        let mut session =
            WorldSession::new_session(seed, PathBuf::from("/tmp/farm.mcvx"), season_length_days);
        insert_air_chunk_with_face_neighbors(&mut session, ChunkPos::new(0, 0, 0));
        session
    }

    /// Stands the player two blocks above `pos`, inside the shared reach budget
    /// and outside the target cell's AABB.
    fn stand_above(session: &mut WorldSession, pos: IVec3) {
        session.player.position =
            Vec3::new(pos.x as f32 + 0.5, pos.y as f32 + 2.0, pos.z as f32 + 0.5);
        session.sync_camera();
    }

    fn wood_hoe() -> Item {
        Item::new(
            ItemId::Tool {
                kind: ToolKind::Hoe,
                tier: ToolTier::Wood,
            },
            1,
        )
        .expect("a wood hoe is a valid tool")
    }

    fn wheat_seeds(count: u16) -> Item {
        Item::new(ItemId::Material(Material::WheatSeeds), count).expect("valid seed stack")
    }

    #[test]
    fn till_at_converts_grass_and_dirt_and_wears_exactly_one_point() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let grass = IVec3::new(5, 10, 5);
        let dirt = IVec3::new(7, 10, 5);
        session
            .world
            .set_block_immediate(grass.x, grass.y, grass.z, BlockId::Grass);
        session
            .world
            .set_block_immediate(dirt.x, dirt.y, dirt.z, BlockId::Dirt);
        session
            .player
            .insert_items(&[wood_hoe()])
            .expect("the empty inventory accepts the hoe");
        assert!(session.player.select_slot(0));
        stand_above(&mut session, grass);

        let remeshed = session.till_at(grass).expect("a held hoe tills live grass");
        assert!(!remeshed.is_empty(), "a resident chunk with faces remeshes");
        assert_eq!(
            session.world.get_block(grass.x, grass.y, grass.z),
            BlockId::Farmland
        );
        let worn = session
            .player
            .inventory()
            .hotbar(0)
            .expect("tilling never consumes the tool");
        assert_eq!(
            worn.durability(),
            Some(ToolTier::Wood.max_durability() - 1),
            "exactly one durability point is spent"
        );
        assert_eq!(worn.count(), 1);

        // A second, fresh Hoe proves Dirt is tilled the same way.
        session
            .player
            .insert_items(&[wood_hoe()])
            .expect("slot 1 accepts the second hoe");
        assert!(session.player.select_slot(1));
        stand_above(&mut session, dirt);
        assert!(session.till_at(dirt).is_some());
        assert_eq!(
            session.world.get_block(dirt.x, dirt.y, dirt.z),
            BlockId::Farmland
        );
        let second = session
            .player
            .inventory()
            .hotbar(1)
            .expect("the second hoe survives");
        assert_eq!(
            second.durability(),
            Some(ToolTier::Wood.max_durability() - 1)
        );
    }

    #[test]
    fn till_at_refuses_without_hoe_wrong_block_out_of_reach_and_unloaded() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let pos = IVec3::new(5, 10, 5);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Grass);
        stand_above(&mut session, pos);

        // No hoe selected.
        session
            .player
            .insert_items(&[wheat_seeds(3)])
            .expect("seeds fit");
        assert!(session.player.select_slot(0));
        let before = session.player.inventory().clone();
        assert!(session.till_at(pos).is_none());
        assert_eq!(session.world.get_block(pos.x, pos.y, pos.z), BlockId::Grass);
        assert_eq!(session.player.inventory(), &before);

        // Hoe selected, wrong live block.
        session
            .player
            .insert_items(&[wood_hoe()])
            .expect("hoe fits");
        assert!(session.player.select_slot(1));
        let stone = IVec3::new(6, 10, 5);
        session
            .world
            .set_block_immediate(stone.x, stone.y, stone.z, BlockId::Stone);
        stand_above(&mut session, stone);
        let before = session.player.inventory().clone();
        assert!(session.till_at(stone).is_none());
        assert_eq!(
            session.world.get_block(stone.x, stone.y, stone.z),
            BlockId::Stone
        );
        assert_eq!(session.player.inventory(), &before);

        // Hoe selected, live grass out of reach.
        session.player.position = Vec3::new(40.5, 12.0, 40.5);
        session.sync_camera();
        let before = session.player.inventory().clone();
        assert!(session.till_at(pos).is_none());
        assert_eq!(session.world.get_block(pos.x, pos.y, pos.z), BlockId::Grass);
        assert_eq!(session.player.inventory(), &before);

        // Unloaded chunk: no edit and no wear.
        let unloaded = IVec3::new(200, 10, 200);
        session.player.position = Vec3::new(200.5, 12.0, 200.5);
        session.sync_camera();
        let before = session.player.inventory().clone();
        assert!(session.till_at(unloaded).is_none());
        assert_eq!(
            session.world.get_block(unloaded.x, unloaded.y, unloaded.z),
            BlockId::Air
        );
        assert_eq!(session.player.inventory(), &before);
    }

    #[test]
    fn till_at_breaks_a_one_durability_hoe_after_the_edit() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let pos = IVec3::new(5, 10, 5);
        session
            .world
            .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Grass);
        stand_above(&mut session, pos);
        session
            .player
            .insert_items(
                &[Item::tool_with_durability(ToolKind::Hoe, ToolTier::Wood, 1)
                    .expect("one durability point is valid")],
            )
            .expect("fits");
        assert!(session.player.select_slot(0));

        assert!(session.till_at(pos).is_some());
        assert_eq!(
            session.world.get_block(pos.x, pos.y, pos.z),
            BlockId::Farmland
        );
        assert_eq!(
            session.player.inventory().hotbar(0),
            None,
            "the last point breaks the hoe only after the edit landed"
        );
    }

    #[test]
    fn plant_wheat_at_places_stage_zero_and_consumes_one_seed() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let farmland = IVec3::new(5, 10, 5);
        let crop = farmland + IVec3::Y;
        session
            .world
            .set_block_immediate(farmland.x, farmland.y, farmland.z, BlockId::Farmland);
        stand_above(&mut session, farmland);
        session
            .player
            .insert_items(&[wheat_seeds(5)])
            .expect("seeds fit");
        assert!(session.player.select_slot(0));

        assert!(session.plant_wheat_at(crop).is_some());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage0
        );
        assert_eq!(
            session
                .player
                .inventory()
                .hotbar(0)
                .map(|item| item.count()),
            Some(4),
            "exactly one seed is consumed"
        );
        assert_eq!(session.crops.progress(crop), Some(0.0));
        assert_eq!(session.crops.len(), 1);

        // The cell is occupied now, so a second planting is refused untouched.
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(crop).is_none());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage0
        );
        assert_eq!(session.player.inventory(), &before);
        assert_eq!(session.crops.len(), 1);
    }

    #[test]
    fn plant_wheat_at_refuses_without_seeds_occupied_non_farmland_overlap_out_of_reach_and_unloaded(
    ) {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let farmland = IVec3::new(5, 10, 5);
        let crop = farmland + IVec3::Y;
        session
            .world
            .set_block_immediate(farmland.x, farmland.y, farmland.z, BlockId::Farmland);
        stand_above(&mut session, farmland);

        // No seeds selected.
        session
            .player
            .insert_items(&[wood_hoe()])
            .expect("hoe fits");
        assert!(session.player.select_slot(0));
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(crop).is_none());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::Air
        );
        assert_eq!(session.crops.len(), 0);
        assert_eq!(session.player.inventory(), &before);

        // Seeds selected, but the target cell is occupied.
        session
            .player
            .insert_items(&[wheat_seeds(4)])
            .expect("seeds fit");
        assert!(session.player.select_slot(1));
        session
            .world
            .set_block_immediate(crop.x, crop.y, crop.z, BlockId::Stone);
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(crop).is_none());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::Stone
        );
        assert_eq!(session.crops.len(), 0);
        assert_eq!(session.player.inventory(), &before);

        // Air target above non-farmland support.
        session
            .world
            .set_block_immediate(crop.x, crop.y, crop.z, BlockId::Air);
        let grass_support = IVec3::new(7, 10, 5);
        session.world.set_block_immediate(
            grass_support.x,
            grass_support.y,
            grass_support.z,
            BlockId::Grass,
        );
        let other_crop = grass_support + IVec3::Y;
        stand_above(&mut session, grass_support);
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(other_crop).is_none());
        assert_eq!(
            session
                .world
                .get_block(other_crop.x, other_crop.y, other_crop.z),
            BlockId::Air
        );
        assert_eq!(session.crops.len(), 0);
        assert_eq!(session.player.inventory(), &before);

        // Player AABB overlaps the target cell.
        session.player.position =
            Vec3::new(crop.x as f32 + 0.5, crop.y as f32, crop.z as f32 + 0.5);
        session.sync_camera();
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(crop).is_none());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::Air
        );
        assert_eq!(session.crops.len(), 0);
        assert_eq!(session.player.inventory(), &before);

        // Seeds selected, target out of reach.
        session.player.position = Vec3::new(40.5, 12.0, 40.5);
        session.sync_camera();
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(crop).is_none());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::Air
        );
        assert_eq!(session.crops.len(), 0);
        assert_eq!(session.player.inventory(), &before);

        // Unloaded chunk.
        let unloaded_crop = IVec3::new(200, 11, 200);
        session.player.position = Vec3::new(200.5, 13.0, 200.5);
        session.sync_camera();
        let before = session.player.inventory().clone();
        assert!(session.plant_wheat_at(unloaded_crop).is_none());
        assert_eq!(
            session
                .world
                .get_block(unloaded_crop.x, unloaded_crop.y, unloaded_crop.z),
            BlockId::Air
        );
        assert_eq!(session.crops.len(), 0);
        assert_eq!(session.player.inventory(), &before);
    }

    #[test]
    fn tick_crops_advances_all_four_stages_and_never_past_mature() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        assert_eq!(session.season(), Season::Spring);
        let farmland = IVec3::new(5, 10, 5);
        let crop = farmland + IVec3::Y;
        session
            .world
            .set_block_immediate(farmland.x, farmland.y, farmland.z, BlockId::Farmland);
        stand_above(&mut session, farmland);
        session
            .player
            .insert_items(&[wheat_seeds(3)])
            .expect("seeds fit");
        assert!(session.player.select_slot(0));
        assert!(session.plant_wheat_at(crop).is_some());

        for (stage, expected) in [
            (1, BlockId::WheatStage1),
            (2, BlockId::WheatStage2),
            (3, BlockId::WheatStage3),
        ] {
            let remeshed = session.tick_crops(0.1, 600.0);
            assert!(
                !remeshed.is_empty(),
                "stage {stage} must remesh the crop chunk"
            );
            assert_eq!(session.world.get_block(crop.x, crop.y, crop.z), expected);
        }
        assert_eq!(session.crops.len(), 0, "maturity removes the candidate");

        // A fourth tick must not advance or rewrite the mature block.
        assert!(session.tick_crops(0.1, 600.0).is_empty());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage3
        );
    }

    #[test]
    fn tick_crops_pauses_in_winter_and_resumes_in_spring() {
        use crate::world::daycycle::{GameClock, DEFAULT_TIME_OF_DAY};

        let mut session = farm_session(1, 1);
        assert_eq!(session.season(), Season::Spring);
        let farmland = IVec3::new(5, 10, 5);
        let crop = farmland + IVec3::Y;
        session
            .world
            .set_block_immediate(farmland.x, farmland.y, farmland.z, BlockId::Farmland);
        stand_above(&mut session, farmland);
        session
            .player
            .insert_items(&[wheat_seeds(2)])
            .expect("seeds fit");
        assert!(session.player.select_slot(0));
        assert!(session.plant_wheat_at(crop).is_some());

        // Bank 30 game seconds of Spring progress without a stage change.
        assert!(session.tick_crops(0.05, 600.0).is_empty());
        assert_eq!(session.crops.progress(crop), Some(30.0));

        session.clock = GameClock::from_day_and_time(3, DEFAULT_TIME_OF_DAY);
        assert_eq!(session.season(), Season::Winter);
        assert!(
            session.tick_crops(0.1, 600.0).is_empty(),
            "winter pauses growth"
        );
        assert_eq!(
            session.crops.progress(crop),
            Some(30.0),
            "winter retains banked progress"
        );
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage0
        );

        session.clock = GameClock::from_day_and_time(0, DEFAULT_TIME_OF_DAY);
        assert_eq!(session.season(), Season::Spring);
        assert!(!session.tick_crops(0.1, 600.0).is_empty());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage1
        );
    }

    #[test]
    fn tick_crops_skips_unloaded_chunks_and_resumes_when_resident() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let chunk = ChunkPos::new(2, 0, 2);
        let crop = IVec3::new(2 * 32 + 5, 11, 2 * 32 + 5);
        assert!(!session.world.is_resident(chunk));

        // A candidate without resident block data is retained but paused.
        session.crops.insert(crop);
        assert!(session.tick_crops(0.1, 600.0).is_empty());
        assert_eq!(session.crops.progress(crop), Some(0.0));
        assert_eq!(session.crops.len(), 1);

        // Load the chunk with live wheat on farmland: growth resumes.
        insert_air_chunk_with_face_neighbors(&mut session, chunk);
        session
            .world
            .set_block_immediate(crop.x, crop.y - 1, crop.z, BlockId::Farmland);
        session
            .world
            .set_block_immediate(crop.x, crop.y, crop.z, BlockId::WheatStage0);
        let remeshed = session.tick_crops(0.1, 600.0);
        assert!(!remeshed.is_empty());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage1
        );
    }

    #[test]
    fn crop_stage_survives_save_reload_and_candidate_rebuilds_at_zero_progress() {
        use crate::world::chunk::linear_index;
        use crate::world::persistence::{
            load_world_from_path, save_world_to_path, SavedPlayerState,
        };

        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);
        let farmland = IVec3::new(5, 10, 5);
        let crop = farmland + IVec3::Y;
        session
            .world
            .set_block_immediate(farmland.x, farmland.y, farmland.z, BlockId::Farmland);
        stand_above(&mut session, farmland);
        session
            .player
            .insert_items(&[wheat_seeds(3)])
            .expect("seeds fit");
        assert!(session.player.select_slot(0));
        assert!(session.plant_wheat_at(crop).is_some());
        assert!(!session.tick_crops(0.1, 600.0).is_empty());
        assert_eq!(
            session.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage1
        );

        let data = WorldSaveData {
            seed: session.world.seed(),
            time_of_day: session.clock.time_of_day,
            day_index: session.clock.day_index,
            season_length_days: session.season_length_days,
            mode: session.save_mode,
            player: SavedPlayerState {
                position: session.player.position.to_array(),
                yaw: session.player.yaw,
                pitch: session.player.pitch,
                fly_mode: session.player.fly_mode,
                selected_slot: session.player.selected_slot() as u8,
                render_distance: session.world.render_distance() as u16,
                inventory: session.player.inventory().clone(),
                player_grid: session.player.player_grid().clone(),
                table_grid: session.player.table_grid().clone(),
                survival: session.player.survival,
            },
            chests: session.chests.clone(),
            mobs: Vec::new(),
            chunk_edits: session.world.chunk_edits().clone(),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("crop.mcvx");
        save_world_to_path(&path, &data).expect("the save writes");
        let loaded = load_world_from_path(&path).expect("the save loads");
        let mut restored = WorldSession::session_from_save(loaded, path.clone());

        let (chunk, (lx, ly, lz)) = world_to_chunk_and_local(crop.x, crop.y, crop.z);
        assert_eq!(
            restored
                .world
                .chunk_edits()
                .get(&chunk)
                .and_then(|edits| edits.get(&(linear_index(lx, ly, lz) as u16))),
            Some(&BlockId::WheatStage1),
            "the visible stage stays an ordinary v6 chunk edit"
        );
        assert_eq!(restored.crops.progress(crop), Some(0.0));
        assert_eq!(restored.crops.len(), 1);

        // Materialize the saved chunk and reapply its persisted stage, then one
        // tick proves the candidate rebuilt from the edit map grows.
        insert_air_chunk_with_face_neighbors(&mut restored, chunk);
        restored
            .world
            .set_block_immediate(farmland.x, farmland.y, farmland.z, BlockId::Farmland);
        restored
            .world
            .set_block_immediate(crop.x, crop.y, crop.z, BlockId::WheatStage1);
        let remeshed = restored.tick_crops(0.1, 600.0);
        assert!(!remeshed.is_empty());
        assert_eq!(
            restored.world.get_block(crop.x, crop.y, crop.z),
            BlockId::WheatStage2
        );
    }

    #[test]
    fn session_constructors_start_with_no_crop_candidates() {
        use crate::world::chunk::linear_index;
        use crate::world::daycycle::{GameClock, DEFAULT_TIME_OF_DAY};

        let fresh = WorldSession::new_session_with_mode(
            1,
            PathBuf::from("/tmp/fresh-crops.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        assert!(fresh.crops.is_empty());

        let edited = WorldSession::new_at(
            1,
            PathBuf::from("/tmp/edit-crops.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(0.5, 20.0, 0.5),
            GameClock::from_day_and_time(0, DEFAULT_TIME_OF_DAY),
        );
        assert!(edited.crops.is_empty());

        let mut edits = ChunkEditMap::default();
        let chunk = ChunkPos::new(0, 0, 0);
        let inner = edits.entry(chunk).or_default();
        for (index, block) in [
            (linear_index(1, 2, 3), BlockId::WheatStage0),
            (linear_index(2, 2, 3), BlockId::WheatStage1),
            (linear_index(3, 2, 3), BlockId::WheatStage2),
            (linear_index(4, 2, 3), BlockId::WheatStage3),
            (linear_index(5, 2, 3), BlockId::Farmland),
        ] {
            inner.insert(index as u16, block);
        }
        let mut data = make_save(1);
        data.chunk_edits = edits;
        let restored =
            WorldSession::session_from_save(data, PathBuf::from("/tmp/restored-crops.mcvx"));
        assert_eq!(restored.crops.len(), 3);
        assert_eq!(restored.crops.progress(IVec3::new(1, 2, 3)), Some(0.0));
        assert_eq!(restored.crops.progress(IVec3::new(2, 2, 3)), Some(0.0));
        assert_eq!(restored.crops.progress(IVec3::new(3, 2, 3)), Some(0.0));
        assert_eq!(restored.crops.progress(IVec3::new(4, 2, 3)), None);
        assert_eq!(restored.crops.progress(IVec3::new(5, 2, 3)), None);
    }

    #[test]
    fn wheat_harvest_items_maps_every_wheat_stage_and_remove_crop_clears() {
        let mut session = farm_session(1, DEFAULT_SEASON_LENGTH_DAYS);

        let mature = session
            .wheat_harvest_items(BlockId::WheatStage3)
            .expect("mature wheat yields");
        assert_eq!(mature.len(), 2);
        assert_eq!(mature[0].id(), ItemId::Material(Material::Wheat));
        assert_eq!(mature[0].count(), 1);
        assert_eq!(mature[1].id(), ItemId::Material(Material::WheatSeeds));
        assert_eq!(mature[1].count(), 2);

        for stage in [
            BlockId::WheatStage0,
            BlockId::WheatStage1,
            BlockId::WheatStage2,
        ] {
            let items = session
                .wheat_harvest_items(stage)
                .unwrap_or_else(|| panic!("{stage:?} yields"));
            assert_eq!(items.len(), 1, "{stage:?} yields one entry");
            assert_eq!(items[0].id(), ItemId::Material(Material::WheatSeeds));
            assert_eq!(items[0].count(), 1);
        }
        assert_eq!(session.wheat_harvest_items(BlockId::Air), None);
        assert_eq!(session.wheat_harvest_items(BlockId::Farmland), None);

        let pos = IVec3::new(5, 11, 5);
        session.crops.insert(pos);
        assert_eq!(session.crops.progress(pos), Some(0.0));
        session.remove_crop(pos);
        assert_eq!(session.crops.progress(pos), None);
    }

    fn make_mob_session(seed: u32) -> WorldSession {
        WorldSession::new_session(
            seed,
            PathBuf::from("/tmp/session-mobs.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        )
    }

    fn add_resident_air(session: &mut WorldSession) {
        session.world.insert_chunk_for_test(
            ChunkPos::new(0, 0, 0),
            crate::world::chunk::Chunk::new_uniform(BlockId::Air),
        );
    }

    #[test]
    fn tick_mobs_moves_resident_and_freezes_unloaded_but_keeps_the_record() {
        let mut session = make_mob_session(31);
        add_resident_air(&mut session);
        let resident_home = Vec3::new(8.5, 5.0, 8.5);
        let unloaded_home = Vec3::new(100.5, 5.0, 100.5);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, resident_home, 0.0));
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Cow, unloaded_home, 0.0));

        for _ in 0..10 {
            assert_eq!(session.tick_mobs(0.05, 1.0).ai_steps, 1);
        }

        assert_eq!(session.mobs().len(), 2, "records are never dropped");
        assert_ne!(session.mobs()[0].position, resident_home);
        assert_eq!(session.mobs()[1].position, unloaded_home);
        assert_eq!(session.mobs()[1].walk_phase, 0.0);
    }

    #[test]
    fn tick_mobs_is_frame_rate_independent_through_the_session() {
        let mut a = make_mob_session(32);
        let mut b = make_mob_session(32);
        add_resident_air(&mut a);
        add_resident_air(&mut b);
        let spawn = Vec3::new(8.5, 5.0, 8.5);
        a.mobs_mut().push(Mob::new(MobKind::Zombie, spawn, 0.0));
        b.mobs_mut().push(Mob::new(MobKind::Zombie, spawn, 0.0));

        let mut a_steps = 0;
        for _ in 0..10 {
            a_steps += a.tick_mobs(0.05, 1.0).ai_steps;
        }
        let mut b_steps = 0;
        for _ in 0..50 {
            b_steps += b.tick_mobs(0.01, 1.0).ai_steps;
        }

        assert_eq!(a_steps, 10);
        assert_eq!(b_steps, 10);
        assert!((a.mobs()[0].position - b.mobs()[0].position).length() < 1e-3);
        assert!((a.mobs()[0].walk_phase - b.mobs()[0].walk_phase).abs() < 1e-3);
    }

    #[test]
    fn tick_mobs_clamps_dt_and_freezes_at_zero_time_scale() {
        let mut session = make_mob_session(33);
        add_resident_air(&mut session);
        let spawn = Vec3::new(8.5, 5.0, 8.5);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Skeleton, spawn, 0.0));

        // Raw dt 5.0 clamps to 0.1 s => exactly two fixed steps.
        assert_eq!(session.tick_mobs(5.0, 1.0).ai_steps, 2);
        let after_clamp = session.mobs()[0].position;

        assert_eq!(session.tick_mobs(0.05, 0.0).ai_steps, 0);
        assert_eq!(session.tick_mobs(0.05, -2.0).ai_steps, 0);
        assert_eq!(session.mobs()[0].position, after_clamp);

        // A resident mob still advances when time is allowed to flow.
        assert_eq!(session.tick_mobs(0.05, 1.0).ai_steps, 1);
        assert_ne!(session.mobs()[0].position, after_clamp);
    }

    /// A Survival session with one hostile two blocks in front of the player
    /// on an all-air resident chunk. Ambient spawning is closed so the mob
    /// list (and therefore every attack count) is exactly what the test put
    /// there.
    fn survival_hostile_session(seed: u32, kind: MobKind) -> WorldSession {
        let mut session = WorldSession::new_session_with_mode(
            seed,
            PathBuf::from("/tmp/session-hostile.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        add_resident_air(&mut session);
        session.set_ambient_spawning(false);
        session.player.position = Vec3::new(8.5, 5.0, 8.5);
        session.sync_camera();
        session
            .mobs_mut()
            .push(Mob::new(kind, Vec3::new(8.5, 5.0, 10.5), 0.0));
        session
    }

    #[test]
    fn survival_zombie_attacks_through_the_session_once_per_cooldown() {
        let mut session = survival_hostile_session(41, MobKind::Zombie);

        let first = session.tick_mobs(0.05, 1.0);
        assert_eq!(first.ai_steps, 1);
        assert_eq!(first.attacks.len(), 1, "one fixed step, one zombie swing");
        assert_eq!(first.attacks[0].kind, MobKind::Zombie);
        let cooldown = session.mobs()[0].cooldown;
        assert!(
            (cooldown - 1.0).abs() < 1e-5,
            "the attack must arm the 1.0 s cadence, got {cooldown}"
        );

        // Inside the cooldown window: no second swing even though the zombie
        // keeps chasing.
        let inside = session.tick_mobs(0.05, 1.0);
        assert!(inside.attacks.is_empty(), "cooldown must gate the swing");
        assert!(session.mobs()[0].cooldown < cooldown);

        // Over the rest of a 1.5 s window the cadence fires again.
        let mut attacks = 1u32;
        for _ in 0..28 {
            attacks += session.tick_mobs(0.05, 1.0).attacks.len() as u32;
        }
        assert!(
            attacks >= 2,
            "the 1.0 s cadence must fire again, got {attacks} attacks"
        );
    }

    #[test]
    fn creative_builds_no_target_so_no_hostile_ever_chases_or_attacks() {
        let mut session = WorldSession::new_session(
            42,
            PathBuf::from("/tmp/session-hostile-creative.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        add_resident_air(&mut session);
        session.set_ambient_spawning(false);
        session.player.position = Vec3::new(8.5, 5.0, 8.5);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Zombie, Vec3::new(8.5, 5.0, 10.5), 0.0));
        let start = session.mobs()[0].position;

        for _ in 0..30 {
            let tick = session.tick_mobs(0.05, 1.0);
            assert!(tick.attacks.is_empty(), "Creative must never be attacked");
            assert_ne!(
                session.mobs()[0].state,
                MobAiState::Chase,
                "Creative must never be targeted"
            );
        }
        // A hostile with no target keeps its pre-S06 wander (the pinned demo
        // quartet must keep walking while the demo gate is closed), but never
        // turns that motion into a chase or an attack.
        assert_ne!(session.mobs()[0].position, start);
    }

    #[test]
    fn closed_hostile_gate_disables_targeting_in_survival() {
        let mut session = survival_hostile_session(43, MobKind::Zombie);
        session.set_hostile_ai_enabled(false);
        assert!(!session.hostile_ai_enabled());
        let start = session.mobs()[0].position;

        for _ in 0..30 {
            let tick = session.tick_mobs(0.05, 1.0);
            assert!(tick.attacks.is_empty(), "a closed gate emits no attacks");
            assert_ne!(session.mobs()[0].state, MobAiState::Chase);
        }
        // The pinned `--demo-combat` quartet must keep walking with the gate
        // closed: the gate stops targeting, not the pre-S06 wander.
        assert_ne!(session.mobs()[0].position, start);
        assert_eq!(
            session.mobs()[0].cooldown,
            0.0,
            "no swing means the cadence is never armed"
        );
    }

    #[test]
    fn dead_player_builds_no_target() {
        let mut session = survival_hostile_session(44, MobKind::Zombie);
        session.player.survival.damage(20);
        assert!(session.is_dead());

        for _ in 0..30 {
            assert!(
                session.tick_mobs(0.05, 1.0).attacks.is_empty(),
                "a dead player must never be attacked again"
            );
        }
        assert_ne!(session.mobs()[0].state, MobAiState::Chase);
    }

    #[test]
    fn skeleton_chases_but_never_melees_through_the_session() {
        let mut session = survival_hostile_session(45, MobKind::Skeleton);
        let mut attacked = false;
        let mut chased = false;
        for _ in 0..30 {
            let tick = session.tick_mobs(0.05, 1.0);
            attacked |= !tick.attacks.is_empty();
            chased |= session.mobs()[0].state == MobAiState::Chase;
        }
        assert!(
            !attacked,
            "Skeleton melee must never exist (S08 owns ranged)"
        );
        assert!(chased, "the shared steering still lets it chase");
        assert_eq!(session.mobs()[0].cooldown, 0.0);
    }

    #[test]
    fn hostile_attacks_are_frame_rate_independent_through_the_session() {
        let mut a = survival_hostile_session(46, MobKind::Zombie);
        let mut b = survival_hostile_session(46, MobKind::Zombie);

        let mut a_attacks = 0u32;
        for _ in 0..10 {
            a_attacks += a.tick_mobs(0.05, 1.0).attacks.len() as u32;
        }
        let mut b_attacks = 0u32;
        for _ in 0..50 {
            b_attacks += b.tick_mobs(0.01, 1.0).attacks.len() as u32;
        }

        assert_eq!(a_attacks, b_attacks, "attack counts must match exactly");
        assert!(a_attacks >= 1);
        assert_eq!(a.mobs()[0].kind, b.mobs()[0].kind);
        assert!((a.mobs()[0].position - b.mobs()[0].position).length() < 1e-3);
        assert!((a.mobs()[0].yaw - b.mobs()[0].yaw).abs() < 1e-3);
        assert!((a.mobs()[0].cooldown - b.mobs()[0].cooldown).abs() < 1e-5);
    }

    // -----------------------------------------------------------------------
    // S06-T04: session damage authority, causes, and the hurt flash
    // -----------------------------------------------------------------------

    fn zombie_attack(from: Vec3) -> MobAttack {
        MobAttack {
            kind: MobKind::Zombie,
            from,
        }
    }

    #[test]
    fn zombie_melee_applies_three_and_records_the_zombie_cause() {
        let mut session = survival_hostile_session(51, MobKind::Zombie);
        let feet = session.player.position;
        let health = session.player.survival.health();

        let applied = session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]);

        assert_eq!(applied, 3, "a zombie hit is exactly 3 damage");
        assert_eq!(session.player.survival.health(), health - 3);
        assert_eq!(session.last_damage_cause(), Some(DamageCause::Zombie));
        assert!(session.player_hurt_intensity() > 0.0, "the flash is armed");
    }

    #[test]
    fn a_second_slice_inside_the_iframe_window_applies_nothing() {
        let mut session = survival_hostile_session(52, MobKind::Zombie);
        let feet = session.player.position;
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            3
        );
        let health = session.player.survival.health();

        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            0,
            "the 0.5 s i-frame window refuses the second slice"
        );
        assert_eq!(session.player.survival.health(), health);
        assert_eq!(
            session.last_damage_cause(),
            Some(DamageCause::Zombie),
            "a refused hit never rewrites the cause"
        );
    }

    #[test]
    fn two_adjacent_zombies_land_one_hit_per_iframe_window() {
        let mut session = survival_hostile_session(53, MobKind::Zombie);
        let feet = session.player.position;
        let health = session.player.survival.health();
        let attacks = [zombie_attack(feet + Vec3::Z), zombie_attack(feet - Vec3::Z)];

        assert_eq!(
            session.apply_mob_attacks(&attacks),
            3,
            "two attacks in one slice apply one hit"
        );
        assert_eq!(session.player.survival.health(), health - 3);
    }

    #[test]
    fn creative_melee_applies_nothing_and_records_no_cause() {
        let mut session = WorldSession::new_session(
            54,
            PathBuf::from("/tmp/session-hostile-creative-damage.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        let feet = session.player.position;
        let health = session.player.survival.health();

        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            0
        );
        assert_eq!(session.player.survival.health(), health);
        assert_eq!(session.last_damage_cause(), None);
        assert_eq!(session.player_hurt_intensity(), 0.0);
    }

    #[test]
    fn a_dead_player_takes_no_further_melee_damage() {
        let mut session = survival_hostile_session(55, MobKind::Zombie);
        session.player.survival.damage(20);
        assert!(session.is_dead());
        let feet = session.player.position;

        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            0
        );
        assert_eq!(session.player.survival.health(), 0);
        assert_eq!(
            session.last_damage_cause(),
            None,
            "a refused hit on a corpse records no cause"
        );
    }

    #[test]
    fn skeleton_attacks_apply_no_melee_damage() {
        let mut session = survival_hostile_session(56, MobKind::Zombie);
        let feet = session.player.position;
        let health = session.player.survival.health();
        let attack = MobAttack {
            kind: MobKind::Skeleton,
            from: feet + Vec3::Z,
        };

        assert_eq!(
            session.apply_mob_attacks(&[attack]),
            0,
            "Skeleton melee must never exist (S08 owns ranged)"
        );
        assert_eq!(session.player.survival.health(), health);
        assert_eq!(session.last_damage_cause(), None);
    }

    #[test]
    fn tick_mobs_applies_zombie_melee_and_records_the_cause() {
        let mut session = survival_hostile_session(57, MobKind::Zombie);
        let health = session.player.survival.health();

        for _ in 0..30 {
            session.tick_mobs(0.05, 1.0);
        }

        assert!(
            session.player.survival.health() < health,
            "an adjacent zombie must actually damage the player"
        );
        assert_eq!(session.last_damage_cause(), Some(DamageCause::Zombie));
    }

    #[test]
    fn tick_mobs_at_zero_time_scale_can_neither_age_nor_damage() {
        let mut session = survival_hostile_session(58, MobKind::Zombie);
        let feet = session.player.position;
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            3
        );
        let health = session.player.survival.health();
        let iframe = session.player.combat.iframe();

        for _ in 0..10 {
            assert_eq!(session.tick_mobs(0.05, 0.0).ai_steps, 0);
        }

        assert_eq!(session.player.survival.health(), health);
        assert_eq!(session.player.combat.iframe(), iframe);
    }

    #[test]
    fn a_restored_session_has_no_damage_cause_even_when_saved_dead() {
        let mut data = make_save(59);
        data.mode = GameMode::Survival;
        data.player.survival = SurvivalState::new(0, 10, FULL_AIR).expect("valid dead vitals");

        let session =
            WorldSession::session_from_save(data, PathBuf::from("/tmp/session-restored-dead.mcvx"));

        assert!(session.is_dead());
        assert_eq!(
            session.last_damage_cause(),
            None,
            "the cause is ephemeral: a loaded dead save has none"
        );
        assert_eq!(session.player_hurt_intensity(), 0.0);
    }

    #[test]
    fn respawn_clears_the_cause_and_every_player_combat_timer() {
        let mut session = survival_hostile_session(60, MobKind::Zombie);
        let feet = session.player.position;
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            3
        );
        assert!(session.player.combat.iframe() > 0.0);
        assert!(session.player.combat.knockback().length_squared() > 0.0);

        session.respawn();

        assert_eq!(session.last_damage_cause(), None);
        assert_eq!(session.player_hurt_intensity(), 0.0);
        assert_eq!(session.player.combat.iframe(), 0.0);
        assert_eq!(session.player.combat.knockback(), Vec3::ZERO);
        assert_eq!(session.player.survival, SurvivalState::full());
        assert_eq!(
            session.player.position,
            WorldSession::spawn_position(session.world.seed())
        );
    }

    #[test]
    fn player_hurt_intensity_rises_on_a_hit_and_decays_over_fixed_steps() {
        let mut session = survival_hostile_session(61, MobKind::Zombie);
        let feet = session.player.position;
        assert_eq!(session.player_hurt_intensity(), 0.0);

        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            3
        );
        assert_eq!(
            session.player_hurt_intensity(),
            1.0,
            "a fresh hit is a full-intensity flash"
        );

        // Twelve executed fixed steps (> the 0.5 s window) must fully decay it;
        // the zombie's own 1.0 s cadence keeps it from landing a second hit.
        let mut steps = 0u32;
        while steps < 12 {
            steps += session.tick_mobs(0.05, 1.0).ai_steps;
        }
        assert_eq!(session.player_hurt_intensity(), 0.0);
    }

    #[test]
    fn tick_survival_records_the_survival_cause_and_keeps_it_across_idle_ticks() {
        let mut session = survival_session(62);
        session.player.survival =
            SurvivalState::new(crate::player::survival::MAX_HEALTH, 0, FULL_AIR)
                .expect("valid vitals");
        assert_eq!(session.last_damage_cause(), None);

        let mut damaged = false;
        for _ in 0..4 {
            damaged |= session.tick_survival(0.1, 10.0, false).damage > 0;
        }

        assert!(damaged, "zero hunger must starve after the 4 s interval");
        assert_eq!(
            session.last_damage_cause(),
            Some(DamageCause::Starvation),
            "the survival rules are the starvation cause writer"
        );

        assert_eq!(
            session.tick_survival(0.0, 1.0, false).damage,
            0,
            "an idle tick damages nothing"
        );
        assert_eq!(
            session.last_damage_cause(),
            Some(DamageCause::Starvation),
            "an idle tick keeps the last real cause"
        );
    }

    #[test]
    fn respawn_drops_a_live_chase_without_moving_the_body() {
        let mut session = survival_hostile_session(47, MobKind::Zombie);
        // Put the record in a live chase with an armed aggro memory.
        {
            let zombie = &mut session.mobs_mut()[0];
            zombie.state = MobAiState::Chase;
            zombie.aggro = 1.5;
            zombie.stuck = 0.4;
            zombie.reaggro = 0.25;
        }
        let position = session.mobs()[0].position;
        let health = session.mobs()[0].health;

        session.respawn();

        let zombie = &session.mobs()[0];
        assert_eq!(zombie.state, MobAiState::Idle);
        assert_eq!(zombie.aggro, 0.0);
        assert_eq!(zombie.stuck, 0.0);
        assert_eq!(zombie.reaggro, 0.0);
        assert_eq!(zombie.position, position, "the body must not be moved");
        assert_eq!(zombie.health, health);
        assert_eq!(
            session.player.position,
            WorldSession::spawn_position(session.world.seed()),
            "respawn still returns to the deterministic spawn"
        );
    }

    #[test]
    fn session_from_save_restores_the_hostile_gate_enabled() {
        let session = WorldSession::session_from_save(
            make_save(48),
            PathBuf::from("/tmp/session-hostile-restored.mcvx"),
        );
        assert!(session.hostile_ai_enabled());
    }

    #[test]
    fn resident_mobs_filters_by_feet_chunk() {
        let mut session = make_mob_session(34);
        add_resident_air(&mut session);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 8.5), 0.0));
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Skeleton, Vec3::new(-8.5, 5.0, -8.5), 0.0));
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Cow, Vec3::new(33.5, 5.0, 8.5), 0.0));

        assert!(session.mob_is_resident(&session.mobs()[0]));
        assert!(!session.mob_is_resident(&session.mobs()[1]));
        assert!(!session.mob_is_resident(&session.mobs()[2]));

        let kinds: Vec<MobKind> = session.resident_mobs().map(|mob| mob.kind).collect();
        assert_eq!(kinds, vec![MobKind::Pig]);
    }

    #[test]
    fn hurt_decays_only_on_resident_mobs_through_the_session() {
        let mut session = make_mob_session(35);
        add_resident_air(&mut session);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 8.5), 0.0));
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Zombie, Vec3::new(100.5, 5.0, 100.5), 0.0));
        session.mobs_mut()[0].hurt = 0.2;
        session.mobs_mut()[1].hurt = 0.2;

        for _ in 0..6 {
            session.tick_mobs(0.05, 1.0);
        }

        assert!(session.mobs()[0].hurt <= 1e-5);
        assert!((session.mobs()[1].hurt - 0.2).abs() < 1e-6);
    }

    #[test]
    fn mob_positions_stay_finite_over_100_session_steps() {
        let mut session = make_mob_session(36);
        add_resident_air(&mut session);
        for (i, kind) in MobKind::ALL.iter().copied().enumerate() {
            session
                .mobs_mut()
                .push(Mob::new(kind, Vec3::new(8.5 + i as f32, 5.0, 8.5), 0.0));
        }
        for _ in 0..100 {
            session.tick_mobs(0.05, 1.0);
        }
        for mob in session.mobs() {
            assert!(mob.position.is_finite());
            assert!(mob.velocity.is_finite());
            assert!(mob.walk_phase.is_finite());
        }
    }

    #[test]
    fn tick_mobs_reaps_a_finished_corpse_and_keeps_a_mid_fade_one() {
        let mut session = make_mob_session(41);
        add_resident_air(&mut session);

        // One step of collapse left: this one is reaped by the next tick.
        let mut finished = Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 8.5), 0.0);
        assert!(finished.kill());
        finished.death = MOB_TICK_SECS;
        // A full collapse duration: still fading, so it must survive.
        let mut fading = Mob::new(MobKind::Cow, Vec3::new(9.5, 5.0, 8.5), 0.0);
        assert!(fading.kill());
        assert_eq!(fading.death, MOB_DEATH_SECS);
        session.mobs_mut().push(finished);
        session.mobs_mut().push(fading);

        session.tick_mobs(0.05, 1.0);

        assert_eq!(
            session.mobs().len(),
            1,
            "only the finished corpse is reaped"
        );
        assert_eq!(session.mobs()[0].kind, MobKind::Cow);
        assert!(session.mobs()[0].death > 0.0, "the mid-fade corpse stays");
        assert!(!session.mobs()[0].death_finished());
    }

    #[test]
    fn corpse_in_an_unloaded_chunk_keeps_its_record_and_death_timer() {
        let mut session = make_mob_session(42);
        add_resident_air(&mut session);
        let mut corpse = Mob::new(MobKind::Pig, Vec3::new(100.5, 5.0, 100.5), 0.0);
        assert!(corpse.kill());
        corpse.death = MOB_TICK_SECS;
        session.mobs_mut().push(corpse);

        session.tick_mobs(0.05, 1.0);

        assert_eq!(session.mobs().len(), 1, "an unloaded corpse is not reaped");
        assert_eq!(
            session.mobs()[0].death,
            MOB_TICK_SECS,
            "the collapse timer only decays while resident"
        );
    }

    #[test]
    fn melee_candidate_skips_a_corpse_and_finds_the_live_mob_behind() {
        let mut session = make_mob_session(43);
        add_resident_air(&mut session);
        let origin = Vec3::new(8.5, 5.2, 8.5);
        let dir = Vec3::Z;

        let mut corpse = Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 10.5), 0.0);
        assert!(corpse.kill());
        assert!(!corpse.death_finished());
        session.mobs_mut().push(corpse);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 12.5), 0.0));

        let hit = session
            .melee_candidate(origin, dir, 6.0)
            .expect("the live mob behind the corpse is selectable");
        assert_eq!(hit.index, 1, "the health-0 corpse must be skipped");
        assert!(
            (hit.distance - 3.65).abs() < 1e-4,
            "entry distance was {}",
            hit.distance
        );
    }

    #[test]
    fn attack_mob_on_a_corpse_returns_none_and_leaves_the_record_untouched() {
        let mut session = make_mob_session(44);
        add_resident_air(&mut session);
        let mut corpse = Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 8.5), 0.0);
        assert!(corpse.kill());
        corpse.death = MOB_DEATH_SECS * 0.5;
        corpse.hurt_iframe = 0.0;
        session.mobs_mut().push(corpse);
        let snapshot = session.mobs()[0].clone();

        assert_eq!(session.attack_mob(0, 4, Vec3::new(8.5, 5.0, 6.5)), None);
        assert_eq!(
            session.mobs()[0],
            snapshot,
            "a corpse hit must mutate nothing"
        );
        assert_eq!(session.mobs().len(), 1, "melee never removes a record");
    }

    #[test]
    fn retain_live_mobs_is_idempotent_and_returns_the_exact_count() {
        let mut session = make_mob_session(45);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, Vec3::new(8.5, 5.0, 8.5), 0.0));

        let mut fading = Mob::new(MobKind::Cow, Vec3::new(9.5, 5.0, 8.5), 0.0);
        assert!(fading.kill());
        fading.death = MOB_DEATH_SECS * 0.5;
        session.mobs_mut().push(fading);

        // A restored dead record arrives with no collapse timer: finished.
        session.mobs_mut().push(Mob::restored(
            MobKind::Skeleton,
            Vec3::new(10.5, 5.0, 8.5),
            0.0,
            0,
        ));

        assert_eq!(session.retain_live_mobs(), 1);
        assert_eq!(session.mobs().len(), 2);
        assert_eq!(session.retain_live_mobs(), 0, "a second pass is a no-op");
        assert_eq!(session.mobs().len(), 2);
    }

    // -----------------------------------------------------------------------
    // Population layer wiring: cached world spawn, single tick/reap sites, gate
    // -----------------------------------------------------------------------

    /// A clock tick inside the inclusive sun-zero night window.
    const NIGHT_TICK: u32 = 18_000;
    /// The default morning tick: daylight.
    const DAY_TICK: u32 = crate::world::daycycle::DEFAULT_TIME_OF_DAY;

    fn horizontal_distance(a: Vec3, b: Vec3) -> f32 {
        Vec3::new(a.x - b.x, 0.0, a.z - b.z).length()
    }

    /// Builds a resident, fully lit flat grass plain and puts the player at the
    /// centre of one of its chunks.
    ///
    /// Columns are built through the production `WorldManager` seam and lit
    /// through the real CPU light pass (`recompute_light_for_test`), because
    /// `SpawnWorld::column_ready` demands every Y layer *and* a light grid — a
    /// missing grid would fall back to full sky light and spawn passives out of
    /// thin air. `chunk_radius` 3 fully covers the ±48 block ring plus the
    /// 4-block wander leash; 2 is enough when the test only asserts spawn
    /// positions.
    fn lit_grass_plain(session: &mut WorldSession, chunk_radius: i32) {
        use crate::world::chunk::{Chunk, CHUNK_SIZE};
        use crate::world::manager::{MAX_CHUNK_Y, MIN_CHUNK_Y};

        const SURFACE_Y: i32 = 64;
        const SURFACE_CHUNK_Y: i32 = SURFACE_Y / CHUNK_SIZE as i32;

        let world_spawn = session.world_spawn();
        let centre_x = (world_spawn.x.floor() as i32).div_euclid(CHUNK_SIZE as i32);
        let centre_z = (world_spawn.z.floor() as i32).div_euclid(CHUNK_SIZE as i32);
        session.player.position = Vec3::new(
            centre_x as f32 * CHUNK_SIZE as f32 + CHUNK_SIZE as f32 * 0.5,
            (SURFACE_Y + 1) as f32,
            centre_z as f32 * CHUNK_SIZE as f32 + CHUNK_SIZE as f32 * 0.5,
        );
        session.sync_camera();

        for cx in centre_x - chunk_radius..=centre_x + chunk_radius {
            for cz in centre_z - chunk_radius..=centre_z + chunk_radius {
                for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                    let chunk = if cy < SURFACE_CHUNK_Y {
                        Chunk::new_uniform(BlockId::Stone)
                    } else if cy == SURFACE_CHUNK_Y {
                        // A grass floor at world y = 64 with open sky above, so
                        // the y scan resolves feet at 65 and sky light is 15.
                        let mut chunk = Chunk::new_uniform(BlockId::Air);
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                chunk.set(lx, 0, lz, BlockId::Grass);
                            }
                        }
                        chunk
                    } else {
                        Chunk::new_uniform(BlockId::Air)
                    };
                    session
                        .world
                        .insert_chunk_for_test(ChunkPos::new(cx, cy, cz), chunk);
                }
            }
        }
        session.world.recompute_light_for_test();
    }

    fn lit_grass_plain_session(seed: u32, chunk_radius: i32) -> WorldSession {
        let mut session = make_mob_session(seed);
        lit_grass_plain(&mut session, chunk_radius);
        session
    }

    #[test]
    fn a_fresh_session_starts_with_the_ambient_gate_open_and_a_zero_report() {
        let session = make_mob_session(51);

        assert!(session.ambient_spawning());
        assert_eq!(session.spawn_report(), SpawnReport::default());
        assert_eq!(
            session.world_spawn(),
            WorldSession::spawn_position(session.world.seed())
        );
        assert!(session.world_spawn().is_finite());
    }

    #[test]
    fn the_ambient_gate_stops_spawning_and_reopens_on_a_fresh_window() {
        let mut session = lit_grass_plain_session(52, 2);
        session.clock = GameClock::from_time(NIGHT_TICK);
        session.set_ambient_spawning(false);
        assert!(!session.ambient_spawning());

        // 30 s of night over a ready, lit plain: the closed gate must create
        // nothing and must leave the report at its zero baseline.
        for _ in 0..300 {
            session.tick_mobs(0.1, 1.0);
        }
        assert!(session.mobs().is_empty(), "the closed gate created mobs");
        assert_eq!(session.spawn_report(), SpawnReport::default());

        session.set_ambient_spawning(true);
        assert!(session.ambient_spawning());
        // Re-enabling cleared the partial interval, so the first attempt still
        // waits a full `SPAWN_ATTEMPT_SECS`: 0.2 s of game time fires nothing.
        session.tick_mobs(0.1, 1.0);
        session.tick_mobs(0.1, 1.0);
        assert_eq!(session.spawn_report(), SpawnReport::default());

        // And the gate really reopens: the same plain now populates.
        for _ in 0..300 {
            session.tick_mobs(0.1, 1.0);
        }
        assert!(
            session.spawn_report().spawned > 0,
            "a re-enabled gate spawned nothing: {:?}",
            session.spawn_report()
        );
    }

    #[test]
    fn apply_rules_drives_the_spawn_cap_and_counts_blocks() {
        let mut session = lit_grass_plain_session(55, 2);
        session.clock = GameClock::from_time(NIGHT_TICK);
        let rules = RulesBundle {
            spawning: SpawnRules {
                max_mobs_total: 1,
                ..SpawnRules::default()
            },
            ..RulesBundle::default()
        };
        session.apply_rules(&rules);
        assert_eq!(session.rules().spawning.max_mobs_total, 1);

        // 30 s of night over a ready, lit plain: the tuned cap holds the list
        // at one record and every later attempt is counted as blocked.
        for _ in 0..300 {
            session.tick_mobs(0.1, 1.0);
        }

        assert_eq!(
            session.mobs().len(),
            1,
            "the tuned global cap must hold at one record: {:?}",
            session.spawn_report()
        );
        assert!(
            session.spawn_report().cap_blocked > 0,
            "attempts past the tuned cap must be counted: {:?}",
            session.spawn_report()
        );

        // A live re-apply of the same bundle is not a runtime reset.
        let report = session.spawn_report();
        let same = session.rules();
        session.apply_rules(&same);
        assert_eq!(session.spawn_report(), report);
        assert_eq!(session.mobs().len(), 1);
    }

    #[test]
    fn tick_mobs_spawns_night_hostiles_outside_both_safe_radii() {
        let mut session = lit_grass_plain_session(53, 2);
        session.clock = GameClock::from_time(NIGHT_TICK);
        let player = session.player.position;
        let world_spawn = session.world_spawn();

        // A paused or zero-scaled frame runs on `game_dt == 0` and must run
        // neither the AI nor a spawn attempt.
        for _ in 0..10 {
            session.tick_mobs(0.1, 0.0);
            session.tick_mobs(-1.0, 1.0);
            session.tick_mobs(f32::NAN, 1.0);
        }
        assert_eq!(session.spawn_report(), SpawnReport::default());
        assert!(session.mobs().is_empty());

        for _ in 0..300 {
            session.tick_mobs(0.1, 1.0);
        }

        let report = session.spawn_report();
        assert!(
            report.spawned > 0,
            "the lit night plain produced no hostile: {report:?}"
        );
        assert!(!session.mobs().is_empty());
        for mob in session.mobs() {
            assert!(
                mob.kind.is_hostile(),
                "{:?} spawned at night on the surface",
                mob.kind
            );
            // `home` is the exact spawn feet position. Wander can pull the
            // body up to the 4-block leash *inside* the ring later, so the
            // spawn rule is asserted against the anchor, not the pose.
            let from_player = horizontal_distance(mob.home, player);
            assert!(
                from_player >= SAFE_RADIUS,
                "{:?} home {from_player} inside the player safe radius",
                mob.kind
            );
            let from_world_spawn = horizontal_distance(mob.home, world_spawn);
            assert!(
                from_world_spawn >= SAFE_RADIUS,
                "{:?} home {from_world_spawn} inside the world-spawn safe radius",
                mob.kind
            );
        }
    }

    #[test]
    fn tick_mobs_burns_surface_hostiles_to_death_at_day() {
        let mut session = lit_grass_plain_session(54, 3);
        session.clock = GameClock::from_time(NIGHT_TICK);
        for _ in 0..300 {
            session.tick_mobs(0.1, 1.0);
        }
        assert!(
            session.spawn_report().spawned > 0,
            "the burn proof needs a night population first"
        );

        session.clock = GameClock::from_time(DAY_TICK);
        session.reset_spawn_report();
        // 20 hp at 1 hp per 0.5 s burn cadence is 10 s; 12 s absorbs the
        // cadence phase and proves every hostile finished.
        for _ in 0..120 {
            session.tick_mobs(0.1, 1.0);
        }

        let report = session.spawn_report();
        assert!(
            report.burned > 0,
            "no hostile burned in daylight: {report:?}"
        );
        for mob in session.mobs() {
            if mob.kind.is_hostile() {
                assert_eq!(
                    mob.health, 0,
                    "a live surface hostile survived 12 s of daylight"
                );
            }
        }
    }

    #[test]
    fn a_record_past_the_despawn_radius_is_removed_by_the_single_reap_site() {
        let mut session = make_mob_session(55);
        add_resident_air(&mut session);
        let far = session.player.position + Vec3::new(DESPAWN_RADIUS + 1.0, 0.0, 0.0);
        session.mobs_mut().push(Mob::new(MobKind::Pig, far, 0.0));

        // Under one scan interval the record is untouched.
        session.tick_mobs(0.1, 1.0);
        assert_eq!(session.mobs().len(), 1);
        assert_eq!(session.spawn_report().despawned, 0);

        // Crossing `DESPAWN_SCAN_SECS` of game time lets the spawner mark the
        // record and the one reap site drop it on the same tick.
        for _ in 0..10 {
            session.tick_mobs(0.1, 1.0);
        }
        assert!(
            session.mobs().is_empty(),
            "a despawned record must be gone, not merely marked"
        );
        assert_eq!(session.spawn_report().despawned, 1);
    }

    #[test]
    fn ambient_spawning_is_frame_rate_independent_through_the_session() {
        let mut a = lit_grass_plain_session(56, 2);
        let mut b = lit_grass_plain_session(56, 2);
        a.clock = GameClock::from_time(NIGHT_TICK);
        b.clock = GameClock::from_time(NIGHT_TICK);

        for _ in 0..100 {
            a.tick_mobs(0.05, 1.0);
        }
        for _ in 0..500 {
            b.tick_mobs(0.01, 1.0);
        }

        assert_eq!(
            a.spawn_report(),
            b.spawn_report(),
            "5 s of game time must produce one population result"
        );
        assert!(
            a.spawn_report().spawned > 0,
            "the fixture must actually spawn: {:?}",
            a.spawn_report()
        );
        assert_eq!(a.mobs().len(), b.mobs().len());
        for (left, right) in a.mobs().iter().zip(b.mobs()) {
            assert_eq!(left.kind, right.kind);
            assert_eq!(left.home, right.home, "spawn places must match");
            assert!(
                (left.position - right.position).length() < 1e-3,
                "{:?} drifted: {:?} vs {:?}",
                left.kind,
                left.position,
                right.position
            );
        }
    }

    #[test]
    fn a_restored_session_reopens_ambient_spawning_with_the_world_spawn_anchor() {
        let mut data = make_save(57);
        data.mobs = pinned_saved_mobs();
        let seed = data.seed;
        let session =
            WorldSession::session_from_save(data, PathBuf::from("/tmp/spawn-restore.mcvx"));

        assert!(session.ambient_spawning());
        assert_eq!(session.spawn_report(), SpawnReport::default());
        assert_eq!(session.world_spawn(), WorldSession::spawn_position(seed));
        assert!(session.world_spawn().is_finite());
    }

    #[test]
    fn ambient_spawning_is_not_gated_on_game_mode() {
        // S09 owns Creative inertness. This gate is only the scripted demo
        // seam, so a Creative session must populate exactly like a Survival
        // one or a later slice could mistake the gate for the mode rule.
        let mut creative = lit_grass_plain_session(58, 2);
        let mut survival = WorldSession::new_session_with_mode(
            58,
            PathBuf::from("/tmp/session-spawn-survival.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        lit_grass_plain(&mut survival, 2);
        creative.clock = GameClock::from_time(NIGHT_TICK);
        survival.clock = GameClock::from_time(NIGHT_TICK);

        for _ in 0..50 {
            creative.tick_mobs(0.1, 1.0);
            survival.tick_mobs(0.1, 1.0);
        }

        assert!(creative.ambient_spawning() && survival.ambient_spawning());
        assert!(
            creative.spawn_report().spawned > 0,
            "a Creative session must populate like Survival: {:?}",
            creative.spawn_report()
        );
        assert_eq!(creative.spawn_report(), survival.spawn_report());
        assert_eq!(creative.mobs().len(), survival.mobs().len());
        for (left, right) in creative.mobs().iter().zip(survival.mobs()) {
            assert_eq!(left.kind, right.kind);
            assert_eq!(left.home, right.home);
        }
    }

    fn pinned_saved_mobs() -> Vec<SavedMob> {
        vec![
            SavedMob {
                kind: MobKind::Pig,
                position: [1.5, 64.0, -3.25],
                yaw: 0.25,
                health: 4,
            },
            SavedMob {
                kind: MobKind::Skeleton,
                position: [-7.0, 70.5, 2.0],
                yaw: -1.25,
                health: 14,
            },
        ]
    }

    fn mob_save(mobs: Vec<SavedMob>) -> WorldSaveData {
        let mut data = make_save(9182u32);
        data.mobs = mobs;
        data
    }

    #[test]
    fn restored_session_rebuilds_mob_records_exactly() {
        let records = pinned_saved_mobs();
        let session = WorldSession::session_from_save(
            mob_save(records.clone()),
            PathBuf::from("/tmp/mob-restore.mcvx"),
        );

        assert_eq!(session.mobs().len(), records.len());
        for (mob, record) in session.mobs().iter().zip(records.iter()) {
            assert_eq!(mob.kind, record.kind);
            assert_eq!(mob.position, Vec3::from_array(record.position));
            assert_eq!(mob.yaw, record.yaw);
            assert_eq!(mob.health, record.health);
            assert_eq!(mob.home, mob.position, "home re-anchors to the saved feet");
            assert_eq!(mob.velocity, Vec3::ZERO);
            assert_eq!(mob.state, MobAiState::Wander);
            assert_eq!(mob.hurt, 0.0);
            assert_eq!(mob.cooldown, 0.0);
            assert_eq!(mob.walk_phase, 0.0);
        }
    }

    #[test]
    fn restored_session_resaves_byte_identically() {
        use crate::world::persistence::{load_world_from_bytes, save_world_to_bytes};

        let original = mob_save(pinned_saved_mobs());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &original).expect("mob save encodes");

        let loaded = load_world_from_bytes(&bytes).expect("mob save decodes");
        let session =
            WorldSession::session_from_save(loaded.clone(), PathBuf::from("/tmp/mob-resave.mcvx"));

        let mut rebuilt = loaded;
        rebuilt.mobs = session.mobs().iter().map(SavedMob::from).collect();
        let mut resaved = Vec::new();
        save_world_to_bytes(&mut resaved, &rebuilt).expect("restored mobs re-encode");

        assert_eq!(resaved, bytes, "restore then save is byte-identical");
    }

    #[test]
    fn restored_mobs_tick_from_a_restarted_runtime() {
        let mut session = WorldSession::session_from_save(
            mob_save(pinned_saved_mobs()),
            PathBuf::from("/tmp/mob-tick.mcvx"),
        );
        let feet = session.mobs()[0].position;
        let (chunk, _) = world_to_chunk_and_local(
            feet.x.floor() as i32,
            feet.y.floor() as i32,
            feet.z.floor() as i32,
        );
        session
            .world
            .insert_chunk_for_test(chunk, crate::world::chunk::Chunk::new_uniform(BlockId::Air));

        let before = session.mobs()[0].position;
        let tick = session.tick_mobs(0.05, 1.0);

        assert!(
            tick.ai_steps >= 1,
            "the restored runtime restarts at tick 0 and still steps"
        );
        assert_ne!(
            session.mobs()[0].position,
            before,
            "a resident restored mob walks again"
        );
    }

    #[test]
    fn empty_mob_save_keeps_the_179_byte_edit_free_length() {
        use crate::world::persistence::{load_world_from_bytes, save_world_to_bytes};

        let data = make_save(9183u32);
        assert!(data.mobs.is_empty());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("empty mob save encodes");

        // Measured v9 shape: the empty content table adds four bytes before
        // the hotbar, so the mob count moves from 149 to 153, and the v8 pack
        // + digest tail plus the 10-byte config tail complete the payload.
        assert_eq!(bytes.len(), 179);
        assert_eq!(
            &bytes[153..157],
            &0u32.to_le_bytes(),
            "the v9 mob count sits at offset 153"
        );
        let loaded = load_world_from_bytes(&bytes).expect("empty mob save decodes");
        assert!(loaded.mobs.is_empty());
    }

    #[test]
    fn spawn_settle_pending_exposes_only_the_fresh_survival_gate() {
        let survival = survival_session(37);
        assert!(survival.spawn_settle_pending());

        let creative = WorldSession::new_session(
            38,
            PathBuf::from("/tmp/session-mobs-creative.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        assert!(!creative.spawn_settle_pending());

        let restored = WorldSession::session_from_save(
            make_save(39),
            PathBuf::from("/tmp/session-mobs-restored.mcvx"),
        );
        assert!(!restored.spawn_settle_pending());
    }

    fn mob_session_with(kind: MobKind, seed: u32) -> (WorldSession, usize) {
        let mut session = make_mob_session(seed);
        add_resident_air(&mut session);
        let index = session.mobs().len();
        session
            .mobs_mut()
            .push(Mob::new(kind, Vec3::new(8.5, 5.0, 8.5), 0.0));
        (session, index)
    }

    fn lethal_hit(session: &mut WorldSession, index: usize) -> MobHit {
        let kind = session.mobs()[index].kind;
        session
            .attack_mob(index, kind.max_health(), Vec3::new(8.5, 5.0, 6.5))
            .expect("a full-health hit is lethal")
    }

    fn assert_inventory_empty(session: &WorldSession) {
        for slot in 0..crate::player::inventory::INVENTORY_SLOTS {
            assert_eq!(
                session.player.inventory().slot(slot),
                None,
                "slot {slot} must stay empty"
            );
        }
        assert!(session.player.inventory().cursor().is_none());
    }

    #[test]
    fn player_pig_kill_accepts_exactly_meat_and_leather() {
        let (mut session, index) = mob_session_with(MobKind::Pig, 61);
        let hit = lethal_hit(&mut session, index);

        let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(2, 0));
        assert!(!report.lost_any());
        assert_eq!(
            session.player.inventory().slot(0),
            Item::new(ItemId::Material(Material::RawPorkchop), 1).ok()
        );
        assert_eq!(
            session.player.inventory().slot(1),
            Item::new(ItemId::Material(Material::Leather), 1).ok()
        );
        assert_eq!(session.mobs().len(), 1, "the record stays for the collapse");
    }

    #[test]
    fn player_cow_kill_merges_the_two_leather_grants_into_one_stack() {
        let (mut session, pig_index) = mob_session_with(MobKind::Pig, 62);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Cow, Vec3::new(10.5, 5.0, 8.5), 0.0));
        let pig_hit = lethal_hit(&mut session, pig_index);
        assert_eq!(
            session.resolve_mob_kill(pig_index, pig_hit, MobKillCause::Player),
            MobDropReport::new(2, 0)
        );

        let cow_index = 1;
        let cow_hit = lethal_hit(&mut session, cow_index);
        let report = session.resolve_mob_kill(cow_index, cow_hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(3, 0));
        assert_eq!(
            session.player.inventory().slot(1),
            Item::new(ItemId::Material(Material::Leather), 3).ok(),
            "pig leather and cow leather must merge into one stack of three"
        );
        assert_eq!(
            session.player.inventory().slot(2),
            Item::new(ItemId::Material(Material::RawBeef), 1).ok()
        );
    }

    #[test]
    fn environment_cause_grants_nothing_even_when_the_inventory_is_empty() {
        let (mut session, index) = mob_session_with(MobKind::Pig, 63);
        let hit = lethal_hit(&mut session, index);

        let report = session.resolve_mob_kill(index, hit, MobKillCause::Environment);

        assert_eq!(report, MobDropReport::default());
        assert_inventory_empty(&session);
        assert_eq!(
            session.mobs()[index].health,
            0,
            "the kill itself still landed"
        );
    }

    #[test]
    fn non_lethal_hit_and_stale_index_grant_nothing() {
        let (mut session, index) = mob_session_with(MobKind::Pig, 64);
        let hit = session
            .attack_mob(index, 1, Vec3::new(8.5, 5.0, 6.5))
            .expect("a non-lethal hit still lands");
        assert!(!hit.killed);

        assert_eq!(
            session.resolve_mob_kill(index, hit, MobKillCause::Player),
            MobDropReport::default()
        );
        assert_inventory_empty(&session);

        let stale = MobHit {
            damage: 10,
            health_after: 0,
            killed: true,
        };
        assert_eq!(
            session.resolve_mob_kill(99, stale, MobKillCause::Player),
            MobDropReport::default(),
            "a stale index must not grant drops"
        );
        assert_inventory_empty(&session);
    }

    #[test]
    fn a_full_inventory_loses_only_the_overflow() {
        let (mut session, index) = mob_session_with(MobKind::Pig, 65);
        // 35 stone slots leave no room for the porkchop; the last slot holds
        // 63 leather so exactly one leather unit can still merge in.
        let mut slots = [None; crate::player::inventory::INVENTORY_SLOTS];
        for slot in &mut slots[..crate::player::inventory::INVENTORY_SLOTS - 1] {
            *slot = Item::new(ItemId::Block(BlockId::Stone), 64).ok();
        }
        let last = crate::player::inventory::INVENTORY_SLOTS - 1;
        slots[last] = Item::new(ItemId::Material(Material::Leather), 63).ok();
        session.player = Player::with_inventory(
            session.player.position,
            crate::player::inventory::Inventory::from_parts(slots, None),
            crate::player::inventory::CraftingGrid::new(crate::player::inventory::GridKind::Player),
            crate::player::inventory::CraftingGrid::new(crate::player::inventory::GridKind::Table),
        );

        let hit = lethal_hit(&mut session, index);
        let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(1, 1));
        assert!(report.lost_any());
        assert_eq!(
            report.accepted + report.lost,
            2,
            "units are conserved: one porkchop lost, one leather accepted"
        );
        assert_eq!(
            session.player.inventory().slot(last),
            Item::new(ItemId::Material(Material::Leather), 64).ok(),
            "only the accepted unit landed"
        );
    }

    #[test]
    fn environmental_skeleton_death_grants_nothing() {
        let (mut session, index) = mob_session_with(MobKind::Skeleton, 73);
        let hit = lethal_hit(&mut session, index);

        let report = session.resolve_mob_kill(index, hit, MobKillCause::Environment);

        assert_eq!(report, MobDropReport::default());
        assert_inventory_empty(&session);
        assert_eq!(
            session.mobs()[index].health,
            0,
            "the kill itself still landed"
        );
    }

    #[test]
    fn mob_drop_items_resolves_the_t01_bridge_for_every_kind() {
        // The fixture home cell and two seed picks: 73 lands the skeleton's
        // one-in-sixteen bow, 71 does not (see mob_loot::drop_roll).
        let home = IVec3::new(8, 5, 8);
        assert_eq!(
            mob_drop_items(MobKind::Pig, 61, home),
            Some(vec![
                Item::new(ItemId::Material(Material::RawPorkchop), 1).expect("porkchop"),
                Item::new(ItemId::Material(Material::Leather), 1).expect("leather"),
            ])
        );
        assert_eq!(
            mob_drop_items(MobKind::Cow, 61, home),
            Some(vec![
                Item::new(ItemId::Material(Material::RawBeef), 1).expect("beef"),
                Item::new(ItemId::Material(Material::Leather), 2).expect("leather"),
            ])
        );
        assert_eq!(
            mob_drop_items(MobKind::Zombie, 61, home),
            Some(vec![
                Item::new(ItemId::Material(Material::RottenFlesh), 1).expect("rotten flesh"),
            ])
        );
        assert_eq!(
            mob_drop_items(MobKind::Skeleton, 71, home),
            Some(vec![
                Item::new(ItemId::Material(Material::Bone), 1).expect("bone"),
                Item::new(ItemId::Material(Material::Arrow), 2).expect("arrows"),
            ]),
            "a rare-bow miss still drops the unconditional entries"
        );
        assert_eq!(
            mob_drop_items(MobKind::Skeleton, 73, home),
            Some(vec![
                Item::new(ItemId::Material(Material::Bone), 1).expect("bone"),
                Item::new(ItemId::Material(Material::Arrow), 2).expect("arrows"),
                Item::new(
                    ItemId::Tool {
                        kind: ToolKind::Bow,
                        tier: ToolTier::Wood,
                    },
                    1,
                )
                .expect("wood bow"),
            ]),
            "the same inputs must resolve the same rare batch"
        );
        // Deterministic and home-keyed: a different home is a different draw.
        assert_eq!(
            mob_drop_items(MobKind::Skeleton, 73, home),
            mob_drop_items(MobKind::Skeleton, 73, home)
        );
        assert_ne!(
            mob_drop_items(MobKind::Skeleton, 73, IVec3::new(100, 5, 8)),
            mob_drop_items(MobKind::Skeleton, 73, home),
            "a different home cell must not share the fixture's roll"
        );
    }

    #[test]
    fn player_skeleton_kill_grants_bone_arrows_and_a_durable_bow_on_a_hit() {
        let (mut session, index) = mob_session_with(MobKind::Skeleton, 73);
        let hit = lethal_hit(&mut session, index);

        let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(4, 0));
        assert!(!report.lost_any());
        assert_eq!(
            session.player.inventory().slot(0),
            Item::new(ItemId::Material(Material::Bone), 1).ok()
        );
        assert_eq!(
            session.player.inventory().slot(1),
            Item::new(ItemId::Material(Material::Arrow), 2).ok()
        );
        let bow = session
            .player
            .inventory()
            .slot(2)
            .expect("the rare bow must land");
        assert_eq!(
            bow.id(),
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            }
        );
        assert_eq!(bow.count(), 1, "tools are non-stackable");
        assert_eq!(
            bow.durability(),
            Some(ToolTier::Wood.max_durability()),
            "a dropped bow enters at full wood-bow durability"
        );
    }

    #[test]
    fn player_skeleton_kill_without_the_rare_roll_grants_bone_and_arrows_only() {
        let (mut session, index) = mob_session_with(MobKind::Skeleton, 71);
        let hit = lethal_hit(&mut session, index);

        let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(3, 0));
        assert_eq!(
            session.player.inventory().slot(0),
            Item::new(ItemId::Material(Material::Bone), 1).ok()
        );
        assert_eq!(
            session.player.inventory().slot(1),
            Item::new(ItemId::Material(Material::Arrow), 2).ok()
        );
        assert_eq!(
            session.player.inventory().slot(2),
            None,
            "a miss must not invent a bow"
        );
    }

    #[test]
    fn skeleton_full_inventory_loses_only_the_overflow_across_all_three_entries() {
        let (mut session, index) = mob_session_with(MobKind::Skeleton, 73);
        // 34 stone slots leave no room for the bone or the bow; the last slot
        // holds 62 arrows, so exactly the two rolled arrows merge in.
        let mut slots = [None; crate::player::inventory::INVENTORY_SLOTS];
        for slot in &mut slots[..crate::player::inventory::INVENTORY_SLOTS - 1] {
            *slot = Item::new(ItemId::Block(BlockId::Stone), 64).ok();
        }
        let last = crate::player::inventory::INVENTORY_SLOTS - 1;
        slots[last] = Item::new(ItemId::Material(Material::Arrow), 62).ok();
        session.player = Player::with_inventory(
            session.player.position,
            crate::player::inventory::Inventory::from_parts(slots, None),
            crate::player::inventory::CraftingGrid::new(crate::player::inventory::GridKind::Player),
            crate::player::inventory::CraftingGrid::new(crate::player::inventory::GridKind::Table),
        );

        let hit = lethal_hit(&mut session, index);
        let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(2, 2));
        assert!(report.lost_any());
        assert_eq!(
            report.accepted + report.lost,
            4,
            "units are conserved: bone and bow lost, two arrows accepted"
        );
        assert_eq!(
            session.player.inventory().slot(last),
            Item::new(ItemId::Material(Material::Arrow), 64).ok(),
            "only the overflow of the arrow entry was lost"
        );
    }

    #[test]
    fn player_zombie_kill_grants_exactly_one_rotten_flesh() {
        let (mut session, index) = mob_session_with(MobKind::Zombie, 67);
        let hit = lethal_hit(&mut session, index);

        let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);

        assert_eq!(report, MobDropReport::new(1, 0));
        assert!(!report.lost_any());
        assert_eq!(
            session.player.inventory().slot(0),
            Item::new(ItemId::Material(Material::RottenFlesh), 1).ok(),
            "a zombie kill yields one rotten flesh"
        );
    }

    // -- Projectiles (S07-T02) ---------------------------------------------

    fn arrow_session(seed: u32) -> WorldSession {
        // Arrow damage assertions pin the Survival charge rule, so the fixture
        // is Survival; Creative-specific tests opt in with `use_creative_rules`.
        let mut session = WorldSession::new_session_with_mode(
            seed,
            PathBuf::from("/tmp/session-arrows.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        add_resident_air(&mut session);
        session.set_ambient_spawning(false);
        session
    }

    fn give_bow(session: &mut WorldSession) {
        let bow = Item::new(
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            },
            1,
        )
        .expect("wood bow");
        session
            .player
            .insert_items(&[bow])
            .expect("empty inventory accepts a bow");
        assert!(session.player.select_slot(0), "select the bow slot");
    }

    fn give_arrows(session: &mut WorldSession, count: u16) {
        let arrows = Item::new(ItemId::Material(Material::Arrow), count).expect("arrows");
        session
            .player
            .insert_items(&[arrows])
            .expect("empty inventory accepts arrows");
    }

    fn arrow_count(session: &WorldSession) -> u32 {
        session
            .player
            .inventory()
            .slots()
            .iter()
            .flatten()
            .filter(|item| item.id() == ItemId::Material(Material::Arrow))
            .map(|item| u32::from(item.count()))
            .sum()
    }

    fn bow_durability(session: &WorldSession) -> Option<u16> {
        session
            .player
            .selected_item()
            .and_then(|item| item.durability())
    }

    fn shot_origin() -> Vec3 {
        Vec3::new(8.5, 5.5, 8.5)
    }

    #[test]
    fn fire_bow_consumes_one_arrow_and_wears_the_bow_once() {
        let mut session = arrow_session(51);
        give_bow(&mut session);
        give_arrows(&mut session, 4);

        let fired = session
            .fire_bow(shot_origin(), Vec3::X, 1.0)
            .expect("a full charge fires");

        assert_eq!(session.arrows().len(), 1);
        assert_eq!(session.arrows()[0], fired);
        assert_eq!(arrow_count(&session), 3, "exactly one arrow is spent");
        assert_eq!(bow_durability(&session), Some(58), "one point of wear");
        assert_eq!(fired.damage, 4, "full charge deals 4");
        assert!((fired.velocity.length() - 32.0).abs() < 1e-4);
        assert_eq!(fired.owner, crate::world::arrow::ArrowOwner::Player);
    }

    #[test]
    fn fire_bow_refusals_mutate_nothing() {
        // No ammo: the shot is refused after the bow and geometry checks, and
        // neither durability nor the arrow list changes.
        let mut no_ammo = arrow_session(52);
        give_bow(&mut no_ammo);
        assert_eq!(
            no_ammo.fire_bow(shot_origin(), Vec3::X, 1.0),
            Err(BowFireError::NoAmmo)
        );
        assert!(no_ammo.arrows().is_empty());
        assert_eq!(bow_durability(&no_ammo), Some(59));

        // Not holding the bow: a sword in the selected slot refuses before
        // ammo is even consulted.
        let mut sword = arrow_session(53);
        give_bow(&mut sword);
        give_arrows(&mut sword, 2);
        let sword_item = Item::new(
            ItemId::Tool {
                kind: ToolKind::Sword,
                tier: ToolTier::Wood,
            },
            1,
        )
        .expect("wood sword");
        sword
            .player
            .insert_items(&[sword_item])
            .expect("empty slot for the sword");
        assert!(sword.player.select_slot(2));
        assert_eq!(
            sword.fire_bow(shot_origin(), Vec3::X, 1.0),
            Err(BowFireError::NotHoldingBow)
        );
        assert!(sword.arrows().is_empty());
        assert_eq!(arrow_count(&sword), 2);

        // Dead player.
        let mut dead = arrow_session(54);
        give_bow(&mut dead);
        give_arrows(&mut dead, 2);
        dead.player.survival.damage(20);
        assert!(dead.is_dead());
        assert_eq!(
            dead.fire_bow(shot_origin(), Vec3::X, 1.0),
            Err(BowFireError::Dead)
        );
        assert!(dead.arrows().is_empty());
        assert_eq!(arrow_count(&dead), 2);

        // Bad geometry / charge.
        let mut invalid = arrow_session(55);
        give_bow(&mut invalid);
        give_arrows(&mut invalid, 2);
        assert_eq!(
            invalid.fire_bow(shot_origin(), Vec3::new(f32::NAN, 0.0, 0.0), 1.0),
            Err(BowFireError::InvalidShot)
        );
        assert_eq!(
            invalid.fire_bow(shot_origin(), Vec3::X, 1.5),
            Err(BowFireError::InvalidShot)
        );
        assert_eq!(
            invalid.fire_bow(shot_origin(), Vec3::X, -0.1),
            Err(BowFireError::InvalidShot)
        );
        assert!(invalid.arrows().is_empty());
        assert_eq!(arrow_count(&invalid), 2);
        assert_eq!(bow_durability(&invalid), Some(59));

        // Cap reached: no ammo and no wear are spent on a refused shot.
        let mut capped = arrow_session(56);
        let filler = Arrow::player(shot_origin(), Vec3::X, 0.0).expect("filler");
        for _ in 0..ARROW_CAP {
            assert!(capped.spawn_arrow(filler).is_some());
        }
        give_bow(&mut capped);
        give_arrows(&mut capped, 2);
        assert_eq!(
            capped.fire_bow(shot_origin(), Vec3::X, 1.0),
            Err(BowFireError::ArrowCapReached)
        );
        assert_eq!(capped.arrows().len(), ARROW_CAP);
        assert_eq!(arrow_count(&capped), 2);
        assert_eq!(bow_durability(&capped), Some(59));
    }

    #[test]
    fn arrow_cap_is_64_and_embedded_arrows_count() {
        let mut session = arrow_session(57);
        let mut embedded = Arrow::player(shot_origin(), Vec3::X, 0.0).expect("filler");
        embedded.embed();
        assert!(session.spawn_arrow(embedded).is_some());
        let filler = Arrow::player(shot_origin(), Vec3::X, 0.0).expect("filler");
        while session.arrows().len() < ARROW_CAP {
            assert!(session.spawn_arrow(filler).is_some());
        }

        assert_eq!(ARROW_CAP, 64);
        assert_eq!(session.arrows().len(), 64);
        assert_eq!(
            session.spawn_arrow(filler),
            None,
            "the 65th arrow is refused"
        );
        assert_eq!(session.arrows().len(), 64);
        assert_eq!(session.arrows()[0].state, ArrowState::Embedded);

        give_bow(&mut session);
        give_arrows(&mut session, 1);
        assert_eq!(
            session.fire_bow(shot_origin(), Vec3::X, 1.0),
            Err(BowFireError::ArrowCapReached)
        );
        assert_eq!(session.arrows().len(), 64);
        assert_eq!(arrow_count(&session), 1);
    }

    #[test]
    fn arrow_embeds_in_a_wall_then_expires() {
        let mut session = arrow_session(58);
        // Flat floor under the flight path plus a one-block wall ahead of the
        // origin (2.5, 5.5, 8.5) heading +X.
        for x in 1..=8 {
            for z in 7..=10 {
                place_block(&mut session, IVec3::new(x, 4, z), BlockId::Stone);
            }
        }
        place_block(&mut session, IVec3::new(4, 5, 8), BlockId::Stone);
        give_bow(&mut session);
        give_arrows(&mut session, 1);

        let fired = session
            .fire_bow(Vec3::new(2.5, 5.5, 8.5), Vec3::X, 1.0)
            .expect("full charge");
        assert_eq!(fired.state, ArrowState::Flying);

        let tick = session.tick_arrows(0.1, 1.0);
        assert_eq!(tick.block_hits, 1, "the wall stops the arrow");
        assert_eq!(session.arrows().len(), 1);
        let embedded = session.arrows()[0];
        assert_eq!(embedded.state, ArrowState::Embedded);
        assert_eq!(embedded.life, crate::world::arrow::ARROW_EMBED_SECS);
        assert!(embedded.position.x >= 4.0 && embedded.position.x < 5.0);

        let mut expired = 0;
        for _ in 0..20 {
            expired += session.tick_arrows(0.1, 1.0).expired;
        }
        assert!(session.arrows().is_empty(), "the embed timer despawns it");
        assert!(expired >= 1);
    }

    #[test]
    fn a_full_charge_arrow_cannot_tunnel_a_one_block_wall() {
        let mut session = arrow_session(59);
        place_block(&mut session, IVec3::new(4, 5, 8), BlockId::Stone);
        give_bow(&mut session);
        give_arrows(&mut session, 1);
        session
            .fire_bow(Vec3::new(2.5, 5.5, 8.5), Vec3::X, 1.0)
            .expect("full charge");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.block_hits, 1, "the wall is detected inside the sweep");
        assert_eq!(session.arrows().len(), 1);
        assert_eq!(session.arrows()[0].state, ArrowState::Embedded);
        assert!(
            session.arrows()[0].position.x < 5.0,
            "a 3.2-block frame must not tunnel through the one-block wall"
        );
    }

    #[test]
    fn arrow_flight_is_frame_rate_independent() {
        let mut single = arrow_session(60);
        give_bow(&mut single);
        give_arrows(&mut single, 1);
        single
            .fire_bow(shot_origin(), Vec3::X, 1.0)
            .expect("full charge");

        let mut split = arrow_session(60);
        give_bow(&mut split);
        give_arrows(&mut split, 1);
        split
            .fire_bow(shot_origin(), Vec3::X, 1.0)
            .expect("full charge");

        single.tick_arrows(0.1, 1.0);
        split.tick_arrows(0.05, 1.0);
        split.tick_arrows(0.05, 1.0);

        assert_eq!(single.arrows().len(), 1);
        assert_eq!(split.arrows().len(), 1);
        assert!(
            (single.arrows()[0].position - split.arrows()[0].position).length() < 1e-3,
            "one 0.1 s tick and two 0.05 s ticks must agree"
        );
    }

    #[test]
    fn non_resident_arrow_freezes_instead_of_colliding() {
        let mut session = arrow_session(61);
        // Chunk (0,0,0) is the only resident chunk and spans x 0..32: one
        // 32 b/s substep from x = 31.9 crosses into chunk (1,0,0), which is
        // unloaded.
        let arrow = Arrow::player(Vec3::new(31.9, 5.5, 8.5), Vec3::X, 1.0).expect("arrow");
        let index = session.spawn_arrow(arrow).expect("not finished");
        let before = session.arrows()[index];

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.block_hits, 0, "unloaded Air is never a collision");
        assert_eq!(session.arrows().len(), 1, "the frozen arrow is retained");
        assert_eq!(session.arrows()[index].position, before.position);
        assert_eq!(session.arrows()[index].state, ArrowState::Flying);
    }

    /// A Survival session whose player stands in the resident air chunk.
    /// Needed by the skeleton-arrow tests: the projectile path is the only
    /// thing under test, but the damage authority deliberately gates on mode.
    fn arrow_survival_session(seed: u32) -> WorldSession {
        let mut session = WorldSession::new_session_with_mode(
            seed,
            PathBuf::from("/tmp/session-arrows-survival.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Survival,
        );
        add_resident_air(&mut session);
        session.set_ambient_spawning(false);
        session.player.position = Vec3::new(8.5, 5.0, 8.5);
        session
    }

    fn pig_at(session: &mut WorldSession, position: Vec3) {
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Pig, position, 0.0));
    }

    fn inventory_has(session: &WorldSession, item: Item) -> bool {
        session
            .player
            .inventory()
            .slots()
            .iter()
            .flatten()
            .any(|held| *held == item)
    }

    #[test]
    fn arrow_hits_a_mob_for_damage_and_away_knockback() {
        let mut session = arrow_session(70);
        let mob_position = Vec3::new(4.5, 5.0, 8.5);
        pig_at(&mut session, mob_position);
        give_bow(&mut session);
        give_arrows(&mut session, 1);
        let origin = Vec3::new(2.5, 5.5, 8.5);
        session
            .fire_bow(origin, Vec3::X, 1.0)
            .expect("a full charge fires");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.mob_hits, 1, "the body, not the air, stops the arrow");
        assert_eq!(tick.block_hits, 0, "a body hit pre-empts the block test");
        let mob = &session.mobs()[0];
        assert_eq!(mob.health, 6, "10 hp pig takes the full-charge 4");
        assert!(mob.hurt > 0.0, "the hurt flash is armed");
        assert!(mob.hurt_iframe > 0.0, "the mob i-frame window is open");
        let away = (mob_position - origin).normalize();
        assert!(
            mob.knockback.dot(away) > 0.0,
            "knockback points away from the arrow, got {}",
            mob.knockback
        );
        assert!(session.arrows().is_empty(), "a body consumes the arrow");
    }

    #[test]
    fn an_iframe_blocked_arrow_is_spent_without_damage() {
        let mut session = arrow_session(71);
        pig_at(&mut session, Vec3::new(4.5, 5.0, 8.5));
        give_bow(&mut session);
        give_arrows(&mut session, 2);
        let origin = Vec3::new(2.5, 5.5, 8.5);

        session.fire_bow(origin, Vec3::X, 1.0).expect("first shot");
        let first = session.tick_arrows(0.1, 1.0);
        assert_eq!(first.mob_hits, 1);
        let health_after_first = session.mobs()[0].health;
        let iframe_after_first = session.mobs()[0].hurt_iframe;

        // No `tick_mobs` in between, so the mob window is still open.
        session.fire_bow(origin, Vec3::X, 1.0).expect("second shot");
        let second = session.tick_arrows(0.1, 1.0);

        assert_eq!(second.mob_hits, 1, "an i-frame-refused arrow still hit");
        assert_eq!(
            first.mob_hits + second.mob_hits,
            2,
            "both arrows reached the body"
        );
        assert_eq!(
            session.mobs()[0].health,
            health_after_first,
            "the refused hit costs no health"
        );
        assert_eq!(session.mobs()[0].hurt_iframe, iframe_after_first);
        assert!(session.arrows().is_empty(), "both arrows are spent");
    }

    #[test]
    fn arrow_kill_grants_the_drop_table() {
        let mut session = arrow_session(72);
        pig_at(&mut session, Vec3::new(4.5, 5.0, 8.5));
        session.mobs_mut()[0].health = 2;
        give_bow(&mut session);
        give_arrows(&mut session, 1);

        session
            .fire_bow(Vec3::new(2.5, 5.5, 8.5), Vec3::X, 1.0)
            .expect("full charge");
        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.kills, 1, "the kill is credited to the player");
        assert_eq!(
            tick.drops,
            MobDropReport::new(2, 0),
            "one porkchop and one leather land"
        );
        assert_eq!(session.mobs()[0].health, 0, "the record stays for the fade");
        assert!(inventory_has(
            &session,
            Item::new(ItemId::Material(Material::RawPorkchop), 1).expect("porkchop")
        ));
        assert!(inventory_has(
            &session,
            Item::new(ItemId::Material(Material::Leather), 1).expect("leather")
        ));
    }

    #[test]
    fn creative_player_arrow_one_shot_kills_a_full_health_zombie() {
        let mut session = arrow_session(74);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Zombie, Vec3::new(4.5, 5.0, 8.5), 0.0));
        session.use_creative_rules();
        let arrow = Arrow::player(Vec3::new(2.5, 5.5, 8.5), Vec3::X, 1.0).expect("full charge");
        assert_eq!(arrow.damage, 4, "the charge damage stays on the arrow");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.mob_hits, 1);
        assert_eq!(tick.kills, 1, "the kill is player-attributed");
        assert_eq!(
            session.mobs()[0].health,
            0,
            "a Creative arrow lands the remaining 20 hp, not its 4"
        );
        assert!(!tick.drops.lost_any(), "the pinned drop table still lands");
    }

    #[test]
    fn survival_player_arrow_damages_by_charge() {
        let mut session = arrow_session(75);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Zombie, Vec3::new(4.5, 5.0, 8.5), 0.0));
        let arrow = Arrow::player(Vec3::new(2.5, 5.5, 8.5), Vec3::X, 1.0).expect("full charge");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.mob_hits, 1);
        assert_eq!(tick.kills, 0, "4 damage cannot kill a 20 hp zombie");
        assert_eq!(
            session.mobs()[0].health,
            16,
            "Survival keeps the charge-scaled damage"
        );
    }

    #[test]
    fn skeleton_arrow_friendly_fire_keeps_its_two_damage_in_a_creative_session() {
        let mut session = arrow_session(76);
        session
            .mobs_mut()
            .push(Mob::new(MobKind::Zombie, Vec3::new(4.5, 5.0, 8.5), 0.0));
        session.use_creative_rules();
        let arrow = Arrow::skeleton(Vec3::new(2.5, 5.5, 8.5), Vec3::X).expect("skeleton shot");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.mob_hits, 1);
        assert_eq!(tick.kills, 0, "a skeleton earns no kill credit");
        assert_eq!(
            session.mobs()[0].health,
            18,
            "the Creative rule is scoped to player-owned arrows"
        );
        assert_eq!(tick.drops, MobDropReport::default());
    }

    #[test]
    fn an_arrow_starting_inside_a_mob_does_not_hit_it() {
        let mut session = arrow_session(73);
        pig_at(&mut session, Vec3::new(4.5, 5.0, 8.5));
        // Inside the pig's AABB (x 4.15..4.85, y 5.0..5.9), heading outward.
        let arrow = Arrow::player(Vec3::new(4.5, 5.4, 8.5), Vec3::X, 1.0).expect("arrow");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(
            tick.mob_hits, 0,
            "a body the arrow starts inside is skipped"
        );
        assert_eq!(session.mobs()[0].health, 10, "no self-hit damage");
        assert_eq!(session.arrows().len(), 1, "the arrow flies on");
    }

    #[test]
    fn player_owned_arrows_never_hit_the_player() {
        let mut session = arrow_survival_session(74);
        // Straight down from inside the player's own AABB: ownership alone
        // must keep it inert against the shooter.
        let arrow = Arrow::player(Vec3::new(8.5, 5.8, 8.5), -Vec3::Y, 1.0).expect("arrow");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.player_hits, 0);
        assert_eq!(tick.player_damage, 0);
        assert_eq!(session.player.survival.health(), 20);
        assert_eq!(session.last_damage_cause(), None);
    }

    #[test]
    fn skeleton_arrow_damages_a_survival_player_once_per_iframe() {
        let mut session = arrow_survival_session(75);
        let arrow = Arrow::skeleton(Vec3::new(8.5, 5.5, 6.5), Vec3::Z).expect("skeleton arrow");
        session.spawn_arrow(arrow).expect("arrow fits");

        let first = session.tick_arrows(0.1, 1.0);

        assert_eq!(first.player_hits, 1);
        assert_eq!(
            first.player_damage, 2,
            "skeleton arrows are the 2-damage pair"
        );
        assert_eq!(session.player.survival.health(), 18);
        assert_eq!(
            session.last_damage_cause(),
            Some(DamageCause::SkeletonArrow)
        );
        assert!(
            session.player.combat.iframe() > 0.0,
            "the arrow shares the single player i-frame window"
        );
        assert!(session.arrows().is_empty());

        // A second arrow inside the window: no damage, no cause rewrite, and
        // the arrow is still spent.
        let blocked = Arrow::skeleton(Vec3::new(8.5, 5.5, 6.5), Vec3::Z).expect("skeleton arrow");
        session.spawn_arrow(blocked).expect("arrow fits");
        let second = session.tick_arrows(0.1, 1.0);

        assert_eq!(second.player_hits, 0);
        assert_eq!(second.player_damage, 0);
        assert_eq!(session.player.survival.health(), 18);
        assert_eq!(
            session.last_damage_cause(),
            Some(DamageCause::SkeletonArrow),
            "a refused hit cannot rewrite the cause"
        );
        assert!(session.arrows().is_empty(), "the refused arrow is spent");

        // Let the fixed-rate authority close the window, then a third arrow
        // lands again. `tick_mobs` owns combat-timer aging.
        for _ in 0..10 {
            session.tick_mobs(0.1, 1.0);
        }
        assert_eq!(session.player.combat.iframe(), 0.0);
        let third = Arrow::skeleton(Vec3::new(8.5, 5.5, 6.5), Vec3::Z).expect("skeleton arrow");
        session.spawn_arrow(third).expect("arrow fits");
        let third_tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(third_tick.player_hits, 1);
        assert_eq!(third_tick.player_damage, 2);
        assert_eq!(session.player.survival.health(), 16);
    }

    #[test]
    fn skeleton_arrow_is_inert_against_a_creative_player() {
        let mut session = arrow_survival_session(76);
        session.player.set_mode(GameMode::Creative);
        let arrow = Arrow::skeleton(Vec3::new(8.5, 5.5, 6.5), Vec3::Z).expect("skeleton arrow");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.player_hits, 0);
        assert_eq!(tick.player_damage, 0);
        assert_eq!(session.player.survival.health(), 20);
        assert_eq!(session.last_damage_cause(), None);
        assert!(session.arrows().is_empty(), "the inert hit still spends it");
    }

    // -- Skeleton shot seam (S08-T02) --------------------------------------

    /// A Survival session with one Skeleton `distance` blocks in front of the
    /// player on an all-air resident chunk, inside its 5..=12 fire band.
    /// Ambient spawning is closed so the mob list is exactly the fixture.
    fn skeleton_shot_session(seed: u32, distance: f32) -> WorldSession {
        let mut session = arrow_survival_session(seed);
        session.mobs_mut().push(Mob::new(
            MobKind::Skeleton,
            Vec3::new(8.5, 5.0, 8.5 + distance),
            0.0,
        ));
        session
    }

    /// A synthetic geometry-only decision, used to exercise the session seam
    /// without depending on the brain's aggro state.
    fn skeleton_shot(origin: Vec3, dir: Vec3) -> MobShot {
        MobShot {
            kind: MobKind::Skeleton,
            origin,
            dir,
        }
    }

    #[test]
    fn tick_mobs_spawns_one_skeleton_arrow_at_the_pinned_pair() {
        let mut session = skeleton_shot_session(81, 6.0);

        let tick = session.tick_mobs(0.05, 1.0);

        assert_eq!(tick.ai_steps, 1);
        assert!(tick.attacks.is_empty(), "a skeleton never melees");
        assert_eq!(tick.shots.len(), 1, "one step, one decided shot");
        assert_eq!(tick.shots[0].kind, MobKind::Skeleton);
        assert_eq!(session.arrows().len(), 1, "the seam spawns the arrow");

        let arrow = session.arrows()[0];
        assert_eq!(arrow.owner, ArrowOwner::Skeleton);
        assert_eq!(arrow.damage, 2, "skeleton arrows are the pinned pair");
        assert!(
            (arrow.velocity.length() - 28.0).abs() < 1e-3,
            "skeleton arrows fly at 28 b/s, got {}",
            arrow.velocity.length()
        );
        assert_eq!(arrow.state, ArrowState::Flying);
    }

    #[test]
    fn no_shots_without_survival_or_time() {
        // Creative: the target build emits nothing, and the explicit seam
        // guard refuses a shot produced anywhere else.
        let mut creative = WorldSession::new_session(
            82,
            PathBuf::from("/tmp/session-skeleton-shots-creative.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        add_resident_air(&mut creative);
        creative.set_ambient_spawning(false);
        creative.player.position = Vec3::new(8.5, 5.0, 8.5);
        creative
            .mobs_mut()
            .push(Mob::new(MobKind::Skeleton, Vec3::new(8.5, 5.0, 14.5), 0.0));
        for _ in 0..10 {
            assert!(creative.tick_mobs(0.05, 1.0).shots.is_empty());
        }
        assert!(creative.arrows().is_empty());
        assert_eq!(
            creative.apply_mob_shots(&[skeleton_shot(Vec3::new(8.5, 6.53, 14.5), -Vec3::Z)]),
            0,
            "Creative refuses the seam even for an externally built shot"
        );
        assert!(creative.arrows().is_empty());

        // Closed hostile gate: no target, so no shot decision at all.
        let mut gated = skeleton_shot_session(83, 6.0);
        gated.set_hostile_ai_enabled(false);
        for _ in 0..10 {
            assert!(gated.tick_mobs(0.05, 1.0).shots.is_empty());
        }
        assert!(gated.arrows().is_empty());

        // Paused / degenerate frames return before the AI and the seam.
        let mut frozen = skeleton_shot_session(84, 6.0);
        assert!(frozen.tick_mobs(0.0, 1.0).shots.is_empty());
        assert!(frozen.tick_mobs(0.05, 0.0).shots.is_empty());
        assert!(frozen.tick_mobs(0.05, -2.0).shots.is_empty());
        assert!(frozen.tick_mobs(f32::NAN, 1.0).shots.is_empty());
        assert!(frozen.tick_mobs(-1.0, 1.0).shots.is_empty());
        assert!(frozen.arrows().is_empty());
    }

    #[test]
    fn the_arrow_cap_refuses_skeleton_shots_without_mutating_the_list() {
        let mut session = skeleton_shot_session(85, 6.0);
        let filler = Arrow::skeleton(shot_origin(), Vec3::Z).expect("skeleton filler");
        while session.arrows().len() < ARROW_CAP {
            assert!(session.spawn_arrow(filler).is_some());
        }
        assert_eq!(session.arrows().len(), ARROW_CAP);

        let spawned =
            session.apply_mob_shots(&[skeleton_shot(Vec3::new(8.5, 6.53, 14.5), -Vec3::Z)]);

        assert_eq!(spawned, 0, "a full list accepts no new shot");
        assert_eq!(
            session.arrows().len(),
            ARROW_CAP,
            "a refused shot mutates nothing"
        );
    }

    #[test]
    fn a_skeleton_arrow_inside_its_owners_box_never_hits_the_owner() {
        let mut session = skeleton_shot_session(86, 6.0);
        let owner = session.mobs()[0].position;
        let health = session.mobs()[0].health;
        let arrow =
            Arrow::skeleton(owner + Vec3::new(0.0, 0.5, 0.0), Vec3::Z).expect("point-blank arrow");
        session.spawn_arrow(arrow).expect("arrow fits");

        let tick = session.tick_arrows(0.05, 1.0);

        assert_eq!(tick.mob_hits, 0, "the owner is never its own target");
        assert_eq!(session.mobs()[0].health, health);
        assert_eq!(session.mobs()[0].state, MobAiState::Wander);
    }

    #[test]
    fn one_tick_mobs_and_tick_arrows_pass_lands_one_skeleton_arrow_hit() {
        let mut session = skeleton_shot_session(87, 6.0);
        let health = session.player.survival.health();

        let fired = session.tick_mobs(0.05, 1.0);
        assert_eq!(fired.shots.len(), 1, "the brain decides, the seam spawns");
        assert_eq!(session.arrows().len(), 1);

        let mut hits = 0u32;
        let mut damage = 0u16;
        for _ in 0..20 {
            let tick = session.tick_arrows(0.05, 1.0);
            hits += tick.player_hits;
            damage += tick.player_damage;
        }

        assert_eq!(hits, 1, "the arc lands exactly one hit");
        assert_eq!(damage, 2);
        assert_eq!(session.player.survival.health(), health - 2);
        assert_eq!(
            session.last_damage_cause(),
            Some(DamageCause::SkeletonArrow)
        );
        assert!(session.arrows().is_empty(), "the hit spends the arrow");
    }

    #[test]
    fn a_dead_player_emits_no_shots_and_takes_no_arrow_damage() {
        let mut session = skeleton_shot_session(88, 6.0);
        session.player.survival.damage(20);
        assert!(session.is_dead());

        for _ in 0..10 {
            assert!(
                session.tick_mobs(0.05, 1.0).shots.is_empty(),
                "a dead player is never a target, so nothing is fired"
            );
        }
        assert!(session.arrows().is_empty());

        // An arrow already in flight is still swept and spent, but the combat
        // authority refuses the damage.
        let arrow = Arrow::skeleton(Vec3::new(8.5, 5.5, 6.5), Vec3::Z).expect("skeleton arrow");
        session.spawn_arrow(arrow).expect("arrow fits");
        let tick = session.tick_arrows(0.1, 1.0);

        assert_eq!(tick.player_hits, 0);
        assert_eq!(tick.player_damage, 0);
        assert_eq!(session.player.survival.health(), 0);
        assert_eq!(session.last_damage_cause(), None);
        assert!(session.arrows().is_empty(), "the inert hit still spends it");
    }

    #[test]
    fn respawn_clears_arrows_and_a_reload_starts_with_none() {
        use crate::world::persistence::{load_world_from_bytes, save_world_to_bytes};

        let mut session = arrow_session(62);
        session
            .spawn_arrow(Arrow::player(shot_origin(), Vec3::X, 1.0).expect("arrow"))
            .expect("arrow fits");
        session.last_damage_cause = Some(DamageCause::SkeletonArrow);
        assert_eq!(session.arrows().len(), 1);

        session.respawn();
        assert!(session.arrows().is_empty(), "respawn clears projectiles");
        assert_eq!(
            session.last_damage_cause(),
            None,
            "respawn forgets the projectile cause too"
        );

        // Arrows have no wire representation: the v9 save stays the same
        // 179-byte edit-free layout and a reload starts in flight-free air.
        let data = make_save(62);
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save encodes");
        assert_eq!(bytes.len(), 179, "arrows add no save bytes");
        let restored = WorldSession::session_from_save(
            load_world_from_bytes(&bytes).expect("save decodes"),
            PathBuf::from("/tmp/arrow-reload.mcvx"),
        );
        assert!(restored.arrows().is_empty());
    }

    // -----------------------------------------------------------------------
    // S09-T02: Creative inertness pins at both authority sites, one-shot drops
    // -----------------------------------------------------------------------

    /// Total `RottenFlesh` units held anywhere in the inventory. Zombie drops
    /// are a single always-yielding entry, so this is an exact delta check.
    fn rotten_flesh_count(session: &WorldSession) -> u32 {
        session
            .player
            .inventory()
            .slots()
            .iter()
            .flatten()
            .filter(|item| item.id() == ItemId::Material(Material::RottenFlesh))
            .map(|item| u32::from(item.count()))
            .sum()
    }

    #[test]
    fn creative_skeleton_in_fire_band_emits_no_shots_or_chases() {
        // The shared helper puts the record 2.0 blocks ahead (inside the
        // skeleton's 5..=12 stand-off band it would immediately back away
        // from). Move it to 6 blocks: squarely inside the fire band, still
        // inside the all-air resident chunk, so only the mode can explain an
        // empty decision list.
        let mut session = survival_hostile_session(91, MobKind::Skeleton);
        {
            let mob = &mut session.mobs_mut()[0];
            mob.position = Vec3::new(8.5, 5.0, 14.5);
            mob.home = mob.position;
        }
        session.use_creative_rules();
        let health = session.player.survival.health();

        for _ in 0..30 {
            let tick = session.tick_mobs(0.05, 1.0);
            assert!(tick.attacks.is_empty(), "Creative is never meleed");
            assert!(
                tick.shots.is_empty(),
                "a Creative player is never a ranged target"
            );
            assert_ne!(
                session.mobs()[0].state,
                MobAiState::Chase,
                "Creative is never targeted"
            );
        }
        assert!(
            session.arrows().is_empty(),
            "the ranged arm of authority site 1 spawns nothing"
        );
        assert_eq!(session.player.survival.health(), health);
        assert_eq!(session.player_hurt_intensity(), 0.0);
        assert_eq!(session.last_damage_cause(), None);
    }

    #[test]
    fn creative_apply_mob_shots_spawns_nothing() {
        let mut session = survival_hostile_session(92, MobKind::Zombie);
        session.use_creative_rules();
        let eye = session.player.eye_position();
        let shot = MobShot {
            kind: MobKind::Skeleton,
            origin: eye,
            dir: Vec3::Z,
        };

        assert_eq!(
            session.apply_mob_shots(&[shot]),
            0,
            "authority site 2 refuses a ranged payload built anywhere else"
        );
        assert!(session.arrows().is_empty());

        let feet = session.player.position;
        let health = session.player.survival.health();
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            0,
            "the melee arm of the same site refuses every attack"
        );
        assert_eq!(session.player.survival.health(), health);
        assert_eq!(session.last_damage_cause(), None);
        assert_eq!(session.player_hurt_intensity(), 0.0);
    }

    #[test]
    fn creative_melee_one_shot_kills_and_grants_drops() {
        let mut session = survival_hostile_session(93, MobKind::Zombie);
        session.use_creative_rules();
        let feet = session.player.position;
        let before = rotten_flesh_count(&session);
        assert_eq!(session.mobs()[0].health, 20, "a full-health zombie");

        assert_eq!(attack_damage(None, GameMode::Creative, 20), 20);
        let damage = attack_damage(None, session.player.mode(), session.mobs()[0].health);
        let hit = session
            .attack_mob(0, damage, feet)
            .expect("a resident zombie accepts the swing");
        assert_eq!(
            hit,
            MobHit {
                damage: 20,
                health_after: 0,
                killed: true,
            }
        );

        let report = session.resolve_mob_kill(0, hit, MobKillCause::Player);
        assert_eq!(report, MobDropReport::new(1, 0));
        assert_eq!(
            rotten_flesh_count(&session) - before,
            1,
            "the pinned drop table still lands on a Creative kill"
        );
    }

    #[test]
    fn creative_arrow_one_shot_kills_and_grants_drops() {
        let mut session = survival_hostile_session(94, MobKind::Zombie);
        session.use_creative_rules();
        let before = rotten_flesh_count(&session);
        let arrow = Arrow::player(Vec3::new(8.5, 5.5, 8.5), Vec3::Z, 0.0)
            .expect("a minimum-draw arrow is valid");
        assert_eq!(
            arrow.damage, 1,
            "base damage, so a kill cannot be explained by charge"
        );
        session.spawn_arrow(arrow).expect("empty list accepts it");

        let mut mob_hits = 0u32;
        let mut kills = 0u32;
        let mut drops = MobDropReport::default();
        for _ in 0..10 {
            let tick = session.tick_arrows(0.1, 1.0);
            mob_hits += tick.mob_hits;
            kills += tick.kills;
            drops.accepted += tick.drops.accepted;
            drops.lost += tick.drops.lost;
            if mob_hits > 0 {
                break;
            }
        }

        assert_eq!(mob_hits, 1, "the single arrow reaches the body");
        assert_eq!(kills, 1, "a Creative player arrow one-shots");
        assert_eq!(drops, MobDropReport::new(1, 0));
        assert_eq!(session.mobs()[0].health, 0);
        assert_eq!(
            rotten_flesh_count(&session) - before,
            1,
            "the shot kill grants the pinned drop table too"
        );
    }

    #[test]
    fn survival_arrow_still_damages_by_charge() {
        let mut session = survival_hostile_session(95, MobKind::Zombie);
        assert_eq!(session.player.mode(), GameMode::Survival);
        let before = rotten_flesh_count(&session);
        let arrow = Arrow::player(Vec3::new(8.5, 5.5, 8.5), Vec3::Z, 0.0)
            .expect("a minimum-draw arrow is valid");
        assert_eq!(arrow.damage, 1);
        session.spawn_arrow(arrow).expect("empty list accepts it");

        let mut mob_hits = 0u32;
        let mut kills = 0u32;
        let mut drops = MobDropReport::default();
        for _ in 0..10 {
            let tick = session.tick_arrows(0.1, 1.0);
            mob_hits += tick.mob_hits;
            kills += tick.kills;
            drops.accepted += tick.drops.accepted;
            drops.lost += tick.drops.lost;
            if mob_hits > 0 {
                break;
            }
        }

        assert_eq!(mob_hits, 1);
        assert_eq!(
            session.mobs()[0].health,
            19,
            "Survival keeps the charge damage (20 - 1)"
        );
        assert_eq!(kills, 0, "1 damage cannot kill a full-health zombie");
        assert_eq!(drops, MobDropReport::default());
        assert_eq!(rotten_flesh_count(&session), before, "no kill, no drop");
    }

    #[test]
    fn creative_rule_keys_on_the_effective_mode_not_save_mode() {
        let mut session = survival_hostile_session(96, MobKind::Zombie);
        assert_eq!(session.player.mode(), GameMode::Survival);
        assert_eq!(session.save_mode, GameMode::Survival);

        session.use_creative_rules();

        assert_eq!(session.player.mode(), GameMode::Creative);
        assert_eq!(
            session.save_mode,
            GameMode::Survival,
            "Edit Background must not rewrite the stored world mode"
        );

        // Inertness, read off the live mode while the save mode is Survival.
        let feet = session.player.position;
        let health = session.player.survival.health();
        for _ in 0..30 {
            let tick = session.tick_mobs(0.05, 1.0);
            assert!(tick.attacks.is_empty());
            assert!(tick.shots.is_empty());
            assert_ne!(session.mobs()[0].state, MobAiState::Chase);
        }
        assert_eq!(
            session.apply_mob_attacks(&[zombie_attack(feet + Vec3::Z)]),
            0
        );
        assert_eq!(
            session.apply_mob_shots(&[MobShot {
                kind: MobKind::Skeleton,
                origin: feet + Vec3::Y,
                dir: Vec3::Z,
            }]),
            0
        );
        assert_eq!(session.player.survival.health(), health);
        assert_eq!(session.player_hurt_intensity(), 0.0);
        assert_eq!(session.last_damage_cause(), None);

        // One-shot behaviour, also read off the live mode.
        let remaining = session.mobs()[0].health;
        let damage = attack_damage(None, session.player.mode(), remaining);
        assert_eq!(damage, remaining, "Creative resolves the remaining health");
        let hit = session
            .attack_mob(0, damage, feet)
            .expect("a resident zombie accepts the swing");
        assert!(hit.killed);
        assert_eq!(hit.health_after, 0);
        assert_eq!(
            session
                .resolve_mob_kill(0, hit, MobKillCause::Player)
                .accepted,
            1
        );
    }

    #[test]
    fn from_loaded_world_keeps_the_placeholder_registry_and_consumes_the_notice_once() {
        use crate::content::PackIdentity;
        use crate::world::persistence::{LoadedWorld, SaveContentMeta};

        let mut registry = ContentRegistry::builtin().clone();
        let raw = u16::try_from(registry.blocks().len()).expect("test table fits u16");
        registry.append_placeholder_block("mymod:ruby_ore");
        let loaded = LoadedWorld {
            data: make_save(11),
            registry,
            saved_content: Some(SaveContentMeta {
                packs: vec![PackIdentity {
                    folder: "mymod".to_string(),
                    version: Some("1.0".to_string()),
                }],
                digest: 0x1234,
            }),
            saved_config: None,
            unresolved: 1,
        };

        let mut session =
            WorldSession::from_loaded_world(loaded, PathBuf::from("/tmp/loaded.mcvx"));
        assert_eq!(
            session.registry.block_name(BlockId::Modded(raw)),
            Some("mymod:ruby_ore"),
            "the placeholder-augmented registry is installed on the session"
        );
        let notice = session
            .take_content_notice()
            .expect("a changed stack yields a readable notice");
        assert!(notice.contains("missing pack `mymod`"), "{notice}");
        assert!(
            notice.contains("1 saved content name is unavailable and render as placeholders"),
            "{notice}"
        );
        assert_eq!(
            session.take_content_notice(),
            None,
            "the notice is one-shot"
        );
    }

    #[test]
    fn registry_constructors_freeze_the_supplied_registry() {
        let registry = Arc::new(ContentRegistry::builtin().clone());
        let mut fresh = WorldSession::new_session_with_mode_and_registry(
            7,
            PathBuf::from("/tmp/fresh-registry.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
            Arc::clone(&registry),
        );
        assert!(Arc::ptr_eq(&fresh.registry, &registry));
        assert_eq!(fresh.take_content_notice(), None);

        let at = WorldSession::new_at_with_registry(
            7,
            PathBuf::from("/tmp/at-registry.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(1.0, 20.0, 2.0),
            GameClock::new(),
            Arc::clone(&registry),
        );
        assert!(Arc::ptr_eq(&at.registry, &registry));

        let plain = WorldSession::session_from_save(
            make_save(7),
            PathBuf::from("/tmp/legacy-wrapper.mcvx"),
        );
        assert!(Arc::ptr_eq(
            &plain.registry,
            &ContentRegistry::builtin_arc()
        ));
        assert_eq!(plain.registry.digest(), crate::content::EMPTY_DIGEST);
    }

    /// A pack registry's face table must reach the manager via every
    /// registry-aware constructor, so mesh jobs resolve authored layers.
    #[test]
    fn registry_constructors_install_the_registry_face_table() {
        use crate::content::spec::{BlockEntry, BlockTextures, PackAsset};
        use crate::content::{build_registry, PackContentSpec, PackIdentity, PACK_LAYER_BASE};
        use crate::world::block::BUILTIN_BLOCK_COUNT;

        let mut spec = PackContentSpec::default();
        spec.assets.insert(
            "ruby.png".to_string(),
            PackAsset {
                rgba: vec![0x5A; 1024],
                source_side: 16,
            },
        );
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_block".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ruby.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.5,
            drop: None,
            tool: None,
            stack_limit: 64,
        });
        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "mymod".to_string(),
                    version: Some("1.0".to_string()),
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        let registry = Arc::new(registry);
        let modded = BlockId::Modded(BUILTIN_BLOCK_COUNT);

        let fresh = WorldSession::new_session_with_mode_and_registry(
            7,
            PathBuf::from("/tmp/fresh-faces.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
            Arc::clone(&registry),
        );
        for face in 0u8..6 {
            assert_eq!(
                fresh.world.block_faces().layer(modded, face),
                PACK_LAYER_BASE,
                "fresh session face {face} must use the pack layer"
            );
        }

        let at = WorldSession::new_at_with_registry(
            7,
            PathBuf::from("/tmp/at-faces.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(1.0, 20.0, 2.0),
            GameClock::new(),
            registry,
        );
        for face in 0u8..6 {
            assert_eq!(at.world.block_faces().layer(modded, face), PACK_LAYER_BASE);
        }
    }

    /// T02: every registry-aware constructor carries the resolved worldgen
    /// rules into the manager, so a pack's authored ore reaches generated
    /// terrain on fresh, editor, save-load and pack-load paths.
    #[test]
    fn registry_constructors_install_the_registry_worldgen_rules() {
        use crate::content::spec::{WorldgenEntry, WorldgenKind};
        use crate::content::{build_registry, PackContentSpec, PackIdentity};
        use crate::world::persistence::LoadedWorld;

        let mut spec = PackContentSpec::default();
        spec.worldgen.push(WorldgenEntry {
            kind: WorldgenKind::Ore {
                block: "brick".to_string(),
                replace: "stone".to_string(),
                min_y: 4,
                max_y: 40,
                vein: 4,
                attempts: 8,
            },
        });
        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "mymod".to_string(),
                    version: Some("1.0".to_string()),
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        let expected = registry.worldgen_rules().to_vec();
        assert_eq!(expected.len(), 1, "the pack must contribute one rule");
        let registry = Arc::new(registry);

        let fresh = WorldSession::new_session_with_mode_and_registry(
            7,
            PathBuf::from("/tmp/fresh-rules.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
            Arc::clone(&registry),
        );
        assert_eq!(
            (*fresh.world.worldgen_rules()).as_ref(),
            expected.as_slice(),
            "fresh session must generate with the pack rules"
        );

        let at = WorldSession::new_at_with_registry(
            7,
            PathBuf::from("/tmp/at-rules.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            Vec3::new(1.0, 20.0, 2.0),
            GameClock::new(),
            Arc::clone(&registry),
        );
        assert_eq!(
            (*at.world.worldgen_rules()).as_ref(),
            expected.as_slice(),
            "editor session must generate with the pack rules"
        );

        let restored = WorldSession::session_from_save_with_registry(
            make_save(7),
            PathBuf::from("/tmp/save-rules.mcvx"),
            Arc::clone(&registry),
            None,
            None,
        );
        assert_eq!(
            (*restored.world.worldgen_rules()).as_ref(),
            expected.as_slice(),
            "save-load session must generate new chunks with the pack rules"
        );

        let loaded = WorldSession::from_loaded_world(
            LoadedWorld {
                data: make_save(7),
                registry: (*registry).clone(),
                saved_content: None,
                saved_config: None,
                unresolved: 0,
            },
            PathBuf::from("/tmp/loaded-rules.mcvx"),
        );
        assert_eq!(
            (*loaded.world.worldgen_rules()).as_ref(),
            expected.as_slice(),
            "pack-present load must generate new chunks with the pack rules"
        );

        let empty = WorldSession::new_session_with_mode_and_registry(
            7,
            PathBuf::from("/tmp/empty-rules.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
            ContentRegistry::builtin_arc(),
        );
        assert!(
            empty.world.worldgen_rules().is_empty(),
            "a built-in registry must install zero rules"
        );
    }

    /// The save-load constructor builds its table from the placeholder-
    /// augmented registry S03 produces, so an unresolved name renders as the
    /// reserved checker and a known pack block keeps its authored layer.
    #[test]
    fn save_load_install_a_placeholder_augmented_face_table() {
        use crate::world::persistence::LoadedWorld;

        let mut registry = ContentRegistry::builtin().clone();
        let placeholder = registry.append_placeholder_block("mymod:missing");
        let raw = u16::try_from(registry.blocks().len()).expect("test table fits u16");
        let loaded = LoadedWorld {
            data: make_save(7),
            registry,
            saved_content: None,
            saved_config: None,
            unresolved: 0,
        };
        let session =
            WorldSession::from_loaded_world(loaded, PathBuf::from("/tmp/faces-load.mcvx"));
        assert_eq!(
            session.world.block_faces().layer(placeholder, 0),
            crate::content::PLACEHOLDER_BLOCK_LAYER
        );
        // The next free raw id was never appended, so it stays unknown.
        assert_eq!(
            session.world.block_faces().layer(BlockId::Modded(raw), 0),
            crate::content::PLACEHOLDER_BLOCK_LAYER
        );
    }
}
