//! Pure, GPU-free math for the main-menu vista panorama.
//!
//! The vista is a deterministic demo world rendered behind the main-menu
//! cluster. This module owns only the constants, the drift camera, and the
//! screen discriminator so all of it is headless-testable. Rendering, world
//! ownership, and streaming stay in the application layer (D008 layering).

use std::sync::OnceLock;

use glam::Vec3;

use crate::player::camera::Camera;
use crate::ui::lifecycle::{LifecycleScreen, SettingsCaller};
use crate::world::season::Season;
use crate::world::terrain::{find_spawn_column, terrain_height_at_seeded};

/// Fixed seed for the vista demo world.
///
/// Deliberately stable rather than random: every launch must show the same
/// terrain, so the vista is reproducible and its camera anchor is meaningful.
pub const VISTA_SEED: u32 = 0x5EED_4A11;

/// Season the menu panorama always renders in.
///
/// Pinned to the blanket-absent baseline rather than derived from the live
/// clock: the vista is a fixed demo world, so the menu must look identical
/// across real-time years and across saves. Never replace this with a
/// clock-derived value -- a season-keyed world manager would then reshuffle the
/// background on every menu visit.
pub const VISTA_SEASON: Season = Season::Summer;

/// Slow linear yaw drift, in radians per second. At this rate a full turn
/// takes roughly three minutes, so the panorama moves without being busy.
pub const VISTA_DRIFT_RATE_RADIANS_PER_SEC: f32 = 0.035;

/// Fixed slight downward look (negative pitch) so the horizon sits high in
/// frame behind the menu panels.
pub const VISTA_PITCH_RADIANS: f32 = -0.18;

/// Clearance, in blocks, between the seeded terrain surface and the camera.
pub const VISTA_ANCHOR_HEIGHT: f32 = 12.0;

/// The world column the vista camera is anchored over: the deterministic
/// land-search column for [`VISTA_SEED`].
///
/// D071: the vista used to sit over the fixed (0, 0) column, which is open
/// ocean for this seed after the four-biome rewrite — the menu camera would
/// have looked out from under the sea surface. The land search moves it to the
/// nearest land column above the sea while keeping the anchor a pure function
/// of the seed.
pub fn vista_anchor_column() -> (i32, i32) {
    find_spawn_column(VISTA_SEED)
}

/// The camera anchor as a plain position: the land-search column's x/z, y just
/// above the seeded demo terrain. `main.rs` derives the vista streaming anchor
/// chunk from this same function so the camera and the streamed world cannot
/// drift apart.
///
/// Cached: `main.rs` calls this once per rendered menu frame, and the land
/// search walks thousands of columns, so the result is computed once. The
/// cached value is exactly what the pure function returns, so determinism and
/// the exposed-anchor contract are unchanged.
pub fn vista_anchor() -> Vec3 {
    static ANCHOR: OnceLock<Vec3> = OnceLock::new();
    *ANCHOR.get_or_init(|| {
        let (x, z) = vista_anchor_column();
        let terrain = terrain_height_at_seeded(x, z, VISTA_SEED);
        Vec3::new(x as f32, terrain as f32 + VISTA_ANCHOR_HEIGHT, z as f32)
    })
}

/// Drifting vista camera as a pure function of elapsed time.
///
/// Yaw advances linearly from zero; position and pitch are fixed. FOV is
/// supplied by the caller so Settings remains the single source of truth for
/// FOV (same rule as `GameApp::new` / `start_world_session`) and live changes
/// reach the vista. Near/far planes keep `Camera::new` defaults.
pub fn vista_camera(elapsed_secs: f32, fov_y_radians: f32) -> Camera {
    let mut camera = Camera::new(vista_anchor());
    camera.yaw = elapsed_secs * VISTA_DRIFT_RATE_RADIANS_PER_SEC;
    camera.pitch = VISTA_PITCH_RADIANS;
    camera.fov_y_radians = fov_y_radians;
    camera
}

