//! The replaceable active world session: the CPU-side world state
//! (`WorldManager` + `Player` + `Camera`) plus the save path every save
//! site writes to. Holding the camera inside the session lets the swap
//! atomically replace view state too. This module is pure lib code — no
//! I/O, no winit/wgpu — so the session-boundary contracts are unit-testable
//! without a window or GPU (D008 layering: S03-T02 wires this into
//! GameApp the way S01's lifecycle contracts were wired in S02).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use glam::{IVec3, Vec3};

use crate::mesh::quad::ChunkMesh;
use crate::player::camera::Camera;
use crate::player::container::Container;
use crate::player::inventory::{
    ChestSlots, InventoryError, Item, ItemId, Material, ToolKind, CHEST_SLOTS,
};
use crate::player::melee::MeleeHit;
use crate::player::physics::Player;
use crate::player::raycast::raycast_voxels;
use crate::player::survival::{
    GameMode, SurvivalContext, SurvivalRuntime, SurvivalState, SurvivalTick,
};
use crate::world::block::BlockId;
use crate::world::chunk::{world_to_chunk_and_local, ChunkPos};
use crate::world::daycycle::GameClock;
use crate::world::farming::CropRuntime;
use crate::world::loot::{wheat_harvest, LootEntry};
use crate::world::manager::WorldManager;
use crate::world::mob::{Mob, MobHit, MobRuntime, MobTick};
use crate::world::persistence::{ChestMap, ChunkEditMap, WorldSaveData};
use crate::world::season::{season_of, Season};
use crate::world::terrain::{find_spawn_column, ruin_loot_chest_at, terrain_height_at_seeded};

/// Maximum eye-to-block distance (blocks) at which a placed crafting table is
/// reachable. Shared with the placement/mining ray length so every world
/// interaction uses one reach budget.
pub const TABLE_INTERACT_REACH: f32 = 8.0;

