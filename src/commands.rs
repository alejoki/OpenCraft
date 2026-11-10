//! Pure command adapters: one function per player command, returning typed
//! outcomes with warnings-as-data (D006/MEM009); no window, GPU, audio or
//! event-loop dependency.
//!
//! Every function here is a thin translation over `WorldSession`/`Player`
//! state: the caller owns input decoding and rendering, the session owns all
//! mutation. Moving them out of the bin keeps `main.rs` to the app shell and
//! lets the headless demo seams import the same adapters.

use glam::IVec3;

use crate::audio::SoundSlot;
use crate::content::ContentRegistry;
use crate::mesh::quad::ChunkMesh;
use crate::player::bow::{BowState, BowTarget};
use crate::player::eating::{food_value_with, EatingTarget};
use crate::player::inventory::{GridKind, Item, ItemId, Material, ToolKind, ToolWear};
use crate::player::melee::MeleeHit;
use crate::player::mining::MiningTarget;
use crate::player::physics::{InputState, InventoryCommandError, Player};
use crate::player::raycast::raycast_voxels;
use crate::player::survival::GameMode;
use crate::ui::chest::{ChestScreen, ChestTarget};
use crate::ui::hud::{bow_progress_quads, eating_progress_quads, mining_progress_quads};
use crate::ui::inventory::{InventoryMode, InventoryScreen, InventoryTarget};
use crate::ui::menu::{self, MenuMessage};
use crate::ui::palette::{self, PaletteScreen};
use crate::ui::Quad;
use crate::world::block::BlockId;
use crate::world::chunk::ChunkPos;
use crate::world::loot;
use crate::world::mob::MobHit;
use crate::world::mob_audio::MobAudioFrame;
use crate::world::mob_loot::{MobDropReport, MobKillCause};
use crate::world::persistence::PersistenceError;
use crate::world::session::{BowFireError, ChestCommandError, WorldSession};

/// Save the active session's world to the session's own save path
/// (S03-T02: every save site — F5 quick-save, streaming autosave on
/// unload, pause Save-and-Quit, Exit Game, CloseRequested, OutOfMemory —
/// funnels through this one session-tied function, so no site can drift
/// back to writing cli.save_path). `WorldSession::save()` owns the whole
/// write: projection to the v9 payload, the atomic file write (including
/// parent-directory creation), and clearing the unsaved-changes flag only
/// on success. A failed save returns the `PersistenceError` as data with
/// the session's unsaved-changes flag intact for the next save
/// opportunity, and the caller surfaces the error where a message slot
/// exists (no log by convention; D006/MEM009).
pub fn save_current_world(session: &mut WorldSession) -> Result<(), PersistenceError> {
    session.save()
}

/// Why a mining break produced no world edit, and therefore no wear and no
/// collected drop.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BreakRejection {
    /// The reported block is structurally unbreakable (Air, Bedrock), so the
    /// catalog refuses it regardless of what a caller claims.
    Unbreakable,
    /// The live world block at `pos` is not the reported pre-break block
    /// (already removed, replaced, or in an unloaded chunk).
    StaleTarget,
    /// The world edit returned no remesh work, so it did not apply (unloaded
    /// chunk or outside the vertical world range).
    EditRejected,
    /// A chest break's contents plus its own drop do not all fit in the 36
    /// inventory slots, so the all-or-nothing preflight refused before any
    /// edit, wear, or record change.
    ChestNoRoom,
    /// A wheat harvest's deterministic drops do not fit in the 36 inventory
    /// slots, so the all-or-nothing preflight refused before any edit, wear,
    /// or inventory change. The crop stays intact and the break is retryable.
    HarvestNoRoom,
}

/// The transient gameplay notice a rejected break shows, or `None` for
/// rejections that are already visible in the world (a stale target or an
/// unbreakable block needs no message).
pub fn break_rejection_notice(rejection: BreakRejection) -> Option<&'static str> {
    match rejection {
        BreakRejection::ChestNoRoom => Some("Not enough inventory space to break the chest"),
        BreakRejection::HarvestNoRoom => Some("Not enough inventory space to harvest the wheat"),
        BreakRejection::Unbreakable
        | BreakRejection::StaleTarget
        | BreakRejection::EditRejected => None,
    }
}

/// The transient gameplay notice a player-attributed mob kill shows when the
/// best-effort delivery had to discard part of the table, or `None` when every
/// unit fit.
///
/// A clean delivery stays silent: the items appearing in the hotbar is the
/// feedback. Only a loss needs words, because those items are gone for good.
pub fn mob_drop_notice(report: MobDropReport) -> Option<&'static str> {
    if report.lost_any() {
        Some("Inventory full: some mob drops were lost")
    } else {
        None
    }
}

