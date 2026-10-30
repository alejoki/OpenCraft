//! The persisted main-menu background scene (M012).
//!
//! Owns the demo/edit world, its game clock, and the menu camera anchor: the
//! data the menu needs to render a saved backdrop. GPU-free — rendering,
//! render-store ownership, and the temporary edit session stay in the
//! application layer (D008 layering).
//!
//! A missing background save is not an error: it means "never customized", so
//! the deterministic demo scene from [`vista`] is used and nothing is written
//! until the first successful edit. A present but unreadable file also falls
//! back to the demo scene, and the caller surfaces the error as menu data
//! (warnings-as-data, D006/MEM009) rather than blocking launch.

use std::io::ErrorKind;
use std::path::Path;

use glam::Vec3;

use crate::player::physics::PLAYER_EYE_HEIGHT;
use crate::world::daycycle::{GameClock, VISTA_GOLDEN_HOUR};
use crate::world::manager::WorldManager;
use crate::world::persistence::{
    load_world_from_path, ChunkEditMap, PersistenceError, WorldSaveData,
};
use crate::world::season::{season_of, Season, DEFAULT_SEASON_LENGTH_DAYS};
use crate::world::vista::{self, VISTA_SEED};

/// The one world state the main-menu panorama renders.
///
/// The camera anchor is the saved player's eye position, so the framing the
/// editor saw when quitting is exactly the menu framing. `persisted` records
/// whether an on-disk save backed this scene; the deterministic demo scene is
/// never persisted until the player edits it.
pub struct BackgroundScene {
    pub world: WorldManager,
    pub clock: GameClock,
    pub season_length_days: u32,
    /// Menu camera position in world space.
    pub anchor: Vec3,
    /// True when constructed from an on-disk save (or an edit session).
    pub persisted: bool,
}

/// Outcome of loading the background save at startup.
///
/// Every variant carries a usable scene: the fallback is already constructed,
/// so a caller can render immediately and decide separately whether to surface
/// an error message.
pub enum BackgroundLoad {
    /// No file exists yet: deterministic demo scene, nothing written.
    Missing(BackgroundScene),
    /// A valid save was loaded.
    Loaded(BackgroundScene),
    /// A file exists but does not decode: demo fallback plus the reason.
    Invalid {
        scene: BackgroundScene,
        error: PersistenceError,
    },
}

impl BackgroundScene {
    /// The deterministic demo scene used when no background save exists.
    ///
    /// Uses the same fixed seed, anchor, golden-hour clock, and calendar as
    /// the pre-M012 vista. Under [`VISTA_SEED`] the seed-derived starting
    /// season is [`Season::Summer`], so the calendar and the historical
    /// pinned season agree.
    pub fn new_default(render_distance: i32) -> Self {
        Self {
            world: WorldManager::with_seed_and_edits(
                render_distance,
                VISTA_SEED,
                ChunkEditMap::default(),
            ),
            clock: GameClock::from_day_and_time(0, VISTA_GOLDEN_HOUR),
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            anchor: vista::vista_anchor(),
            persisted: false,
        }
    }

