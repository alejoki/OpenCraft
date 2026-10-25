//! The replaceable active world session: the CPU-side world state
//! (`WorldManager` + `Player` + `Camera`) plus the save path every save
//! site writes to. Holding the camera inside the session lets the swap
//! atomically replace view state too. This module is pure lib code — no
//! I/O, no winit/wgpu — so the session-boundary contracts are unit-testable
//! without a window or GPU (D008 layering: S03-T02 wires this into
//! GameApp the way S01's lifecycle contracts were wired in S02).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use glam::Vec3;

use crate::player::camera::Camera;
use crate::player::physics::Player;
use crate::world::daycycle::GameClock;
use crate::world::manager::WorldManager;
use crate::world::persistence::{ChunkEditMap, WorldSaveData};
use crate::world::terrain::terrain_height_at_seeded;

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
}

impl WorldSession {
    /// A fresh world, exactly mirroring main()'s fresh-world branch:
    /// render distance 16, the given seed, no chunk edits, and the player
    /// spawned 4 blocks above the seeded terrain at column (0, 0).
    pub fn new_session(seed: u32, save_path: PathBuf) -> WorldSession {
        let spawn_y = (terrain_height_at_seeded(0, 0, seed) + 4) as f32;
        let mut session = WorldSession {
            world: WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            player: Player::new(Vec3::new(0.5, spawn_y, 0.5)),
            camera: Camera::new(Vec3::ZERO),
            clock: GameClock::new(),
            save_path,
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
    fn new_session_spawns_player_on_the_seeded_terrain() {
        let seed = 12345u32;
        let session = WorldSession::new_session(seed, PathBuf::from("/tmp/fresh.mcvx"));
        let expected_y = (terrain_height_at_seeded(0, 0, seed) + 4) as f32;
        assert_eq!(session.player.position, Vec3::new(0.5, expected_y, 0.5));
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
}