/// Grants a proven player kill's drops after the hit already landed and the
/// selected tool was already worn.
///
/// The caller owns the ordering (attack -> wear -> drops -> notice): a sword
/// that breaks on the killing blow must free its hotbar slot before the drop
/// is delivered, otherwise a full inventory would lose an item the break just
/// made room for (MEM214/MEM392). This helper performs only the grant and the
/// message projection; the mob record stays for the collapse and the reaper.
pub fn after_lethal_player_hit(
    session: &mut WorldSession,
    index: usize,
    hit: MobHit,
) -> Option<MenuMessage> {
    let report = session.resolve_mob_kill(index, hit, MobKillCause::Player);
    mob_drop_notice(report).map(|text| MenuMessage {
        text: text.to_string(),
        ticks_left: menu::MESSAGE_TICKS,
    })
}

/// What an applied break did to the selected hotbar slot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ToolWearOutcome {
    /// The selected tool lost exactly one point and is still usable.
    Worn { remaining: u16 },
    /// The selected tool reached zero and its slot was cleared.
    Broke,
    /// The selected slot was empty or held a non-tool: nothing was worn.
    Nothing,
}

/// Spends exactly one durability point after an edit or hit actually applied.
///
/// One mapping serves both production mutation sites — the mining break and the
/// melee hit — so the two paths can never diverge on what a wear reports. The
/// caller owns the ordering (mutate first, wear second); this helper only
/// classifies the result.
pub fn wear_after_proven_action(session: &mut WorldSession) -> ToolWearOutcome {
    match session.player.wear_selected_tool() {
        Ok(ToolWear::Worn(item)) => ToolWearOutcome::Worn {
            remaining: item
                .durability()
                .expect("a successfully worn tool still reports durability"),
        },
        Ok(ToolWear::Broke) => ToolWearOutcome::Broke,
        // Empty slot / non-tool / out-of-range selection: no mutation.
        Err(_) => ToolWearOutcome::Nothing,
    }
}

/// Outcome of one production mining break.
///
/// `Applied` is the only arm that carries remesh work, and it is produced
/// only after the world edit proved it happened. Every other path — a stale
/// target, an unbreakable block, a malformed harvest table, an edit that
/// returned nothing, or a full-inventory preflight refusal (chest or wheat) —
/// is a rejection that left the world, the inventory, and durability
/// untouched.
///
/// Production only needs the remesh work, but the wear transition and the
/// rejection reason are the outcome contract the executable harness asserts
/// on, so they stay part of the value instead of being flattened away in the
/// loop.
#[derive(Debug)]
pub enum BreakOutcome {
    Applied {
        remeshed: Vec<(ChunkPos, ChunkMesh)>,
        wear: ToolWearOutcome,
    },
    Rejected(BreakRejection),
}

