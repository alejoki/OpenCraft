//! Scripted content demo seam (`--demo-content`).
//!
//! A pure move out of the bin crate (S09): the ruin-chest/farm/bread chain
//! state machine and its deterministic search helpers. `scripts/content_smoke.sh`
//! runs this seam through the real binary and pins its digest line.

use std::time::Instant;

use glam::{IVec3, Vec3};

use crate::commands::{
    break_mined_block, save_current_world, try_place_selected_block, BreakOutcome, BreakRejection,
    ToolWearOutcome,
};
use crate::demo::survival::inventory_item_count;
use crate::player::inventory::{
    CraftingGrid, GridKind, Item, ItemId, Material, RecipeId, ToolKind, ToolTier, HOTBAR_SLOTS,
};
use crate::player::physics::Player;
use crate::player::survival::{GameMode, SurvivalState};
use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, world_to_chunk_and_local, ChunkPos, CHUNK_SIZE};
use crate::world::daycycle::{GameClock, DEFAULT_TIME_OF_DAY};
use crate::world::persistence::load_world_from_path;
use crate::world::persistence::WorldSaveData;
use crate::world::season::Season;
use crate::world::terrain::{ruin_at_seeded, ruin_loot_chest_at, terrain_height_at_seeded};
use crate::WorldSession;

// ---------------------------------------------------------------------------
// Scripted content demo (`--demo-content`, S08-T03)
// ---------------------------------------------------------------------------

/// Wall-clock safety net for the scripted ruin/farm/chest chain. Every stage
/// also has its own deterministic gate, so this only catches a run whose
/// chunks never stream in.
pub const CONTENT_DEMO_DEADLINE_SECS: u64 = 240;

/// The scripted content loop's fixed stage order. Scan resolves the legal
/// ruin-chest candidates, ChestWait walks them until a live `LootChest` is
/// resident, and the remaining stages perform one production action per
/// `about_to_wait` turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentStage {
    Scan,
    ChestWait,
    Take,
    Craft,
    FarmSearch,
    TillPlant,
    Grow,
    Harvest,
    Bread,
    Place,
    Replant,
    Save,
    Done,
}

/// The `--demo-content` seam's whole state: the pinned seed, the candidate
/// chest positions, the stage cursor, the one latched digest, and the facts
/// the success digest reports.
#[derive(Debug)]
pub struct ContentDemo {
    pub seed: u32,
    pub stage: ContentStage,
    pub candidates: Vec<IVec3>,
    pub candidate_index: usize,
    pub chest_pos: Option<IVec3>,
    pub farm_targets: Vec<IVec3>,
    pub farm_index: usize,
    pub chest_dest: Option<IVec3>,
    pub loot_stacks: usize,
    pub bread_crafted: u16,
    pub digest: Option<String>,
    pub failed: bool,
    pub deadline: Option<Instant>,
}

impl ContentDemo {
    pub fn new(seed: u32, deadline: Option<Instant>) -> Self {
        Self {
            seed,
            stage: ContentStage::Scan,
            candidates: Vec::new(),
            candidate_index: 0,
            chest_pos: None,
            farm_targets: Vec::new(),
            farm_index: 0,
            chest_dest: None,
            loot_stacks: 0,
            bread_crafted: 0,
            digest: None,
            failed: false,
            deadline,
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
            "content_demo: seed={} stage={} observed={} ok=false",
            self.seed, stage, observed
        ));
        self.failed = true;
    }

    /// The exact success digest printed once on the demo-shutdown path.
    pub fn success_digest(&self) -> String {
        let chest = self.chest_pos.unwrap_or(IVec3::ZERO);
        let farm = self.farm_targets.first().copied().unwrap_or(IVec3::ZERO);
        format!(
            "content_demo: seed={} chest={},{},{} loot_stacks={} loot_taken=true chest_broken=true chest_item=1 farm={},{},{} tilled=true planted=true winter_paused=true stage3=true harvest_wheat=2 harvest_seeds=4 bread={} chest_placed=true stored_bread=true saved=true reload_chests=1 reload_stage=1 ok=true",
            self.seed,
            chest.x,
            chest.y,
            chest.z,
            self.loot_stacks,
            farm.x,
            farm.y,
            farm.z,
            self.bread_crafted,
        )
    }
}