    /// Rebuild a scene from decoded save data.
    ///
    /// The menu camera sits at the saved player's eye, not the saved feet
    /// position, so the panorama framing matches the editor's view.
    pub fn from_save(data: WorldSaveData, render_distance: i32) -> Self {
        let clock = GameClock::from_day_and_time(data.day_index, data.time_of_day);
        let anchor =
            Vec3::from_array(data.player.position) + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0);
        Self {
            world: WorldManager::with_seed_and_edits(render_distance, data.seed, data.chunk_edits),
            clock,
            season_length_days: data.season_length_days,
            anchor,
            persisted: true,
        }
    }

    /// Load the background save at `path`, falling back deterministically.
    ///
    /// Read-only: a missing file creates nothing and an invalid file is never
    /// repaired, rewritten, or deleted here.
    pub fn load(path: &Path, render_distance: i32) -> BackgroundLoad {
        match load_world_from_path(path) {
            Ok(data) => BackgroundLoad::Loaded(Self::from_save(data, render_distance)),
            Err(PersistenceError::Io(error)) if error.kind() == ErrorKind::NotFound => {
                BackgroundLoad::Missing(Self::new_default(render_distance))
            }
            Err(error) => BackgroundLoad::Invalid {
                scene: Self::new_default(render_distance),
                error,
            },
        }
    }

    /// The live season of this scene, derived from its persisted calendar.
    ///
    /// Mirrors [`crate::world::session::WorldSession::season`]: one calendar
    /// rule for gameplay and for the menu backdrop.
    pub fn season(&self) -> Season {
        season_of(
            self.clock.day_index,
            self.season_length_days,
            self.world.seed(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{CraftingGrid, GridKind, Inventory};
    use crate::world::chunk::{linear_index, ChunkPos};
    use crate::world::persistence::{
        load_world_from_bytes, load_world_from_path, save_world_to_bytes, save_world_to_path,
        SavedPlayerState,
    };
    use crate::world::vista::VISTA_SEASON;
    use rustc_hash::FxHashMap;

    /// One edited block in chunk (0, 0, 0), enough to prove edits survive.
    fn sample_save() -> WorldSaveData {
        let mut edits = ChunkEditMap::default();
        let mut blocks = FxHashMap::default();
        blocks.insert(
            linear_index(1, 2, 3) as u16,
            crate::world::block::BlockId::Stone,
        );
        edits.insert(ChunkPos::new(0, 0, 0), blocks);

        WorldSaveData {
            seed: 424_242,
            time_of_day: 4_321,
            day_index: 3,
            season_length_days: 2,
            player: SavedPlayerState {
                position: [-12.5, 64.25, 108.0],
                yaw: 1.25,
                pitch: -0.4,
                fly_mode: false,
                selected_slot: 4,
                render_distance: 20,
                inventory: Inventory::empty(),
                player_grid: CraftingGrid::new(GridKind::Player),
                table_grid: CraftingGrid::new(GridKind::Table),
            },
            chunk_edits: edits,
        }
    }

    /// Unique per-test path; never reuses another test's file.
    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "opencraft-background-{}-{name}.mcvx",
            std::process::id()
        ))
    }

    #[test]
    fn default_scene_matches_the_demo_contract() {
        let scene = BackgroundScene::new_default(16);
        assert_eq!(scene.world.seed(), VISTA_SEED);
        assert_eq!(scene.clock.time_of_day, VISTA_GOLDEN_HOUR);
        assert_eq!(scene.clock.day_index, 0);
        assert_eq!(scene.season_length_days, DEFAULT_SEASON_LENGTH_DAYS);
        assert_eq!(scene.anchor, vista::vista_anchor());
        assert!(!scene.persisted);
        assert_eq!(scene.season(), VISTA_SEASON);
        assert_eq!(
            scene.season(),
            season_of(0, DEFAULT_SEASON_LENGTH_DAYS, VISTA_SEED),
            "the demo calendar agrees with the historical pinned season"
        );
    }

    #[test]
    fn from_save_restores_world_clock_and_anchor() {
        let data = sample_save();
        let expected_edits = data.chunk_edits.clone();
        let expected_position = Vec3::from_array(data.player.position);
        let scene = BackgroundScene::from_save(data, 12);

        assert_eq!(scene.world.seed(), 424_242);
        assert_eq!(scene.world.chunk_edits(), &expected_edits);
        assert_eq!(scene.world.render_distance(), 12);
        assert_eq!(scene.clock.day_index, 3);
        assert_eq!(scene.clock.time_of_day, 4_321);
        assert_eq!(scene.season_length_days, 2);
        assert_eq!(
            scene.anchor,
            expected_position + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0),
            "menu framing uses the saved eye, not the saved feet"
        );
        assert!(scene.persisted);
        assert_eq!(scene.season(), season_of(3, 2, 424_242));
    }

    #[test]
    fn missing_path_falls_back_without_writing() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let load = BackgroundScene::load(&path, 16);
        let BackgroundLoad::Missing(scene) = load else {
            panic!("a missing file must be Missing, got a different variant");
        };
        assert_eq!(scene.world.seed(), VISTA_SEED);
        assert!(!scene.persisted);
        assert!(
            !path.exists(),
            "loading a missing background must not create the file"
        );
    }

    #[test]
    fn invalid_file_falls_back_and_leaves_bytes_untouched() {
        let path = temp_path("invalid");
        let junk = b"not a world".to_vec();
        std::fs::write(&path, &junk).expect("write junk fixture");

        let load = BackgroundScene::load(&path, 16);
        let BackgroundLoad::Invalid { scene, .. } = load else {
            panic!("junk bytes must be Invalid");
        };
        assert_eq!(scene.world.seed(), VISTA_SEED);
        assert!(!scene.persisted);
        assert_eq!(
            std::fs::read(&path).expect("read junk back"),
            junk,
            "the loader must not repair or rewrite an invalid background"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saved_scene_round_trips_through_the_byte_format() {
        let data = sample_save();
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("encode save bytes");
        let decoded = load_world_from_bytes(&bytes).expect("decode save bytes");
        let scene = BackgroundScene::from_save(decoded, 16);

        assert_eq!(scene.world.seed(), data.seed);
        assert_eq!(scene.world.chunk_edits(), &data.chunk_edits);
        assert_eq!(scene.clock.day_index, data.day_index);
        assert_eq!(scene.clock.time_of_day, data.time_of_day);
        assert_eq!(scene.season_length_days, data.season_length_days);
        assert_eq!(
            scene.anchor,
            Vec3::from_array(data.player.position) + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0)
        );
        assert!(scene.persisted);
    }

    #[test]
    fn valid_save_loads_as_persisted_scene() {
        let path = temp_path("valid");
        let _ = std::fs::remove_file(&path);
        save_world_to_path(&path, &sample_save()).expect("write valid save");

        let load = BackgroundScene::load(&path, 16);
        let BackgroundLoad::Loaded(scene) = load else {
            panic!("a valid save must load");
        };
        assert!(scene.persisted);
        assert_eq!(scene.world.seed(), 424_242);
        assert_eq!(scene.clock.time_of_day, 4_321);
        assert_eq!(
            scene.world.chunk_edits(),
            &load_world_from_path(&path)
                .expect("save decodes independently")
                .chunk_edits
        );

        let _ = std::fs::remove_file(&path);
    }
}