/// Applies one mining break with a fixed, testable ordering:
///
/// 1. re-read the world and refuse unbreakable/stale targets,
/// 2. for wheat, derive the deterministic harvest and preflight the whole
///    batch; for a container, preflight its contents plus its own drop, so a
///    full inventory refuses before any edit, wear, or record change,
/// 3. apply the world edit (the only authority that a break happened),
/// 4. wear the selected tool exactly once, but only after non-empty remesh
///    work proved the edit applied,
/// 5. collect the pre-break block last, so a tool that broke has already
///    freed its hotbar slot and the drop can land there.
///
/// Wheat is not a carryable block, so its drops come from
/// [`WorldSession::wheat_harvest_items`] rather than `collect_block`, and the
/// crop candidate is removed only after the proven edit. Breaking Farmland
/// forgets a floating crop candidate above it without editing that crop block.
///
/// An empty remesh result means the edit did not apply (unloaded chunk or
/// outside the vertical world range), so nothing is worn or collected —
/// mining can never duplicate items or spend durability through an edit that
/// never happened.
pub fn break_mined_block(session: &mut WorldSession, pos: IVec3, block: BlockId) -> BreakOutcome {
    // Breakability is authored content: the session registry is the authority,
    // so a pack block with authored hardness is mineable while a placeholder
    // (`hardness: None`) or an unknown id stays unbreakable.
    let Some(def) = session.registry.block(block) else {
        return BreakOutcome::Rejected(BreakRejection::Unbreakable);
    };
    if def.hardness.is_none() {
        return BreakOutcome::Rejected(BreakRejection::Unbreakable);
    }
    // `pos`/`block` come from the mining accumulator, which is not authority:
    // re-read the live world so a stale or replaced target edits nothing.
    if session.world.get_block(pos.x, pos.y, pos.z) != block {
        return BreakOutcome::Rejected(BreakRejection::StaleTarget);
    }
    // Wheat's deterministic drops are the only source of truth for a harvest,
    // and the all-or-nothing preflight runs before the edit and the wear so a
    // refusal leaves the block, inventory, and durability byte-identical and
    // the break retryable. A malformed table fails closed with no loss.
    let wheat_items = if block.is_wheat_stage() {
        let Some(items) = session.wheat_harvest_items(block) else {
            return BreakOutcome::Rejected(BreakRejection::EditRejected);
        };
        if !session.player.can_insert_items(&items) {
            return BreakOutcome::Rejected(BreakRejection::HarvestNoRoom);
        }
        Some(items)
    } else {
        None
    };
    // A chest or loot-chest break drops its whole contents plus the container
    // drop. The batch preflight is the only correct capacity test
    // (heterogeneous stacks compete for the same mergeable room), and it runs
    // before the edit, the wear, and the record change so a refusal leaves
    // every byte untouched. An unopened generated LootChest derives the same
    // contents first open would materialize, so its loot is never lost.
    // Wheat and containers are mutually exclusive classifications.
    let chest_items = if block.is_container_interactable() {
        let items = session.chest_break_items(pos);
        if !session.player.can_insert_items(&items) {
            return BreakOutcome::Rejected(BreakRejection::ChestNoRoom);
        }
        Some(items)
    } else {
        None
    };

    let remeshed = session
        .world
        .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Air);
    if remeshed.is_empty() {
        return BreakOutcome::Rejected(BreakRejection::EditRejected);
    }

    let wear = wear_after_proven_action(session);
    if let Some(items) = chest_items {
        // The preflight proved capacity, and wearing only ever frees room, so
        // this batch cannot fail. The record is removed with the block, and an
        // open station on this exact chest closes; the world edit already set
        // the unsaved flag.
        session
            .player
            .insert_items(&items)
            .expect("preflight proved capacity");
        session.chests.remove(&pos);
        session.close_chest_at(pos);
    } else if let Some(items) = wheat_items {
        // Wheat has no `collect_block` path (the stage blocks are not
        // carryable); the preflighted batch is inserted after the proven edit
        // and the crop candidate is forgotten at the same time it leaves the
        // world, so a stale candidate can never keep ticking.
        session
            .player
            .insert_items(&items)
            .expect("preflight proved capacity");
        session.remove_crop(pos);
    } else {
        // Drop mapping is authored content (Stone -> Cobblestone, a pack block
        // -> its authored item and stack limit). The item identity and limit
        // both come from the frozen session registry, and the drop is still
        // collected last so MEM195 ordering is preserved. The registry-aware
        // collector can construct a non-stackable pack tool drop.
        if let Some((id, limit)) = session.registry.drop_entry(block) {
            session
                .player
                .collect_limited_with(&session.registry, id, 1, limit);
        }

        // Breaking the support ends the crop's eligibility even though the
        // floating wheat block itself is not edited: the candidate is
        // forgotten so it can never advance without Farmland below.
        if block == BlockId::Farmland {
            session.remove_crop(pos + IVec3::Y);
        }

        // Oak leaves carry a deterministic secondary apple drop, resolved
        // here because `BlockId::mined_drop` returns a single identity. The
        // leaf drop was collected first, so the bonus can never displace it.
        // The bonus is best-effort: M008 has no item entities or ground spill,
        // so a full inventory simply loses the apple.
        if block == BlockId::OakLeaves && loot::oak_leaves_apple(session.world.seed(), pos) {
            let apple = Item::new(ItemId::Material(Material::Apple), 1)
                .expect("an apple is a valid carryable item");
            session.player.collect_item(apple);
        }
    }

    BreakOutcome::Applied { remeshed, wear }
}

/// Places the selected hotbar stack with the game's right-click semantics:
/// raycast 8 blocks from the camera, reject a placement that overlaps the
/// player, place the stack's block into the hit cell's adjacent cell, and
/// consume exactly one item only after the world edit actually applied.
///
/// Returns the remeshed chunks for the caller to upload, or `None` when the
/// ray misses, the cell is inside the player, the selected slot is empty, or
/// the edit did not apply (unloaded/out-of-range).
pub fn try_place_selected_block(session: &mut WorldSession) -> Option<Vec<(ChunkPos, ChunkMesh)>> {
    let hit = raycast_voxels(
        session.camera.position,
        session.camera.forward(),
        8.0,
        |x, y, z| session.world.get_block(x, y, z),
    )?;
    let ap = hit.adjacent_pos;
    if session.player.aabb().intersects_block(ap.x, ap.y, ap.z) {
        return None;
    }
    let block = session.player.selected_block()?;
    let remeshed = session.world.set_block_immediate(ap.x, ap.y, ap.z, block);
    if remeshed.is_empty() {
        return None;
    }
    // A fresh chest must never inherit contents a removed chest left behind at
    // this position. Only after the edit actually applied, so a rejected
    // placement cannot erase a live record.
    if block == BlockId::Chest {
        session.chests.remove(&ap);
    }
    session.player.consume_selected();
    Some(remeshed)
}