/// Every legal ruin-chest cell in the bounded `±3`-chunk region around the
/// origin, sorted by `(x, y, z)` so two runs visit the same candidates.
///
/// The candidate rule is the production terrain seam, never a reimplementation
/// of ruin placement: `ruin_at_seeded` proves an anchor exists and
/// `ruin_loot_chest_at` proves the exact centre cell is its loot chest. A legal
/// candidate can still be suppressed in the live chunk (tree overlap, winter
/// surface, an already occupied cell), so ChestWait verifies the live block.
pub fn content_demo_chest_candidates(seed: u32) -> Vec<IVec3> {
    let span = 3 * CHUNK_SIZE as i32;
    let mut candidates = Vec::new();
    for ax in -span..=span {
        for az in -span..=span {
            let Some(spec) = ruin_at_seeded(ax, az, seed) else {
                continue;
            };
            let candidate = IVec3::new(ax, spec.surface + 1, az);
            if ruin_loot_chest_at(candidate, seed).is_some() {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort_unstable_by_key(|pos| (pos.x, pos.y, pos.z));
    candidates.dedup();
    candidates
}

/// Whether a block edit at `pos` can produce remesh work right now.
///
/// Mirrors [`WorldSession`]'s edit-first guard (the chunk plus its six face
/// neighbours must be resident) without making the private session helper
/// public. The demo uses this before every till/plant/place so a refusal is a
/// wait, never a silent world mutation.
pub fn content_demo_edit_ready(session: &WorldSession, pos: IVec3) -> bool {
    use crate::world::manager::{MAX_CHUNK_Y, MIN_CHUNK_Y};

    let (chunk, _) = world_to_chunk_and_local(pos.x, pos.y, pos.z);
    if !session.world.is_resident(chunk) {
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
        if !session
            .world
            .is_resident(ChunkPos::new(chunk.x + dx, ny, chunk.z + dz))
        {
            return false;
        }
    }
    true
}

/// Deterministic ring search for farmable soil around the ruin chest.
///
/// For each column on rings 4..=24, sample the production terrain height and
/// scan `h-1..=h+3` for a live `Grass` block with live `Air` directly above.
/// Residency is deliberately not required here: TillPlant waits for the edit
/// guard, so a search result that is not resident yet is a wait, not a loss.
pub fn content_demo_find_farm_targets(
    session: &WorldSession,
    chest: IVec3,
    need: usize,
) -> Vec<IVec3> {
    let seed = session.world.seed();
    let mut targets = Vec::with_capacity(need);
    'rings: for radius in 4i32..=24 {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs() != radius && dz.abs() != radius {
                    continue;
                }
                let x = chest.x + dx;
                let z = chest.z + dz;
                let h = terrain_height_at_seeded(x, z, seed);
                for y in (h - 1)..=(h + 3) {
                    let pos = IVec3::new(x, y, z);
                    if pos == chest {
                        continue;
                    }
                    if session.world.get_block(x, y, z) == BlockId::Grass
                        && session.world.get_block(x, y + 1, z) == BlockId::Air
                    {
                        targets.push(pos);
                        if targets.len() >= need {
                            break 'rings;
                        }
                    }
                }
            }
        }
    }
    targets
}

/// Moves every non-empty slot of the open chest into the inventory through the
/// production click seams, returning the number of loot stacks moved.
pub fn content_demo_take_all(session: &mut WorldSession) -> Option<usize> {
    let pos = session.open_chest_position()?;
    let occupied: Vec<usize> = {
        let record = session.chests.get(&pos)?;
        (0..record.slots().len())
            .filter(|&index| record.slot(index).is_some())
            .collect()
    };
    let count = occupied.len();
    for index in occupied {
        session.click_open_chest_slot(index).ok()?;
        let empty = (0..session.player.inventory().slots().len())
            .find(|&slot| session.player.inventory().slot(slot).is_none())?;
        session.player.click_inventory_slot(empty).ok()?;
    }
    if session.player.inventory().cursor().is_some() {
        return None;
    }
    if !session
        .chests
        .get(&pos)
        .map(|record| record.is_empty())
        .unwrap_or(false)
    {
        return None;
    }
    Some(count)
}

