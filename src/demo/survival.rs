//! Scripted survival demo seam (`--demo-survival`).
//!
//! A pure move out of the bin crate (S09): the stage machine that proves fall
//! damage, eating, drowning, starvation, death/respawn and the survival save.
//! `scripts/survival_smoke.sh` runs this seam through the real binary and pins
//! its digest line.

use std::time::Instant;

use glam::Vec3;

use crate::commands::save_current_world;
use crate::demo::swim::SwimRoute;
use crate::player::inventory::{Inventory, Item, ItemId, Material, HOTBAR_SLOTS};
use crate::player::physics::InputState;
use crate::player::survival::{GameMode, SurvivalState, FULL_AIR, MAX_HUNGER};
use crate::world::block::BlockId;
use crate::world::chunk::world_to_chunk_and_local;
use crate::world::persistence::load_world_from_path;
use crate::world::terrain::SEA_LEVEL;
use crate::WorldSession;

// ---------------------------------------------------------------------------
// Scripted survival demo (`--demo-survival`, S08-T02)
// ---------------------------------------------------------------------------

/// Wall-clock safety net for the scripted survival loop. Every stage is also
/// bounded by its own 200-update loop, so this only catches a run whose chunks
/// never stream in (the settle/drown waits).
pub const SURVIVAL_DEMO_DEADLINE_SECS: u64 = 120;

/// The scripted survival loop's fixed stage order. Each stage is fed once per
/// `about_to_wait` call; the wait stages (settle/drown) return `false` until
/// their world gate is satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurvivalStage {
    Settle,
    Fall,
    Eat,
    Drown,
    Starve,
    Save,
    Done,
}

/// The `--demo-survival` seam's whole state: the pinned seed, the resolved
/// ocean route, the stage cursor, the one latched digest, and the observed
/// numbers the success digest reports.
#[derive(Debug)]
pub struct SurvivalDemo {
    pub seed: u32,
    pub route: Option<SwimRoute>,
    pub stage: SurvivalStage,
    /// The digest latched when the run ends (success or abort). Printed by the
    /// demo-shutdown block with the other bounded-demo diagnostics.
    pub digest: Option<String>,
    /// Whether the latched outcome was an abort. `main` turns this into a
    /// non-zero process status.
    pub failed: bool,
    /// Wall-clock safety net, armed only for a direct-to-gameplay launch.
    pub deadline: Option<Instant>,
    /// The applied fall damage the demo proved (4 for the pinned 7-block
    /// drop).
    pub fall_damage: u16,
    /// The hunger the apple restored (4).
    pub eat_restored: u16,
}

impl SurvivalDemo {
    pub fn new(seed: u32, route: Option<SwimRoute>, deadline: Option<Instant>) -> Self {
        Self {
            seed,
            route,
            stage: SurvivalStage::Settle,
            digest: None,
            failed: false,
            deadline,
            fall_damage: 0,
            eat_restored: 0,
        }
    }

    pub fn finished(&self) -> bool {
        self.digest.is_some()
    }

    /// Latch a self-describing abort. Idempotent: the first outcome wins.
    pub fn abort(&mut self, stage: &'static str, observed: &str) {
        if self.finished() {
            return;
        }
        self.digest = Some(format!(
            "survival_demo: seed={} stage={} observed={} ok=false",
            self.seed, stage, observed
        ));
        self.failed = true;
    }

    /// The exact success digest printed once on the demo-shutdown path.
    pub fn success_digest(&self) -> String {
        let full = SurvivalState::full();
        format!(
            "survival_demo: seed={} fall=true fall_damage={} eat=true eat_restored={} drown=true air_empty=true starve=true death=true respawn=true inventory_kept=true mode=survival saved=true health={} hunger={} air={} ok=true",
            self.seed,
            self.fall_damage,
            self.eat_restored,
            full.health(),
            full.hunger(),
            full.air(),
        )
    }
}

/// Total carried count of one identity across the 36 durable inventory slots.
pub fn inventory_item_count(inventory: &Inventory, id: ItemId) -> u16 {
    inventory
        .slots()
        .iter()
        .flatten()
        .filter(|item| item.id() == id)
        .map(|item| item.count())
        .fold(0u16, u16::saturating_add)
}

/// The first solid block below the deterministic spawn column, scanning the
/// same bounded band the fall stage uses. `None` when the spawn ground is not
/// resident or not solid, so the caller aborts with a self-describing digest.
pub fn spawn_ground_y(session: &WorldSession) -> Option<i32> {
    let spawn = WorldSession::spawn_position(session.world.seed());
    let spawn_x = spawn.x.floor() as i32;
    let spawn_z = spawn.z.floor() as i32;
    let spawn_y = spawn.y.floor() as i32;
    let top = spawn_y + 2;
    let bottom = spawn_y - 8;
    (bottom..=top)
        .rev()
        .find(|&y| session.world.get_block(spawn_x, y, spawn_z).is_solid())
}