/// Places one socket marker on the looked-at block face as a Creative-only
/// authoring action.
///
/// The marker is deliberately non-carryable, so this never reads or consumes
/// the selected stack: it raycasts 8 blocks from the camera, refuses a cell
/// that overlaps the player AABB, and writes [`BlockId::SocketMarker`] through
/// `set_block_immediate`. Returns the remeshed chunks for the caller to
/// upload, or `None` when the mode is not Creative, the ray misses, the cell
/// overlaps the player, or the edit did not apply (unloaded/out-of-range).
pub fn try_place_socket_marker(session: &mut WorldSession) -> Option<Vec<(ChunkPos, ChunkMesh)>> {
    if session.player.mode() != GameMode::Creative {
        return None;
    }
    let hit = raycast_voxels(
        session.camera.position,
        session.camera.forward(),
        8.0,
        |x, y, z| session.world.get_block(x, y, z),
    )?;
    let ap = hit.adjacent_pos;
    if session.player.aabb().intersects_block(ap.x, ap.y, ap.z) {
        return None;
    }
    let remeshed = session
        .world
        .set_block_immediate(ap.x, ap.y, ap.z, BlockId::SocketMarker);
    if remeshed.is_empty() {
        return None;
    }
    Some(remeshed)
}

/// One prefab-capture corner: the targeted block, not the adjacent cell.
///
/// Creative-only, exactly like the socket-marker authoring key; Survival and a
/// ray miss are silent because there is no capture surface to advance. The
/// returned position is the block the crosshair is on, so a selection cage
/// encloses the built structure rather than the air in front of it.
pub fn try_capture_corner(session: &WorldSession) -> Option<IVec3> {
    if session.player.mode() != GameMode::Creative {
        return None;
    }
    let hit = raycast_voxels(
        session.camera.position,
        session.camera.forward(),
        8.0,
        |x, y, z| session.world.get_block(x, y, z),
    )?;
    Some(hit.block_pos)
}

/// Outcome of one production right-click world interaction.
///
/// The full precedence is chest/table -> held bow -> Survival food -> hoe/seed
/// farming -> block placement. Stations and Survival food run before everything else and
/// are pure session decisions: a successfully targeted storage block or
/// crafting table neither edits the world nor consumes the selected stack, and
/// a food item never places a block. Farming follows because the top-face rule
/// needs the raycast normal, which only this layer sees; a refused farm action
/// falls through to placement, which is a no-op for tools and materials, so one
/// click can never edit twice. Farming is mode-independent (Creative farms
/// too). Anything else falls through to the pre-existing placement path, so
/// non-station clicks stay byte-compatible with the placement-only behavior.
///
/// Both station syncs run at the interaction boundary so a stale open station
/// (removed/replaced or out of reach) is closed before the click is
/// interpreted — replaced stations fall through to placement. Chest targeting
/// runs before table targeting so one shared ray can only ever open the block
/// the player is actually looking at.
///
/// A held bow comes after both station checks so a bow aimed at a chest still
/// opens the chest, and before the Survival food branch so a bow press always
/// arms a draw. It is mode-independent: a Creative player may draw and fire
/// (S09 owns Creative damage). The press only arms the hold — charge and the
/// actual shot happen on the frame tick and the release arm.
///
/// Survival food comes after both station checks and before farming/placement:
/// holding a food item never places a block, a full-hunger click is refused
/// through the transient notice channel instead of silently doing nothing, and
/// Creative keeps its byte-identical placement fallback because food rules are
/// Survival-only.
pub fn right_click_interaction(session: &mut WorldSession) -> RightClickOutcome {
    session.sync_open_chest();
    session.sync_open_table();
    if let Some(pos) = session.try_open_targeted_chest() {
        return RightClickOutcome::ChestOpened(pos);
    }
    if session.try_open_targeted_table().is_some() {
        return RightClickOutcome::TableOpened;
    }
    if matches!(
        session.player.selected_item().map(|item| item.id()),
        Some(ItemId::Tool {
            kind: ToolKind::Bow,
            ..
        })
    ) {
        return RightClickOutcome::BowChargeStarted;
    }
    if session.player.mode() == GameMode::Survival {
        if let Some(item) = session.player.selected_item() {
            if food_value_with(session.registry.as_ref(), item.id()).is_some() {
                return match EatingTarget::new_with_rules(
                    session.registry.as_ref(),
                    &session.rules().eating,
                    session.player.selected_slot(),
                    item,
                    session.player.survival.hunger(),
                ) {
                    Some(_) => RightClickOutcome::EatingStarted,
                    // Full hunger is the only way a food item refuses here;
                    // the target constructor already validated slot and food.
                    None => RightClickOutcome::EatingRefused,
                };
            }
        }
    }
    if let Some(outcome) = try_farm_interaction(session) {
        return outcome;
    }
    match try_place_selected_block(session) {
        Some(remeshed) => RightClickOutcome::Placed(remeshed),
        None => RightClickOutcome::Nothing,
    }
}