/// Makes `id` the selected hotbar item, moving it out of main storage when
/// needed.
///
/// A main-storage stack needs a free hotbar slot. When the hotbar is full, the
/// first hotbar stack is moved into the first empty main slot to free one; a
/// full inventory (no empty main slot) refuses with the cursor untouched.
pub fn content_demo_ensure_selected(player: &mut Player, id: ItemId) -> bool {
    let Some(slot) = (0..player.inventory().slots().len())
        .find(|&slot| player.inventory().slot(slot).map(|item| item.id()) == Some(id))
    else {
        return false;
    };
    if slot < HOTBAR_SLOTS {
        return player.select_slot(slot);
    }

    let hotbar = match (0..HOTBAR_SLOTS).find(|&h| player.inventory().hotbar(h).is_none()) {
        Some(empty) => empty,
        None => {
            let Some(empty_main) = (HOTBAR_SLOTS..player.inventory().slots().len())
                .find(|&main| player.inventory().slot(main).is_none())
            else {
                return false;
            };
            let Some(victim) =
                (0..HOTBAR_SLOTS).find(|&hotbar| player.inventory().hotbar(hotbar).is_some())
            else {
                return false;
            };
            if player.click_inventory_slot(victim).is_err() {
                return false;
            }
            if player.click_inventory_slot(empty_main).is_err() {
                // Put the stack back so a refused move never strands the
                // cursor; the caller only cares that nothing was selected.
                let _ = player.click_inventory_slot(victim);
                return false;
            }
            victim
        }
    };

    if player.click_inventory_slot(slot).is_err() {
        return false;
    }
    if player.click_inventory_slot(hotbar).is_err() {
        // Return the item to its original main slot rather than leaving it on
        // the cursor for later gameplay.
        let _ = player.click_inventory_slot(slot);
        return false;
    }
    player.inventory().cursor().is_none() && player.select_slot(hotbar)
}

/// Crafts one item through the real 4x4 table grid, preserving player state
/// around the [`Player::with_inventory`] replacement.
///
/// Every ingredient is consumed from a clone first, the recipe preview must
/// match `expected`, and the output is inserted by the production
/// `take_craft_result` seam. A refusal before the install leaves the live
/// session untouched; the install and camera sync happen only after the craft
/// succeeded.
pub fn content_demo_craft(
    session: &mut WorldSession,
    cells: &[(usize, ItemId)],
    expected: RecipeId,
) -> Option<Item> {
    let mut inventory = session.player.inventory().clone();
    let mut grid = CraftingGrid::new(GridKind::Table);
    for &(cell, id) in cells {
        let slot = (0..inventory.slots().len())
            .find(|&slot| inventory.slot(slot).map(|item| item.id()) == Some(id))?;
        let item = inventory.consume_one(slot)?;
        grid.set(cell, Some(item)).ok()?;
    }
    if grid.preview()?.recipe != expected {
        return None;
    }

    let position = session.player.position;
    let yaw = session.player.yaw;
    let pitch = session.player.pitch;
    let mode = session.player.mode();
    let survival = session.player.survival;
    let fly_mode = session.player.fly_mode;
    let selected = session.player.selected_slot();
    let player_grid = session.player.player_grid().clone();

    let mut player = Player::with_inventory(position, inventory, player_grid, grid);
    player.yaw = yaw;
    player.pitch = pitch;
    player.fly_mode = fly_mode;
    player.survival = survival;
    player.set_mode(mode);
    player.restore_selected_slot(selected);
    let item = player.take_craft_result(GridKind::Table).ok()?;

    session.player = player;
    session.sync_camera();
    Some(item)
}

/// Total carried count of one identity across the 36 durable slots.
pub fn content_demo_count(player: &Player, id: ItemId) -> u16 {
    inventory_item_count(player.inventory(), id)
}

/// Put the player's feet at `feet`, look straight down, and sync the camera.
pub fn content_demo_aim(session: &mut WorldSession, feet: Vec3, pitch: f32) {
    session.player.position = feet;
    session.player.velocity = Vec3::ZERO;
    session.player.on_ground = false;
    session.player.yaw = 0.0;
    session.player.pitch = pitch;
    session.sync_camera();
}

/// The saved block at `pos`, decoded from a v6 chunk-edit map.
pub fn content_demo_saved_block(data: &WorldSaveData, pos: IVec3) -> Option<BlockId> {
    let (chunk, (lx, ly, lz)) = world_to_chunk_and_local(pos.x, pos.y, pos.z);
    data.chunk_edits
        .get(&chunk)
        .and_then(|edits| edits.get(&(linear_index(lx, ly, lz) as u16)))
        .copied()
}

