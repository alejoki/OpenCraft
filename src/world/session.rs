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

use crate::player::camera::Camera;
use crate::player::physics::Player;
use crate::player::raycast::raycast_voxels;
use crate::world::daycycle::GameClock;
use crate::world::manager::WorldManager;
use crate::world::persistence::{ChunkEditMap, WorldSaveData};
use crate::world::terrain::{find_spawn_column, terrain_height_at_seeded};

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
    pub save_path: PathBuf,
    /// Ephemeral crafting-station state: the position of the table whose
    /// interface is open, if any. Deliberately not persisted (`SAVE_VERSION`
    /// unchanged) and reset by both constructors, so a restart always starts
    /// with no table open.
    open_table: Option<IVec3>,
}

impl WorldSession {
    /// A fresh world, exactly mirroring main()'s fresh-world branch:
    /// render distance 16, the given seed, no chunk edits, and the player
    /// spawned 4 blocks above the deterministic land-search column (D071: the
    /// old fixed (0, 0) column can be open ocean after the biome rewrite).
    pub fn new_session(seed: u32, save_path: PathBuf) -> WorldSession {
        let (spawn_x, spawn_z) = find_spawn_column(seed);
        let spawn_y = (terrain_height_at_seeded(spawn_x, spawn_z, seed) + 4) as f32;
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            player: Player::new(Vec3::new(
                spawn_x as f32 + 0.5,
                spawn_y,
                spawn_z as f32 + 0.5,
            )),
            camera: Camera::new(Vec3::ZERO),
            clock: GameClock::new(),
            save_path,
            open_table: None,
        };
        session.sync_camera();
        session
    }

    /// Reconstruct a session from persisted save data, exactly mirroring
    /// main()'s load branch: player position/yaw/pitch/fly_mode copied, the
    /// validated inventory aggregate and both crafting grids restored, and
    /// `selected_slot` clamped to the 9-slot hotbar, and the world rebuilt
    /// with the saved render distance, seed, and chunk edits.
    ///
    /// `Player::with_inventory` and `restore_selected_slot` are the only
    /// restore entry points, so a save can never publish an out-of-range
    /// selection or a grid stored under the wrong kind.
    pub fn session_from_save(data: WorldSaveData, save_path: PathBuf) -> WorldSession {
        let clock = GameClock::from_time(data.time_of_day);
        let mut player = Player::with_inventory(
            Vec3::from_array(data.player.position),
            data.player.inventory,
            data.player.player_grid,
            data.player.table_grid,
        );
        player.yaw = data.player.yaw;
        player.pitch = data.player.pitch;
        player.fly_mode = data.player.fly_mode;
        player.restore_selected_slot(data.player.selected_slot as usize);
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(
                data.player.render_distance as i32,
                data.seed,
                data.chunk_edits,
            ),
            player,
            camera: Camera::new(Vec3::ZERO),
            clock,
            save_path,
            open_table: None,
        };
        session.sync_camera();
        session
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
        if !self.table_within_reach(pos) {
            return false;
        }
        self.open_table = Some(pos);
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
            && self.table_within_reach(pos)
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

    /// Reach check against the block's axis-aligned box: the player may
    /// interact when the nearest point of the table block is within
    /// [`TABLE_INTERACT_REACH`] of the eye.
    fn table_within_reach(&self, pos: IVec3) -> bool {
        let eye = self.player.eye_position();
        let min = pos.as_vec3();
        let closest = eye.clamp(min, min + Vec3::ONE);
        eye.distance_squared(closest) <= TABLE_INTERACT_REACH * TABLE_INTERACT_REACH
    }
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

    fn make_save(seed: u32) -> WorldSaveData {
        WorldSaveData {
            seed,
            time_of_day: crate::world::daycycle::DEFAULT_TIME_OF_DAY,
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
            },
            chunk_edits: ChunkEditMap::default(),
        }
    }

    #[test]
    fn new_session_spawns_player_on_the_searched_land_column() {
        // Seed 12345's (0, 0) column is open ocean after the T03 biome rewrite,
        // so this seed proves the session really moved instead of spawning on
        // the seabed.
        let seed = 12345u32;
        let session = WorldSession::new_session(seed, PathBuf::from("/tmp/fresh.mcvx"));
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
        let session = WorldSession::new_session(seed, PathBuf::from("/tmp/fresh.mcvx"));
        assert_eq!(session.world.seed(), seed);
    }

    #[test]
    fn new_session_is_deterministic_for_the_same_seed() {
        let seed = 4242u32;
        let a = WorldSession::new_session(seed, PathBuf::from("/tmp/a.mcvx"));
        let b = WorldSession::new_session(seed, PathBuf::from("/tmp/b.mcvx"));
        assert_eq!(a.player.position, b.player.position);
        assert_eq!(a.camera.position, b.camera.position);
        assert_eq!(a.camera.yaw, b.camera.yaw);
        assert_eq!(a.camera.pitch, b.camera.pitch);
        assert_eq!(a.world.seed(), b.world.seed());
    }

    #[test]
    fn new_session_syncs_the_camera_from_the_player() {
        let session = WorldSession::new_session(9u32, PathBuf::from("/tmp/fresh.mcvx"));
        assert_eq!(session.camera.position, session.player.eye_position());
        assert_eq!(session.camera.yaw, session.player.yaw);
        assert_eq!(session.camera.pitch, session.player.pitch);
    }

    #[test]
    fn new_session_passes_the_save_path_through_unchanged() {
        let path = PathBuf::from("saves/alpha.mcvx");
        let session = WorldSession::new_session(1u32, path.clone());
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
            },
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
        let session = WorldSession::new_session(4u32, PathBuf::from("/tmp/fresh.mcvx"));
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
        let session = WorldSession::new_session(4u32, PathBuf::from("/tmp/fresh.mcvx"));
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

        let mut session = WorldSession::new_session(31u32, PathBuf::from("/tmp/table.mcvx"));
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
            WorldSession::new_session(31u32, PathBuf::from("/tmp/fresh.mcvx"))
                .open_table_position(),
            None
        );
    }
}