/// Tries the hoe and wheat-seed branches of a right click.
///
/// Only the two farming selections raycast here: any other held item or empty
/// slot returns `None` without work and falls through to placement. The
/// top-face rule lives at this layer because it is raycast metadata the session
/// seams never see — tilling a side face and planting into a side face are both
/// refusals, not mutations. Hoe tills the hit block; seeds plant into its
/// adjacent cell, with the session re-checking Air/Farmland/reach/overlap.
/// Returns `None` for every refusal so the click falls through exactly once.
pub fn try_farm_interaction(session: &mut WorldSession) -> Option<RightClickOutcome> {
    let selected = session.player.selected_item()?;
    let is_hoe = matches!(
        selected.id(),
        ItemId::Tool {
            kind: ToolKind::Hoe,
            ..
        }
    );
    let is_seeds = selected.id() == ItemId::Material(Material::WheatSeeds);
    if !is_hoe && !is_seeds {
        return None;
    }
    let hit = raycast_voxels(
        session.camera.position,
        session.camera.forward(),
        8.0,
        |x, y, z| session.world.get_block(x, y, z),
    )?;
    if hit.normal != IVec3::Y {
        return None;
    }
    if is_hoe {
        session
            .till_at(hit.block_pos)
            .map(RightClickOutcome::Tilled)
    } else {
        session
            .plant_wheat_at(hit.adjacent_pos)
            .map(RightClickOutcome::Planted)
    }
}

pub enum RightClickOutcome {
    /// A chest was opened; the authoritative position is exposed by
    /// [`WorldSession::open_chest_position`], not duplicated in this outcome.
    ChestOpened(IVec3),
    /// A table was opened; the authoritative position is exposed by
    /// [`WorldSession::open_table_position`], not duplicated in this outcome.
    TableOpened,
    /// A Survival food click started the hold-to-eat gesture. No item is
    /// consumed here: the redraw loop owns completion through `eat_selected`.
    EatingStarted,
    /// A Survival food click was refused because hunger is already full. The
    /// press arm reports [`right_click_notice`] through the transient message
    /// channel.
    EatingRefused,
    /// A held bow press started a charge. The press only arms the right hold;
    /// progress and the shot are owned by the frame tick and the release arm.
    BowChargeStarted,
    /// A hoe tilled soil into Farmland; carries the remesh work to upload.
    Tilled(Vec<(ChunkPos, ChunkMesh)>),
    /// Wheat seeds were planted; carries the remesh work to upload.
    Planted(Vec<(ChunkPos, ChunkMesh)>),
    Placed(Vec<(ChunkPos, ChunkMesh)>),
    Nothing,
}

/// Transient notice text for a right-click outcome, if any.
///
/// Only the full-hunger refusal needs an explanation: every other outcome is
/// already visible in the world (a station opened, a block appeared) or is an
/// ordinary non-event.
pub fn right_click_notice(outcome: &RightClickOutcome) -> Option<&'static str> {
    match outcome {
        RightClickOutcome::EatingRefused => Some("Cannot eat while full"),
        _ => None,
    }
}

/// Transient notice for a bow-release refusal, if any.
///
/// Only [`BowFireError::NoAmmo`] needs words: it is the one refusal a player
/// cannot infer from the world. Every other refusal (dead, not holding a bow,
/// invalid shot, arrow cap) is already visible or is an ordinary non-event.
pub fn bow_release_notice(error: Option<BowFireError>) -> Option<&'static str> {
    match error {
        Some(BowFireError::NoAmmo) => Some("No arrows"),
        _ => None,
    }
}

/// The hold-to-eat candidate for this frame, or `None` when eating must not
/// progress.
///
/// Cancellation is structural: a released right button, a panel/menu frame
/// (non-gameplay), an ungrabbed cursor, a non-Survival mode, a dead player, any
/// movement key, or a selected item that is not food all produce `None`, which
/// makes [`EatingState::tick`] reset accumulated progress. There is no
/// separate cancellation switch that a reset site could forget.
pub fn eating_candidate(
    session: &WorldSession,
    input: &InputState,
    right_mouse_held: bool,
    gameplay: bool,
    cursor_grabbed: bool,
) -> Option<EatingTarget> {
    if !(gameplay && cursor_grabbed && right_mouse_held) {
        return None;
    }
    let player = &session.player;
    if player.mode() != GameMode::Survival || session.is_dead() || input.is_moving() {
        return None;
    }
    let item = player.selected_item()?;
    EatingTarget::new_with_rules(
        session.registry.as_ref(),
        &session.rules().eating,
        player.selected_slot(),
        item,
        player.survival.hunger(),
    )
}

/// The hold-to-draw candidate for this frame, or `None` when the draw must
/// not progress.
///
/// Cancellation is structural exactly like [`eating_candidate`]: a released
/// right button, a panel/menu frame, an ungrabbed cursor, a dead player, or a
/// selection that is not a wood bow all produce `None`, which makes
/// [`BowState::tick`] reset the accumulated charge. Unlike eating, movement
/// deliberately does **not** cancel: a draw survives walking (see
/// [`crate::player::bow`]). There is no separate cancellation switch that a
/// reset site could forget.
pub fn bow_charge_candidate(
    session: &WorldSession,
    right_mouse_held: bool,
    gameplay: bool,
    cursor_grabbed: bool,
) -> Option<BowTarget> {
    if !(gameplay && cursor_grabbed && right_mouse_held) || session.is_dead() {
        return None;
    }
    let player = &session.player;
    BowTarget::new(player.selected_slot(), player.selected_item()?)
}