/// Drive one Playing frame of the scripted ruin/farm/chest chain. Returns true
/// when this call ended the run (success or abort).
///
/// Every wait stage checks the production edit/station gates and returns
/// `false` until the world is ready; every refusal latches a self-describing
/// abort digest instead of panicking on live-world variance.
pub fn content_demo_step(demo: &mut ContentDemo, session: &mut WorldSession) -> bool {
    if demo.finished() {
        return true;
    }
    if let Some(deadline) = demo.deadline {
        if Instant::now() >= deadline {
            demo.abort(
                "deadline",
                &format!("bounded content run exceeded {CONTENT_DEMO_DEADLINE_SECS}s"),
            );
            return true;
        }
    }

    let plank_id = ItemId::Block(BlockId::OakPlanks);
    let stick_id = ItemId::Material(Material::Stick);
    let wheat_id = ItemId::Material(Material::Wheat);
    let seeds_id = ItemId::Material(Material::WheatSeeds);
    let bread_id = ItemId::Material(Material::Bread);
    let chest_id = ItemId::Block(BlockId::Chest);
    let hoe_id = ItemId::Tool {
        kind: ToolKind::Hoe,
        tier: ToolTier::Wood,
    };

    match demo.stage {
        ContentStage::Scan => {
            if demo.candidates.is_empty() {
                demo.candidates = content_demo_chest_candidates(demo.seed);
            }
            if demo.candidates.is_empty() {
                demo.abort("scan", "no_ruin_chest");
                return true;
            }
            demo.candidate_index = 0;
            demo.stage = ContentStage::ChestWait;
            false
        }
        ContentStage::ChestWait => {
            let Some(&candidate) = demo.candidates.get(demo.candidate_index) else {
                demo.abort("chest", "no_live_loot_chest");
                return true;
            };
            let (chunk, _) = world_to_chunk_and_local(candidate.x, candidate.y, candidate.z);
            if !session.world.is_resident(chunk) {
                return false;
            }
            content_demo_aim(
                session,
                Vec3::new(
                    candidate.x as f32 + 0.5,
                    candidate.y as f32 + 3.0,
                    candidate.z as f32 + 0.5,
                ),
                -std::f32::consts::FRAC_PI_2,
            );
            if session
                .world
                .get_block(candidate.x, candidate.y, candidate.z)
                == BlockId::LootChest
            {
                demo.chest_pos = Some(candidate);
                demo.stage = ContentStage::Take;
            } else {
                demo.candidate_index += 1;
                if demo.candidate_index >= demo.candidates.len() {
                    demo.abort("chest", "no_live_loot_chest");
                    return true;
                }
            }
            false
        }
        ContentStage::Take => {
            let Some(chest) = demo.chest_pos else {
                demo.abort("take", "no_chest_position");
                return true;
            };
            content_demo_aim(
                session,
                Vec3::new(
                    chest.x as f32 + 0.5,
                    chest.y as f32 + 3.0,
                    chest.z as f32 + 0.5,
                ),
                -std::f32::consts::FRAC_PI_2,
            );
            if !content_demo_edit_ready(session, chest) {
                return false;
            }
            if session.world.get_block(chest.x, chest.y, chest.z) != BlockId::LootChest
                || !session.open_chest_at(chest)
            {
                demo.abort("take", "chest_open_refused");
                return true;
            }
            let Some(loot_stacks) = content_demo_take_all(session) else {
                demo.abort("take", "take_all_failed");
                return true;
            };
            // A retry after an EditRejected break sees the already-empty
            // record; keep the stack count the first take actually moved.
            if loot_stacks > 0 {
                demo.loot_stacks = loot_stacks;
            }
            match break_mined_block(session, chest, BlockId::LootChest) {
                BreakOutcome::Applied { .. } => {}
                BreakOutcome::Rejected(BreakRejection::EditRejected) => return false,
                BreakOutcome::Rejected(reason) => {
                    demo.abort("take", &format!("break_rejected={reason:?}"));
                    return true;
                }
            }
            if session.world.get_block(chest.x, chest.y, chest.z) != BlockId::Air {
                demo.abort("take", "chest_still_present");
                return true;
            }
            if content_demo_count(&session.player, chest_id) != 1 {
                demo.abort(
                    "take",
                    &format!(
                        "chest_item={} (expected 1)",
                        content_demo_count(&session.player, chest_id)
                    ),
                );
                return true;
            }
            demo.stage = ContentStage::Craft;
            false
        }
        ContentStage::Craft => {
            let Ok(planks) = Item::new(plank_id, 2) else {
                demo.abort("craft", "could_not_build_planks");
                return true;
            };
            let Ok(sticks) = Item::new(stick_id, 2) else {
                demo.abort("craft", "could_not_build_sticks");
                return true;
            };
            if session.player.insert_items(&[planks, sticks]).is_err() {
                demo.abort("craft", "no_room_for_hoe_ingredients");
                return true;
            }
            let Some(hoe) = content_demo_craft(
                session,
                &[(0, plank_id), (1, plank_id), (5, stick_id), (9, stick_id)],
                RecipeId::WoodHoe,
            ) else {
                demo.abort("craft", "wood_hoe_craft_failed");
                return true;
            };
            if hoe.id() != hoe_id || hoe.count() != 1 {
                demo.abort(
                    "craft",
                    &format!("crafted={:?} (expected one wood hoe)", hoe.id()),
                );
                return true;
            }
            if !content_demo_ensure_selected(&mut session.player, hoe_id) {
                demo.abort("craft", "could_not_select_hoe");
                return true;
            }
            demo.stage = ContentStage::FarmSearch;
            false
        }
        ContentStage::FarmSearch => {
            let Some(chest) = demo.chest_pos else {
                demo.abort("farm", "no_chest_position");
                return true;
            };
            demo.farm_targets = content_demo_find_farm_targets(session, chest, 2);
            if demo.farm_targets.len() < 2 {
                demo.abort(
                    "farm",
                    &format!("no_grass targets={}", demo.farm_targets.len()),
                );
                return true;
            }
            demo.farm_index = 0;
            demo.stage = ContentStage::TillPlant;
            false
        }
        ContentStage::TillPlant => {
            let Some(&target) = demo.farm_targets.get(demo.farm_index) else {
                demo.abort("tillplant", "missing_target");
                return true;
            };
            let crop = target + IVec3::Y;
            if !content_demo_edit_ready(session, target) || !content_demo_edit_ready(session, crop)
            {
                return false;
            }
            content_demo_aim(
                session,
                Vec3::new(
                    target.x as f32 + 0.5,
                    target.y as f32 - 1.5,
                    target.z as f32 + 0.5,
                ),
                -std::f32::consts::FRAC_PI_2,
            );
            if !content_demo_ensure_selected(&mut session.player, hoe_id) {
                demo.abort("tillplant", "could_not_select_hoe");
                return true;
            }
            let durability_before = session
                .player
                .selected_item()
                .and_then(|item| item.durability());
            if session.till_at(target).is_none() {
                demo.abort("tillplant", "till_refused");
                return true;
            }
            if session.world.get_block(target.x, target.y, target.z) != BlockId::Farmland {
                demo.abort("tillplant", "till_did_not_produce_farmland");
                return true;
            }
            let durability_after = session
                .player
                .selected_item()
                .and_then(|item| item.durability());
            let expected_durability = durability_before.and_then(|value| value.checked_sub(1));
            if durability_before.is_none() || durability_after != expected_durability {
                demo.abort(
                    "tillplant",
                    &format!(
                        "hoe_durability before={durability_before:?} after={durability_after:?}"
                    ),
                );
                return true;
            }
            if !content_demo_ensure_selected(&mut session.player, seeds_id) {
                demo.abort("tillplant", "could_not_select_seeds");
                return true;
            }
            let seeds_before = content_demo_count(&session.player, seeds_id);
            if session.plant_wheat_at(crop).is_none() {
                demo.abort("tillplant", "plant_refused");
                return true;
            }
            if session.world.get_block(crop.x, crop.y, crop.z) != BlockId::WheatStage0 {
                demo.abort("tillplant", "plant_did_not_produce_stage0");
                return true;
            }
            let seeds_after = content_demo_count(&session.player, seeds_id);
            if seeds_after.checked_add(1) != Some(seeds_before) {
                demo.abort(
                    "tillplant",
                    &format!("seeds before={seeds_before} after={seeds_after} (expected one seed)"),
                );
                return true;
            }
            demo.farm_index += 1;
            if demo.farm_index >= demo.farm_targets.len() {
                demo.stage = ContentStage::Grow;
            }
            false
        }
        ContentStage::Grow => {
            let targets = demo.farm_targets.clone();
            session.clock = GameClock::from_day_and_time(3, DEFAULT_TIME_OF_DAY);
            if session.season() != Season::Winter {
                demo.abort(
                    "grow",
                    &format!("season={:?} (expected Winter)", session.season()),
                );
                return true;
            }
            if !session.tick_crops(0.1, 600.0).is_empty() {
                demo.abort("grow", "crops_advanced_in_winter");
                return true;
            }
            for target in &targets {
                let crop = *target + IVec3::Y;
                if session.world.get_block(crop.x, crop.y, crop.z) != BlockId::WheatStage0 {
                    demo.abort("grow", "winter_changed_a_stage0_crop");
                    return true;
                }
            }
            session.clock = GameClock::from_day_and_time(0, DEFAULT_TIME_OF_DAY);
            if session.season() != Season::Spring {
                demo.abort(
                    "grow",
                    &format!("season={:?} (expected Spring)", session.season()),
                );
                return true;
            }
            for (expected, name) in [
                (BlockId::WheatStage1, "stage1"),
                (BlockId::WheatStage2, "stage2"),
                (BlockId::WheatStage3, "stage3"),
            ] {
                if session.tick_crops(0.1, 600.0).is_empty() {
                    demo.abort("grow", &format!("no_remesh_at_{name}"));
                    return true;
                }
                for target in &targets {
                    let crop = *target + IVec3::Y;
                    let live = session.world.get_block(crop.x, crop.y, crop.z);
                    if live != expected {
                        demo.abort(
                            "grow",
                            &format!("{name}_block={live:?} (expected {expected:?})"),
                        );
                        return true;
                    }
                }
            }
            demo.stage = ContentStage::Harvest;
            false
        }
        ContentStage::Harvest => {
            let targets = demo.farm_targets.clone();
            let wheat_before = content_demo_count(&session.player, wheat_id);
            let seeds_before = content_demo_count(&session.player, seeds_id);
            for target in &targets {
                let crop = *target + IVec3::Y;
                match break_mined_block(session, crop, BlockId::WheatStage3) {
                    BreakOutcome::Applied { wear, .. } => {
                        if wear != ToolWearOutcome::Nothing {
                            demo.abort("harvest", &format!("wear={wear:?} (expected none)"));
                            return true;
                        }
                    }
                    BreakOutcome::Rejected(reason) => {
                        demo.abort("harvest", &format!("break_rejected={reason:?}"));
                        return true;
                    }
                }
                if session.world.get_block(crop.x, crop.y, crop.z) != BlockId::Air {
                    demo.abort("harvest", "crop_still_present");
                    return true;
                }
            }
            let wheat_after = content_demo_count(&session.player, wheat_id);
            let seeds_after = content_demo_count(&session.player, seeds_id);
            let wheat_delta = i32::from(wheat_after) - i32::from(wheat_before);
            let seeds_delta = i32::from(seeds_after) - i32::from(seeds_before);
            if wheat_delta != 2 || seeds_delta != 4 {
                demo.abort(
                    "harvest",
                    &format!(
                        "wheat_delta={wheat_delta} seeds_delta={seeds_delta} (expected 2 and 4)"
                    ),
                );
                return true;
            }
            demo.stage = ContentStage::Bread;
            false
        }
        ContentStage::Bread => {
            let Some(bread) = content_demo_craft(
                session,
                &[(0, wheat_id), (1, wheat_id), (2, wheat_id)],
                RecipeId::Bread,
            ) else {
                demo.abort("bread", "bread_craft_failed");
                return true;
            };
            if bread.id() != bread_id || bread.count() != 1 {
                demo.abort(
                    "bread",
                    &format!("crafted={:?} (expected one bread)", bread.id()),
                );
                return true;
            }
            demo.bread_crafted = 1;
            demo.stage = ContentStage::Place;
            false
        }
        ContentStage::Place => {
            let Some(&target) = demo.farm_targets.first() else {
                demo.abort("place", "missing_farm_target");
                return true;
            };
            let dest = target + IVec3::Y;
            content_demo_aim(
                session,
                Vec3::new(
                    target.x as f32 + 0.5,
                    target.y as f32 + 3.0,
                    target.z as f32 + 0.5,
                ),
                -std::f32::consts::FRAC_PI_2,
            );
            if !content_demo_edit_ready(session, dest) {
                return false;
            }
            if !content_demo_ensure_selected(&mut session.player, chest_id) {
                demo.abort("place", "could_not_select_chest");
                return true;
            }
            match try_place_selected_block(session) {
                Some(remeshed) if !remeshed.is_empty() => {}
                _ => {
                    demo.abort("place", "place_refused");
                    return true;
                }
            }
            if session.world.get_block(dest.x, dest.y, dest.z) != BlockId::Chest {
                demo.abort("place", "chest_not_placed_at_destination");
                return true;
            }
            if !session.open_chest_at(dest) {
                demo.abort("place", "placed_chest_open_refused");
                return true;
            }
            let Some(bread_slot) = (0..session.player.inventory().slots().len()).find(|&slot| {
                session.player.inventory().slot(slot).map(|item| item.id()) == Some(bread_id)
            }) else {
                demo.abort("place", "bread_slot_missing");
                return true;
            };
            if session.player.click_inventory_slot(bread_slot).is_err()
                || session.click_open_chest_slot(0).is_err()
            {
                demo.abort("place", "bread_transfer_refused");
                return true;
            }
            let stored = session
                .open_chest_contents()
                .and_then(|chest| chest.slot(0))
                .map(|item| item.id())
                == Some(bread_id);
            if !stored || session.player.inventory().cursor().is_some() {
                demo.abort("place", "bread_not_stored");
                return true;
            }
            demo.chest_dest = Some(dest);
            demo.stage = ContentStage::Replant;
            false
        }
        ContentStage::Replant => {
            let Some(&target) = demo.farm_targets.get(1) else {
                demo.abort("replant", "missing_farm_target");
                return true;
            };
            let crop = target + IVec3::Y;
            content_demo_aim(
                session,
                Vec3::new(
                    target.x as f32 + 0.5,
                    target.y as f32 - 1.5,
                    target.z as f32 + 0.5,
                ),
                -std::f32::consts::FRAC_PI_2,
            );
            if !content_demo_edit_ready(session, crop) {
                return false;
            }
            if !content_demo_ensure_selected(&mut session.player, seeds_id) {
                demo.abort("replant", "could_not_select_seeds");
                return true;
            }
            if session.plant_wheat_at(crop).is_none() {
                demo.abort("replant", "plant_refused");
                return true;
            }
            if session.tick_crops(0.1, 600.0).is_empty() {
                demo.abort("replant", "no_remesh_after_replant");
                return true;
            }
            let live = session.world.get_block(crop.x, crop.y, crop.z);
            if live != BlockId::WheatStage1 {
                demo.abort(
                    "replant",
                    &format!("replant_stage={live:?} (expected WheatStage1)"),
                );
                return true;
            }
            demo.stage = ContentStage::Save;
            false
        }
        ContentStage::Save => {
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
            if loaded.mode != GameMode::Creative {
                demo.abort(
                    "save",
                    &format!("mode={:?} (expected Creative)", loaded.mode),
                );
                return true;
            }
            if loaded.player.survival != SurvivalState::full() {
                demo.abort("save", "loaded vitals are not full");
                return true;
            }
            if loaded.chests.len() != 1 {
                demo.abort(
                    "save",
                    &format!("reload_chests={} (expected 1)", loaded.chests.len()),
                );
                return true;
            }
            let Some(chest_dest) = demo.chest_dest else {
                demo.abort("save", "no_chest_destination");
                return true;
            };
            let stored_bread = loaded
                .chests
                .get(&chest_dest)
                .and_then(|chest| chest.slot(0))
                .map(|item| item.id())
                == Some(bread_id);
            if !stored_bread {
                demo.abort("save", "reloaded chest slot 0 is not bread");
                return true;
            }
            let Some(&replant_target) = demo.farm_targets.get(1) else {
                demo.abort("save", "missing_replant_target");
                return true;
            };
            let replant_crop = replant_target + IVec3::Y;
            match content_demo_saved_block(&loaded, replant_crop) {
                Some(BlockId::WheatStage1) => {}
                other => {
                    demo.abort(
                        "save",
                        &format!("reload_stage={other:?} (expected WheatStage1)"),
                    );
                    return true;
                }
            }
            match content_demo_saved_block(&loaded, chest_dest) {
                Some(BlockId::Chest) => {}
                other => {
                    demo.abort(
                        "save",
                        &format!("reload_chest_edit={other:?} (expected Chest)"),
                    );
                    return true;
                }
            }
            demo.digest = Some(demo.success_digest());
            demo.stage = ContentStage::Done;
            true
        }
        ContentStage::Done => true,
    }
}