/// The single screen discriminator for the vista panorama.
///
/// True only for the main-menu cluster: MainMenu, NewGame, LoadGame, and
/// Settings opened from the main menu. Pause, Settings-from-pause, and Playing
/// keep showing the player's world behind their panel.
pub fn vista_visible(screen: LifecycleScreen) -> bool {
    matches!(
        screen,
        LifecycleScreen::MainMenu
            | LifecycleScreen::NewGame
            | LifecycleScreen::LoadGame
            | LifecycleScreen::Settings(SettingsCaller::MainMenu)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::lifecycle::{LifecycleScreen, SettingsCaller};

    #[test]
    fn drift_yaw_is_linear_in_elapsed_time() {
        let t0 = vista_camera(0.0, 1.0);
        assert_eq!(t0.yaw, 0.0);

        let t1 = vista_camera(3.5, 1.0);
        assert_eq!(t1.yaw, VISTA_DRIFT_RATE_RADIANS_PER_SEC * 3.5);

        let t2 = vista_camera(17.0, 1.0);
        assert_eq!(t2.yaw, VISTA_DRIFT_RATE_RADIANS_PER_SEC * 17.0);
    }

    #[test]
    fn vista_season_is_pinned_to_the_blanket_absent_baseline() {
        // The menu panorama is a fixed demo world: it must never follow the
        // player's persisted clock, so the season is a named constant pinned
        // here. Changing this intentionally also changes this assertion.
        assert_eq!(VISTA_SEASON, Season::Summer);
    }

    #[test]
    fn anchor_sits_above_land_at_the_searched_column() {
        let anchor = vista_anchor();
        let (x, z) = vista_anchor_column();
        let terrain = crate::world::terrain::terrain_height_at_seeded(x, z, VISTA_SEED);
        assert_eq!(anchor.x, x as f32);
        assert_eq!(anchor.z, z as f32);
        assert_eq!(anchor.y, terrain as f32 + VISTA_ANCHOR_HEIGHT);

        // D071: this seed's (0, 0) column is open ocean, so the anchor must
        // have moved to dry land above the water line.
        assert!(
            crate::world::terrain::terrain_height_at_seeded(0, 0, VISTA_SEED)
                < crate::world::terrain::SEA_LEVEL,
            "test premise: VISTA_SEED must have an ocean at (0, 0)"
        );
        assert_ne!((x, z), (0, 0));
        assert!(terrain >= crate::world::terrain::SPAWN_MIN_HEIGHT);
        assert_ne!(
            crate::world::terrain::biome_at_seeded(x, z, VISTA_SEED),
            crate::world::terrain::Biome::Ocean
        );
        assert!(anchor.y > crate::world::terrain::SEA_LEVEL as f32);
    }

    #[test]
    fn anchor_is_stable_across_repeat_calls() {
        // The cached anchor must equal the pure land-search result every time:
        // the menu camera may not drift while the main menu is open.
        let first = vista_anchor();
        let second = vista_anchor();
        assert_eq!(first, second);
        assert_eq!(vista_anchor_column(), vista_anchor_column());
    }

    #[test]
    fn camera_position_matches_the_exposed_anchor() {
        let camera = vista_camera(12.0, 1.0);
        assert_eq!(camera.position, vista_anchor());
    }

    #[test]
    fn pitch_is_a_fixed_downward_angle() {
        let camera = vista_camera(1.0, 1.0);
        assert_eq!(camera.pitch, VISTA_PITCH_RADIANS);
        assert!(camera.pitch < 0.0);
    }

    #[test]
    fn caller_fov_passes_through_unchanged() {
        let fov = 1.234_5_f32;
        let camera = vista_camera(5.0, fov);
        assert_eq!(camera.fov_y_radians, fov);
    }

    #[test]
    fn only_the_main_menu_cluster_shows_the_vista() {
        let truth_table = [
            (LifecycleScreen::MainMenu, true),
            (LifecycleScreen::NewGame, true),
            (LifecycleScreen::LoadGame, true),
            (LifecycleScreen::Settings(SettingsCaller::MainMenu), true),
            (LifecycleScreen::Playing, false),
            (LifecycleScreen::Pause, false),
            (LifecycleScreen::Settings(SettingsCaller::Pause), false),
        ];
        for (screen, expected) in truth_table {
            assert_eq!(vista_visible(screen), expected, "screen: {screen:?}");
        }
    }

    #[test]
    fn large_elapsed_stays_finite() {
        let camera = vista_camera(1_000_000.0, 1.0);
        assert!(camera.yaw.is_finite());
        assert!(camera.pitch.is_finite());
        assert!(camera.position.is_finite());
        assert!(camera.forward().is_finite());
    }
}