/// Outcome of one bow release: whether an arrow actually left the bow, and
/// the refusal reason when it did not.
///
/// Deliberately richer than `Option<BowFireError>`: `None` there conflated a
/// real shot with a sub-threshold tap-cancel, so a bow-release sound would
/// have played on every cancel. `fired` is true only for an accepted shot.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BowRelease {
    /// A real arrow was spawned by this release.
    pub fired: bool,
    /// The refusal reason, if the release attempted a shot and was refused.
    pub error: Option<BowFireError>,
}

/// Releases the held bow: resets the charge unconditionally, then fires one
/// arrow when this is a gameplay frame with a grabbed cursor.
///
/// A free function because `window_event` already holds disjoint
/// `gpu`/`window` borrows, making a `&mut self` method unusable there (P005).
/// The reset happens **before** the gameplay gate, so a release on a menu or
/// panel frame can never leave a live charge behind. Only `fired` is
/// significant to the audio arm: a sub-threshold tap or a non-gameplay release
/// is a silent cancel, while a refused shot reports its error for the notice.
pub fn release_bow(
    bow: &mut BowState,
    session: &mut WorldSession,
    gameplay: bool,
    cursor_grabbed: bool,
) -> BowRelease {
    let Some(charge) = bow.release_with(&session.rules().bow) else {
        return BowRelease::default();
    };
    if !(gameplay && cursor_grabbed) {
        return BowRelease::default();
    }
    // Copy the camera inputs before the mutable session borrow: argument
    // evaluation would otherwise overlap with `fire_bow`'s receiver borrow.
    let origin = session.camera.position;
    let dir = session.camera.forward();
    match session.fire_bow(origin, dir, charge) {
        Ok(_) => BowRelease {
            fired: true,
            error: None,
        },
        Err(error) => BowRelease {
            fired: false,
            error: Some(error),
        },
    }
}

/// Map one classified audio frame to the fixed slot order the frame loop plays:
/// idle, hurt, death, bow release, arrow impact.
pub fn mob_audio_slots(frame: MobAudioFrame) -> [Option<SoundSlot>; 5] {
    [
        frame.idle.then_some(SoundSlot::MobIdle),
        frame.hurt.then_some(SoundSlot::MobHurt),
        frame.death.then_some(SoundSlot::MobDeath),
        frame.bow_release.then_some(SoundSlot::BowRelease),
        frame.arrow_impact.then_some(SoundSlot::ArrowImpact),
    ]
}

/// Attack-before-mining priority for one Playing frame.
///
/// A resident mob under the crosshair within melee reach is the frame's
/// action: it suppresses the mining candidate entirely, so one left press can
/// never both hit a mob and start breaking the block behind it. With no attack
/// candidate the mining candidate passes through unchanged — the 8.0-block
/// raycast reach and every duration rule stay owned by
/// [`MiningTarget::from_block`]. Suppression is free because
/// [`MiningState::tick`] resets its own progress on `None`; do not add a second
/// reset call at the call site.
pub fn mining_after_attack(
    attack: Option<MeleeHit>,
    mining: Option<MiningTarget>,
) -> Option<MiningTarget> {
    if attack.is_some() {
        None
    } else {
        mining
    }
}

/// Crosshair progress bar for this frame.
///
/// One shared bar slot, so priority decides who owns it: eating -> bow ->
/// mining. Eating consumes a real item, a bow draw is a held input the player
/// must see, and mining is the fallback. A completed or cancelled action resets
/// its own state, so the next-priority bar comes back on the very next frame.
/// `None` means no action is progressing and no bar is drawn.
pub fn frame_progress_bar(
    eating: Option<f32>,
    bow: Option<f32>,
    mining: Option<f32>,
    surface_w: u32,
    surface_h: u32,
) -> Option<[Quad; 2]> {
    if let Some(progress) = eating {
        return Some(eating_progress_quads(progress, surface_w, surface_h));
    }
    if let Some(progress) = bow {
        return Some(bow_progress_quads(progress, surface_w, surface_h));
    }
    mining.map(|progress| mining_progress_quads(progress, surface_w, surface_h))
}

// ---------------------------------------------------------------------------
// Inventory-screen adapter (S03-T03)
//
// The pure geometry lives in `ui::inventory` (T01) and every mutation lives in
// the narrow `Player` command façade (T02). This layer only translates a
// decoded input target into one of those commands and back into a status line, so the screen
// can never perform stack arithmetic of its own and gameplay input can never
// leak through an open panel.
// ---------------------------------------------------------------------------

/// What one accepted inventory click did. `Crafted` carries the item so the
/// status line can name it; transfer clicks stay a bare `Moved` (D061: the UI
/// never inspects counts to decide behavior).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InventoryClickOutcome {
    /// A whole stack moved between slots, the cursor, or a grid cell.
    Moved,
    /// A grid result slot produced this item exactly once.
    Crafted(Item),
}