/// The active world session GameApp holds (S03-T02). One always-present
/// session is swapped atomically at the `start_world_session` boundary.
pub struct WorldSession {
    pub world: WorldManager,
    pub player: Player,
    pub camera: Camera,
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
    /// Fixed-rate 20 Hz mob driver, seeded from the session's world seed.
    /// Ephemeral like `survival_runtime`.
    mob_runtime: MobRuntime,
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
        let mut player = Player::new_with_mode(Self::spawn_position(seed), mode);
        if mode == GameMode::Survival {
            // The deterministic spawn is four blocks above the surface, so the
            // first dry landing is discarded instead of costing health while
            // the spawn chunks stream in.
            player.arm_spawn_immunity();
        }
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            player,
            camera: Camera::new(Vec3::ZERO),
            clock: GameClock::new(),
            season_length_days,
            save_mode: mode,
            chests: ChestMap::default(),
            save_path,
            open_table: None,
            open_chest: None,
            survival_runtime: SurvivalRuntime::default(),
            crops: CropRuntime::default(),
            spawn_settle: mode == GameMode::Survival,
            mobs: Vec::new(),
            mob_runtime: MobRuntime::new(seed),
        };
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
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            player: Player::new(position),
            camera: Camera::new(Vec3::ZERO),
            clock,
            season_length_days,
            // Edit Background is a Creative-rules editor; a stored world mode
            // only enters through `session_from_save`.
            save_mode: GameMode::Creative,
            chests: ChestMap::default(),
            save_path,
            open_table: None,
            open_chest: None,
            survival_runtime: SurvivalRuntime::default(),
            crops: CropRuntime::default(),
            // Edit Background is a Creative-rules editor and is never gated.
            spawn_settle: false,
            mobs: Vec::new(),
            mob_runtime: MobRuntime::new(seed),
        };
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
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(
                saved.render_distance as i32,
                seed,
                chunk_edits,
            ),
            player,
            camera: Camera::new(Vec3::ZERO),
            clock,
            season_length_days,
            save_mode: mode,
            chests,
            save_path,
            open_table: None,
            open_chest: None,
            survival_runtime: SurvivalRuntime::default(),
            crops,
            // A restored player is never spawn-gated: the visible world state
            // is the save's, and a mid-air fall must still hurt.
            spawn_settle: false,
            mobs,
            // The fixed-rate driver deliberately restarts at tick 0: a reload
            // is a deterministic restart of the AI clock, not a continuation
            // of the pre-save tick index.
            mob_runtime: MobRuntime::new(seed),
        };
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
        self.survival_runtime
            .tick(&mut self.player.survival, ctx, game_dt)
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

    /// Removes every mob whose death collapse has finished, returning how
    /// many records were dropped.
    ///
    /// This is the one removal site for mob records. A record is reaped only
    /// when it is dead (`health == 0`) and its collapse timer has fully
    /// drained ([`Mob::death_finished`]). Because [`Mob::death`] decays only
    /// while the mob's feet chunk is resident, a mid-fade corpse in an
    /// unloaded chunk freezes and can legally be saved as a health-0 record;
    /// it vanishes on the first tick after its chunk reloads. Drops are
    /// granted by the killer, never by the reaper.
    pub fn retain_live_mobs(&mut self) -> usize {
        let before = self.mobs.len();
        self.mobs.retain(|mob| !mob.death_finished());
        before - self.mobs.len()
    }

    /// Whether the chunk containing the mob's feet is resident. A mob in an
    /// unloaded chunk stays in the list but is frozen until it streams in.
    pub fn mob_is_resident(&self, mob: &Mob) -> bool {
        let (chunk, _) = world_to_chunk_and_local(
            mob.position.x.floor() as i32,
            mob.position.y.floor() as i32,
            mob.position.z.floor() as i32,
        );
        self.world.is_resident(chunk)
    }

    /// Every mob whose feet chunk is currently resident. Ordering matches
    /// [`WorldSession::mobs`], so render batch and entity list stay aligned.
    pub fn resident_mobs(&self) -> impl Iterator<Item = &Mob> + '_ {
        self.mobs.iter().filter(|mob| self.mob_is_resident(mob))
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

    /// Whether the fresh Survival spawn gate is still waiting for chunks.
    /// T05 uses this to tell a fresh `--demo-combat` session from a loaded
    /// one without exposing the mutable flag.
    #[inline]
    pub fn spawn_settle_pending(&self) -> bool {
        self.spawn_settle
    }

    /// Advances the fixed-rate 20 Hz mob runtime by one frame of game time.
    ///
    /// Exactly the [`WorldSession::tick_survival`] clamp contract: `dt` is
    /// clamped to `0..=0.1`, then scaled by `time_scale` (negative becomes
    /// zero), and non-finite/non-positive game time is a no-op. Residency is
    /// resolved per mob from the live world manager, so a mob in an unloaded
    /// chunk neither moves nor decays. Finished corpses are reaped at the end
    /// through [`WorldSession::retain_live_mobs`].
    pub fn tick_mobs(&mut self, dt: f32, time_scale: f32) -> MobTick {
        let dt = dt.clamp(0.0, 0.1);
        let game_dt = dt * time_scale.max(0.0);
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return MobTick::default();
        }

        let report = {
            let world = &self.world;
            let runtime = &mut self.mob_runtime;
            let mobs = &mut self.mobs;
            runtime.tick(game_dt, mobs, |position| {
                let (chunk, _) = world_to_chunk_and_local(
                    position.x.floor() as i32,
                    position.y.floor() as i32,
                    position.z.floor() as i32,
                );
                world.is_resident(chunk)
            })
        };
        self.retain_live_mobs();
        report
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
    pub fn respawn(&mut self) {
        self.player.position = Self::spawn_position(self.world.seed());
        self.player.velocity = Vec3::ZERO;
        self.player.on_ground = false;
        self.player.survival = SurvivalState::full();
        self.player.arm_spawn_immunity();
        self.survival_runtime.reset();
        self.spawn_settle = true;
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
    use crate::player::container::Container;
    use crate::player::inventory::{
        InventoryError, Item, ItemId, Material, ToolKind, ToolTier, CHEST_SLOTS,
    };
    use crate::player::physics::InputState;
    use crate::player::survival::{GameMode, SurvivalState, SurvivalTick, FULL_AIR};
    use crate::world::block::BlockId;
    use crate::world::mob::{MobAiState, MobKind, MOB_DEATH_SECS, MOB_TICK_SECS};
    use crate::world::persistence::SavedMob;
    use crate::world::season::DEFAULT_SEASON_LENGTH_DAYS;

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
                    bytes.push(stack.block as u8);
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
    fn empty_mob_save_keeps_the_153_byte_edit_free_length() {
        use crate::world::persistence::{load_world_from_bytes, save_world_to_bytes};

        let data = make_save(9183u32);
        assert!(data.mobs.is_empty());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("empty mob save encodes");

        assert_eq!(bytes.len(), 153);
        assert_eq!(
            &bytes[149..153],
            &0u32.to_le_bytes(),
            "the v7 mob count sits at offset 149"
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
}