/// Drive one Playing frame of the scripted survival loop. Returns true when
/// this call ended the run (success or abort).
///
/// Every stage is bounded: the fall and starvation/drowning simulations are
/// capped at 200 updates, the wait stages are bounded by the wall-clock
/// deadline, and every failure latches an abort digest instead of panicking on
/// live-world variance.
pub fn survival_demo_step(demo: &mut SurvivalDemo, session: &mut WorldSession) -> bool {
    if demo.finished() {
        return true;
    }
    if let Some(deadline) = demo.deadline {
        if Instant::now() >= deadline {
            demo.abort(
                "deadline",
                &format!("bounded survival run exceeded {SURVIVAL_DEMO_DEADLINE_SECS}s"),
            );
            return true;
        }
    }
    let input = InputState::default();
    match demo.stage {
        SurvivalStage::Settle => {
            if !session.settle_spawn() {
                return false;
            }
            demo.stage = SurvivalStage::Fall;
            false
        }
        SurvivalStage::Fall => {
            let spawn = WorldSession::spawn_position(session.world.seed());
            if !session.player.on_ground {
                let mut landed = false;
                for _ in 0..200 {
                    session.player.update(1.0 / 60.0, &input, |x, y, z| {
                        session.world.get_block(x, y, z)
                    });
                    if session.player.on_ground {
                        landed = true;
                        break;
                    }
                }
                if !landed {
                    demo.abort(
                        "fall",
                        &format!(
                            "never landed on the spawn column; y={:.3}",
                            session.player.position.y
                        ),
                    );
                    return true;
                }
            }
            let Some(ground_y) = spawn_ground_y(session) else {
                demo.abort("fall", "no_ground");
                return true;
            };
            // The spawn landing above consumed the fresh-spawn immunity, so
            // this pinned 7-block drop is the first landing that can hurt.
            session.player.survival = SurvivalState::full();
            session.player.position = Vec3::new(spawn.x, ground_y as f32 + 1.0 + 7.0, spawn.z);
            session.player.velocity = Vec3::ZERO;
            session.player.on_ground = false;
            let mut landed = false;
            for _ in 0..200 {
                session.player.update(1.0 / 60.0, &input, |x, y, z| {
                    session.world.get_block(x, y, z)
                });
                if session.player.on_ground {
                    landed = true;
                    break;
                }
            }
            if !landed {
                demo.abort(
                    "fall",
                    &format!(
                        "did not land after the pinned 7-block drop; y={:.3} ground_y={ground_y}",
                        session.player.position.y
                    ),
                );
                return true;
            }
            session.tick_survival(0.1, 1.0, false);
            let health = session.player.survival.health();
            if health != 16 {
                demo.abort(
                    "fall",
                    &format!("health={health} after a 7-block drop (expected 16)"),
                );
                return true;
            }
            demo.fall_damage = 4;
            demo.stage = SurvivalStage::Eat;
            false
        }
        SurvivalStage::Eat => {
            let health = session.player.survival.health();
            let Some(mut vitals) = SurvivalState::new(health, MAX_HUNGER, FULL_AIR) else {
                demo.abort("eat", "could_not_construct_checked_vitals");
                return true;
            };
            let spent = vitals.spend_hunger(6);
            if spent != 6 {
                demo.abort("eat", &format!("spent_hunger={spent} (expected 6)"));
                return true;
            }
            session.player.survival = vitals;
            let Ok(apple) = Item::new(ItemId::Material(Material::Apple), 2) else {
                demo.abort("eat", "could_not_build_apple_stack");
                return true;
            };
            let Ok(stick) = Item::new(ItemId::Material(Material::Stick), 1) else {
                demo.abort("eat", "could_not_build_stick_stack");
                return true;
            };
            if session.player.insert_items(&[apple, stick]).is_err() {
                demo.abort("eat", "inventory_refused_the_food_and_marker");
                return true;
            }
            let apple_id = ItemId::Material(Material::Apple);
            let Some(slot) = (0..HOTBAR_SLOTS).find(|&slot| {
                session
                    .player
                    .inventory()
                    .hotbar(slot)
                    .map(|item| item.id())
                    == Some(apple_id)
            }) else {
                demo.abort("eat", "apple_slot_missing_after_insert");
                return true;
            };
            if !session.player.select_slot(slot) {
                demo.abort("eat", "apple_slot_out_of_hotbar_range");
                return true;
            }
            let Some(restored) = session.player.eat_selected(slot, apple_id) else {
                demo.abort("eat", "eat_selected_refused_the_apple");
                return true;
            };
            if restored != 4 {
                demo.abort("eat", &format!("eat_restored={restored} (expected 4)"));
                return true;
            }
            let hunger = session.player.survival.hunger();
            if hunger != 18 {
                demo.abort("eat", &format!("hunger={hunger} (expected 18)"));
                return true;
            }
            let apples = inventory_item_count(session.player.inventory(), apple_id);
            if apples != 1 {
                demo.abort("eat", &format!("apples={apples} (expected 1)"));
                return true;
            }
            demo.eat_restored = restored;
            demo.stage = SurvivalStage::Drown;
            false
        }
        SurvivalStage::Drown => {
            let Some(route) = demo.route else {
                demo.abort("drown", "no_ocean_route");
                return true;
            };
            let spawn = WorldSession::spawn_position(session.world.seed());
            session.player.position = Vec3::new(
                0.5,
                SEA_LEVEL as f32 - 2.0,
                route.ocean_start_z as f32 + 0.5,
            );
            session.player.velocity = Vec3::ZERO;
            session.player.on_ground = false;
            session.sync_camera();
            let (ocean_chunk, _) = world_to_chunk_and_local(0, SEA_LEVEL, route.ocean_start_z);
            if !session.world.is_resident(ocean_chunk)
                || session.world.get_block(0, SEA_LEVEL, route.ocean_start_z) != BlockId::Water
            {
                return false;
            }
            session.player.survival.refill_air();
            let health_before = session.player.survival.health();
            let mut drowned = false;
            for _ in 0..200 {
                session.tick_survival(0.1, 10.0, true);
                if session.player.survival.air() == 0
                    && session.player.survival.health() < health_before
                {
                    drowned = true;
                    break;
                }
            }
            if !drowned {
                demo.abort(
                    "drown",
                    &format!(
                        "air={} health={} (expected empty air and damage from {health_before})",
                        session.player.survival.air(),
                        session.player.survival.health()
                    ),
                );
                return true;
            }
            let Some(ground_y) = spawn_ground_y(session) else {
                demo.abort("drown", "no_ground");
                return true;
            };
            session.player.position = Vec3::new(spawn.x, ground_y as f32 + 1.0, spawn.z);
            session.player.velocity = Vec3::ZERO;
            session.player.on_ground = true;
            session.player.survival.refill_air();
            session.sync_camera();
            demo.stage = SurvivalStage::Starve;
            false
        }
        SurvivalStage::Starve => {
            // The eaten apple leaves hunger below MAX_HUNGER; spending the
            // maximum simply empties it (the mutator reports the clamped
            // amount). The observable contract is hunger == 0.
            session.player.survival.spend_hunger(MAX_HUNGER);
            let hunger = session.player.survival.hunger();
            if hunger != 0 {
                demo.abort(
                    "starve",
                    &format!("hunger={hunger} after spending {MAX_HUNGER}"),
                );
                return true;
            }
            let mut died = false;
            for _ in 0..200 {
                session.tick_survival(0.1, 100.0, false);
                if session.is_dead() {
                    died = true;
                    break;
                }
            }
            if !died {
                demo.abort(
                    "starve",
                    &format!(
                        "health={} hunger={} (expected death within the starvation window)",
                        session.player.survival.health(),
                        session.player.survival.hunger()
                    ),
                );
                return true;
            }
            session.respawn();
            if session.player.survival != SurvivalState::full() {
                demo.abort(
                    "starve",
                    &format!(
                        "respawn left health={} hunger={} air={} (expected full vitals)",
                        session.player.survival.health(),
                        session.player.survival.hunger(),
                        session.player.survival.air()
                    ),
                );
                return true;
            }
            let stick_id = ItemId::Material(Material::Stick);
            if inventory_item_count(session.player.inventory(), stick_id) == 0 {
                demo.abort("starve", "stick_marker_missing_after_respawn");
                return true;
            }
            demo.stage = SurvivalStage::Save;
            false
        }
        SurvivalStage::Save => {
            if let Err(err) = save_current_world(session) {
                demo.abort("save", &format!("save_failed={err}"));
                return true;
            }
            let loaded = match load_world_from_path(&session.save_path) {
                Ok(loaded) => loaded,
                Err(err) => {
                    demo.abort("save", &format!("load_failed={err}"));
                    return true;
                }
            };
            if loaded.mode != GameMode::Survival {
                demo.abort(
                    "save",
                    &format!("mode={:?} (expected Survival)", loaded.mode),
                );
                return true;
            }
            if loaded.player.survival != SurvivalState::full() {
                demo.abort(
                    "save",
                    &format!(
                        "loaded health={} hunger={} air={} (expected full vitals)",
                        loaded.player.survival.health(),
                        loaded.player.survival.hunger(),
                        loaded.player.survival.air()
                    ),
                );
                return true;
            }
            if !loaded.chests.is_empty() {
                demo.abort(
                    "save",
                    &format!("chests={} (expected empty)", loaded.chests.len()),
                );
                return true;
            }
            if !loaded.chunk_edits.is_empty() {
                demo.abort(
                    "save",
                    &format!("chunk_edits={} (expected empty)", loaded.chunk_edits.len()),
                );
                return true;
            }
            let stick_id = ItemId::Material(Material::Stick);
            if inventory_item_count(&loaded.player.inventory, stick_id) == 0 {
                demo.abort("save", "stick_marker_missing_in_the_loaded_save");
                return true;
            }
            demo.digest = Some(demo.success_digest());
            demo.stage = SurvivalStage::Done;
            true
        }
        SurvivalStage::Done => true,
    }
}