/// Grants the palette entry under `cursor` into the hotbar.
///
/// Pure apart from the player inventory mutation: the hit test and filtered
/// entry resolution come from `ui::palette`, and the grant path is the T02
/// `Player::grant_palette_item_with` (authored stack limit, exactly one hotbar
/// slot, granted slot selected). Returns `None` for a closed screen, empty
/// space, or an entry the registry refuses, so the caller reports one status
/// line and never mutates on a miss.
pub fn apply_palette_click(
    player: &mut Player,
    registry: &ContentRegistry,
    screen: &PaletteScreen,
    layout: &palette::PaletteLayout,
    cursor: Option<(f32, f32)>,
) -> Option<(usize, Item)> {
    let index = palette::hit_test(screen, layout, cursor?)?;
    let entry = screen.filtered_entry(index)?;
    player.grant_palette_item_with(registry, entry.id).ok()
}

/// Resolves the palette entry under `cursor` into a tooltip item.
///
/// Uses the same filtered-entry model as the click and icon paths, so the
/// tooltip describes exactly the row the user is pointing at. Returns `None`
/// for a closed screen, empty space, or an identity the registry cannot
/// construct (the palette excludes placeholders, so this is a defensive arm).
pub fn palette_hovered_item(
    registry: &ContentRegistry,
    screen: &PaletteScreen,
    layout: &palette::PaletteLayout,
    cursor: Option<(f32, f32)>,
) -> Option<Item> {
    let index = palette::hit_test(screen, layout, cursor?)?;
    let entry = screen.filtered_entry(index)?;
    Item::new_with(registry, entry.id, 1).ok()
}

/// Which mouse button issued an inventory command.
///
/// Left is whole-stack movement; right is the split-half / place-one
/// gesture. Keeping the button explicit in the adapter means the domain
/// never has to guess intent from cursor state alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryClickButton {
    Left,
    Right,
}

/// Translates one clicked target into exactly one `Player` inventory command.
///
/// Table targets are rejected unless this frame's mode is `Table` (an attached
/// screen alone is never enough — the live table must still exist), and every
/// other target routes to the kind-specific façade method. No branch reads or
/// computes stack counts. This is the left-click entry point; right-click
/// gestures go through [`apply_inventory_click_button`].
pub fn apply_inventory_click(
    player: &mut Player,
    screen: &InventoryScreen,
    table_open: Option<IVec3>,
    target: InventoryTarget,
) -> Result<InventoryClickOutcome, InventoryCommandError> {
    apply_inventory_click_button(
        player,
        screen,
        table_open,
        target,
        InventoryClickButton::Left,
    )
}

/// Translates one clicked target and mouse button into exactly one `Player`
/// inventory command.
///
/// Left clicks move whole stacks; right clicks split half out of a source or
/// place exactly one unit into a destination. Result slots craft once for
/// either button (there is no half-craft), so a right-click on the output can
/// never mint a partial item. Rejected commands propagate the domain error
/// unchanged, which keeps the conservation guarantees in one place.
pub fn apply_inventory_click_button(
    player: &mut Player,
    screen: &InventoryScreen,
    table_open: Option<IVec3>,
    target: InventoryTarget,
    button: InventoryClickButton,
) -> Result<InventoryClickOutcome, InventoryCommandError> {
    apply_inventory_click_button_with(
        player,
        ContentRegistry::builtin(),
        screen,
        table_open,
        target,
        button,
    )
}

/// Registry-aware production result-slot seam.
///
/// Identical to [`apply_inventory_click_button`] except a result-slot click
/// crafts against `registry`, so pack recipes match through the real click
/// path and a crafted pack tool keeps its [`ItemId::ModTool`] identity. Every
/// other target is registry-independent and answers unchanged.
pub fn apply_inventory_click_button_with(
    player: &mut Player,
    registry: &ContentRegistry,
    screen: &InventoryScreen,
    table_open: Option<IVec3>,
    target: InventoryTarget,
    button: InventoryClickButton,
) -> Result<InventoryClickOutcome, InventoryCommandError> {
    let mode = screen.mode(table_open);
    let table_target = matches!(
        target,
        InventoryTarget::GridCell {
            kind: GridKind::Table,
            ..
        } | InventoryTarget::ResultSlot(GridKind::Table)
    );
    if table_target && mode != InventoryMode::Table {
        return Err(InventoryCommandError::TableUnavailable);
    }
    match (button, target) {
        (InventoryClickButton::Left, InventoryTarget::Slot(index)) => player
            .click_inventory_slot(index)
            .map(|()| InventoryClickOutcome::Moved)
            .map_err(InventoryCommandError::from),
        (InventoryClickButton::Right, InventoryTarget::Slot(index)) => player
            .click_inventory_slot_right(index)
            .map(|()| InventoryClickOutcome::Moved)
            .map_err(InventoryCommandError::from),
        (
            InventoryClickButton::Left,
            InventoryTarget::GridCell {
                kind: GridKind::Player,
                index,
            },
        ) => player
            .click_grid_cell(GridKind::Player, index)
            .map(|()| InventoryClickOutcome::Moved),
        (
            InventoryClickButton::Right,
            InventoryTarget::GridCell {
                kind: GridKind::Player,
                index,
            },
        ) => player
            .click_grid_cell_right(GridKind::Player, index)
            .map(|()| InventoryClickOutcome::Moved),
        (
            InventoryClickButton::Left,
            InventoryTarget::GridCell {
                kind: GridKind::Table,
                index,
            },
        ) => player
            .click_grid_cell(GridKind::Table, index)
            .map(|()| InventoryClickOutcome::Moved),
        (
            InventoryClickButton::Right,
            InventoryTarget::GridCell {
                kind: GridKind::Table,
                index,
            },
        ) => player
            .click_grid_cell_right(GridKind::Table, index)
            .map(|()| InventoryClickOutcome::Moved),
        (_, InventoryTarget::ResultSlot(GridKind::Player)) => player
            .take_craft_result_with(registry, GridKind::Player)
            .map(InventoryClickOutcome::Crafted)
            .map_err(InventoryCommandError::Craft),
        (_, InventoryTarget::ResultSlot(GridKind::Table)) => player
            .take_craft_result_with(registry, GridKind::Table)
            .map(InventoryClickOutcome::Crafted)
            .map_err(InventoryCommandError::Craft),
    }
}

/// One-line status for the panel footer: what moved, what was crafted, or the
/// typed rejection (warnings-as-data, never a log — D006/MEM009).
pub fn inventory_status_text(
    result: Result<InventoryClickOutcome, InventoryCommandError>,
) -> String {
    match result {
        Ok(InventoryClickOutcome::Moved) => "Moved".to_string(),
        Ok(InventoryClickOutcome::Crafted(item)) => format!("Crafted {}", item.id().name()),
        Err(InventoryCommandError::Inventory(error)) => format!("Inventory: {error:?}"),
        Err(InventoryCommandError::Craft(error)) => format!("Craft: {error:?}"),
        Err(InventoryCommandError::TableUnavailable) => "Table unavailable".to_string(),
    }
}

/// True only when the screen was opened for a table that has since disappeared
/// (broken, replaced, or out of reach). Delegates to the pure T01 state.
pub fn inventory_should_close(screen: &InventoryScreen, table_open: Option<IVec3>) -> bool {
    screen.table_lost(table_open)
}

/// Re-validates the attached crafting-table station at an inventory click
/// boundary and closes a panel whose station disappeared (S04-T02).
///
/// The redraw path already syncs once per playing frame, but gameplay state is
/// not frozen for chunk streaming: a streamed chunk can unload (or the player
/// can step out of reach) between two redraws, and a mouse press/release can be
/// delivered before the next one. Without a sync here the click would still see
/// the stale station, so a table-grid press would mutate the 4x4 grid as if the
/// table were live. Syncing first makes the press and release arms
/// self-defending: a removed/replaced/out-of-reach table closes the panel (and
/// its station) before any command is interpreted, so the click is dropped.
///
/// Returns `true` when the panel was closed because the station was lost.
pub fn revalidate_inventory_station(
    screen: &mut InventoryScreen,
    session: &mut WorldSession,
) -> bool {
    let table_open = session.sync_open_table();
    if !inventory_should_close(screen, table_open) {
        return false;
    }
    screen.close();
    session.close_table();
    true
}

// ---------------------------------------------------------------------------
// Chest-screen adapter (S03-T05)
//
// The pure geometry lives in `ui::chest` (T04) and every mutation lives in
// `WorldSession`'s narrow chest command (T03). This layer only translates a
// decoded input target into one of those commands and back into a status line; the
// screen never performs stack arithmetic and gameplay input never leaks
// through an open chest.
// ---------------------------------------------------------------------------

/// Translates one clicked chest-screen target into exactly one domain command:
/// a chest slot goes through the station-aware session command (which
/// revalidates the chest and owns record lifecycle), while a player slot goes
/// through the shared inventory façade. Neither branch reads or computes stack
/// counts, and neither can touch the other aggregate.
pub fn apply_chest_click(
    session: &mut WorldSession,
    target: ChestTarget,
) -> Result<(), ChestCommandError> {
    match target {
        ChestTarget::ChestSlot(index) => session.click_open_chest_slot(index),
        ChestTarget::InventorySlot(index) => session
            .player
            .click_inventory_slot(index)
            .map_err(ChestCommandError::Slot),
    }
}

/// One-line status for the chest footer: what moved, or the typed rejection
/// (warnings-as-data, never a log — D006/MEM009).
pub fn chest_status_text(result: Result<(), ChestCommandError>) -> String {
    match result {
        Ok(()) => "Moved".to_string(),
        Err(ChestCommandError::NotOpen) => "Chest closed".to_string(),
        Err(ChestCommandError::Slot(error)) => format!("Inventory: {error:?}"),
    }
}

/// True only when the chest screen is open but its live station has vanished
/// (broken, replaced, or out of reach). Delegates to the pure T04 state.
pub fn chest_should_close(screen: &ChestScreen, open_chest: Option<IVec3>) -> bool {
    screen.station_lost(open_chest)
}
