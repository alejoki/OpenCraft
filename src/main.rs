use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{IVec3, Vec3};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use opencraft::audio::{default_buffers, AudioEngine, SoundBuffer, SoundSlot};
use opencraft::mesh::quad::ChunkMesh;
use opencraft::pack::{self, LoadedPack};
use opencraft::player::inventory::{GridKind, Item, ToolWear, HOTBAR_SLOTS};
use opencraft::player::mining::{MiningState, MiningTarget, MiningTick};
use opencraft::player::physics::{InputState, InventoryCommandError, Player};
use opencraft::player::raycast::raycast_voxels;
use opencraft::render::buffer::ChunkRenderStore;
use opencraft::render::context::RenderContext;
use opencraft::render::pipeline::{SkyParams, UiPass, VoxelRenderer};
use opencraft::render::texture::generate_all_texture_layers_rgba;
use opencraft::settings::keybind::key_to_string;
use opencraft::settings::{
    load_with_warnings, Keybinds, Settings, SettingsWarning, DEFAULT_SETTINGS_PATH, VOLUME_MAX,
    VOLUME_MIN,
};
use opencraft::ui::hud::{
    hotbar_count_label_for, hotbar_count_text_origin, hotbar_slot_rects, mining_progress_quads,
    selected_item_label, HOTBAR_COUNT_FONT_SIZE, HOTBAR_COUNT_LINE_HEIGHT,
};
use opencraft::ui::inventory::{
    hit_test, layout, screen_labels, screen_quads, InventoryMode, InventoryScreen, InventoryTarget,
    MAX_LABELS,
};
use opencraft::ui::logo;
use opencraft::ui::menu::{self, AdjustDirection, MenuMessage, MenuRow};
use opencraft::ui::screens::{
    delete_prompt_line, format_row_line, hovered_load_target, hovered_main_menu_row,
    hovered_new_game_target, hovered_pause_row, load_back_rect, load_cancel_rect,
    load_confirm_rect, load_delete_rect, load_panel_rect, load_play_rect, load_row_rect,
    main_menu_panel_rect, main_menu_row_rect, new_game_panel_rect, new_game_row_lines,
    new_game_row_rect, pause_panel_rect, pause_row_rect, screen_key_action, validate_create,
    FocusField, LoadScreen, LoadTarget, MainMenuRow, NewGameScreen, NewGameTarget, PauseRow,
    ScreenKeyAction, MAIN_MENU_ROWS, NEW_GAME_ROWS, PAUSE_ROWS,
};
use opencraft::ui::{
    LifecycleDecision, LifecycleIntent, LifecycleScreen, LifecycleState, LoadGameIntent,
    MainMenuIntent, NewGameIntent, PauseIntent, Quad, SettingsIntent, StatsOverlay,
    TextInputAction, UiRenderer, FONT_SIZE, LINE_HEIGHT,
};
use opencraft::world::block::BlockId;
use opencraft::world::catalog::{resolve_seed_or, CatalogRow, PendingDelete};
use opencraft::world::catalog_fs::WorldCatalogFs;
use opencraft::world::chunk::{world_to_chunk_and_local, ChunkPos};
use opencraft::world::daycycle;
use opencraft::world::manager::WorldManager;
use opencraft::world::persistence::{
    load_world_from_path, save_world_to_path, ChunkEditMap, CliConfig, PersistenceError,
    SavedPlayerState, WorldSaveData, DEFAULT_SAVE_PATH, DEFAULT_TIME_SCALE,
};
#[cfg(test)]
use opencraft::world::season::DEFAULT_SEASON_LENGTH_DAYS;
use opencraft::world::terrain::DEFAULT_WORLD_SEED;
use opencraft::world::vista;
use opencraft::{random_seed, WorldSession};

/// Top-left corner of the stats overlay text, in physical pixels.
const OVERLAY_TEXT_LEFT: f32 = 8.0;
const OVERLAY_TEXT_TOP: f32 = 8.0;

/// Message-expiry tick interval: the loop runs in `ControlFlow::Poll`,
/// potentially far above 60 fps, while `menu::MESSAGE_TICKS` is calibrated
/// as ~10 seconds at ~60 ticks/s. Ticking the menu message at most this
/// often keeps the T01 pure tick counter time-sane without giving the menu
/// module a clock.
const MENU_TICK_INTERVAL: f32 = 1.0 / 60.0;

/// Load the persisted texture pack at startup, returning it together with its
/// per-file fallback warnings so the caller can merge them into
/// `startup_warnings` (the S03 routing contract: warnings are data shown in the
/// menu, never logs). `None` selection is the built-in Default pack: no data,
/// no warnings.
fn load_startup_pack(
    packs_root: &Path,
    selected: Option<&str>,
) -> (Option<LoadedPack>, Vec<SettingsWarning>) {
    let Some(name) = selected else {
        return (None, Vec::new());
    };
    let loaded = pack::load_pack_by_name(packs_root, name);
    let warnings = loaded.warnings.clone();
    (Some(loaded), warnings)
}

/// Resolve the startup sound buffers and warnings for the persisted pack.
///
/// `None` selection is the built-in Default pack: the synthesized buffers stay
/// untouched and no warnings are produced. A selected pack is resolved
/// independently of its texture result (a pack whose layers partially failed
/// may still ship usable sounds); the loader never fails, so this can never
/// block launch (D054).
fn load_startup_sounds(
    packs_root: &Path,
    selected: Option<&str>,
) -> ([SoundBuffer; 4], Vec<SettingsWarning>) {
    let Some(name) = selected else {
        return (default_buffers(), Vec::new());
    };
    pack::sound::load_sound_overrides(&packs_root.join(name))
}

/// Build the app's sound engine. Production opens the default output device
/// and degrades to a silent no-op engine on any construction failure (launch
/// is never blocked by audio). Test builds use the explicit silent
/// constructor instead: the suite builds many headless `GameApp`s and must
/// never open real CPAL streams or a device for them.
#[cfg(not(test))]
fn new_audio_engine() -> AudioEngine {
    AudioEngine::new()
}

#[cfg(test)]
fn new_audio_engine() -> AudioEngine {
    AudioEngine::disabled()
}

fn set_cursor_grab(window: &winit::window::Window, grab: bool) {
    if grab {
        let _ = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

/// Apply the vsync setting to the surface's present mode and reconfigure.
/// Mode selection is capability-checked inside
/// [`RenderContext::set_vsync`]: vsync prefers `Mailbox` (v-synchronized but
/// non-blocking) with `Fifo` as the always-supported fallback, and no-vsync
/// prefers uncapped `Immediate`. The render loop pairs a vsync-on Mailbox
/// surface with the refresh-rate limiter in `about_to_wait`.
///
/// NOTE: on Wayland the compositor may not honor `Immediate` — presenting
/// then degrades to the compositor's schedule (an accepted visual no-op per
/// plan; do not fight the compositor).
fn apply_vsync(ctx: &mut RenderContext, vsync: bool) {
    ctx.set_vsync(vsync);
}

/// Frame period for a monitor refresh expressed in millihertz (the unit
/// winit reports). Returns `None` for an implausible or missing rate rather
/// than scheduling a bogus wakeup.
fn refresh_interval(refresh_millihertz: u32) -> Option<Duration> {
    if !(1_000..=1_000_000).contains(&refresh_millihertz) {
        return None;
    }
    // 1 Hz = 1_000 mHz, so one period is 1e12 / mHz nanoseconds.
    Some(Duration::from_nanos(
        1_000_000_000_000 / u64::from(refresh_millihertz),
    ))
}

/// The display refresh period for the window's current monitor, if the
/// platform reports a usable rate. Used only while vsync is enabled.
fn vsync_interval(window: &Window) -> Option<Duration> {
    window
        .current_monitor()
        .and_then(|monitor| monitor.refresh_rate_millihertz())
        .and_then(refresh_interval)
}

/// Save the active session's world to the session's own save path
/// (S03-T02: every save site — F5 quick-save, streaming autosave on
/// unload, pause Save-and-Quit, Exit Game, CloseRequested, OutOfMemory —
/// funnels through this one session-tied function, so no site can drift
/// back to writing cli.save_path). `save_world_to_path` creates parent
/// directories atomically. Returns `Ok` only after the write succeeded
/// (only then is the world's unsaved-changes flag cleared); on failure the
/// `PersistenceError` is returned as data — the session's world keeps
/// its unsaved-changes flag for the next save opportunity, and the caller
/// surfaces the error where a message slot exists (no log by convention;
/// D006/MEM009).
fn save_current_world(session: &mut WorldSession) -> Result<(), PersistenceError> {
    let data = WorldSaveData {
        seed: session.world.seed(),
        time_of_day: session.clock.time_of_day,
        day_index: session.clock.day_index,
        season_length_days: session.season_length_days,
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
        },
        chunk_edits: session.world.chunk_edits().clone(),
    };
    let result = save_world_to_path(&session.save_path, &data);
    if result.is_ok() {
        session.world.mark_saved();
    }
    result
}

/// Why a mining break produced no world edit, and therefore no wear and no
/// collected drop.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BreakRejection {
    /// The reported block is structurally unbreakable (Air, Bedrock), so the
    /// catalog refuses it regardless of what a caller claims.
    Unbreakable,
    /// The live world block at `pos` is not the reported pre-break block
    /// (already removed, replaced, or in an unloaded chunk).
    StaleTarget,
    /// The world edit returned no remesh work, so it did not apply (unloaded
    /// chunk or outside the vertical world range).
    EditRejected,
}

/// What an applied break did to the selected hotbar slot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ToolWearOutcome {
    /// The selected tool lost exactly one point and is still usable.
    Worn { remaining: u16 },
    /// The selected tool reached zero and its slot was cleared.
    Broke,
    /// The selected slot was empty or held a non-tool: nothing was worn.
    Nothing,
}

/// Outcome of one production mining break.
///
/// `Applied` is the only arm that carries remesh work, and it is produced
/// only after the world edit proved it happened. Every other path — a stale
/// target, an unbreakable block, or an edit that returned nothing — is a
/// rejection that left the world, the inventory, and durability untouched.
///
/// Production only needs the remesh work, but the wear transition and the
/// rejection reason are the outcome contract the executable harness asserts
/// on, so they stay part of the value instead of being flattened away in the
/// loop.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
enum BreakOutcome {
    Applied {
        remeshed: Vec<(ChunkPos, ChunkMesh)>,
        wear: ToolWearOutcome,
    },
    Rejected(BreakRejection),
}

/// Applies one mining break with a fixed, testable ordering:
///
/// 1. re-read the world and refuse unbreakable/stale targets,
/// 2. apply the world edit (the only authority that a break happened),
/// 3. wear the selected tool exactly once, but only after non-empty remesh
///    work proved the edit applied,
/// 4. collect the pre-break block last, so a tool that broke has already
///    freed its hotbar slot and the drop can land there.
///
/// An empty remesh result means the edit did not apply (unloaded chunk or
/// outside the vertical world range), so nothing is worn or collected —
/// mining can never duplicate items or spend durability through an edit that
/// never happened.
fn break_mined_block(session: &mut WorldSession, pos: IVec3, block: BlockId) -> BreakOutcome {
    if block.break_duration_secs().is_none() {
        return BreakOutcome::Rejected(BreakRejection::Unbreakable);
    }
    // `pos`/`block` come from the mining accumulator, which is not authority:
    // re-read the live world so a stale or replaced target edits nothing.
    if session.world.get_block(pos.x, pos.y, pos.z) != block {
        return BreakOutcome::Rejected(BreakRejection::StaleTarget);
    }
    let remeshed = session
        .world
        .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Air);
    if remeshed.is_empty() {
        return BreakOutcome::Rejected(BreakRejection::EditRejected);
    }

    let wear = match session.player.wear_selected_tool() {
        Ok(ToolWear::Worn(item)) => ToolWearOutcome::Worn {
            remaining: item
                .durability()
                .expect("a successfully worn tool still reports durability"),
        },
        Ok(ToolWear::Broke) => ToolWearOutcome::Broke,
        // Empty slot / non-tool / out-of-range selection: no mutation.
        Err(_) => ToolWearOutcome::Nothing,
    };
    // Drop mapping is block metadata (Stone -> Cobblestone); the item identity
    // is the only thing that differs, and the drop is still collected last so
    // MEM195 ordering is preserved.
    session.player.collect_block(block.mined_drop());

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
fn try_place_selected_block(session: &mut WorldSession) -> Option<Vec<(ChunkPos, ChunkMesh)>> {
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
    session.player.consume_selected();
    Some(remeshed)
}

/// Outcome of one production right-click world interaction.
///
/// Table targeting runs first and is a pure session decision: a successfully
/// targeted crafting table neither edits the world nor consumes the selected
/// stack. Anything else falls through to the pre-existing placement path, so
/// non-table clicks stay byte-compatible with the placement-only behavior.
///
/// `sync_open_table` runs at the interaction boundary so a stale open station
/// (the table was removed/replaced or the player walked away) is closed before
/// the click is interpreted — replaced tables fall through to placement.
fn right_click_interaction(session: &mut WorldSession) -> RightClickOutcome {
    session.sync_open_table();
    if session.try_open_targeted_table().is_some() {
        return RightClickOutcome::TableOpened;
    }
    match try_place_selected_block(session) {
        Some(remeshed) => RightClickOutcome::Placed(remeshed),
        None => RightClickOutcome::Nothing,
    }
}

enum RightClickOutcome {
    /// A table was opened; the authoritative position is exposed by
    /// [`WorldSession::open_table_position`], not duplicated in this outcome.
    TableOpened,
    Placed(Vec<(ChunkPos, ChunkMesh)>),
    Nothing,
}

// ---------------------------------------------------------------------------
// Inventory-screen adapter (S03-T03)
//
// The pure geometry lives in `ui::inventory` (T01) and every mutation lives in
// the narrow `Player` command façade (T02). This layer only translates winit
// input into one of those commands and back into a status line, so the screen
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

/// Opens the inventory panel from either the E key (`via_table = false`) or a
/// successful table interaction (`via_table = true`), releasing the cursor and
/// dropping every gameplay hold so the panel owns the pointer.
///
/// A free function because `window_event` already holds disjoint `gpu`/`window`
/// borrows, making a `&mut self` method unusable there (P005).
#[allow(clippy::too_many_arguments)]
fn open_inventory(
    screen: &mut InventoryScreen,
    table_open: Option<IVec3>,
    via_table: bool,
    input: &mut InputState,
    left_mouse_held: &mut bool,
    drag: &mut Option<InventoryTarget>,
    mining: &mut MiningState,
    cursor_grabbed: &mut bool,
    window: &Window,
) {
    if via_table {
        screen.open_table();
    } else {
        screen.toggle(table_open);
    }
    input.reset();
    mining.reset();
    *left_mouse_held = false;
    *drag = None;
    *cursor_grabbed = false;
    set_cursor_grab(window, false);
}

/// Closes the inventory panel from the E/Escape paths and restores gameplay
/// input ownership: the cursor is re-grabbed, holds are cleared, and the
/// session station is closed so the 4x4 grid cannot outlive the screen.
#[allow(clippy::too_many_arguments)]
fn close_inventory(
    screen: &mut InventoryScreen,
    session: &mut WorldSession,
    input: &mut InputState,
    left_mouse_held: &mut bool,
    drag: &mut Option<InventoryTarget>,
    mining: &mut MiningState,
    cursor_grabbed: &mut bool,
    window: &Window,
) {
    screen.close();
    session.close_table();
    input.reset();
    mining.reset();
    *left_mouse_held = false;
    *drag = None;
    *cursor_grabbed = true;
    set_cursor_grab(window, true);
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
            .take_craft_result(GridKind::Player)
            .map(InventoryClickOutcome::Crafted)
            .map_err(InventoryCommandError::Craft),
        (_, InventoryTarget::ResultSlot(GridKind::Table)) => player
            .take_craft_result(GridKind::Table)
            .map(InventoryClickOutcome::Crafted)
            .map_err(InventoryCommandError::Craft),
    }
}

/// One-line status for the panel footer: what moved, what was crafted, or the
/// typed rejection (warnings-as-data, never a log — D006/MEM009).
fn inventory_status_text(result: Result<InventoryClickOutcome, InventoryCommandError>) -> String {
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
fn inventory_should_close(screen: &InventoryScreen, table_open: Option<IVec3>) -> bool {
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
fn revalidate_inventory_station(screen: &mut InventoryScreen, session: &mut WorldSession) -> bool {
    let table_open = session.sync_open_table();
    if !inventory_should_close(screen, table_open) {
        return false;
    }
    screen.close();
    session.close_table();
    true
}

/// Where a press-drag-release ends: only a different cell than the press origin
/// receives the held stack. A same-cell release (the click-to-pick style) and a
/// release off the panel are inert, leaving the stack on the cursor.
fn drag_release_target(
    origin: Option<InventoryTarget>,
    target: Option<InventoryTarget>,
) -> Option<InventoryTarget> {
    match (origin, target) {
        (Some(origin), Some(target)) if origin != target => Some(target),
        _ => None,
    }
}

/// Whether a left panel press on `target` arms a press-drag-release gesture.
///
/// Only a pure pick-up arms a drag: the cursor must have been empty *before*
/// the press (so the press lifted a stack) and occupied *after* it, and the
/// command must have been an accepted `Moved`. A swap, place, partial merge, or
/// craft result leaves a different stack on the cursor, so it ends the gesture
/// — otherwise the release over another cell would issue a second, unintended
/// whole-stack move (S04-T03). This is a one-armed-when rule only: the domain
/// arithmetic in `apply_inventory_click` is untouched (MEM203).
fn drag_origin_after_press(
    cursor_empty_before: bool,
    result: Result<InventoryClickOutcome, InventoryCommandError>,
    cursor_occupied_after: bool,
    target: InventoryTarget,
) -> Option<InventoryTarget> {
    if cursor_empty_before && cursor_occupied_after && result == Ok(InventoryClickOutcome::Moved) {
        Some(target)
    } else {
        None
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "On"
    } else {
        "Off"
    }
}

/// Compose the menu's display text: one "label: value" line per row, a
/// capture hint line while a rebind is pending, and the transient message
/// line. `pack_label` is the active texture pack's display name ("Default"
/// for the built-in procedural pack), kept as a parameter so this stays a
/// pure function of its inputs — unit-testable without GPU or events.
/// Called per frame while the menu is open, where `StatsOverlay::update`
/// skips the expensive re-shape unless the composed string actually
/// changed.
fn compose_menu_text(settings: &Settings, menu: &menu::SettingsMenu, pack_label: &str) -> String {
    let mut lines = Vec::with_capacity(menu::ROW_COUNT + 2);
    for row in menu::ROWS {
        let line = match row {
            MenuRow::Bind(action) => {
                if menu.awaiting_action() == Some(action) {
                    format!("{}: <press a key>", row.label())
                } else {
                    let key =
                        key_to_string(action.key_of(&settings.keybinds)).unwrap_or("<unsupported>");
                    format!("{}: {}", row.label(), key)
                }
            }
            MenuRow::MouseSensitivity => {
                format!("{}: {:.4}", row.label(), settings.mouse_sensitivity)
            }
            MenuRow::Fov => format!("{}: {:.0}", row.label(), settings.fov_degrees),
            MenuRow::RenderDistance => {
                format!("{}: {}", row.label(), settings.render_distance)
            }
            MenuRow::Fullscreen => {
                format!("{}: {}", row.label(), on_off(settings.fullscreen))
            }
            MenuRow::Vsync => format!("{}: {}", row.label(), on_off(settings.vsync)),
            MenuRow::Volume => format!("{}: {}%", row.label(), settings.volume),
            MenuRow::TexturePack => format!("{}: {pack_label}", row.label()),
            MenuRow::Close => row.label().to_string(),
        };
        lines.push(line);
    }
    if let Some(action) = menu.awaiting_action() {
        lines.push(format!(
            "press a key for '{}' (Escape cancels)",
            action.name()
        ));
    }
    if let Some(message) = &menu.message {
        lines.push(message.text.clone());
    }
    lines.join("\n")
}

/// Render-side sky inputs from a pure day-cycle palette plus a game time.
///
/// Session frames use the live clock's palette *and* its
/// `clock.time_of_day`; vista frames use the pinned
/// [`daycycle::VISTA_GOLDEN_HOUR`] palette and that same constant as the sky
/// time, never deriving either from the world clock or `vista_epoch` (that
/// only drives camera drift). Passing the constant here is what makes the
/// golden-hour sun appear: `sun_direction(VISTA_GOLDEN_HOUR)` sits ~15° above
/// the eastern horizon.
fn sky_params(palette: daycycle::SkyPalette, time_of_day: u32) -> SkyParams {
    SkyParams {
        time_of_day,
        // The per-frame eye-submersion lookup lands in S04-T03; until then the
        // air palette is always used (the water pass itself already ships).
        underwater: false,
        day_factor: palette.day_factor,
        fog_color: [
            palette.fog_color[0],
            palette.fog_color[1],
            palette.fog_color[2],
            1.0,
        ],
        clear_color: wgpu::Color {
            r: palette.sky_color[0] as f64,
            g: palette.sky_color[1] as f64,
            b: palette.sky_color[2] as f64,
            a: 1.0,
        },
    }
}

/// CLI world intent: the exact three-flag disjunction from the plan. Any
/// of `--new`/`--new-world`, `--seed`, or a non-default `--save` means the
/// launch carries an explicit world choice and enters gameplay directly,
/// bypassing every menu screen (preserving both smoke scripts' flagged
/// launches unchanged). A bare launch opens the main menu instead.
fn cli_has_world_intent(cli: &CliConfig) -> bool {
    cli.force_new_world
        || cli.seed_override.is_some()
        || cli.save_path != Path::new(DEFAULT_SAVE_PATH)
}

/// The startup world session, mirroring main()'s pre-S03 load-or-fresh
/// block EXACTLY (byte-identical legacy behavior — including the
/// default.mcvx launch and every CLI-flag direct launch — now testable
/// without an event loop): when the cli save path exists and the launch
/// did not force a new world, load it and rebuild the session via
/// `WorldSession::session_from_save` (player state, seed, render
/// distance, chunk edits) with `save_path = cli.save_path`; otherwise a
/// fresh `WorldSession::new_session` at
/// `cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED)` with the same save
/// path.
fn startup_session(cli: &CliConfig) -> WorldSession {
    if !cli.force_new_world && cli.save_path.exists() {
        if let Ok(save) = load_world_from_path(&cli.save_path) {
            return WorldSession::session_from_save(save, cli.save_path.clone());
        }
    }
    let seed = cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED);
    // Fresh worlds start at the default season length; the New Game row and
    // the `--season-length` CLI seam override it on this branch only. A load
    // branch above never reaches here, so a persisted calendar always wins
    // over a launch flag.
    WorldSession::new_session(seed, cli.save_path.clone(), cli.season_length)
}

/// Apply a S01-confirmed pending delete through the live catalog and
/// compose the post-delete load-screen state (S02-T03 wiring): rows
/// refresh from disk on success AND failure alike — the screen must stay
/// truthful about what is actually on disk (a failed delete leaves the
/// row present) — and a `CatalogDeleteError` surfaces inline as the
/// screen's transient message (warnings-as-data, D006/MEM009) instead of
/// being swallowed. Free helper over (catalog, pending) so the failure
/// modes are unit-testable with a tempdir catalog; deleting an already
/// vanished file is a success (S01 idempotency — the observable outcome,
/// the save being gone, is already true).
fn apply_confirmed_delete(
    catalog: &WorldCatalogFs,
    pending: &PendingDelete,
) -> (Vec<CatalogRow>, Option<MenuMessage>) {
    let outcome = catalog.delete_confirmed(pending);
    let rows = catalog.list_rows();
    let message = match outcome {
        Ok(()) => None,
        Err(err) => Some(MenuMessage {
            text: err.to_string(),
            ticks_left: menu::MESSAGE_TICKS,
        }),
    };
    (rows, message)
}

/// Decrement a GameApp-owned transient [`MenuMessage`] tick counter (the
/// same pattern as `SettingsMenu::tick` and `NewGameScreen::tick`). The
/// T01 pure screens own no message slots of their own, so GameApp owns
/// the load screen's and the main menu's transient messages directly and
/// ticks them here (S03-T03: the main-menu slot carries the Save-and-Quit
/// save-failure message).
fn tick_transient_message(message: &mut Option<MenuMessage>) -> bool {
    match message {
        None => false,
        Some(msg) => {
            msg.ticks_left = msg.ticks_left.saturating_sub(1);
            if msg.ticks_left == 0 {
                *message = None;
                false
            } else {
                true
            }
        }
    }
}

/// Compose the active lifecycle screen's display text (main menu, new
/// game, load game, pause): one line per row/target in T01 geometry order
/// top-to-bottom, then transient state lines (the delete prompt while a
/// delete is pending), then the message line last. Pure function of
/// (screen, form, load, messages) so it is unit-testable without GPU or
/// events — same pattern as `compose_menu_text` (called per frame;
/// `StatsOverlay::update` skips the expensive re-shape unless the composed
/// string changed). The settings screen composes via `compose_menu_text`
/// in its own render path instead; Playing renders no screen text.
///
/// Load-screen line order matches T01 geometry exactly so hover highlights
/// align with their rows: catalog rows, then Back, then — only while a
/// delete is pending — the Confirm row (which doubles as the prompt naming
/// the row it would delete) and Cancel. An empty catalog (fresh install,
/// missing saves dir — not an error) displays as `(no saved worlds)`. The
/// main menu appends its transient message (the S03-T03 Save-and-Quit
/// save-failure note) as its last line when present; with no message it
/// lists exactly its four rows.
fn compose_screen_text(
    screen: LifecycleScreen,
    new_game: &NewGameScreen,
    load: &LoadScreen,
    load_message: Option<&MenuMessage>,
    main_menu_message: Option<&MenuMessage>,
) -> String {
    let lines: Vec<String> = match screen {
        LifecycleScreen::MainMenu => {
            let mut lines = MAIN_MENU_ROWS
                .iter()
                .map(|row| row.label().to_string())
                .collect::<Vec<_>>();
            if let Some(message) = main_menu_message {
                lines.push(message.text.clone());
            }
            lines
        }
        LifecycleScreen::Pause => PAUSE_ROWS
            .iter()
            .map(|row| row.label().to_string())
            .collect(),
        LifecycleScreen::NewGame => {
            let mut lines = new_game_row_lines(new_game);
            if let Some(message) = new_game.message() {
                lines.push(message.text.clone());
            }
            lines
        }
        LifecycleScreen::LoadGame => {
            let mut lines = Vec::new();
            if load.row_count() == 0 {
                lines.push("(no saved worlds)".to_string());
            } else {
                for row in load.rows() {
                    lines.push(format_row_line(row));
                }
            }
            lines.push(
                LoadTarget::Back
                    .label()
                    .expect("Back has a fixed label")
                    .to_string(),
            );
            if load.is_delete_pending() {
                // The confirm row doubles as the delete prompt naming the
                // row it would delete; a missing row identity (cannot
                // happen while pending) falls back to the plain label.
                let prompt = load
                    .pending_row()
                    .and_then(|index| load.rows().get(index))
                    .map(|row| delete_prompt_line(&row.name))
                    .unwrap_or_else(|| {
                        LoadTarget::ConfirmDelete
                            .label()
                            .expect("ConfirmDelete has a fixed label")
                            .to_string()
                    });
                lines.push(prompt);
                lines.push(
                    LoadTarget::CancelDelete
                        .label()
                        .expect("CancelDelete has a fixed label")
                        .to_string(),
                );
                if let Some(message) = load_message {
                    lines.push(message.text.clone());
                }
            }
            lines
        }
        LifecycleScreen::Settings(_) | LifecycleScreen::Playing => return String::new(),
    };
    lines.join("\n")
}

/// Compose one load-screen action-label column: `count` copies of `label`,
/// one line per catalog row, newline-joined (S05-T02). The load screen's
/// "Play"/"Delete" columns render as two positioned buffers at their
/// hit-test rects — `load_play_rect(surface_w, 0)` / `load_delete_rect
/// (surface_w, 0)` supply the left/top, and glyphon's LINE_HEIGHT equals
/// the `row_y` step, so buffer line i lands on row i's rect top. In-string
/// padding is deliberately NOT used (the monospace pixel advance is
/// unknowable in pure code — the label/rect drift MEM056 warns about), so
/// `format_row_line` stays byte-identical. Pure function of (label, count)
/// so it is unit-testable without GPU; called per frame while the load
/// screen is open, where `StatsOverlay::update` skips the expensive
/// re-shape unless the composed string changed (it only changes when the
/// row count changes). An empty catalog composes the empty string — a
/// convention pinned by tests and moot in the render path, which skips
/// both label areas when `row_count == 0`.
fn repeated_lines(label: &str, count: usize) -> String {
    (0..count).map(|_| label).collect::<Vec<_>>().join("\n")
}

/// The active lifecycle screen's backing panel rect (T01 geometry, derived
/// per frame from the surface size so a `Resized` reflows every screen).
/// The settings screen's panel comes from `menu::panel_rect` in its own
/// render path; the Settings/Playing arms here are unreachable fallbacks.
fn screen_panel_rect(
    screen: LifecycleScreen,
    load: &LoadScreen,
    surface_w: u32,
    surface_h: u32,
) -> (f32, f32, f32, f32) {
    match screen {
        LifecycleScreen::MainMenu => main_menu_panel_rect(surface_w, surface_h),
        LifecycleScreen::Pause => pause_panel_rect(surface_w, surface_h),
        LifecycleScreen::NewGame => new_game_panel_rect(surface_w, surface_h),
        LifecycleScreen::LoadGame => load_panel_rect(surface_w, load.row_count()),
        LifecycleScreen::Settings(_) | LifecycleScreen::Playing => {
            menu::panel_rect(surface_w, surface_h)
        }
    }
}

/// The hovered target's highlight rect on the active lifecycle screen (T01
/// geometry + hit tests; half-open like the pixel grid, so stacked rows
/// share edges). `None` when the cursor is outside the window or over no
/// target. The confirm/cancel targets highlight only while a delete is
/// pending: their geometry always exists, the pending state decides
/// liveness (T01 documents this convention).
fn hovered_target_rect(
    screen: LifecycleScreen,
    load: &LoadScreen,
    cursor: Option<(f32, f32)>,
    surface_w: u32,
    surface_h: u32,
) -> Option<(f32, f32, f32, f32)> {
    let cursor = cursor?;
    match screen {
        LifecycleScreen::MainMenu => {
            let row = hovered_main_menu_row(cursor, surface_w, surface_h)?;
            let index = MAIN_MENU_ROWS.iter().position(|&r| r == row)?;
            main_menu_row_rect(surface_w, surface_h, index)
        }
        LifecycleScreen::Pause => {
            let row = hovered_pause_row(cursor, surface_w, surface_h)?;
            let index = PAUSE_ROWS.iter().position(|&r| r == row)?;
            pause_row_rect(surface_w, surface_h, index)
        }
        LifecycleScreen::NewGame => {
            let target = hovered_new_game_target(cursor, surface_w, surface_h)?;
            let index = NEW_GAME_ROWS.iter().position(|&t| t == target)?;
            new_game_row_rect(surface_w, surface_h, index)
        }
        LifecycleScreen::LoadGame => {
            let row_count = load.row_count();
            let target = hovered_load_target(cursor, surface_w, surface_h, row_count)?;
            match target {
                LoadTarget::Row(index) => load_row_rect(surface_w, surface_h, index, row_count),
                LoadTarget::Play(index) => Some(load_play_rect(surface_w, index)),
                LoadTarget::Delete(index) => Some(load_delete_rect(surface_w, index)),
                LoadTarget::Back => Some(load_back_rect(surface_w, row_count)),
                LoadTarget::ConfirmDelete => load
                    .is_delete_pending()
                    .then(|| load_confirm_rect(surface_w, row_count)),
                LoadTarget::CancelDelete => load
                    .is_delete_pending()
                    .then(|| load_cancel_rect(surface_w, row_count)),
            }
        }
        LifecycleScreen::Settings(_) | LifecycleScreen::Playing => None,
    }
}

/// Window-title suffix while a screen is open (empty in gameplay), so the
/// title reflects app-state routing. Optional polish, not a success
/// criterion.
fn screen_title_tag(screen: LifecycleScreen) -> &'static str {
    match screen {
        LifecycleScreen::MainMenu => " | Main Menu",
        LifecycleScreen::NewGame => " | New Game",
        LifecycleScreen::LoadGame => " | Load Game",
        LifecycleScreen::Pause => " | Paused",
        LifecycleScreen::Settings(_) => " | Settings",
        LifecycleScreen::Playing => "",
    }
}

/// Click-to-direction mapping for the stepped value rows: left click steps
/// the value up, right click steps it down; other buttons don't adjust.
fn adjust_direction(button: MouseButton) -> Option<AdjustDirection> {
    match button {
        MouseButton::Left => Some(AdjustDirection::Up),
        MouseButton::Right => Some(AdjustDirection::Down),
        _ => None,
    }
}

/// Save settings atomically (tmp-sibling + rename) and surface an
/// io::Error as an in-menu message instead of swallowing it (D006: the
/// crate stays log-free; failure data is displayed, never dropped).
/// Free function with disjoint args (not a `&mut self` method) so both
/// the whole-`self` `menu_click` call site and the keyboard rebind path
/// can call it while the `gpu`/`window` field borrows are live (P005).
/// The path is the caller-injected `GameApp::settings_path` (S04-T02:
/// runtime behavior is identical — `DEFAULT_SETTINGS_PATH` — but a
/// headless test can point it at a tempdir so the crate root is never
/// written).
fn save_settings_or_warn(menu: &mut menu::SettingsMenu, settings: &Settings, path: &Path) {
    if let Err(err) = settings.save(path) {
        menu.set_message(format!("could not save settings: {err}"));
    }
}

/// Route one gameplay key event through the loaded keybind map (S04-T03).
/// Returns whether `code` is a bound gameplay key. Press sets the
/// `InputState` flag, release clears it — both transitions route through
/// the same map, preserving the pre-rebind press/release semantics exactly.
/// NOTE: only the single bound `crouch_or_down` / `sprint` key triggers —
/// the old ShiftLeft|ShiftRight / ControlLeft|ControlRight pairs were
/// narrowed on purpose when keybinds became user-rebindable (see the
/// `Keybinds` doc comment in src/settings for the rationale). Duplicate
/// bindings in a hand-edited settings file resolve by first-match priority
/// in `Keybinds` declaration order, keeping routing deterministic.
fn apply_gameplay_key(
    input: &mut InputState,
    keybinds: &Keybinds,
    code: KeyCode,
    pressed: bool,
) -> bool {
    if code == keybinds.forward {
        input.forward = pressed;
    } else if code == keybinds.backward {
        input.backward = pressed;
    } else if code == keybinds.left {
        input.left = pressed;
    } else if code == keybinds.right {
        input.right = pressed;
    } else if code == keybinds.jump_or_up {
        input.jump_or_up = pressed;
    } else if code == keybinds.crouch_or_down {
        input.crouch_or_down = pressed;
    } else if code == keybinds.sprint {
        input.sprint = pressed;
    } else {
        return false;
    }
    true
}

/// Window-dependent GPU stack, initialized together in `resumed()`.
struct Ready {
    ctx: RenderContext,
    renderer: VoxelRenderer,
    render_store: ChunkRenderStore,
    /// GPU meshes for the menu vista world (M004/S02). Deliberately separate
    /// from `render_store`: the vista and the player session are distinct
    /// worlds, and a session swap must never clear the menu's GPU state.
    vista_store: ChunkRenderStore,
    ui: UiRenderer,
    stats: StatsOverlay,
    /// Settings-menu text buffer (same change-gated pattern as `stats`):
    /// drawn in the same UI pass, after the stats text area.
    menu_text: StatsOverlay,
    /// Vista text title (M004/S04): the large "OpenCraft" label shown on the
    /// main-menu cluster when the active pack has no logo. Sized with
    /// `logo::TITLE_FONT_SIZE`/`TITLE_LINE_HEIGHT`; change-gated like
    /// `menu_text`.
    title_text: StatsOverlay,
    /// Load-screen "Play" label column (S05-T02): one "Play" line per
    /// catalog row, drawn in the same UI pass after `menu_text`, at
    /// `load_play_rect`'s left/top so buffer line i lands on row i's
    /// hit-test rect. Same change-gated pattern as `menu_text` — the
    /// text only re-shapes when the row count changes.
    load_play_text: StatsOverlay,
    /// Load-screen "Delete" label column (S05-T02): same as
    /// `load_play_text`, positioned at `load_delete_rect`'s left/top.
    load_delete_text: StatsOverlay,
    /// Per-slot hotbar count labels (S03-T04): one change-gated buffer per
    /// slot, positioned at the same `hotbar_slot_rects` the outlines use.
    hotbar_counts: [StatsOverlay; HOTBAR_SLOTS],
    /// Inventory/crafting panel labels (S03-T04): one change-gated buffer per
    /// possible label, sized once to the pure `MAX_LABELS` bound. Only the
    /// first `n` buffers are drawn for an `n`-label frame; a closed screen
    /// draws none and never re-shapes anything.
    inventory_labels: Vec<StatsOverlay>,
}

/// Bounded accelerated-demo watch (S02-T04): `--time-scale` is a test seam
/// for observing a full day cycle. Sums the game-time ticks actually
/// advanced by the frame loop and reports completion once one whole
/// `DAY_TICKS` cycle has elapsed. Frame-rate independent because it reads
/// the clock, not the wall time.
#[derive(Debug)]
struct DemoCycleWatch {
    remaining_ticks: u32,
    last_time_of_day: u32,
}

impl DemoCycleWatch {
    fn new(time_of_day: u32) -> Self {
        Self {
            remaining_ticks: daycycle::DAY_TICKS,
            last_time_of_day: time_of_day % daycycle::DAY_TICKS,
        }
    }

    /// Feed one frame's time of day. Returns true exactly when the summed
    /// advance reaches a full cycle (wrap-safe at the cycle boundary).
    fn observe(&mut self, time_of_day: u32) -> bool {
        let time_of_day = time_of_day % daycycle::DAY_TICKS;
        let delta =
            (time_of_day + daycycle::DAY_TICKS - self.last_time_of_day) % daycycle::DAY_TICKS;
        self.last_time_of_day = time_of_day;
        self.remaining_ticks = self.remaining_ticks.saturating_sub(delta);
        self.remaining_ticks == 0
    }
}

/// Arm/observe the bounded accelerated demo in one place, shared by the
/// RedrawRequested pass and its headless tests. Never arms at the default
/// 1.0 scale (a normal run is unbounded) and never counts paused frames
/// (the clock itself is frozen there).
fn observe_demo_cycle(
    watch: &mut Option<DemoCycleWatch>,
    time_scale: f32,
    playing: bool,
    time_of_day: u32,
) -> bool {
    if !playing || time_scale <= DEFAULT_TIME_SCALE {
        return false;
    }
    match watch {
        Some(watch) => watch.observe(time_of_day),
        None => {
            *watch = Some(DemoCycleWatch::new(time_of_day));
            false
        }
    }
}

/// Extra wall-clock allowance on top of one accelerated cycle before the
/// bounded demo shuts down even if no redraws are being delivered.
const DEMO_SHUTDOWN_GRACE_SECS: f32 = 5.0;

/// Wall-clock safety net for a bounded accelerated run: one full cycle at
/// the given scale plus a small grace for startup. The frame-driven
/// [`DemoCycleWatch`] is the primary completion signal; this deadline keeps
/// the run bounded even if the compositor stops delivering redraws.
/// `None` at the default scale (a normal run is unbounded).
fn demo_shutdown_deadline(time_scale: f32) -> Option<Instant> {
    if time_scale <= DEFAULT_TIME_SCALE {
        return None;
    }
    let cycle_secs = daycycle::DAY_SECONDS / time_scale;
    Some(Instant::now() + Duration::from_secs_f32(cycle_secs + DEMO_SHUTDOWN_GRACE_SECS))
}

struct GameApp {
    window: Option<Arc<Window>>,
    gpu: Option<Ready>,
    /// The active world session (S03-T02): world + player + camera + the
    /// save path every save site writes to. One always-present session,
    /// swapped atomically at `start_world_session`.
    session: WorldSession,
    /// The menu vista's own demo world (M004/S02): built directly with the
    /// fixed `vista::VISTA_SEED`, never a `WorldSession`, never saved and
    /// never swapped through `start_world_session`. Streamed only while
    /// `vista::vista_visible` reports a main-menu-cluster screen.
    vista_world: WorldManager,
    /// Construction epoch for the vista's procedural yaw drift: drift is
    /// `elapsed()` from this single Instant, so it survives menu<->game
    /// transitions with no per-frame accumulation or reset bookkeeping.
    vista_epoch: Instant,
    input: InputState,
    cursor_grabbed: bool,
    /// The hold-to-mine accumulator (S02): pure state machine driven once per
    /// frame from the RedrawRequested pass. Any reset boundary below clears it
    /// so a menu/focus/session change never inherits partial progress.
    mining: MiningState,
    /// Fail-soft sound engine (M005/S04). Construction never blocks launch;
    /// `play` is a silent no-op when no device was opened. Every call site is
    /// confined to `window_event`/`menu_click`/`screen_click` arms so the
    /// headless tests (which drive free helpers and `GameApp::new` directly)
    /// never emit sound.
    audio: AudioEngine,
    /// Whether the left mouse button is physically held from a gameplay
    /// press. Set only after the screen routing and click-to-grab early
    /// returns, cleared on release, focus loss, and screen/session changes.
    left_mouse_held: bool,
    /// Inventory/crafting screen state (S03-T03): mode + table attachment from
    /// the pure T01 model. Open only while the lifecycle screen is Playing —
    /// the panel is a gameplay surface, not a lifecycle screen.
    inventory: InventoryScreen,
    /// Last inventory-click status line. Read by the T04 panel renderer as the
    /// footer message; `None` until the first click of a session.
    inventory_status: Option<String>,
    /// Press-drag-release origin (S03-T03): the target a Left press picked a
    /// stack from. Cleared by any non-pick press, release, close, and screen
    /// change so a stale origin can never move a later stack.
    inventory_drag: Option<InventoryTarget>,
    last_frame: Instant,
    title_timer: Instant,
    last_saved_badge: Option<Instant>,
    frame_count: u32,
    last_overlay_text: String,
    /// Last reported cursor position in physical pixels (None when the
    /// cursor left the window). Same px space as ctx.config, cosmic-text
    /// buffers, and the quad renderer's API — S04 hit-testing consumes it.
    cursor_pos: Option<(f32, f32)>,
    /// Loaded player settings (settings.toml, or defaults on any load
    /// failure): the single in-memory source of truth the menu edits.
    settings: Settings,
    /// Startup load warnings, surfaced as the menu's initial message on the
    /// first open (MEM009/D006: warnings are data displayed in-menu, never
    /// logged — the crate stays log-free).
    startup_warnings: Vec<SettingsWarning>,
    /// Whether the startup warnings have already been shown once.
    startup_warnings_shown: bool,
    /// The settings menu state machine (pure logic from `ui::menu`).
    /// Invariant (S02-T02): open exactly while the lifecycle screen is
    /// Settings — every transition goes through `enter_screen`/
    /// `enter_playing`, which keep the two in sync.
    menu: menu::SettingsMenu,
    /// App-state routing: which screen owns input and rendering (the S01
    /// pure lifecycle contracts, applied by S02-T02).
    lifecycle: LifecycleState,
    /// Filesystem world catalog over the default saves directory (S02-T03
    /// catalog wiring): the load screen's rows and the new-game duplicate
    /// check read through it. Construction is path-only (no I/O), so the
    /// CLI-bypass launch path never touches the disk before gameplay.
    catalog: WorldCatalogFs,
    /// New-game screen state (T01 pure model). The duplicate check on
    /// Create fetches FRESH rows from the catalog at Create time (the
    /// S02-T03 decision — documented on `attempt_create`), so it is
    /// truthful against the real saves/*.mcvx.
    new_game: NewGameScreen,
    /// Load-game screen state (T01 pure model); rows refresh from the
    /// catalog on every entry into the screen and after every delete
    /// (S02-T03 wiring — `refresh_load_rows` / `confirm_pending_delete`).
    load: LoadScreen,
    /// The load screen's transient message (inline errors; the T01
    /// LoadScreen owns no message slot of its own — same tick-counter
    /// pattern as the settings menu, ticked by
    /// `tick_transient_message`).
    load_message: Option<MenuMessage>,
    /// The main menu's transient message (S03-T03): a Save-and-Quit save
    /// failure surfaces here as a user-readable "could not save world:
    /// ..." line while the session's unsaved-changes flag stays set for
    /// retry; same tick-counter pattern, ticked alongside the other
    /// message slots by `tick_transient_message`.
    main_menu_message: Option<MenuMessage>,
    /// Timestamp of the last message-expiry tick (bounded at ~60 Hz; see
    /// `MENU_TICK_INTERVAL`).
    message_tick_timer: Instant,
    /// The path every settings SAVE writes to (S04-T02): the injected
    /// twin of `session.save_path` for settings — the same extraction
    /// pattern as `startup_session`/`cli_has_world_intent`. Runtime
    /// behavior is identical (`DEFAULT_SETTINGS_PATH`, i.e. the process
    /// cwd); tests point it at a tempdir so the ExitGame/CloseRequested
    /// settings saves are provable headless without writing the crate
    /// root. Loading still reads the same path at startup.
    settings_path: PathBuf,
    /// Root directory pack folders are discovered under (M004/S03):
    /// `packs/` relative to the process cwd, the same convention as
    /// `saves/` and `settings.toml`. Injected by tests like
    /// `settings_path`.
    packs_root: PathBuf,
    /// Sorted pack names discovered under `packs_root` at startup. Cycling
    /// walks this list; a pack authored while the game is running appears
    /// on the next launch (no filesystem polling in the menu).
    available_packs: Vec<String>,
    /// The active pack's loaded layers and presentation data; `None` is
    /// the built-in procedural Default pack (MEM113). Never holds a
    /// half-loaded pack: `load_pack_by_name` always returns usable layers.
    current_pack: Option<LoadedPack>,
    /// Clock acceleration from `--time-scale` (S02-T04): an app-level
    /// setting like FOV, NOT session state — the session clock stays
    /// world-swappable through `start_world_session`, and every session
    /// carries its own correct `time_of_day`. Passed per frame to
    /// `GameClock::tick`, so the clock itself stays config-free.
    time_scale: f32,
    /// Bounded accelerated-demo watch (S02-T04): armed on the first Playing
    /// frame of an accelerated run, cleared when one full cycle has been
    /// observed. `demo_finished` is then latched so `about_to_wait` can
    /// perform the shutdown (the RedrawRequested region already holds
    /// disjoint `gpu`/`window` borrows).
    demo_cycle: Option<DemoCycleWatch>,
    demo_finished: bool,
    /// Wall-clock safety net for the bounded accelerated demo (S02-T04):
    /// armed only when the launch starts directly in Playing with an
    /// accelerated scale. `None` for every normal run.
    demo_shutdown_deadline: Option<Instant>,
}

impl GameApp {
    /// Clean shutdown shared by the window-close path and the bounded
    /// accelerated demo: persist the world and settings, then leave the
    /// loop. Save errors are ignored — the process is exiting and there is
    /// no menu left to display them in.
    fn shutdown(&mut self, event_loop: &ActiveEventLoop) {
        let _ = save_current_world(&mut self.session);
        let _ = self.settings.save(&self.settings_path);
        event_loop.exit();
    }

    fn new(cli: CliConfig, mut session: WorldSession) -> Self {
        // Startup settings load: never blocks launch — a missing or corrupt
        // settings.toml yields defaults plus structured warnings, surfaced
        // as menu text on the first open.
        let settings_path = Path::new(DEFAULT_SETTINGS_PATH).to_path_buf();
        let (settings, mut startup_warnings) = load_with_warnings(&settings_path);
        // Pack discovery and startup apply (M004/S03): the persisted choice
        // is re-applied on every launch. A missing/deleted pack folder still
        // yields a fully usable procedural pack plus one warning naming it,
        // surfaced through the existing first-Settings-open message path
        // (D031/MEM121, MEM009: warnings are data, never logs).
        let packs_root = Path::new(pack::PACKS_DIR).to_path_buf();
        let available_packs = pack::available_pack_names(&packs_root);
        let (current_pack, pack_warnings) =
            load_startup_pack(&packs_root, settings.texture_pack.as_deref());
        startup_warnings.extend(pack_warnings);
        // Startup sound overrides (M005/S05): resolved independently of the
        // texture result, so a pack whose layers partially failed still
        // supplies its sounds. The never-fail loader returns a complete
        // buffer set; its warnings merge into the same one-time Settings-entry
        // message path as every other startup warning (MEM009/D054). The
        // engine is constructed first because `set_buffers` is device-free.
        let mut audio = new_audio_engine();
        let (startup_sounds, sound_warnings) =
            load_startup_sounds(&packs_root, settings.texture_pack.as_deref());
        audio.set_buffers(startup_sounds);
        startup_warnings.extend(sound_warnings);
        // Startup screen (app-state routing): a CLI world intent — the
        // exact three-flag disjunction in `cli_has_world_intent` — enters
        // gameplay directly, bypassing every menu screen (preserving every
        // flagged launch, including both smoke scripts' --new/--seed/--save
        // runs); a bare launch opens the main menu over the loaded world.
        // Cursor state follows the screen: menu screens start with the
        // cursor released so they are clickable immediately.
        let startup_screen = if cli_has_world_intent(&cli) {
            LifecycleScreen::Playing
        } else {
            LifecycleScreen::MainMenu
        };
        // Bounded accelerated demo (S02-T04): a direct-launch gameplay run
        // with `--time-scale > 1` carries a wall-clock deadline as a safety
        // net. Menu-first launches stay unbounded until gameplay begins (the
        // frame-driven cycle watch still completes them).
        let demo_shutdown_deadline = if startup_screen == LifecycleScreen::Playing {
            demo_shutdown_deadline(cli.time_scale)
        } else {
            None
        };
        // The loaded Settings are the single source of truth for video
        // parameters: FOV applies to the session's camera at startup, and
        // render distance to its world (defaults are 75 degrees / 16
        // chunks either way, so this is behavior-neutral). The session's
        // camera arrives already synced to its player by the WorldSession
        // constructors. Settings win over a save's stored render distance:
        // the setting is global and the world save follows it from now on.
        session.camera.fov_y_radians = settings.fov_degrees.to_radians();
        session.world.set_render_distance(settings.render_distance);
        // The vista is a second, independent world with the same
        // settings-driven render distance (the session's is a source of
        // truth; the vista is never a WorldSession). `ChunkEditMap::default()`
        // matches `WorldManager::new`; the fixed seed is the demo contract.
        let mut vista_world = WorldManager::with_seed_and_edits(
            settings.render_distance,
            vista::VISTA_SEED,
            ChunkEditMap::default(),
        );
        vista_world.set_render_distance(settings.render_distance);
        Self {
            window: None,
            gpu: None,
            session,
            vista_world,
            vista_epoch: Instant::now(),
            input: InputState::default(),
            cursor_grabbed: startup_screen == LifecycleScreen::Playing,
            mining: MiningState::new(),
            audio,
            left_mouse_held: false,
            inventory: InventoryScreen::new(),
            inventory_status: None,
            inventory_drag: None,
            last_frame: Instant::now(),
            title_timer: Instant::now(),
            last_saved_badge: None,
            frame_count: 0,
            last_overlay_text: String::new(),
            cursor_pos: None,
            settings,
            startup_warnings,
            startup_warnings_shown: false,
            menu: menu::SettingsMenu::new(),
            lifecycle: LifecycleState::from_screen(startup_screen),
            catalog: WorldCatalogFs::for_default_saves_dir(),
            new_game: NewGameScreen::new(),
            load: LoadScreen::new(),
            load_message: None,
            main_menu_message: None,
            message_tick_timer: Instant::now(),
            settings_path,
            packs_root,
            available_packs,
            current_pack,
            time_scale: cli.time_scale,
            demo_cycle: None,
            demo_finished: false,
            demo_shutdown_deadline,
        }
    }

    /// Route one mouse click while the menu is open: hit-test the row under
    /// the cursor and apply that row's action. Clicks outside the panel are
    /// ignored (no accidental gameplay); a `None` cursor position (cursor
    /// left the window) is ignored too. Every accepted change saves
    /// immediately (crash-safe, never deferred). Sensitivity, FOV (live to
    /// the camera's pub `fov_y_radians`), and fullscreen (live via the
    /// `window` param) apply HERE; the vsync row only toggles + saves here
    /// — its surface reconfigure needs the RenderContext, whose borrow
    /// cannot coexist with this whole-`self` method, so the caller applies
    /// it right after (P005).
    /// NOTE (S02-T02): takes `event_loop` because the Close row now
    /// dispatches `SettingsIntent::Back` through the lifecycle state.
    fn menu_click(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Window,
        button: MouseButton,
        cursor: Option<(f32, f32)>,
        surface_w: u32,
        surface_h: u32,
    ) {
        let Some(cursor) = cursor else { return };
        let Some(row) = menu::hovered_row(cursor, surface_w, surface_h) else {
            return;
        };
        // One UI click per ACTING row click. The Volume row is excluded here
        // because its arm below plays its own click at the NEW volume — that
        // is the row's audible live-apply proof. The stepped rows only act on
        // the Left/Right adjust buttons, so other buttons stay silent, as do
        // clicks outside the panel (returned above).
        let acts = match row {
            MenuRow::Volume => false,
            MenuRow::MouseSensitivity | MenuRow::Fov | MenuRow::RenderDistance => {
                adjust_direction(button).is_some()
            }
            _ => true,
        };
        if acts {
            self.audio.play(SoundSlot::UiClick, self.settings.volume);
        }
        match row {
            MenuRow::Bind(action) => self.menu.begin_rebind(action),
            MenuRow::MouseSensitivity => {
                if let Some(direction) = adjust_direction(button) {
                    // Clamp BEFORE storing: in-memory Settings are unclamped
                    // (only load clamps), so the T01 helper is mandatory here.
                    self.settings.mouse_sensitivity =
                        menu::adjust_sensitivity(self.settings.mouse_sensitivity, direction);
                    save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
                }
            }
            MenuRow::Fov => {
                if let Some(direction) = adjust_direction(button) {
                    self.settings.fov_degrees =
                        menu::adjust_fov(self.settings.fov_degrees, direction);
                    // Live apply: Camera reads `fov_y_radians` every frame
                    // in view_proj, so updating the pub field updates the
                    // projection with no reconstruction.
                    self.session.camera.fov_y_radians = self.settings.fov_degrees.to_radians();
                    save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
                }
            }
            MenuRow::RenderDistance => {
                if let Some(direction) = adjust_direction(button) {
                    self.apply_render_distance_adjust(direction);
                }
            }
            MenuRow::Fullscreen => {
                self.settings.fullscreen = !self.settings.fullscreen;
                // Live apply: borderless fullscreen on, windowed off. The
                // resulting Resized flows through the existing ctx.resize
                // + overlay re-shape path; the menu layout derives per
                // frame from ctx.config.width/height so it reflows
                // automatically — no cached rects survive a Resized.
                if self.settings.fullscreen {
                    window.set_fullscreen(Some(Fullscreen::Borderless(None)));
                } else {
                    window.set_fullscreen(None);
                }
                save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
            }
            MenuRow::Vsync => {
                self.settings.vsync = !self.settings.vsync;
                // The live present-mode swap happens in the caller (needs
                // the RenderContext; see the doc comment above).
                save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
            }
            MenuRow::Volume => {
                if let Some(direction) = adjust_direction(button) {
                    // Clamp BEFORE storing: in-memory Settings are unclamped
                    // (only load clamps), so the menu helper plus the shared
                    // bounds are mandatory here. Per-play volume reads this
                    // field, so the next sound already plays at the new level.
                    let next = menu::adjust_volume(i32::from(self.settings.volume), direction);
                    self.settings.volume = next.clamp(VOLUME_MIN, VOLUME_MAX) as u8;
                    save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
                    // Audible live-apply proof: per-play volume reads the
                    // setting, so this click already uses the level just
                    // stored. Exactly one click per Volume adjustment (the
                    // generic pre-match click skips this row).
                    self.audio.play(SoundSlot::UiClick, self.settings.volume);
                }
            }
            MenuRow::TexturePack => {
                // Cycling is not an adjust row: any button advances one step.
                // The GPU re-upload needs the RenderContext, so it happens in
                // the caller right after this call (same P005 split as vsync).
                self.apply_texture_pack_cycle();
            }
            MenuRow::Close => {
                // Close now dispatches Back through the lifecycle state
                // (S02-T02): return to the caller screen — MainMenu or
                // Pause — instead of straight to gameplay. The cursor
                // stays released across settings entry/return from menu
                // contexts; gameplay re-grabs on its own click path.
                self.dispatch_screen_intent(
                    event_loop,
                    LifecycleIntent::Settings(SettingsIntent::Back),
                );
            }
        }
    }

    /// Apply one Render Distance row adjustment: clamp/store the setting,
    /// live-apply it to BOTH worlds (player session and menu vista), and
    /// save. The session and vista loops both read `render_distance()`
    /// every frame, so updating the managers immediately changes load
    /// radius and fog distance. Extracted from `menu_click` so the
    /// headless tests can drive the exact click semantics without a live
    /// `ActiveEventLoop`/`Window` (menu_click's other arms need those
    /// only for Close/Fullscreen).
    fn apply_render_distance_adjust(&mut self, direction: AdjustDirection) {
        self.settings.render_distance =
            menu::adjust_render_distance(self.settings.render_distance, direction);
        self.session
            .world
            .set_render_distance(self.settings.render_distance);
        self.vista_world
            .set_render_distance(self.settings.render_distance);
        save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
    }

    /// Apply one Texture Pack row click: advance through the discovered
    /// packs (Default -> first -> ... -> Default), load the new pack, surface
    /// any load warnings in the menu message slot (MEM009), and persist the
    /// choice. Extracted from `menu_click` so the headless tests can drive
    /// the exact click semantics without a live event loop; the live GPU
    /// re-upload stays in the caller (P005 borrow split).
    fn apply_texture_pack_cycle(&mut self) {
        let next =
            menu::cycle_texture_pack(self.settings.texture_pack.as_deref(), &self.available_packs);
        self.settings.texture_pack = next.clone();
        match next {
            None => {
                // Cycling back to Default restores the synthesized sounds
                // (mirrors MEM135's procedural texture re-upload).
                self.current_pack = None;
                self.audio.set_buffers(default_buffers());
            }
            Some(name) => {
                let loaded = pack::load_pack_by_name(&self.packs_root, &name);
                // Sounds load independently of texture success and apply live
                // on the same click (the helper holds no GPU borrow).
                let (buffers, sound_warnings) =
                    pack::sound::load_sound_overrides(&self.packs_root.join(&name));
                self.audio.set_buffers(buffers);
                let mut warnings = loaded.warnings.clone();
                warnings.extend(sound_warnings);
                if !warnings.is_empty() {
                    let lines = menu::format_warnings(&warnings).join(" | ");
                    self.menu.set_message(lines);
                }
                self.current_pack = Some(loaded);
            }
        }
        save_settings_or_warn(&mut self.menu, &self.settings, &self.settings_path);
    }

    /// Enter a lifecycle screen — the one transition entry point every
    /// navigation funnels through. On every entry into a non-Playing screen:
    /// reset held inputs (no stuck sprint/jump while a screen owns the
    /// input) and release the cursor so it can hover and click screen
    /// targets. The settings-menu state machine stays open exactly while
    /// the screen is Settings, surfacing the startup load warnings once,
    /// from either caller (MEM009/D006: data displayed in-menu, never
    /// logged — the crate stays log-free).
    fn enter_screen(&mut self, screen: LifecycleScreen) {
        if screen == LifecycleScreen::Playing {
            self.enter_playing();
            return;
        }
        // Leaving the main menu retires its transient message (the
        // S03-T03 Save-and-Quit failure note described the main-menu
        // state the player navigated away from); navigating to any other
        // screen starts that screen's state clean, and the load screen's
        // row refresh clears its own message the same way.
        if screen != LifecycleScreen::MainMenu {
            self.main_menu_message = None;
        }
        self.input.reset();
        self.left_mouse_held = false;
        self.mining.reset();
        // The inventory panel is a Playing-only surface. Any lifecycle screen
        // entry drops it (with its transient drag/status) so a stale panel can
        // never keep absorbing clicks meant for a menu.
        self.inventory.close();
        self.inventory_status = None;
        self.inventory_drag = None;
        self.cursor_grabbed = false;
        if let Some(window) = self.window.as_ref() {
            set_cursor_grab(window, false);
        }
        if matches!(screen, LifecycleScreen::Settings(_)) {
            self.menu.open();
            if !self.startup_warnings_shown {
                self.startup_warnings_shown = true;
                if !self.startup_warnings.is_empty() {
                    let lines = menu::format_warnings(&self.startup_warnings).join(" | ");
                    self.menu.set_message(lines);
                }
            }
        } else if self.menu.is_open() {
            self.menu.close();
        }
        // Entering the load screen refreshes its rows from the real saves
        // directory (S02-T03 catalog wiring): the screen must be truthful
        // about what is on disk, and the row replacement cancels any stale
        // pending delete (a delete must never fire from stale row
        // identity — `set_rows` cancels it).
        if screen == LifecycleScreen::LoadGame {
            self.refresh_load_rows();
        }
        self.lifecycle = LifecycleState::from_screen(screen);
    }

    /// Return to gameplay: re-grab the cursor (settings/pause return paths)
    /// and close any open settings menu, keeping the sync invariant.
    fn enter_playing(&mut self) {
        // Defensive: a hold or partial progress must never survive a trip
        // through a menu and back into gameplay.
        self.left_mouse_held = false;
        self.mining.reset();
        self.cursor_grabbed = true;
        if let Some(window) = self.window.as_ref() {
            set_cursor_grab(window, true);
        }
        if self.menu.is_open() {
            self.menu.close();
        }
        self.lifecycle = LifecycleState::from_screen(LifecycleScreen::Playing);
    }

    /// Swap in a new world session atomically — the ONE boundary a world
    /// start flows through (S03-T02, wired by Create/Play in S03-T03).
    /// The swap drops the outgoing session's CPU state wholesale, clears
    /// every GPU chunk mesh via `ChunkRenderStore::clear` (a freshly
    /// started world must never render the outgoing world's meshes — the
    /// streaming pass re-uploads the new world's chunks as they load),
    /// and resets held inputs (a swap can only be initiated from a menu
    /// screen, but resetting here makes the boundary self-contained). The
    /// loaded Settings' FOV applies to the new session's camera here (the
    /// same source-of-truth rule as `GameApp::new`), and the camera arrives
    /// pre-synced to the new player by the WorldSession constructors
    /// (`new_session`/`session_from_save` both sync it).
    ///
    /// No outgoing-session autosave happens here, by design: only Playing
    /// can accumulate unsaved chunk edits, and every path out of Playing
    /// (Pause → Save and Quit; Pause → Exit Game; CloseRequested) saves
    /// the session first through `save_current_world`. Menu Create/Play
    /// can only be reached from a screen, never from Playing, so a session
    /// swapped here was already saved (or never edited). If a future path
    /// ever swaps directly out of Playing, that path owns its save.
    fn start_world_session(&mut self, mut session: WorldSession) {
        // The loaded Settings stay the single source of truth for video
        // parameters across a session swap too (GameApp::new does the
        // same at startup; defaults are 75 degrees / 16 chunks either way).
        session.camera.fov_y_radians = self.settings.fov_degrees.to_radians();
        session
            .world
            .set_render_distance(self.settings.render_distance);
        self.session = session;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.render_store.clear();
        }
        self.input.reset();
        self.left_mouse_held = false;
        self.mining.reset();
        self.inventory.close();
        self.inventory_status = None;
        self.inventory_drag = None;
    }

    /// Apply a lifecycle decision from the S01 state machine (navigation,
    /// cursor/input resets, settings-menu sync, save-and-quit, exit save).
    /// Returns whether the caller should exit the event loop: the ExitGame
    /// arm performs its world+settings save here but defers
    /// `event_loop.exit()` to the caller, so the create, play, and
    /// save-and-quit paths need no `&ActiveEventLoop` and are unit-testable
    /// without a live loop. `StartNewGame`/`LoadWorld` are no-ops here by
    /// design: the create and play paths dispatch directly and consume the
    /// decision alongside the `WorldSession` they prepared (the decision
    /// carries no session data).
    fn apply_lifecycle_decision(&mut self, decision: LifecycleDecision) -> bool {
        match decision {
            LifecycleDecision::Navigate(screen) => self.enter_screen(screen),
            LifecycleDecision::ResumeGame => self.enter_playing(),
            LifecycleDecision::StartNewGame | LifecycleDecision::LoadWorld => {}
            LifecycleDecision::SaveAndQuit => {
                // Save the session's world to the session's own save path
                // (S03-T02: every save site is session-tied), then return
                // to the main menu. On failure: surface a transient
                // main-menu message (warnings-as-data, D006/MEM009 — the
                // crate stays log-free) and KEEP the unsaved-changes flag
                // set for the next save opportunity (retry, or a later
                // save/exit path saves it again).
                if let Err(err) = save_current_world(&mut self.session) {
                    self.main_menu_message = Some(MenuMessage {
                        text: format!("could not save world: {err}"),
                        ticks_left: menu::MESSAGE_TICKS,
                    });
                }
                // Drop every session GPU chunk mesh at menu return (the
                // same boundary rule as `start_world_session`): the menu
                // renders the vista from its own store, and leaving the
                // outgoing world's meshes resident both drew the old world
                // behind the menu and doubled VRAM (MEM115). Clear ONLY the
                // session store — the vista store belongs to the menu.
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render_store.clear();
                }
                self.enter_screen(LifecycleScreen::MainMenu);
            }
            LifecycleDecision::ExitGame => {
                // Mirror the CloseRequested path: save the current world
                // and settings; the caller owns exiting the loop.
                let _ = save_current_world(&mut self.session);
                let _ = self.settings.save(&self.settings_path);
                return true;
            }
        }
        false
    }

    /// Dispatch a lifecycle intent through the S01 state machine and apply
    /// the resulting decision (navigation, cursor/input resets,
    /// settings-menu sync, save-and-quit, exit). Returns the decision, or
    /// `None` when the state machine rejected the intent for the current
    /// screen (a wrong-screen intent is a no-op, never a panic — the crate
    /// stays log-free; the structured `LifecycleError` is data the caller
    /// could surface, but every call site here builds its intent from the
    /// same screen it dispatches to, so rejection is unreachable).
    ///
    /// `StartNewGame` and `LoadWorld` are never produced by these call
    /// sites: a valid Create and a loadable-row Play dispatch directly
    /// (owning their session start at the `start_world_session`
    /// boundary) instead of going through this loop-taking wrapper.
    fn dispatch_screen_intent(
        &mut self,
        event_loop: &ActiveEventLoop,
        intent: LifecycleIntent,
    ) -> Option<LifecycleDecision> {
        let decision = self.lifecycle.dispatch(intent).ok()?;
        if self.apply_lifecycle_decision(decision) {
            event_loop.exit();
        }
        Some(decision)
    }

    /// Route one keyboard event to the open lifecycle screen (called only
    /// while a non-Playing, non-Settings screen is open — route-before-
    /// gameplay: the gameplay match never runs while a screen is open).
    /// Screen-level keys map through T01's pure `screen_key_action`
    /// (Escape backs out — cancelling a pending delete first on the load
    /// screen; Enter creates/confirms; Tab cycles the field); printable
    /// characters (winit `event.text`) and field-editing keys route into
    /// the focused new-game field. Releases are consumed without effect
    /// (screens hold no key state); repeats are ignored for screen-level
    /// actions but still type into the focused field (OS-driven repeat
    /// typing, bounded by `TextInput`).
    fn screen_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        code: KeyCode,
        pressed: bool,
        repeat: bool,
        text: Option<&str>,
    ) {
        if !pressed {
            return;
        }
        let screen = self.lifecycle.screen();
        let action = screen_key_action(screen, code, self.load.is_delete_pending());
        if !repeat {
            match action {
                ScreenKeyAction::Back => self.screen_back(event_loop),
                ScreenKeyAction::CancelPendingDelete => self.load.cancel_delete(),
                ScreenKeyAction::ConfirmPendingDelete => self.confirm_pending_delete(),
                ScreenKeyAction::Create => self.attempt_create(),
                ScreenKeyAction::CycleField => {
                    self.new_game.cycle_focus();
                }
                ScreenKeyAction::None => {}
            }
        }
        if action != ScreenKeyAction::None {
            // The key had screen-level meaning (Escape/Enter/Tab); its
            // text, if any, is control text and never reaches a field.
            return;
        }
        // Only the new-game screen has text fields; the other screens
        // consume printable keys as no-ops.
        if screen == LifecycleScreen::NewGame {
            match code {
                KeyCode::Backspace => {
                    self.new_game.apply(TextInputAction::Backspace);
                }
                KeyCode::Delete => {
                    self.new_game.apply(TextInputAction::Delete);
                }
                KeyCode::ArrowLeft => {
                    self.new_game.apply(TextInputAction::MoveLeft);
                }
                KeyCode::ArrowRight => {
                    self.new_game.apply(TextInputAction::MoveRight);
                }
                KeyCode::Home => {
                    self.new_game.apply(TextInputAction::Home);
                }
                KeyCode::End => {
                    self.new_game.apply(TextInputAction::End);
                }
                // Printable characters arrive as winit's logical text
                // (`event.text`): one Insert per character into the
                // focused field — bounded and control-filtered by
                // `TextInput` itself, so no character check here.
                _ => {
                    if let Some(text) = text {
                        for character in text.chars() {
                            if !character.is_control() {
                                self.new_game.apply(TextInputAction::Insert(character));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Refresh the load screen's rows from the real saves directory
    /// (S02-T03 catalog wiring): newest-first via `catalog.list_rows()`;
    /// a missing saves dir lists empty (fresh install — not an error). The
    /// transient message is cleared so stale errors never survive a
    /// refresh, and the row replacement cancels any pending delete
    /// (`set_rows` does that — a delete must never fire from stale row
    /// identity).
    fn refresh_load_rows(&mut self) {
        self.load.set_rows(self.catalog.list_rows());
        self.load_message = None;
    }

    /// Escape on a backing-out screen: navigate back through the lifecycle
    /// state (new-game/load-game → main menu; pause → Resume, the
    /// non-destructive default T01 documents). The main menu is the root
    /// screen — T01's `screen_key_action` returns `None` for it, so this
    /// is never called from there (the documented no-op).
    fn screen_back(&mut self, event_loop: &ActiveEventLoop) {
        let intent = match self.lifecycle.screen() {
            LifecycleScreen::NewGame => LifecycleIntent::NewGame(NewGameIntent::Back),
            LifecycleScreen::LoadGame => LifecycleIntent::LoadGame(LoadGameIntent::Back),
            LifecycleScreen::Pause => LifecycleIntent::Pause(PauseIntent::Resume),
            _ => return,
        };
        self.dispatch_screen_intent(event_loop, intent);
    }

    /// Enter/Create on the new-game screen: validate via T01's pure
    /// `validate_create` against FRESH catalog rows fetched at Create time
    /// (the S02-T03 decision: fresh rows over reusing the rows loaded when
    /// some earlier screen opened — a save written after the load screen
    /// opened must still collide; the load screen refreshes its own rows
    /// on every entry, so this list is validation-only and not stored),
    /// show any failure inline as the form's transient message
    /// (warnings-as-data, D006/MEM009), and start the real world session:
    /// the seed resolves through `catalog::resolve_seed_or` (a blank seed
    /// means a fresh time-derived `session::random_seed`; any other text
    /// hashes deterministically), the save path is the catalog adapter's
    /// validated `<saves_dir>/<Name>.mcvx` (no save is written here — the
    /// session persists through the normal save sites once edits exist),
    /// the `WorldSession` is built via `session::new_session`, and the S01
    /// decision `StartNewGame` (from dispatching `NewGameIntent::Create`)
    /// drives the swap through the one `start_world_session` boundary and
    /// enters Playing semantics (cursor grab via `enter_playing`; the
    /// lifecycle state is already Playing, set by the dispatch). Needs no
    /// `&ActiveEventLoop`, so the whole create path is unit-testable
    /// without a live loop.
    fn attempt_create(&mut self) {
        let name = self.new_game.name().value().to_string();
        let rows = self.catalog.list_rows();
        if let Err(err) = validate_create(&name, &rows) {
            self.new_game.set_message(err.to_string());
            return;
        }
        // Blank seed = fresh random seed; any other text = stable hash.
        let seed_form = self.new_game.seed().value().to_string();
        let seed = resolve_seed_or(&seed_form, random_seed());
        // The catalog adapter re-validates (and canonicalizes the trim of)
        // the already-valid name into <saves_dir>/<Name>.mcvx. The error
        // arm is unreachable (validate_create enforced the same rule) but
        // stays a warnings-as-data no-panic path.
        let save_path = match self.catalog.world_path(&name) {
            Ok(path) => path,
            Err(err) => {
                self.new_game.set_message(err.to_string());
                return;
            }
        };
        // The New Game row owns the season length (S02-T04): the screen's
        // cycler value, defaulting to one game day per season.
        let session =
            WorldSession::new_session(seed, save_path, self.new_game.season_length_days());
        // Unreachable: attempt_create only runs while the new-game screen is
        // open, and that screen accepts Create.
        if let Ok(LifecycleDecision::StartNewGame) = self
            .lifecycle
            .dispatch(LifecycleIntent::NewGame(NewGameIntent::Create))
        {
            self.start_world_session(session);
            self.enter_playing();
        }
    }

    /// Confirm a pending delete (Enter or the confirm target on the load
    /// screen): consume the pending row identity through the T01 machine
    /// and hand the S01-confirmed [`PendingDelete`] to the live catalog
    /// (S02-T03 catalog wiring) via the pure `apply_confirmed_delete`
    /// helper: the file is deleted, rows refresh from disk either way (a
    /// failed delete keeps the still-present row listed), and a
    /// `CatalogDeleteError` surfaces inline as the load screen's transient
    /// message (warnings-as-data, D006/MEM009). Nothing pending: a no-op
    /// (a delete can never fire without a prior request).
    fn confirm_pending_delete(&mut self) {
        let Some(pending) = self.load.confirm_delete() else {
            return;
        };
        let (rows, message) = apply_confirmed_delete(&self.catalog, &pending);
        self.load.set_rows(rows);
        self.load_message = message;
    }

    /// Play on catalog row `index` (load screen): probe the row through the
    /// live catalog (S02-T03 wiring) — a corrupt row and a file that
    /// vanished between the listing and the Play click both surface their
    /// `CatalogLoadError` display inline as the load screen's transient
    /// message and the screen stays put (warnings-as-data, D006/MEM009:
    /// degraded or vanished rows are displayed, never swallowed — the
    /// crate stays log-free). A loadable row starts the real world session:
    /// the persisted `WorldSaveData` (seed, player state, chunk edits)
    /// rebuilds the session via `session::session_from_save` at the row's
    /// own file path (`<saves_dir>/<file_name>`), and the S01 decision
    /// `LoadWorld` (from dispatching `LoadGameIntent::Load`) drives the
    /// swap through the one `start_world_session` boundary and enters
    /// Playing semantics (cursor grab via `enter_playing`; the lifecycle
    /// state is already Playing, set by the dispatch). An out-of-range
    /// index is ignored (rows only shrink via a confirmed delete, which
    /// cancels any pending identity first). Needs no `&ActiveEventLoop`,
    /// so every failure mode is unit-testable with a tempdir catalog.
    fn play_row(&mut self, index: usize) {
        let Some(row) = self.load.rows().get(index).cloned() else {
            return;
        };
        let save = match self.catalog.load_row(&row) {
            Ok(save) => save,
            Err(err) => {
                self.load_message = Some(MenuMessage {
                    text: err.to_string(),
                    ticks_left: menu::MESSAGE_TICKS,
                });
                return;
            }
        };
        let session =
            WorldSession::session_from_save(save, self.catalog.saves_dir().join(&row.file_name));
        // Unreachable: play_row only runs while the load screen is
        // open, and that screen accepts Load.
        if let Ok(LifecycleDecision::LoadWorld) = self
            .lifecycle
            .dispatch(LifecycleIntent::LoadGame(LoadGameIntent::Load))
        {
            self.start_world_session(session);
            self.enter_playing();
        }
    }

    /// Route one mouse click while a lifecycle screen (not Settings, not
    /// gameplay) is open: hit-test the screen's targets via T01 geometry
    /// and apply the target's action (main-menu rows, pause rows,
    /// new-game randomize/create/back, load rows with their Play and
    /// Delete sub-targets, confirm/cancel while a delete is pending).
    /// Left button only; clicks outside targets are ignored (no
    /// accidental gameplay); a `None` cursor position (cursor left the
    /// window) is ignored too.
    fn screen_click(
        &mut self,
        event_loop: &ActiveEventLoop,
        button: MouseButton,
        surface_w: u32,
        surface_h: u32,
    ) {
        let Some(cursor) = self.cursor_pos else {
            return;
        };
        if button != MouseButton::Left {
            return;
        }
        match self.lifecycle.screen() {
            LifecycleScreen::MainMenu => {
                let Some(row) = hovered_main_menu_row(cursor, surface_w, surface_h) else {
                    return;
                };
                let intent = match row {
                    MainMenuRow::NewGame => LifecycleIntent::MainMenu(MainMenuIntent::NewGame),
                    MainMenuRow::LoadGame => LifecycleIntent::MainMenu(MainMenuIntent::LoadGame),
                    MainMenuRow::Settings => LifecycleIntent::MainMenu(MainMenuIntent::Settings),
                    // Exit mirrors the CloseRequested path via
                    // dispatch_screen_intent (save world + settings,
                    // exit the loop).
                    MainMenuRow::ExitGame => LifecycleIntent::MainMenu(MainMenuIntent::ExitGame),
                };
                self.dispatch_screen_intent(event_loop, intent);
                self.audio.play(SoundSlot::UiClick, self.settings.volume);
            }
            LifecycleScreen::Pause => {
                let Some(row) = hovered_pause_row(cursor, surface_w, surface_h) else {
                    return;
                };
                let intent = match row {
                    PauseRow::Resume => LifecycleIntent::Pause(PauseIntent::Resume),
                    PauseRow::Settings => LifecycleIntent::Pause(PauseIntent::Settings),
                    PauseRow::SaveAndQuit => LifecycleIntent::Pause(PauseIntent::SaveAndQuit),
                };
                self.dispatch_screen_intent(event_loop, intent);
                self.audio.play(SoundSlot::UiClick, self.settings.volume);
            }
            LifecycleScreen::NewGame => {
                let Some(target) = hovered_new_game_target(cursor, surface_w, surface_h) else {
                    return;
                };
                match target {
                    // Click-to-focus: the click sets the same single focus
                    // field Tab cycles, so one field, one writer per
                    // event (MEM028 stays closed).
                    NewGameTarget::NameField => self.new_game.set_focus(FocusField::Name),
                    NewGameTarget::SeedField => self.new_game.set_focus(FocusField::Seed),
                    NewGameTarget::Randomize => self.new_game.randomize(),
                    // Cycler row: advance 1 → 2 → 4 → 8 → 1. Like the
                    // field rows this is a click action (one writer per
                    // event), so the click sound below still plays.
                    NewGameTarget::SeasonLength => {
                        self.new_game.cycle_season_length();
                    }
                    NewGameTarget::Create => self.attempt_create(),
                    NewGameTarget::Back => {
                        self.dispatch_screen_intent(
                            event_loop,
                            LifecycleIntent::NewGame(NewGameIntent::Back),
                        );
                    }
                }
                // Every NewGame target is an action (field focus included),
                // so one click follows the match.
                self.audio.play(SoundSlot::UiClick, self.settings.volume);
            }
            LifecycleScreen::LoadGame => {
                let row_count = self.load.row_count();
                let Some(target) = hovered_load_target(cursor, surface_w, surface_h, row_count)
                else {
                    return;
                };
                match target {
                    // The text portion of a row carries no click action
                    // (Play and Delete are the row's actions), so no click
                    // sound.
                    LoadTarget::Row(_) => {}
                    LoadTarget::Play(index) => {
                        self.play_row(index);
                        self.audio.play(SoundSlot::UiClick, self.settings.volume);
                    }
                    LoadTarget::Delete(index) => {
                        self.load.request_delete(index);
                        self.audio.play(SoundSlot::UiClick, self.settings.volume);
                    }
                    LoadTarget::Back => {
                        self.dispatch_screen_intent(
                            event_loop,
                            LifecycleIntent::LoadGame(LoadGameIntent::Back),
                        );
                        self.audio.play(SoundSlot::UiClick, self.settings.volume);
                    }
                    // Confirm/cancel are live only while a delete is
                    // pending (T01 geometry always exists; the pending
                    // state decides clickability, mirroring the hover
                    // rule in `hovered_target_rect`). An inactive click is
                    // a no-op and stays silent.
                    LoadTarget::ConfirmDelete => {
                        if self.load.is_delete_pending() {
                            self.confirm_pending_delete();
                            self.audio.play(SoundSlot::UiClick, self.settings.volume);
                        }
                    }
                    LoadTarget::CancelDelete => {
                        if self.load.is_delete_pending() {
                            self.load.cancel_delete();
                            self.audio.play(SoundSlot::UiClick, self.settings.volume);
                        }
                    }
                }
            }
            // Settings clicks route through menu_click (its own path);
            // gameplay clicks never reach this method.
            LifecycleScreen::Settings(_) | LifecycleScreen::Playing => {}
        }
    }
}

impl ApplicationHandler for GameApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Idempotent guard: `resumed` can be delivered multiple times on
        // some platforms.
        if self.window.is_some() {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rust Voxel Engine — Initializing...")
                        .with_inner_size(PhysicalSize::new(1280, 720)),
                )
                .expect("Failed to create window"),
        );

        let mut ctx = RenderContext::new(window.clone());
        let renderer = VoxelRenderer::new(&ctx);
        // Re-apply the persisted texture pack to the freshly created GPU
        // texture (M004/S03): the renderer is still a local here, so the
        // initial upload reuses the same live-apply path as cycling. A
        // missing pack still carries usable procedural layers.
        if let Some(pack) = &self.current_pack {
            renderer.write_block_layers(&ctx.queue, &pack.layers);
        }
        let render_store = ChunkRenderStore::new();
        let vista_store = ChunkRenderStore::new();
        let mut ui = UiRenderer::new(&ctx);
        // Startup logo upload (M004/S04): the texture is created exactly once
        // per pack selection, never on the render hot path. Default (`None`)
        // leaves the logo renderer empty so the text title shows instead.
        ui.set_logo(
            &ctx.device,
            &ctx.queue,
            self.current_pack
                .as_ref()
                .and_then(|pack| pack.logo.as_ref()),
        );
        let stats = StatsOverlay::new(&mut ui.font_system);
        let menu_text = StatsOverlay::new(&mut ui.font_system);
        let title_text = StatsOverlay::with_metrics(
            &mut ui.font_system,
            logo::TITLE_FONT_SIZE,
            logo::TITLE_LINE_HEIGHT,
        );
        let load_play_text = StatsOverlay::new(&mut ui.font_system);
        let load_delete_text = StatsOverlay::new(&mut ui.font_system);
        let hotbar_counts = std::array::from_fn(|_| {
            StatsOverlay::with_metrics(
                &mut ui.font_system,
                HOTBAR_COUNT_FONT_SIZE,
                HOTBAR_COUNT_LINE_HEIGHT,
            )
        });
        // Inventory panel labels (S03-T04): same change-gated overlay type and
        // metrics as the hotbar counts, sized once to the pure label bound so
        // no frame allocates a text buffer.
        let inventory_labels = (0..MAX_LABELS)
            .map(|_| {
                StatsOverlay::with_metrics(
                    &mut ui.font_system,
                    HOTBAR_COUNT_FONT_SIZE,
                    HOTBAR_COUNT_LINE_HEIGHT,
                )
            })
            .collect();

        // Loaded video settings apply at startup too (the loaded Settings
        // are the single source of truth for video parameters): borderless
        // fullscreen when enabled, and the Fifo present mode when vsync is
        // on (RenderContext::new prefers uncapped Immediate, which matches
        // the vsync=false default — only vsync=true needs the override).
        if self.settings.fullscreen {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        if self.settings.vsync {
            apply_vsync(&mut ctx, true);
        }

        // Startup cursor state follows the lifecycle screen set in
        // GameApp::new (the CLI bypass decision): a CLI-flag launch enters
        // gameplay directly and grabs the cursor (previous behavior for
        // every flagged launch); a bare launch opens the main menu with
        // the cursor released so it is clickable immediately.
        set_cursor_grab(&window, self.cursor_grabbed);

        self.window = Some(window);
        self.gpu = Some(Ready {
            ctx,
            renderer,
            render_store,
            vista_store,
            ui,
            stats,
            menu_text,
            title_text,
            load_play_text,
            load_delete_text,
            hotbar_counts,
            inventory_labels,
        });
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Single-window application; nothing to do before `resumed()` builds
        // the window and GPU stack.
        let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                // Save-first shutdown: world + settings, then leave the loop.
                // Crash-safety settings save on quit: every accepted change
                // already saved immediately, so this only catches anything
                // not yet persisted (e.g. a hand-edited in-memory state).
                // Save errors are ignored here — the process is exiting and
                // there is no menu left to display them in.
                self.shutdown(event_loop);
            }
            WindowEvent::Focused(false) => {
                self.input.reset();
                self.left_mouse_held = false;
                self.mining.reset();
            }
            WindowEvent::Resized(new_size) => {
                gpu.ctx.resize(new_size);
                // Re-shape the stats overlay at the new size so text does not
                // keep the old layout (wrapping) after a resize.
                gpu.stats.update(
                    &mut gpu.ui.font_system,
                    new_size.width as f32,
                    new_size.height as f32,
                    &self.last_overlay_text,
                );
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    // Route-before-gameplay (S02-T02): while any non-Playing
                    // screen is open it owns the keyboard — every key routes
                    // to the screen FIRST and the gameplay match below never
                    // runs. Gameplay keys must not set InputState while a
                    // screen is open — especially in the settings menu's
                    // AwaitingKey, where ANY pressed KeyCode is a capture
                    // candidate.
                    let screen = self.lifecycle.screen();
                    if screen != LifecycleScreen::Playing {
                        if matches!(screen, LifecycleScreen::Settings(_)) {
                            // The settings menu owns its keys exactly as
                            // before (invariant: open exactly while the
                            // screen is Settings), with one change: Escape
                            // no longer returns straight to gameplay — it
                            // dispatches Back through the lifecycle state to
                            // the caller screen (MainMenu or Pause).
                            if pressed && !event.repeat {
                                if code == KeyCode::Escape {
                                    // In capture mode, Escape cancels the
                                    // pending capture first (the menu stays
                                    // open and the cursor stays released);
                                    // otherwise it backs out to the caller.
                                    if !self.menu.cancel_capture() {
                                        // The cursor stays released across
                                        // settings entry/return from menu
                                        // contexts; gameplay re-grabs on its
                                        // own click path.
                                        self.dispatch_screen_intent(
                                            event_loop,
                                            LifecycleIntent::Settings(SettingsIntent::Back),
                                        );
                                    }
                                } else if let Some(action) = self.menu.awaiting_action() {
                                    // Press-to-rebind: T01's pure decision —
                                    // the unsupported-key filter runs first,
                                    // then the duplicate check.
                                    let decision = menu::evaluate_rebind_candidate(
                                        &self.settings.keybinds,
                                        action,
                                        code,
                                    );
                                    match decision {
                                        menu::RebindDecision::Accept(key) => {
                                            // Apply, then save on every accepted
                                            // rebind — never deferred, so a later
                                            // panic cannot lose the player's
                                            // rebind. The atomic tmp+rename write
                                            // is crash-safe; an io::Error surfaces
                                            // as in-menu text instead of being
                                            // swallowed.
                                            action.set_key(&mut self.settings.keybinds, key);
                                            save_settings_or_warn(
                                                &mut self.menu,
                                                &self.settings,
                                                &self.settings_path,
                                            );
                                            self.menu.finish_capture();
                                            // The menu text composes per frame
                                            // from the live settings, so the new
                                            // binding is shown immediately.
                                        }
                                        menu::RebindDecision::RejectUnsupported { .. }
                                        | menu::RebindDecision::RejectDuplicate { .. } => {
                                            if let Some(text) =
                                                menu::rebind_rejection_message(&decision)
                                            {
                                                self.menu.set_message(text);
                                            }
                                        }
                                    }
                                }
                                // Any other key while the menu is open is
                                // consumed and ignored — the gameplay layer
                                // never sees it.
                            }
                            return;
                        }
                        // The other lifecycle screens own the keyboard:
                        // presses map through T01's pure
                        // `screen_key_action` (Escape backs out — cancelling
                        // a pending delete first on the load screen; Enter
                        // creates/confirms; Tab cycles the field), printable
                        // text and field edits route into the focused
                        // new-game field. Releases are consumed too — no
                        // gameplay key can ever fire while a screen is open.
                        self.screen_key(
                            event_loop,
                            code,
                            pressed,
                            event.repeat,
                            event.text.as_deref(),
                        );
                        return;
                    }

                    // Inventory panel owns the keyboard while open (S03-T03):
                    // every key and release is consumed, so no digit, movement,
                    // fly, save, or Escape-to-pause binding can fire through
                    // the screen. E and Escape close it.
                    if self.inventory.is_open() {
                        if pressed
                            && !event.repeat
                            && matches!(code, KeyCode::KeyE | KeyCode::Escape)
                        {
                            close_inventory(
                                &mut self.inventory,
                                &mut self.session,
                                &mut self.input,
                                &mut self.left_mouse_held,
                                &mut self.inventory_drag,
                                &mut self.mining,
                                &mut self.cursor_grabbed,
                                window,
                            );
                        }
                        return;
                    }

                    match code {
                        // E opens the 9+27 inventory with the 2x2 player grid
                        // (S03-T03). When a table station is already open the
                        // pure toggle attaches the screen to it and the 4x4
                        // grid becomes reachable.
                        KeyCode::KeyE if pressed && !event.repeat => {
                            open_inventory(
                                &mut self.inventory,
                                self.session.open_table_position(),
                                false,
                                &mut self.input,
                                &mut self.left_mouse_held,
                                &mut self.inventory_drag,
                                &mut self.mining,
                                &mut self.cursor_grabbed,
                                window,
                            );
                        }
                        KeyCode::KeyF if pressed && !event.repeat => {
                            self.session.player.fly_mode = !self.session.player.fly_mode;
                            self.session.player.velocity = Vec3::ZERO;
                        }
                        KeyCode::F5 if pressed && !event.repeat => {
                            if save_current_world(&mut self.session).is_ok() {
                                self.last_saved_badge = Some(Instant::now());
                            }
                        }
                        KeyCode::Escape if pressed && !event.repeat => {
                            // Escape in-game opens the Pause screen (S02-T02:
                            // replaces the old direct settings-menu open —
                            // the settings menu is now only reachable via the
                            // main-menu and pause rows). This edge is
                            // frame-loop-owned: the S01 lifecycle intents all
                            // act from menu screens (PauseIntent::Resume is
                            // the way back here).
                            self.enter_screen(LifecycleScreen::Pause);
                        }
                        KeyCode::Digit1 if pressed => {
                            self.session.player.select_slot(0);
                        }
                        KeyCode::Digit2 if pressed => {
                            self.session.player.select_slot(1);
                        }
                        KeyCode::Digit3 if pressed => {
                            self.session.player.select_slot(2);
                        }
                        KeyCode::Digit4 if pressed => {
                            self.session.player.select_slot(3);
                        }
                        KeyCode::Digit5 if pressed => {
                            self.session.player.select_slot(4);
                        }
                        KeyCode::Digit6 if pressed => {
                            self.session.player.select_slot(5);
                        }
                        KeyCode::Digit7 if pressed => {
                            self.session.player.select_slot(6);
                        }
                        KeyCode::Digit8 if pressed => {
                            self.session.player.select_slot(7);
                        }
                        KeyCode::Digit9 if pressed => {
                            self.session.player.select_slot(8);
                        }
                        // Rebindable gameplay keys (S04-T03): press sets the
                        // InputState flag, release clears it — both
                        // transitions route through the loaded keybind map
                        // via the pure helper. The 7 rebindable routes sit
                        // AFTER the non-rebindable utility keys on purpose:
                        // Escape must always toggle the menu and F/F5/
                        // digits keep their hardcoded behavior even
                        // if a gameplay action is rebound onto one of them
                        // (Escape is a rebindable target, so the menu must
                        // stay reachable no matter what is bound).
                        _ => {
                            apply_gameplay_key(
                                &mut self.input,
                                &self.settings.keybinds,
                                code,
                                pressed,
                            );
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Route-before-gameplay: while a screen is open the wheel is
                // consumed — the hotbar is gameplay state and no screen has
                // a scroll action in this slice. The inventory panel consumes
                // it for the same reason (S03-T03): no hotbar selection may
                // change from inside the panel.
                if self.lifecycle.screen() != LifecycleScreen::Playing || self.inventory.is_open() {
                    return;
                }
                let step: isize = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        if y > 0.0 {
                            -1
                        } else if y < 0.0 {
                            1
                        } else {
                            0
                        }
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        if p.y > 0.0 {
                            -1
                        } else if p.y < 0.0 {
                            1
                        } else {
                            0
                        }
                    }
                };
                if step != 0 {
                    self.session.player.select_relative(step);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                // Inventory panel (S03-T03): a release resolves the drop for a
                // press-drag-release gesture and always ends the gesture, so it
                // can never reach gameplay. A same-cell or off-panel release
                // leaves the stack on the cursor (click-to-pick style).
                if self.inventory.is_open() {
                    let (surface_w, surface_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                    // Click-boundary revalidation (S04-T02): a station can be
                    // invalidated between redraws (chunk streaming keeps
                    // running), so sync before the release is routed. A lost
                    // station closes the panel and ends the gesture instead of
                    // dropping into a stale 4x4 grid.
                    if revalidate_inventory_station(&mut self.inventory, &mut self.session) {
                        self.inventory_status = Some("Crafting Table closed".to_string());
                        self.inventory_drag = None;
                        self.left_mouse_held = false;
                        self.mining.reset();
                        self.cursor_grabbed = true;
                        set_cursor_grab(window, true);
                        return;
                    }
                    let table_open = self.session.open_table_position();
                    let mode = self.inventory.mode(table_open);
                    let layout = layout(surface_w, surface_h, mode);
                    let release_target = self
                        .cursor_pos
                        .and_then(|cursor| hit_test(mode, &layout, cursor));
                    if let Some(destination) =
                        drag_release_target(self.inventory_drag.take(), release_target)
                    {
                        let result = apply_inventory_click(
                            &mut self.session.player,
                            &self.inventory,
                            table_open,
                            destination,
                        );
                        self.inventory_status = Some(inventory_status_text(result));
                    }
                    self.left_mouse_held = false;
                    self.mining.reset();
                    return;
                }
                // A release can arrive while a menu owns the input, so this
                // arm deliberately bypasses the screen routing: clearing the
                // hold and any partial progress is always safe.
                self.left_mouse_held = false;
                self.mining.reset();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                // Route-before-gameplay (S02-T02): while any screen is open
                // the cursor is released and every click belongs to that
                // screen — the settings screen through its existing
                // menu_click path, every other lifecycle screen through T01
                // hit tests (screen_click). The click-to-re-grab path below
                // and the gameplay raycast must not steal screen clicks.
                let screen = self.lifecycle.screen();
                if screen != LifecycleScreen::Playing {
                    let (surface_w, surface_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                    if matches!(screen, LifecycleScreen::Settings(_)) {
                        // Hit-test the clicked row BEFORE the whole-`self`
                        // menu_click call (which must not overlap the live
                        // gpu/window field borrows): only the Vsync row needs
                        // a post-call surface reconfigure, and that needs gpu.
                        let clicked = self
                            .cursor_pos
                            .and_then(|pos| menu::hovered_row(pos, surface_w, surface_h));
                        let window = window.clone();
                        let cursor = self.cursor_pos;
                        self.menu_click(event_loop, &window, button, cursor, surface_w, surface_h);
                        if clicked == Some(MenuRow::Vsync) {
                            if let Some(gpu) = self.gpu.as_mut() {
                                // Live apply: Fifo/Immediate present mode +
                                // surface reconfigure (the resize path's
                                // mechanism; see `apply_vsync`).
                                apply_vsync(&mut gpu.ctx, self.settings.vsync);
                            }
                        }
                        if clicked == Some(MenuRow::TexturePack) {
                            // Live apply (MEM111): rewrite the EXISTING block
                            // texture array's contents — same texture, view,
                            // and bind group, so nothing needs recreation.
                            // Clone/copy the layer bytes BEFORE the `as_mut`
                            // borrow split (P005); Default re-uploads the
                            // procedural layers so wrapping back restores
                            // the built-in textures.
                            let layers = match &self.current_pack {
                                Some(pack) => pack.layers.clone(),
                                None => generate_all_texture_layers_rgba(),
                            };
                            // The logo is cloned too, for the same reason:
                            // both reads of `self.current_pack` happen before
                            // the `self.gpu.as_mut()` borrow (P005). Cycling
                            // back to Default passes None and clears the logo
                            // so the text title returns.
                            let logo = match &self.current_pack {
                                Some(pack) => pack.logo.clone(),
                                None => None,
                            };
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.renderer.write_block_layers(&gpu.ctx.queue, &layers);
                                gpu.ui
                                    .set_logo(&gpu.ctx.device, &gpu.ctx.queue, logo.as_ref());
                            }
                        }
                    } else {
                        // Other lifecycle screens own the click (T01 hit
                        // tests); gpu is not touched after this call on
                        // this path.
                        self.screen_click(event_loop, button, surface_w, surface_h);
                    }
                    return;
                }

                // Inventory panel (S03-T03): while open it owns every click.
                // Left press issues exactly one whole-stack command; a press
                // that picked a stack arms a possible drag, while a place,
                // swap, or craft ends the gesture. Right press issues one
                // split-half or place-one command and never arms a drag.
                // Returning here keeps the click away from the re-grab branch,
                // table interaction, placement, and hold-to-mine arming.
                if self.inventory.is_open() {
                    let (surface_w, surface_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                    // Click-boundary revalidation (S04-T02): see the release
                    // arm. A press must never route into a station that a
                    // between-redraw stream update already invalidated.
                    if revalidate_inventory_station(&mut self.inventory, &mut self.session) {
                        self.inventory_status = Some("Crafting Table closed".to_string());
                        self.inventory_drag = None;
                        self.left_mouse_held = false;
                        self.mining.reset();
                        self.cursor_grabbed = true;
                        set_cursor_grab(window, true);
                        return;
                    }
                    let table_open = self.session.open_table_position();
                    let mode = self.inventory.mode(table_open);
                    match button {
                        MouseButton::Left => {
                            let layout = layout(surface_w, surface_h, mode);
                            let target = self
                                .cursor_pos
                                .and_then(|cursor| hit_test(mode, &layout, cursor));
                            if let Some(target) = target {
                                // Capture the pre-press cursor state: a drag may
                                // only arm when this press picked a stack UP
                                // from an empty cursor (S04-T03).
                                let cursor_empty_before =
                                    self.session.player.inventory().cursor().is_none();
                                let result = apply_inventory_click(
                                    &mut self.session.player,
                                    &self.inventory,
                                    table_open,
                                    target,
                                );
                                self.inventory_status = Some(inventory_status_text(result));
                                // Only a pure pick-up begins a drag; a swap,
                                // place, merge, or craft ends it.
                                self.inventory_drag = drag_origin_after_press(
                                    cursor_empty_before,
                                    result,
                                    self.session.player.inventory().cursor().is_some(),
                                    target,
                                );
                            } else {
                                // Empty panel space: no command, no drag origin.
                                self.inventory_drag = None;
                            }
                        }
                        MouseButton::Right => {
                            // Right click never moves a whole stack, so it can
                            // never begin a press-drag-release gesture.
                            let layout = layout(surface_w, surface_h, mode);
                            let target = self
                                .cursor_pos
                                .and_then(|cursor| hit_test(mode, &layout, cursor));
                            if let Some(target) = target {
                                let result = apply_inventory_click_button(
                                    &mut self.session.player,
                                    &self.inventory,
                                    table_open,
                                    target,
                                    InventoryClickButton::Right,
                                );
                                self.inventory_status = Some(inventory_status_text(result));
                            }
                            self.inventory_drag = None;
                        }
                        _ => {}
                    }
                    return;
                }

                if !self.cursor_grabbed {
                    self.cursor_grabbed = true;
                    set_cursor_grab(window, true);
                    return;
                }

                // Hold-to-mine (S02): a Left press only arms the hold — no
                // raycast and no break happen here. The break happens once
                // per frame in RedrawRequested from the held candidate, so a
                // press can never break a block instantly (double-break
                // hazard). Right click keeps its byte-identical instant
                // placement path, including the raycast.
                match button {
                    MouseButton::Left => {
                        self.left_mouse_held = true;
                    }
                    MouseButton::Right => {
                        // Table interaction (S02-T02) is attempted before
                        // placement: a targeted crafting table opens,
                        // edits nothing, and consumes nothing. Non-table
                        // and missing targets fall through to the existing
                        // instant placement path.
                        match right_click_interaction(&mut self.session) {
                            RightClickOutcome::Placed(remeshed) => {
                                // Placement applied: one thump at the current
                                // volume. A rejected placement stays silent.
                                // Field-disjoint from the live `gpu` borrow
                                // (P005).
                                self.audio.play(SoundSlot::Place, self.settings.volume);
                                for (cpos, mesh) in remeshed {
                                    gpu.render_store.upload_mesh(
                                        &gpu.ctx.device,
                                        &gpu.renderer.chunk_bind_group_layout,
                                        cpos,
                                        &mesh,
                                    );
                                }
                            }
                            RightClickOutcome::TableOpened => {
                                // A successful table interaction opens the
                                // panel attached to that live station (S03-T03);
                                // a failed open stays on the placement fallback
                                // above, byte-identical to before.
                                open_inventory(
                                    &mut self.inventory,
                                    self.session.open_table_position(),
                                    true,
                                    &mut self.input,
                                    &mut self.left_mouse_held,
                                    &mut self.inventory_drag,
                                    &mut self.mining,
                                    &mut self.cursor_grabbed,
                                    window,
                                );
                            }
                            RightClickOutcome::Nothing => {}
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32();
                self.last_frame = now;

                // Message expiry at a bounded ~60 ticks/s (see
                // `MENU_TICK_INTERVAL`): the pure tick counters decrement
                // only here, so warnings never stick forever and never flash
                // by at uncapped frame rates. Every screen's transient
                // message ticks on the same timer (settings menu, new-game
                // form, load screen).
                if self.message_tick_timer.elapsed().as_secs_f32() >= MENU_TICK_INTERVAL {
                    self.message_tick_timer = Instant::now();
                    if self.menu.message.is_some()
                        || self.new_game.message().is_some()
                        || self.load_message.is_some()
                        || self.main_menu_message.is_some()
                    {
                        self.menu.tick();
                        self.new_game.tick();
                        tick_transient_message(&mut self.load_message);
                        tick_transient_message(&mut self.main_menu_message);
                    }
                }

                // Pause semantics (S02-T02): while any non-Playing screen is
                // open, gameplay updates and world streaming are skipped —
                // the world still renders below the screen panel, but
                // nothing moves and no chunk loads/unloads (InputState was
                // reset on screen entry, so no key sticks either).
                let playing = self.lifecycle.screen() == LifecycleScreen::Playing;
                // Gameplay vs. world-liveness (S03-T03): the inventory panel
                // freezes player movement, camera aim, the target raycast, and
                // hold-to-mine, but NOT the world: time, table revalidation,
                // streaming, and saves keep running so a panel opened next to a
                // table cannot freeze the chunk pipeline.
                let gameplay = playing && !self.inventory.is_open();
                // Game time (S02-T04): advance only on Playing frames, before
                // the player update, with the app-owned acceleration scale.
                // The same two lines as `GameApp::tick_clock` (which the
                // integration tests drive); inlined here because `gpu` and
                // `window` already hold disjoint `self` borrows across this
                // region, so a `&mut self` method call cannot be used.
                self.session.clock.tick(playing, now, self.time_scale);
                // Bounded accelerated demo (S02-T04): `--time-scale` is a
                // test seam, so an accelerated run self-terminates after
                // exactly one full day cycle and exits cleanly (status 0).
                // That makes completion a health signal the harness can read
                // directly, instead of inferring it from an external
                // timeout kill. The exit itself is deferred to
                // `about_to_wait` because `gpu`/`window` borrows span this
                // region; only plain fields are touched here.
                if !self.demo_finished
                    && observe_demo_cycle(
                        &mut self.demo_cycle,
                        self.time_scale,
                        playing,
                        self.session.clock.time_of_day,
                    )
                {
                    self.demo_cycle = None;
                    self.demo_finished = true;
                }
                // Vista discriminator (M004/S02): the single `vista_visible`
                // truth source — true only for the main-menu cluster. Pause,
                // Settings-from-pause, and Playing keep rendering the session
                // world (frozen for Pause), so neither stream runs there.
                let vista = vista::vista_visible(self.lifecycle.screen());
                if playing {
                    if gameplay {
                        self.session.player.update(dt, &self.input, |x, y, z| {
                            self.session.world.get_block(x, y, z)
                        });
                        self.session.camera.position = self.session.player.eye_position();
                        self.session.camera.yaw = self.session.player.yaw;
                        self.session.camera.pitch = self.session.player.pitch;
                    }
                    // Open-table state is a world fact (S02-T02): close the
                    // station as soon as its block is removed/replaced or
                    // the player steps out of reach. One bounded lookup per
                    // playing frame, never a scan.
                    self.session.sync_open_table();
                    // Table-loss close (S03-T03): an attached panel whose
                    // station disappeared closes itself and restores gameplay
                    // ownership. Field-by-field because `gpu`/`window` borrows
                    // span this region, so no `&mut self` method is usable.
                    if inventory_should_close(&self.inventory, self.session.open_table_position()) {
                        self.inventory.close();
                        self.session.close_table();
                        self.inventory_drag = None;
                        self.inventory_status = Some("Crafting Table closed".to_string());
                        self.cursor_grabbed = true;
                        set_cursor_grab(window, true);
                    }

                    let (player_chunk, _) = world_to_chunk_and_local(
                        self.session.player.position.x.floor() as i32,
                        self.session.player.position.y.floor() as i32,
                        self.session.player.position.z.floor() as i32,
                    );
                    let (new_meshes, unloaded) = self.session.world.update_streaming(player_chunk);
                    if !unloaded.is_empty()
                        && self.session.world.has_unsaved_changes()
                        && save_current_world(&mut self.session).is_ok()
                    {
                        self.last_saved_badge = Some(Instant::now());
                    }
                    for upos in unloaded {
                        gpu.render_store.remove_chunk(upos);
                    }
                    for (cpos, mesh) in new_meshes {
                        gpu.render_store.upload_mesh(
                            &gpu.ctx.device,
                            &gpu.renderer.chunk_bind_group_layout,
                            cpos,
                            &mesh,
                        );
                    }
                } else if vista {
                    // Menu vista streaming (M004/S02): mirror the session
                    // block against the vista world. The vista never saves
                    // and never touches `session`/save paths; paused screens
                    // fail `vista` above, so the frozen paused world stays
                    // frozen with no chunk loads/unloads.
                    let anchor = vista::vista_anchor();
                    let (vista_chunk, _) = world_to_chunk_and_local(
                        anchor.x.floor() as i32,
                        anchor.y.floor() as i32,
                        anchor.z.floor() as i32,
                    );
                    let (new_meshes, unloaded) = self.vista_world.update_streaming(vista_chunk);
                    for upos in unloaded {
                        gpu.vista_store.remove_chunk(upos);
                    }
                    for (cpos, mesh) in new_meshes {
                        gpu.vista_store.upload_mesh(
                            &gpu.ctx.device,
                            &gpu.renderer.chunk_bind_group_layout,
                            cpos,
                            &mesh,
                        );
                    }
                }

                // Gameplay raycast feeds both the targeted-block wireframe
                // and hold-to-mine. Menus suppress it entirely: no wireframe
                // can float behind a panel, and mining.tick(None, ..) resets
                // stale progress exactly as before.
                let hit = if gameplay {
                    raycast_voxels(
                        self.session.camera.position,
                        self.session.camera.forward(),
                        8.0,
                        |x, y, z| self.session.world.get_block(x, y, z),
                    )
                } else {
                    None
                };
                // Hold-to-mine candidate: only a held Left button on the
                // grabbed gameplay surface feeds the accumulator. Every
                // other frame passes None, which resets stale progress
                // (released, paused, or cursor not grabbed).
                let candidate = if gameplay && self.cursor_grabbed && self.left_mouse_held {
                    // T01: the target carries its effective duration, derived
                    // once from the block's hardness and the selected item,
                    // so tool rules live in `player::tools` and the
                    // accumulator only integrates the result.
                    hit.and_then(|h| {
                        let block = self.session.world.get_block(
                            h.block_pos.x,
                            h.block_pos.y,
                            h.block_pos.z,
                        );
                        MiningTarget::from_block(
                            h.block_pos,
                            block,
                            self.session.player.selected_item(),
                        )
                    })
                } else {
                    None
                };
                let mut target_block = hit.map(|h| h.block_pos);
                if let MiningTick::Broke { pos, block } = self.mining.tick(dt, candidate) {
                    // The only production break site (S02): hardness
                    // gating happens in MiningState and is re-checked by the
                    // helper, so Bedrock (no duration) can never reach an
                    // applied break. The helper owns the ordering: world edit
                    // first, one post-edit tool wear, then collection.
                    match break_mined_block(&mut self.session, pos, block) {
                        BreakOutcome::Applied { remeshed, .. } => {
                            // Only an edit that actually applied makes noise.
                            // Break then the rising completion pop, both at the
                            // current volume and field-disjoint from the live
                            // `gpu` borrow (P005).
                            self.audio.play(SoundSlot::Break, self.settings.volume);
                            self.audio
                                .play(SoundSlot::MineComplete, self.settings.volume);
                            for (cpos, mesh) in remeshed {
                                gpu.render_store.upload_mesh(
                                    &gpu.ctx.device,
                                    &gpu.renderer.chunk_bind_group_layout,
                                    cpos,
                                    &mesh,
                                );
                            }
                            // The cell is Air now; do not draw the wireframe
                            // around the block that just disappeared.
                            target_block = None;
                        }
                        // A rejected break edited nothing, so the block is
                        // still there and the wireframe stays truthful.
                        BreakOutcome::Rejected(_) => {}
                    }
                }

                // Translucent dark panel behind the stats text: the visible
                // proof that pixel-space quads render in the UI pass. Width is
                // estimated from the stats string (monospace advance ~0.6 em);
                // the panel only has to be generous, not exact. Built each
                // frame from pure CPU vertex math (cheap).
                let panel_w = OVERLAY_TEXT_LEFT * 2.0
                    + self.last_overlay_text.chars().count().max(1) as f32 * FONT_SIZE * 0.6
                    + 4.0;
                let mut quads = vec![Quad {
                    x: OVERLAY_TEXT_LEFT - 6.0,
                    y: OVERLAY_TEXT_TOP - 4.0,
                    w: panel_w,
                    h: LINE_HEIGHT + 8.0,
                    color: [0.0, 0.0, 0.0, 0.4],
                }];

                // Vista branding (M004/S04), gated to the main-menu cluster:
                // the pack's overlay tint darkens the panorama at the head of
                // the quad list (all panels/text draw above it), the logo
                // frame is rebuilt every frame, and with no logo the large
                // text title is centered in the title strip. Non-vista
                // screens pass `None` explicitly so no stale logo or tint
                // follows the player into Pause/Playing.
                let (brand_w, brand_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                if vista {
                    if let Some(tint) = self
                        .current_pack
                        .as_ref()
                        .and_then(|pack| pack.overlay_tint)
                    {
                        quads.insert(0, logo::tint_quad(brand_w, brand_h, tint));
                    }
                }
                let logo_rect = if vista {
                    self.current_pack.as_ref().and_then(|pack| {
                        pack.logo.as_ref().map(|pack_logo| {
                            logo::logo_rect(
                                brand_w,
                                pack_logo.width,
                                pack_logo.height,
                                logo::TITLE_STRIP_HEIGHT,
                            )
                        })
                    })
                } else {
                    None
                };
                gpu.ui
                    .set_logo_frame(&gpu.ctx.device, logo_rect, brand_w, brand_h);

                // Default title: a large engine-styled "OpenCraft" line,
                // centered manually (cosmic-text 0.10 has no alignment API).
                let mut title_text_area = None;
                if vista && logo_rect.is_none() {
                    gpu.title_text.update(
                        &mut gpu.ui.font_system,
                        brand_w as f32,
                        brand_h as f32,
                        logo::TITLE_TEXT,
                    );
                    let measured_w = gpu.title_text.measured_width();
                    let left = logo::centered_title_left(brand_w as f32, measured_w);
                    title_text_area = Some(gpu.title_text.text_area(left, logo::TITLE_STRIP_TOP));
                }

                // While any non-Playing screen is open (S02-T02): draw its
                // dark panel plus a translucent highlight over the hovered
                // target, and shape the screen text. The settings screen keeps
                // its existing path (menu geometry + `compose_menu_text`);
                // every other lifecycle screen composes through the pure
                // `compose_screen_text` over T01 geometry. All geometry
                // derives per frame from the current surface size, so a
                // fullscreen Resized reflows every screen automatically with
                // no cached rects; the text only re-shapes when the composed
                // string changed (StatsOverlay guards that).
                let mut menu_text_area = None;
                let mut load_play_text_area = None;
                let mut load_delete_text_area = None;
                let active_screen = self.lifecycle.screen();
                if active_screen != LifecycleScreen::Playing {
                    let (surface_w, surface_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                    let settings_open = self.menu.is_open();
                    let (panel_x, panel_y, panel_w, panel_h) = if settings_open {
                        menu::panel_rect(surface_w, surface_h)
                    } else {
                        screen_panel_rect(active_screen, &self.load, surface_w, surface_h)
                    };
                    quads.push(Quad {
                        x: panel_x,
                        y: panel_y,
                        w: panel_w,
                        h: panel_h,
                        color: [0.0, 0.0, 0.0, 0.7],
                    });
                    let hovered = if settings_open {
                        self.cursor_pos
                            .and_then(|cursor| menu::hovered_row(cursor, surface_w, surface_h))
                            .and_then(|hover| {
                                let index = menu::ROWS
                                    .iter()
                                    .position(|&r| r == hover)
                                    .expect("hovered row is one of ROWS");
                                menu::row_rect(surface_w, surface_h, index)
                            })
                    } else {
                        hovered_target_rect(
                            active_screen,
                            &self.load,
                            self.cursor_pos,
                            surface_w,
                            surface_h,
                        )
                    };
                    if let Some((hx, hy, hw, hh)) = hovered {
                        quads.push(Quad {
                            x: hx,
                            y: hy,
                            w: hw,
                            h: hh,
                            color: [0.35, 0.55, 0.9, 0.3],
                        });
                    }
                    let text = if settings_open {
                        let pack_label = self
                            .current_pack
                            .as_ref()
                            .map_or("Default", |pack| pack.display_name.as_str());
                        compose_menu_text(&self.settings, &self.menu, pack_label)
                    } else {
                        compose_screen_text(
                            active_screen,
                            &self.new_game,
                            &self.load,
                            self.load_message.as_ref(),
                            self.main_menu_message.as_ref(),
                        )
                    };
                    gpu.menu_text.update(
                        &mut gpu.ui.font_system,
                        surface_w as f32,
                        surface_h as f32,
                        &text,
                    );
                    menu_text_area = Some(
                        gpu.menu_text
                            .text_area(panel_x + menu::MENU_PAD, panel_y + menu::MENU_PAD),
                    );

                    // S05-T02: the load screen's per-row "Play"/"Delete"
                    // label columns render as two positioned buffers at
                    // their hit-test rects — row 0's `load_play_rect`/
                    // `load_delete_rect` supplies the left/top, and
                    // glyphon's LINE_HEIGHT equals the `row_y` step, so
                    // buffer line i lands on row i's rect top. Skipped
                    // entirely when the catalog is empty ("(no saved
                    // worlds)" stays a single line) and on every other
                    // screen; the buffers only re-shape when the row
                    // count changes (the StatsOverlay change gate —
                    // `format_row_line` stays byte-identical, no
                    // in-string padding per MEM056).
                    if active_screen == LifecycleScreen::LoadGame && self.load.row_count() > 0 {
                        let row_count = self.load.row_count();
                        let (play_x, play_y, _, _) = load_play_rect(surface_w, 0);
                        let (delete_x, _, _, _) = load_delete_rect(surface_w, 0);
                        gpu.load_play_text.update(
                            &mut gpu.ui.font_system,
                            surface_w as f32,
                            surface_h as f32,
                            &repeated_lines("Play", row_count),
                        );
                        gpu.load_delete_text.update(
                            &mut gpu.ui.font_system,
                            surface_w as f32,
                            surface_h as f32,
                            &repeated_lines("Delete", row_count),
                        );
                        load_play_text_area = Some(gpu.load_play_text.text_area(play_x, play_y));
                        load_delete_text_area =
                            Some(gpu.load_delete_text.text_area(delete_x, play_y));
                    }
                }
                if playing {
                    if let Some(progress) = self.mining.progress() {
                        quads.extend(mining_progress_quads(
                            progress,
                            gpu.ctx.config.width,
                            gpu.ctx.config.height,
                        ));
                    }
                }
                // Inventory/crafting panel (S03-T04): extend the same CPU quad
                // list, after the HUD/mining quads and before the single
                // `set_quads` upload, so the panel always draws above the
                // world and the HUD with no new pipeline (P003/P004).
                // Geometry comes only from the pure T01 module; a closed
                // screen extends nothing.
                let inventory_mode = self.inventory.mode(self.session.open_table_position());
                let inventory_layout = if inventory_mode.is_open() {
                    let layout =
                        layout(gpu.ctx.config.width, gpu.ctx.config.height, inventory_mode);
                    quads.extend(screen_quads(
                        inventory_mode,
                        &layout,
                        self.session.player.inventory(),
                        self.session.player.player_grid(),
                        self.session.player.table_grid(),
                        self.session.player.inventory().cursor(),
                        self.cursor_pos,
                    ));
                    Some(layout)
                } else {
                    None
                };
                gpu.ui.set_quads(
                    &gpu.ctx.device,
                    &quads,
                    gpu.ctx.config.width,
                    gpu.ctx.config.height,
                );

                // S03-T04: per-slot hotbar counts share the outlines' layout
                // helper, then draw on every screen (including pause) so the
                // HUD never flashes. First pass updates the buffers while the
                // mutable borrow is confined to `hotbar_counts`; the text
                // areas are assembled afterwards from immutable borrows.
                let hotbar_rects = hotbar_slot_rects(gpu.ctx.config.width, gpu.ctx.config.height);
                for (slot, counts) in gpu.hotbar_counts.iter_mut().enumerate() {
                    counts.update(
                        &mut gpu.ui.font_system,
                        gpu.ctx.config.width as f32,
                        gpu.ctx.config.height as f32,
                        &hotbar_count_label_for(self.session.player.inventory(), slot),
                    );
                }

                // Inventory panel labels (S03-T04): built from the same pure
                // T01 module and the same change-gated overlay type as the
                // hotbar counts. The first loop updates only the buffers this
                // frame actually uses; the text areas are appended after
                // `areas` exists (two-pass borrow, mirroring `hotbar_counts`).
                // A closed screen produces no labels, so nothing is reshaped
                // and no inventory text area is pushed.
                let inventory_panel_labels = match inventory_layout {
                    Some(layout) => screen_labels(
                        inventory_mode,
                        &layout,
                        self.session.player.inventory(),
                        self.session.player.player_grid(),
                        self.session.player.table_grid(),
                        self.inventory_status.as_deref(),
                    ),
                    None => Vec::new(),
                };
                for (buffer, label) in gpu
                    .inventory_labels
                    .iter_mut()
                    .zip(inventory_panel_labels.iter())
                {
                    buffer.update(
                        &mut gpu.ui.font_system,
                        gpu.ctx.config.width as f32,
                        gpu.ctx.config.height as f32,
                        &label.text,
                    );
                }

                // UI overlay: prepare glyphs each frame; on prepare error
                // (glyph atlas full) the UI pass is skipped for that frame
                // (the crate is log-free by convention). The menu's text
                // area rides the same UI pass, AFTER the stats area, with
                // no pipeline change (MEM014).
                let stats_text_area = gpu.stats.text_area(OVERLAY_TEXT_LEFT, OVERLAY_TEXT_TOP);
                let mut areas = vec![stats_text_area];
                // The vista title draws before (under) the screen text, so a
                // panel containing the title still wins visually.
                if let Some(title_text_area) = title_text_area {
                    areas.push(title_text_area);
                }
                if let Some(menu_text_area) = menu_text_area {
                    areas.push(menu_text_area);
                }
                // S05-T02 label columns ride the same UI pass, after the
                // screen text area (no pipeline change).
                if let Some(load_play_text_area) = load_play_text_area {
                    areas.push(load_play_text_area);
                }
                if let Some(load_delete_text_area) = load_delete_text_area {
                    areas.push(load_delete_text_area);
                }
                // Hotbar count labels ride the same UI pass, after the
                // screen text areas. Empty slots push nothing, so the
                // renderer only sees non-empty labels.
                for (slot, counts) in gpu.hotbar_counts.iter().enumerate() {
                    let label = hotbar_count_label_for(self.session.player.inventory(), slot);
                    if !label.is_empty() {
                        let (x, y) = hotbar_count_text_origin(hotbar_rects[slot]);
                        areas.push(counts.text_area(x, y));
                    }
                }
                // Inventory panel labels ride the same UI pass, above the
                // HUD labels. The label count is bounded by the pure
                // `MAX_LABELS` and by the buffer pool length, so the zip
                // never drops a label silently.
                debug_assert!(inventory_panel_labels.len() <= MAX_LABELS);
                for (buffer, label) in gpu
                    .inventory_labels
                    .iter()
                    .zip(inventory_panel_labels.iter())
                {
                    areas.push(buffer.text_area(label.left, label.top));
                }
                let prepare_ok = gpu
                    .ui
                    .text_renderer
                    .prepare(
                        &gpu.ctx.device,
                        &gpu.ctx.queue,
                        &mut gpu.ui.font_system,
                        &mut gpu.ui.atlas,
                        glyphon::Resolution {
                            width: gpu.ctx.config.width,
                            height: gpu.ctx.config.height,
                        },
                        areas,
                        &mut gpu.ui.swash_cache,
                    )
                    .is_ok();
                let ui_pass: Option<&dyn UiPass> = if prepare_ok { Some(&gpu.ui) } else { None };

                // Single render_frame call site (M004/S02): menu-cluster
                // screens render the drifting vista from its own store and
                // world render distance; every other screen renders the
                // session exactly as before. The vista camera is built per
                // frame from the epoch + settings FOV (pure math, cheap),
                // so live FOV changes reach the panorama. target_block is
                // always None on vista frames (overlays are off anyway).
                let vista_camera = if vista {
                    Some(vista::vista_camera(
                        self.vista_epoch.elapsed().as_secs_f32(),
                        self.settings.fov_degrees.to_radians(),
                    ))
                } else {
                    None
                };
                let frame_result = if let Some(vista_camera) = vista_camera.as_ref() {
                    // Vista sky is pinned at golden hour: never the world clock
                    // and never `vista_epoch` (that only drifts the camera).
                    let palette = daycycle::sky_palette(daycycle::VISTA_GOLDEN_HOUR);
                    gpu.renderer.render_frame(
                        &gpu.ctx,
                        &gpu.vista_store,
                        vista_camera,
                        self.vista_world.render_distance(),
                        None,
                        self.session.player.selected_slot(),
                        false,
                        sky_params(palette, daycycle::VISTA_GOLDEN_HOUR),
                        ui_pass,
                    )
                } else {
                    // Per-frame palette from the live session clock (pure math,
                    // cheap — same rationale as the per-frame vista camera).
                    let palette = daycycle::sky_palette(self.session.clock.time_of_day);
                    gpu.renderer.render_frame(
                        &gpu.ctx,
                        &gpu.render_store,
                        &self.session.camera,
                        self.session.world.render_distance(),
                        target_block,
                        self.session.player.selected_slot(),
                        playing,
                        sky_params(palette, self.session.clock.time_of_day),
                        ui_pass,
                    )
                };
                match frame_result {
                    Ok(visible_chunks) => {
                        self.frame_count += 1;
                        let elapsed = self.title_timer.elapsed().as_secs_f32();
                        if elapsed >= 0.25 {
                            let fps = (self.frame_count as f32 / elapsed).round() as u32;
                            let ms = (elapsed * 1000.0) / self.frame_count.max(1) as f32;
                            let vram_kb = gpu.render_store.total_vram_bytes / 1024;
                            let mode = if self.session.player.fly_mode {
                                "Fly"
                            } else {
                                "Walk"
                            };
                            let block_name =
                                selected_item_label(self.session.player.selected_item());
                            let save_tag = if self
                                .last_saved_badge
                                .map(|t| t.elapsed().as_secs_f32() < 2.0)
                                .unwrap_or(false)
                            {
                                " [Saved]"
                            } else {
                                ""
                            };
                            window.set_title(&format!(
                                "OpenCraft{} | {} FPS ({:.2} ms) | Chunks: {}/{} | Quads: {} ({} KB VRAM) | Pos: ({:.0}, {:.0}, {:.0}) [{}] | Block [{}]: {} | RD: {} | Seed: {}{}",
                                screen_title_tag(self.lifecycle.screen()),
                                fps,
                                ms,
                                visible_chunks,
                                self.session.world.loaded_chunk_count(),
                                gpu.render_store.total_quads,
                                vram_kb,
                                self.session.player.position.x,
                                self.session.player.position.y,
                                self.session.player.position.z,
                                mode,
                                self.session.player.selected_slot() + 1,
                                block_name,
                                self.session.world.render_distance(),
                                self.session.world.seed(),
                                save_tag,
                            ));
                            self.title_timer = Instant::now();
                            self.frame_count = 0;

                            // Same stats data drives the on-screen overlay,
                            // refreshed at this 0.25 s cadence (not per frame;
                            // `set_text`/re-shape only runs when it changed).
                            self.last_overlay_text = format!(
                                "FPS {} | {:.1} ms | Pos ({:.0}, {:.0}, {:.0}) [{}] | Chunks {}/{}",
                                fps,
                                ms,
                                self.session.player.position.x,
                                self.session.player.position.y,
                                self.session.player.position.z,
                                mode,
                                visible_chunks,
                                self.session.world.loaded_chunk_count(),
                            );
                            gpu.stats.update(
                                &mut gpu.ui.font_system,
                                gpu.ctx.config.width as f32,
                                gpu.ctx.config.height as f32,
                                &self.last_overlay_text,
                            );
                        }
                    }
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        gpu.ctx.resize(gpu.ctx.size);
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        let _ = save_current_world(&mut self.session);
                        event_loop.exit();
                    }
                    Err(wgpu::SurfaceError::Timeout) => {}
                }
            }
            // Cursor plumbing (no gameplay behavior): stores the latest
            // physical-pixel cursor position, or None once it leaves the
            // window. winit 0.30 delivers PhysicalPosition<f64>.
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = Some((position.x as f32, position.y as f32));
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_pos = None;
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.cursor_grabbed {
                // Settings-driven sensitivity (S04-T04): the loaded value,
                // already clamped at load; menu adjustments clamp via the
                // T01 helpers before every save.
                let sensitivity = self.settings.mouse_sensitivity;
                self.session.player.yaw += delta.0 as f32 * sensitivity;
                self.session.player.pitch =
                    (self.session.player.pitch - delta.1 as f32 * sensitivity).clamp(-1.54, 1.54);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Bounded accelerated demo completed: the last frame latched
        // `demo_finished`; persist and leave the loop with status 0. Checked
        // here rather than in RedrawRequested because the render region
        // holds disjoint `gpu`/`window` borrows and a whole-`self` shutdown
        // call cannot coexist with them.
        if self.demo_finished {
            self.shutdown(event_loop);
            return;
        }

        // Wall-clock safety net: if redraws stall, the accelerated run still
        // ends after one cycle plus grace instead of relying on an external
        // timeout kill.
        if self
            .demo_shutdown_deadline
            .map(|deadline| Instant::now() >= deadline)
            .unwrap_or(false)
        {
            self.shutdown(event_loop);
            return;
        }

        let Some(window) = &self.window else {
            return;
        };

        // Vsync on: Mailbox presents at vblank but does not block the render
        // thread, so pace the loop to the monitor refresh explicitly. This
        // keeps rendering at the display rate (instead of spinning uncapped)
        // and avoids the half-refresh stall a blocking Fifo surface can hit
        // on compositors that hold fullscreen scanout buffers for an extra
        // vblank. `WaitUntil` leaves the loop idle between deadlines while
        // still waking immediately for input events.
        if self.settings.vsync {
            if let Some(interval) = vsync_interval(window) {
                let deadline = self.last_frame + interval;
                let now = Instant::now();
                if now >= deadline {
                    window.request_redraw();
                    // `RedrawRequested` updates `last_frame`; wake once more
                    // after the next period in case the request is delayed.
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline + interval));
                } else {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
                return;
            }
            // Unknown refresh rate: fall back to the old uncapped polling
            // loop. A Fifo fallback still throttles at present time.
        }

        event_loop.set_control_flow(ControlFlow::Poll);
        window.request_redraw();
    }
}

fn main() {
    let cli = CliConfig::parse_args(std::env::args().skip(1));

    let session = startup_session(&cli);

    let event_loop = EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = GameApp::new(cli, session);
    event_loop.run_app(&mut app).expect("Event loop error");
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencraft::player::inventory::{
        CraftError, CraftingGrid, GridKind, Inventory, InventoryError, ItemId, ItemStack,
        HOTBAR_SLOTS,
    };
    use opencraft::ui::SettingsCaller;
    use opencraft::world::catalog::{ConfirmOutcome, DeleteConfirmation, RowProblem};
    use opencraft::world::chunk::{Chunk, ChunkPos};
    use opencraft::world::terrain::generate_chunk_seeded;
    use rustc_hash::FxHashMap;
    use std::path::PathBuf;

    use opencraft::player::camera::Camera;
    use opencraft::world::persistence::ChunkEditMap;
    use opencraft::world::terrain::{find_spawn_column, terrain_height_at_seeded};

    use glam::IVec3;

    #[test]
    fn menu_text_lists_every_row_with_its_live_value() {
        let settings = Settings::default();
        let mut menu = menu::SettingsMenu::new();
        menu.open();
        let text = compose_menu_text(&settings, &menu, "Default");
        assert!(text.contains("Move Forward: KeyW"));
        assert!(text.contains("Move Backward: KeyS"));
        assert!(text.contains("Move Left: KeyA"));
        assert!(text.contains("Move Right: KeyD"));
        assert!(text.contains("Jump / Up: Space"));
        assert!(text.contains("Crouch / Down: ShiftLeft"));
        assert!(text.contains("Sprint: ControlLeft"));
        assert!(text.contains("Mouse Sensitivity: 0.0022"));
        assert!(text.contains("Field of View: 75"));
        assert!(text.contains("Render Distance: 16"));
        assert!(text.contains("Fullscreen: Off"));
        assert!(text.contains("VSync: Off"));
        assert!(text.contains("Volume: 100%"));
        assert!(text.contains("Texture Pack: Default"));
        assert!(text.contains("Close Menu"));
        assert_eq!(text.lines().count(), menu::ROW_COUNT);
        assert!(
            !text.contains("press a key for"),
            "no capture hint while just open"
        );
    }

    #[test]
    fn menu_text_reflects_changed_settings_values() {
        let settings = Settings {
            keybinds: opencraft_keybinds_jump_arrow_up(),
            mouse_sensitivity: 0.005,
            fov_degrees: 90.0,
            render_distance: 24,
            fullscreen: true,
            vsync: true,
            texture_pack: None,
            volume: 100,
        };
        let menu = menu::SettingsMenu::new();
        let text = compose_menu_text(&settings, &menu, "Default");
        assert!(text.contains("Jump / Up: ArrowUp"));
        assert!(text.contains("Mouse Sensitivity: 0.0050"));
        assert!(text.contains("Field of View: 90"));
        assert!(text.contains("Render Distance: 24"));
        assert!(text.contains("Fullscreen: On"));
        assert!(text.contains("VSync: On"));
        assert!(text.contains("Texture Pack: Default"));
    }

    #[test]
    fn menu_text_volume_row_reflects_the_live_setting() {
        for volume in [0u8, 60, 100] {
            let settings = Settings {
                volume,
                ..Settings::default()
            };
            let menu = menu::SettingsMenu::new();
            let text = compose_menu_text(&settings, &menu, "Default");
            assert!(
                text.contains(&format!("Volume: {volume}%")),
                "volume {volume}: {text}"
            );
        }
    }

    #[test]
    fn awaiting_key_row_shows_capture_hint_and_message_line_appends() {
        let settings = Settings::default();
        let mut menu = menu::SettingsMenu::new();
        menu.open();
        menu.begin_rebind(menu::BindAction::JumpOrUp);
        let text = compose_menu_text(&settings, &menu, "Default");
        assert!(text.contains("Jump / Up: <press a key>"));
        assert!(text.contains("press a key for 'jump_or_up' (Escape cancels)"));

        // Transient messages append as the final line.
        menu.set_message_for_ticks("key already bound to 'forward'", 1);
        let text = compose_menu_text(&settings, &menu, "Default");
        assert!(text.ends_with("key already bound to 'forward'"));
    }

    #[test]
    fn startup_warnings_format_into_a_single_message_line() {
        let warnings = vec![
            SettingsWarning {
                field: "video.fov".to_string(),
                reason: "expected a float, found string".to_string(),
            },
            SettingsWarning {
                field: "settings".to_string(),
                reason: "could not read settings file".to_string(),
            },
        ];
        // Same composition the Escape open path uses (join(" | ")).
        let joined = menu::format_warnings(&warnings).join(" | ");
        assert_eq!(
            joined,
            "video.fov: expected a float, found string | settings: could not read settings file"
        );
    }

    #[test]
    fn adjust_direction_maps_left_up_right_down_and_nothing_else() {
        assert_eq!(
            adjust_direction(MouseButton::Left),
            Some(AdjustDirection::Up)
        );
        assert_eq!(
            adjust_direction(MouseButton::Right),
            Some(AdjustDirection::Down)
        );
        assert_eq!(adjust_direction(MouseButton::Middle), None);
        assert_eq!(adjust_direction(MouseButton::Back), None);
    }

    #[test]
    fn default_settings_fov_matches_camera_default_fov() {
        // Wiring guard for the settings-driven FOV (S04-T04): GameApp::new
        // now copies fov_degrees.to_radians() onto the camera at startup,
        // so the default settings FOV must equal Camera::new's hardcoded
        // 75-degree default — the source-of-truth swap stays neutral.
        let camera = Camera::new(Vec3::ZERO);
        assert_eq!(
            camera.fov_y_radians,
            Settings::default().fov_degrees.to_radians()
        );
    }

    #[test]
    fn gameplay_keys_route_through_the_loaded_keybind_map_press_and_release() {
        let mut input = InputState::default();
        let binds = Keybinds::default();
        let presses = [
            (KeyCode::KeyW, "forward"),
            (KeyCode::KeyS, "backward"),
            (KeyCode::KeyA, "left"),
            (KeyCode::KeyD, "right"),
            (KeyCode::Space, "jump_or_up"),
            (KeyCode::ShiftLeft, "crouch_or_down"),
            (KeyCode::ControlLeft, "sprint"),
        ];
        for (code, name) in presses {
            assert!(
                apply_gameplay_key(&mut input, &binds, code, true),
                "{name} press routes through the map"
            );
        }
        assert!(
            input.forward
                && input.backward
                && input.left
                && input.right
                && input.jump_or_up
                && input.crouch_or_down
                && input.sprint
        );
        // releases clear the same flags through the same map — both
        // transitions route identically (pre-rebind semantics preserved).
        for (code, name) in presses {
            assert!(
                apply_gameplay_key(&mut input, &binds, code, false),
                "{name} release routes through the map"
            );
        }
        assert!(
            !input.forward
                && !input.backward
                && !input.left
                && !input.right
                && !input.jump_or_up
                && !input.crouch_or_down
                && !input.sprint
        );
    }

    #[test]
    fn rebound_keys_take_effect_and_the_replaced_key_goes_dead() {
        // same rebind as the compose test: jump_or_up from Space to ArrowUp
        let binds = opencraft_keybinds_jump_arrow_up();
        let mut input = InputState::default();
        assert!(apply_gameplay_key(
            &mut input,
            &binds,
            KeyCode::ArrowUp,
            true
        ));
        assert!(input.jump_or_up, "the new key jumps");
        // the replaced key is unbound now — Space no longer jumps
        assert!(!apply_gameplay_key(
            &mut input,
            &binds,
            KeyCode::Space,
            true
        ));
        // an unbound press must not disturb the flag either way: the
        // ArrowUp press keeps it set, and no stick/clear happens from Space
        assert!(
            input.jump_or_up,
            "an unbound key press must not clear the flag"
        );
        // release through the new key clears the flag
        assert!(apply_gameplay_key(
            &mut input,
            &binds,
            KeyCode::ArrowUp,
            false
        ));
        assert!(!input.jump_or_up);
        // the old key's release is unbound too and must not resurrect it
        assert!(!apply_gameplay_key(
            &mut input,
            &binds,
            KeyCode::Space,
            false
        ));
        assert!(!input.jump_or_up);
    }

    #[test]
    fn only_the_bound_crouch_and_sprint_keys_trigger() {
        // intentional narrowing (documented on Keybinds in src/settings):
        // rebindable bindings are single keys, so ShiftRight/ControlRight no
        // longer crouch/sprint the way the old hardcoded pairs did.
        let binds = Keybinds::default();
        let mut input = InputState::default();
        assert!(!apply_gameplay_key(
            &mut input,
            &binds,
            KeyCode::ShiftRight,
            true
        ));
        assert!(!apply_gameplay_key(
            &mut input,
            &binds,
            KeyCode::ControlRight,
            true
        ));
        assert_eq!(
            format!("{:?}", input),
            format!("{:?}", InputState::default()),
            "unbound siblings must not touch InputState"
        );
    }

    #[test]
    fn non_gameplay_keys_report_not_bound_and_leave_input_untouched() {
        let binds = Keybinds::default();
        let mut input = InputState::default();
        for code in [
            KeyCode::KeyQ,
            KeyCode::KeyZ,
            KeyCode::KeyF,
            KeyCode::F5,
            KeyCode::Escape,
        ] {
            assert!(
                !apply_gameplay_key(&mut input, &binds, code, true),
                "{code:?} is not a gameplay bind"
            );
        }
        assert_eq!(
            format!("{:?}", input),
            format!("{:?}", InputState::default())
        );
    }

    #[test]
    fn duplicate_bindings_in_a_hand_edited_file_resolve_first_match() {
        // load does not deduplicate; first-match priority in Keybinds
        // declaration order keeps routing deterministic instead of
        // last-write-wins per flag.
        let binds = Keybinds {
            backward: KeyCode::KeyW, // collides with forward
            ..Keybinds::default()
        };
        let mut input = InputState::default();
        assert!(apply_gameplay_key(&mut input, &binds, KeyCode::KeyW, true));
        assert!(input.forward, "forward is declared first and wins");
        assert!(!input.backward, "the duplicate loser stays clear");
    }

    #[test]
    fn cli_bypass_condition_is_exactly_the_three_flag_disjunction() {
        // Bare launch: no CLI world intent — the main menu owns startup.
        assert!(!cli_has_world_intent(&CliConfig::parse_args(
            Vec::<String>::new()
        )));
        // The default save path alone is NOT an intent (byte-identical default).
        assert!(!cli_has_world_intent(&CliConfig::parse_args([
            "--save",
            "saves/default.mcvx"
        ])));
        // Each flag alone forces the direct-launch bypass into gameplay.
        assert!(cli_has_world_intent(&CliConfig::parse_args(["--new"])));
        assert!(cli_has_world_intent(&CliConfig::parse_args([
            "--new-world"
        ])));
        assert!(cli_has_world_intent(&CliConfig::parse_args([
            "--seed", "7"
        ])));
        assert!(cli_has_world_intent(&CliConfig::parse_args([
            "--save",
            "other.mcvx"
        ])));
        // `--time-scale` is a test seam, NOT a world intent: a bare
        // accelerated launch still opens the main menu.
        assert!(!cli_has_world_intent(&CliConfig::parse_args([
            "--time-scale",
            "60"
        ])));
        let scaled = GameApp::new(
            CliConfig::parse_args(["--time-scale", "60"]),
            WorldSession::new_session(9, PathBuf::from("unused.mcvx"), DEFAULT_SEASON_LENGTH_DAYS),
        );
        assert_eq!(scaled.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert_eq!(
            scaled.time_scale, 60.0,
            "the CLI scale reaches the app but never forces gameplay"
        );
        // `--season-length` is a fresh-world parameter, NOT a world intent:
        // on its own it must not bypass the main menu (same rule as
        // `--time-scale`).
        assert!(!cli_has_world_intent(&CliConfig::parse_args([
            "--season-length",
            "4"
        ])));
        // Every flag together still bypasses (the smoke scripts' launches).
        assert!(cli_has_world_intent(&CliConfig::parse_args([
            "--new",
            "--seed",
            "999",
            "--save",
            "custom.mcvx"
        ])));
    }

    /// One frame of clock progression exactly as the RedrawRequested pass
    /// does it: `playing` from the live lifecycle screen, scale from the app.
    fn tick_app_clock(app: &mut GameApp, now: Instant) {
        let playing = app.lifecycle.screen() == LifecycleScreen::Playing;
        app.session.clock.tick(playing, now, app.time_scale);
    }

    /// A headless Playing app owning a tempdir save path (no window/GPU).
    fn playing_app(dir: &Path, name: &str) -> GameApp {
        let save_path = dir.join(name);
        let cli =
            CliConfig::parse_args(["--new", "--save", save_path.to_str().expect("utf8 path")]);
        let app = GameApp::new(
            cli,
            WorldSession::new_session(11, save_path, DEFAULT_SEASON_LENGTH_DAYS),
        );
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        app
    }

    #[test]
    fn game_clock_freezes_while_paused_and_resume_leaks_no_paused_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = playing_app(dir.path(), "freeze.mcvx");
        let start = Instant::now();

        // First playing frame only arms the marker (zero advance).
        tick_app_clock(&mut app, start);
        assert_eq!(app.session.clock.time_of_day, daycycle::DEFAULT_TIME_OF_DAY);

        // One real second at scale 1.0 is 20 ticks.
        tick_app_clock(&mut app, start + Duration::from_secs(1));
        let before_pause = app.session.clock.time_of_day;
        assert_eq!(
            before_pause,
            (daycycle::DEFAULT_TIME_OF_DAY + 20) % daycycle::DAY_TICKS
        );

        // Pause: the frame loop keeps ticking the clock with playing=false
        // across a long simulated wall-clock gap. Game time must not move.
        app.enter_screen(LifecycleScreen::Pause);
        tick_app_clock(&mut app, start + Duration::from_secs(2));
        tick_app_clock(&mut app, start + Duration::from_secs(600));
        assert_eq!(
            app.session.clock.time_of_day, before_pause,
            "paused frames never advance game time"
        );

        // Resume: `enter_playing` carries NO clock hook; the clock's own
        // disarm makes the first resumed frame contribute zero, so the 600 s
        // pause cannot leak. The next frame advances normally again.
        app.enter_playing();
        tick_app_clock(&mut app, start + Duration::from_secs(600));
        assert_eq!(
            app.session.clock.time_of_day, before_pause,
            "the first frame after resume contributes zero, no pause leak"
        );
        tick_app_clock(&mut app, start + Duration::from_secs(601));
        assert_eq!(
            app.session.clock.time_of_day,
            (before_pause + 20) % daycycle::DAY_TICKS,
            "normal advance resumes on the second playing frame"
        );
    }

    #[test]
    fn game_clock_wraps_at_cycle_end_under_accelerated_ticks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = playing_app(dir.path(), "wrap.mcvx");
        app.time_scale = 60.0;
        app.session.clock =
            opencraft::world::daycycle::GameClock::from_time(daycycle::DAY_TICKS - 10);

        let start = Instant::now();
        tick_app_clock(&mut app, start);
        // 1 s * 60 * 20 ticks/s = 1,200 ticks: crosses the cycle boundary.
        tick_app_clock(&mut app, start + Duration::from_secs(1));

        assert_eq!(
            app.session.clock.time_of_day,
            (daycycle::DAY_TICKS - 10 + 1_200) % daycycle::DAY_TICKS
        );
        assert!(app.session.clock.time_of_day < daycycle::DAY_TICKS);
    }

    #[test]
    fn accelerated_clock_covers_in_seconds_what_scale_one_covers_in_minutes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = playing_app(dir.path(), "scale.mcvx");
        let start = Instant::now();

        // Scale 1.0 for one second.
        tick_app_clock(&mut app, start);
        tick_app_clock(&mut app, start + Duration::from_secs(1));
        let normal_advance = app.session.clock.time_of_day - daycycle::DEFAULT_TIME_OF_DAY;

        // Fresh clock, scale 60 for one second: 60x the ticks.
        app.session.clock =
            opencraft::world::daycycle::GameClock::from_time(daycycle::DEFAULT_TIME_OF_DAY);
        app.time_scale = 60.0;
        tick_app_clock(&mut app, start);
        tick_app_clock(&mut app, start + Duration::from_secs(1));
        let accelerated_advance = app.session.clock.time_of_day - daycycle::DEFAULT_TIME_OF_DAY;

        assert_eq!(normal_advance, 20);
        assert_eq!(accelerated_advance, normal_advance * 60);
    }

    #[test]
    fn demo_cycle_watch_completes_after_one_full_day_and_never_arms_at_default_scale() {
        // A normal (1.0x) run is unbounded: the bounded-demo seam never arms.
        let mut watch = None;
        for time in [0, 6_000, 12_000, 18_000, 23_999] {
            assert!(!observe_demo_cycle(
                &mut watch,
                DEFAULT_TIME_SCALE,
                true,
                time
            ));
        }
        assert!(watch.is_none(), "default scale never starts a demo watch");

        // An accelerated run arms on the first Playing frame and completes
        // only once a full 24,000-tick cycle has been observed.
        let mut watch = None;
        let start = daycycle::DEFAULT_TIME_OF_DAY;
        assert!(!observe_demo_cycle(&mut watch, 60.0, true, start));
        assert!(!observe_demo_cycle(&mut watch, 60.0, true, start + 12_000));
        assert!(!observe_demo_cycle(
            &mut watch,
            60.0,
            true,
            (start + daycycle::DAY_TICKS - 1) % daycycle::DAY_TICKS
        ));
        assert!(observe_demo_cycle(
            &mut watch,
            60.0,
            true,
            (start + daycycle::DAY_TICKS) % daycycle::DAY_TICKS
        ));
    }

    #[test]
    fn demo_cycle_watch_is_wrap_safe_and_ignores_paused_frames() {
        let mut watch = Some(DemoCycleWatch::new(daycycle::DAY_TICKS - 10));

        // Paused frames contribute nothing: the real clock is frozen there.
        assert!(!observe_demo_cycle(
            &mut watch,
            60.0,
            false,
            daycycle::DAY_TICKS - 10
        ));

        // 15 ticks across the cycle boundary...
        assert!(!observe_demo_cycle(&mut watch, 60.0, true, 5));
        // ...and exactly the remaining ticks complete the full cycle.
        assert!(observe_demo_cycle(
            &mut watch,
            60.0,
            true,
            (5 + daycycle::DAY_TICKS - 15) % daycycle::DAY_TICKS
        ));
    }

    #[test]
    fn accelerated_app_run_reaches_the_bounded_demo_cycle_after_twenty_seconds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = playing_app(dir.path(), "demo.mcvx");
        app.time_scale = 60.0;
        let start = Instant::now();

        // First Playing frame arms both the clock marker and the demo watch.
        tick_app_clock(&mut app, start);
        assert!(!observe_demo_cycle(
            &mut app.demo_cycle,
            app.time_scale,
            true,
            app.session.clock.time_of_day
        ));

        // 60x covers the 1,200 s / 24,000-tick cycle in 20 real seconds.
        let mut frames = 0;
        let mut finished = false;
        while frames < 25 && !finished {
            frames += 1;
            tick_app_clock(&mut app, start + Duration::from_secs(frames));
            finished = observe_demo_cycle(
                &mut app.demo_cycle,
                app.time_scale,
                true,
                app.session.clock.time_of_day,
            );
        }

        assert!(
            finished,
            "a scale-60 run must complete one full cycle in 20 simulated seconds"
        );
        assert_eq!(frames, 20, "24,000 ticks / 1,200 ticks-per-second = 20 s");
    }

    #[test]
    fn accelerated_runs_get_a_bounded_wall_clock_shutdown_deadline() {
        // Default and slower-than-real scales are unbounded normal runs.
        assert!(demo_shutdown_deadline(DEFAULT_TIME_SCALE).is_none());
        assert!(demo_shutdown_deadline(0.5).is_none());

        // 60x: a 20 s cycle plus the 5 s redraw-stall grace.
        let before = Instant::now();
        let deadline = demo_shutdown_deadline(60.0).expect("accelerated deadline");
        let after = Instant::now();
        assert!(deadline >= before + Duration::from_secs(24));
        assert!(deadline <= after + Duration::from_secs(26));
    }

    #[test]
    fn saved_time_of_day_resumes_exactly_across_a_save_and_relaunch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("time.mcvx");
        let mut app = playing_app(dir.path(), "time.mcvx");
        app.session.clock = opencraft::world::daycycle::GameClock::from_time(17_432);
        assert_ne!(17_432, daycycle::DEFAULT_TIME_OF_DAY);

        // The real save path writes `session.clock.time_of_day`.
        save_current_world(&mut app.session).expect("save succeeds");

        // Session-level relaunch: load the file, round-trip it through the
        // bytes API, and rebuild a session the way startup does.
        let loaded = load_world_from_path(&save_path).expect("load succeeds");
        let mut bytes = Vec::new();
        opencraft::world::persistence::save_world_to_bytes(&mut bytes, &loaded)
            .expect("re-save to bytes");
        let decoded = opencraft::world::persistence::load_world_from_bytes(&bytes)
            .expect("bytes load succeeds");
        let relaunched = WorldSession::session_from_save(decoded, save_path.clone());

        assert_eq!(relaunched.clock.time_of_day, 17_432);
        assert_eq!(relaunched.save_path, save_path);
    }

    #[test]
    fn accelerated_demo_cycle_saves_and_reloads_day_index_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("calendar.mcvx");
        let mut app = playing_app(dir.path(), "calendar.mcvx");
        app.time_scale = 60.0;
        app.session.season_length_days = 4;
        let start = Instant::now();

        // The same frame loop the bounded demo runs: one full cycle at 60x
        // takes 20 simulated seconds (24,000 ticks / 1,200 per second).
        tick_app_clock(&mut app, start);
        assert!(!observe_demo_cycle(
            &mut app.demo_cycle,
            app.time_scale,
            true,
            app.session.clock.time_of_day
        ));
        let mut finished = false;
        let mut frames = 0;
        while frames < 25 && !finished {
            frames += 1;
            tick_app_clock(&mut app, start + Duration::from_secs(frames));
            finished = observe_demo_cycle(
                &mut app.demo_cycle,
                app.time_scale,
                true,
                app.session.clock.time_of_day,
            );
        }
        assert!(finished, "the accelerated demo must complete one cycle");

        // From the default noon start, one full cycle returns the clock to
        // noon and crosses dawn (tick 0) exactly once: day index 1.
        assert_eq!(app.session.clock.time_of_day, daycycle::DEFAULT_TIME_OF_DAY);
        assert_eq!(app.session.clock.day_index, 1);

        // The demo's shutdown save persists the calendar tail; the reload
        // restores both fields byte-exactly.
        save_current_world(&mut app.session).expect("demo shutdown save succeeds");
        let loaded = load_world_from_path(&save_path).expect("load succeeds");
        assert_eq!(loaded.day_index, 1);
        assert_eq!(loaded.season_length_days, 4);
        let relaunched = WorldSession::session_from_save(loaded, save_path.clone());
        assert_eq!(relaunched.clock.day_index, 1);
        assert_eq!(relaunched.season_length_days, 4);
    }

    #[test]
    fn startup_session_honors_season_length_on_the_fresh_world_branch_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fresh_path = dir.path().join("fresh.mcvx");
        let path_str = fresh_path.to_str().expect("utf8 path");

        // Fresh branch: the CLI seam overrides the default season length.
        let cli = CliConfig::parse_args(["--new", "--save", path_str, "--season-length", "4"]);
        let session = startup_session(&cli);
        assert_eq!(session.season_length_days, 4);
        assert_eq!(session.clock.day_index, 0, "fresh worlds start on day 0");

        // ...and the default applies when the flag is absent.
        let bare_fresh = CliConfig::parse_args(["--new", "--save", path_str]);
        assert_eq!(
            startup_session(&bare_fresh).season_length_days,
            DEFAULT_SEASON_LENGTH_DAYS
        );

        // Load branch: a persisted calendar wins over the launch flag.
        let mut data = sample_save(42);
        data.day_index = 3;
        data.season_length_days = 8;
        save_world_to_path(&fresh_path, &data).expect("write calendar save");
        let load_cli = CliConfig::parse_args(["--save", path_str, "--season-length", "2"]);
        let loaded = startup_session(&load_cli);
        assert_eq!(loaded.clock.day_index, 3, "persisted day index wins");
        assert_eq!(
            loaded.season_length_days, 8,
            "a load ignores --season-length"
        );
    }

    /// A headless GameApp for seam tests: no window/GPU, a tempdir catalog,
    /// and the given screen entered through the real `enter_screen` path
    /// (LoadGame entry refreshes rows from the tempdir catalog).
    fn app_at_screen(saves_dir: &Path, screen: LifecycleScreen) -> GameApp {
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(
                1,
                PathBuf::from("/nonexistent/initial.mcvx"),
                DEFAULT_SEASON_LENGTH_DAYS,
            ),
        );
        app.catalog = WorldCatalogFs::new(saves_dir);
        app.enter_screen(screen);
        app
    }

    #[test]
    fn game_app_in_test_builds_owns_a_silent_audio_engine() {
        // The suite builds many headless GameApps; the cfg(test) constructor
        // must never open a real device/CPAL stream. Playback itself is
        // unreachable from the test-driven free helpers (the wiring rule),
        // so a silent engine is the complete test-side contract.
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at_screen(dir.path(), LifecycleScreen::MainMenu);
        assert!(
            app.audio.is_silent(),
            "test GameApps must use AudioEngine::disabled()"
        );
        // The real play surface is still callable and a no-op at every
        // volume boundary (0 mutes, 100 normal, >100 clamps internally).
        for slot in SoundSlot::ALL {
            app.audio.play(slot, 0);
            app.audio.play(slot, app.settings.volume);
            app.audio.play(slot, 255);
        }
    }

    #[test]
    fn fresh_game_app_has_a_fixed_seed_vista_at_the_settings_render_distance() {
        let app = app_at_screen(Path::new("/nonexistent/saves"), LifecycleScreen::MainMenu);
        assert_eq!(app.vista_world.seed(), vista::VISTA_SEED);
        assert_eq!(
            app.vista_world.render_distance(),
            app.settings.render_distance,
            "the vista follows the settings-driven render distance"
        );
        // The vista is its own world, not a hidden alias of the session.
        assert_ne!(
            app.vista_world.seed(),
            app.session.world.seed(),
            "vista and session are distinct, independently seeded worlds"
        );
    }

    #[test]
    fn render_distance_adjust_updates_the_session_and_vista_worlds_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(
            dir.path(),
            LifecycleScreen::Settings(SettingsCaller::MainMenu),
        );
        // Keep the click's settings save inside the tempdir, never the crate
        // root (S04-T02's injection precedent).
        app.settings_path = dir.path().join("settings.toml");
        app.settings.render_distance = 16;
        app.session.world.set_render_distance(16);
        app.vista_world.set_render_distance(16);

        // The exact helper the MenuRow::RenderDistance click arm calls
        // (menu_click itself requires a live ActiveEventLoop/Window; the
        // RenderDistance arm's whole behavior lives in this helper).
        app.apply_render_distance_adjust(AdjustDirection::Up);

        assert_eq!(app.settings.render_distance, 18);
        assert_eq!(app.session.world.render_distance(), 18);
        assert_eq!(app.vista_world.render_distance(), 18);
        assert!(
            dir.path().join("settings.toml").exists(),
            "the click saves the settings exactly as before"
        );

        // Boundary: at the max the click clamps and both worlds stay
        // pinned to the same clamped value (negative-path guard).
        app.settings.render_distance = 36;
        app.session.world.set_render_distance(36);
        app.vista_world.set_render_distance(36);
        app.apply_render_distance_adjust(AdjustDirection::Up);
        assert_eq!(app.settings.render_distance, 36);
        assert_eq!(app.session.world.render_distance(), 36);
        assert_eq!(app.vista_world.render_distance(), 36);
    }

    // --- texture pack wiring (M004/S03) ---

    /// A headless GameApp whose pack root and settings save path live inside
    /// `dir` (the same injection pattern as `settings_path`). Defaults to no
    /// pack selected so each test drives the cycle from a known state.
    fn app_with_packs(dir: &Path) -> GameApp {
        let mut app = app_at_screen(dir, LifecycleScreen::Settings(SettingsCaller::MainMenu));
        app.settings_path = dir.join("settings.toml");
        app.packs_root = dir.join("packs");
        app.available_packs = pack::available_pack_names(&app.packs_root);
        app.settings.texture_pack = None;
        app.current_pack = None;
        app
    }

    fn write_pack_fixture(dir: &Path, manifest_name: &str) {
        std::fs::create_dir_all(dir).expect("create pack dir");
        std::fs::write(
            dir.join("pack.toml"),
            format!("name = \"{manifest_name}\"\n"),
        )
        .expect("write pack.toml");
    }

    fn write_solid_png(path: &Path, rgba: [u8; 4]) {
        let file = std::fs::File::create(path).expect("create fixture png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 16, 16);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        let mut data = Vec::with_capacity(16 * 16 * 4);
        for _ in 0..16 * 16 {
            data.extend_from_slice(&rgba);
        }
        writer.write_image_data(&data).expect("write png data");
    }

    #[test]
    fn menu_text_shows_the_active_texture_pack_label() {
        let settings = Settings::default();
        let menu = menu::SettingsMenu::new();
        let text = compose_menu_text(&settings, &menu, "Alpha Pack");
        assert!(text.contains("Texture Pack: Alpha Pack"));
        assert_eq!(text.lines().count(), menu::ROW_COUNT);
    }

    /// Canonical 44-byte-header RIFF/WAVE PCM16 fixture, matching the shape
    /// `src/pack/sound.rs` tests use, so the GameApp wiring tests exercise
    /// rodio's real symphonia decoder rather than a mock.
    fn write_wav_fixture(path: &Path, channels: u16, sample_rate: u32) {
        let samples: Vec<i16> = (0..128usize)
            .map(|i| ((i.wrapping_mul(517) % 20_000) as i16) - 10_000)
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let block_align = channels * 2;
        let byte_rate = sample_rate * u32::from(block_align);
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, bytes).expect("write wav fixture");
    }

    #[test]
    fn texture_pack_cycle_advances_to_the_first_pack_and_persists_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        write_pack_fixture(&packs_root.join("alpha"), "Alpha Pack");
        write_pack_fixture(&packs_root.join("beta"), "Beta Pack");
        // A real override layer proves the loaded pack's layers reach
        // GameApp (not just its manifest).
        write_solid_png(
            &packs_root.join("alpha").join("grass_top.png"),
            [200, 10, 20, 255],
        );
        app.available_packs = pack::available_pack_names(&packs_root);
        assert_eq!(app.available_packs, vec!["alpha", "beta"]);

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack.as_deref(), Some("alpha"));
        let loaded = app.current_pack.as_ref().expect("pack loaded");
        assert_eq!(loaded.display_name, "Alpha Pack");
        assert_eq!(loaded.layers.len(), 16);
        assert_ne!(
            loaded.layers[0],
            generate_all_texture_layers_rgba()[0],
            "the pack's authored layer replaces the procedural one"
        );
        assert!(loaded.warnings.is_empty(), "clean pack has no warnings");

        // The choice is durable at the injected path.
        let (reloaded, warnings) = load_with_warnings(&app.settings_path);
        assert!(warnings.is_empty());
        assert_eq!(reloaded.texture_pack.as_deref(), Some("alpha"));
    }

    #[test]
    fn texture_pack_cycle_wraps_the_last_pack_back_to_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        write_pack_fixture(&packs_root.join("alpha"), "Alpha Pack");
        app.available_packs = pack::available_pack_names(&packs_root);
        app.settings.texture_pack = Some("alpha".to_string());
        app.current_pack = Some(pack::load_pack_by_name(&packs_root, "alpha"));

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack, None, "wraps to Default");
        assert!(app.current_pack.is_none(), "Default holds no pack data");
        let (reloaded, warnings) = load_with_warnings(&app.settings_path);
        assert!(warnings.is_empty());
        assert_eq!(reloaded.texture_pack, None);
    }

    #[test]
    fn texture_pack_cycle_from_a_deleted_pack_lands_on_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        write_pack_fixture(&packs_root.join("alpha"), "Alpha Pack");
        app.available_packs = pack::available_pack_names(&packs_root);
        // The persisted choice names a pack that is no longer on disk.
        app.settings.texture_pack = Some("deleted".to_string());
        app.current_pack = Some(pack::load_pack_by_name(&packs_root, "deleted"));

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack, None);
        assert!(app.current_pack.is_none());
    }

    #[test]
    fn texture_pack_cycle_surfaces_load_warnings_in_the_menu_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        let broken = packs_root.join("broken");
        write_pack_fixture(&broken, "Broken Pack");
        std::fs::write(broken.join("grass_top.png"), b"not a png").expect("corrupt layer");
        app.available_packs = pack::available_pack_names(&packs_root);

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack.as_deref(), Some("broken"));
        let loaded = app.current_pack.as_ref().expect("fallback pack loaded");
        assert_eq!(loaded.layers.len(), 16, "fallback still fully usable");
        let message = app.menu.message.as_ref().expect("warning in the menu");
        assert!(
            message.text.contains("grass_top.png"),
            "warning names the failing layer: {}",
            message.text
        );
    }

    #[test]
    fn texture_pack_cycle_swaps_in_valid_pack_sounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        write_pack_fixture(&packs_root.join("soundy"), "Soundy Pack");
        write_wav_fixture(&packs_root.join("soundy").join("break.wav"), 1, 22_050);
        app.available_packs = pack::available_pack_names(&packs_root);
        assert_eq!(
            *app.audio.buffer(SoundSlot::Break),
            default_buffers()[SoundSlot::Break.index()],
            "baseline is the synthesized default"
        );

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack.as_deref(), Some("soundy"));
        let swapped = app.audio.buffer(SoundSlot::Break);
        assert_eq!(swapped.channels(), 1, "mono WAV keeps its channel count");
        assert_eq!(swapped.sample_rate(), 22_050, "rate passes through");
        assert_ne!(
            *swapped,
            default_buffers()[SoundSlot::Break.index()],
            "the pack file replaces the synthesized buffer"
        );
        // Slots the pack does not provide stay on the synthesized default.
        assert_eq!(
            *app.audio.buffer(SoundSlot::Place),
            default_buffers()[SoundSlot::Place.index()]
        );
        assert!(
            app.menu.message.is_none(),
            "a clean sound pack emits no warning"
        );
    }

    #[test]
    fn texture_pack_cycle_surfaces_corrupt_sound_warning_naming_the_bare_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        let broken = packs_root.join("broken-sound");
        write_pack_fixture(&broken, "Broken Sound Pack");
        std::fs::write(broken.join("place.wav"), b"not a wav at all").expect("corrupt sound");
        app.available_packs = pack::available_pack_names(&packs_root);

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack.as_deref(), Some("broken-sound"));
        let message = app
            .menu
            .message
            .as_ref()
            .expect("sound warning in the menu");
        assert!(
            message.text.contains("place.wav"),
            "warning names the bare sound file: {}",
            message.text
        );
        assert_eq!(
            *app.audio.buffer(SoundSlot::Place),
            default_buffers()[SoundSlot::Place.index()],
            "the corrupt slot falls back to the synthesized default"
        );
    }

    #[test]
    fn texture_pack_cycle_back_to_default_restores_synthesized_sounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_packs(dir.path());
        let packs_root = app.packs_root.clone();
        write_pack_fixture(&packs_root.join("alpha"), "Alpha Pack");
        write_wav_fixture(&packs_root.join("alpha").join("break.wav"), 1, 22_050);
        app.available_packs = pack::available_pack_names(&packs_root);
        // Start on the pack with its sounds already applied, as a real run
        // would after cycling onto it.
        app.settings.texture_pack = Some("alpha".to_string());
        app.current_pack = Some(pack::load_pack_by_name(&packs_root, "alpha"));
        let (pack_buffers, _) = pack::sound::load_sound_overrides(&packs_root.join("alpha"));
        app.audio.set_buffers(pack_buffers);
        assert_ne!(
            *app.audio.buffer(SoundSlot::Break),
            default_buffers()[SoundSlot::Break.index()]
        );

        app.apply_texture_pack_cycle();

        assert_eq!(app.settings.texture_pack, None, "wraps to Default");
        for slot in SoundSlot::ALL {
            assert_eq!(
                *app.audio.buffer(slot),
                default_buffers()[slot.index()],
                "Default must restore the synthesized {} sound",
                slot.name()
            );
        }
    }

    #[test]
    fn startup_sound_overrides_merge_warnings_and_apply_buffers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packs_root = dir.path().join("packs");
        let pack_dir = packs_root.join("soundy");
        write_pack_fixture(&pack_dir, "Soundy Pack");
        write_wav_fixture(&pack_dir.join("break.wav"), 1, 22_050);
        std::fs::write(pack_dir.join("ui_click.wav"), b"junk").expect("corrupt sound");

        // Mirrors GameApp::new: pack warnings and sound warnings feed the same
        // startup Vec shown once through the Settings-entry path (MEM009).
        let (_, mut startup_warnings) = load_startup_pack(&packs_root, Some("soundy"));
        let (buffers, sound_warnings) = load_startup_sounds(&packs_root, Some("soundy"));
        startup_warnings.extend(sound_warnings);

        assert!(
            startup_warnings.iter().any(|w| w.field == "ui_click.wav"),
            "the corrupt sound reaches the startup warning Vec: {startup_warnings:?}"
        );
        assert_ne!(
            buffers[SoundSlot::Break.index()],
            default_buffers()[SoundSlot::Break.index()],
            "the valid startup sound is applied"
        );
        assert_eq!(
            buffers[SoundSlot::UiClick.index()],
            default_buffers()[SoundSlot::UiClick.index()],
            "the corrupt slot falls back"
        );

        // Default selection keeps the synthesized set and adds nothing.
        let (default_sounds, default_warnings) = load_startup_sounds(&packs_root, None);
        assert_eq!(default_sounds, default_buffers());
        assert!(default_warnings.is_empty());
    }

    #[test]
    fn load_startup_pack_corrupt_logo_falls_back_to_none_with_file_named_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packs_root = dir.path().join("packs");
        let broken = packs_root.join("broken");
        write_pack_fixture(&broken, "Broken Pack");
        std::fs::write(
            broken.join("pack.toml"),
            "name = \"Broken Pack\"\n[menu]\nlogo = \"title.png\"\n",
        )
        .expect("write pack.toml");
        std::fs::write(broken.join("title.png"), b"not a png").expect("corrupt logo");

        let (pack, warnings) = load_startup_pack(&packs_root, Some("broken"));

        let pack = pack.expect("a usable pack is still returned");
        assert!(
            pack.logo.is_none(),
            "corrupt logo falls back to the text title"
        );
        assert_eq!(pack.layers.len(), 16, "layers stay fully usable");
        assert!(
            warnings.iter().any(|w| w.field == "title.png"),
            "warning names the bare logo file: {warnings:?}"
        );
    }

    #[test]
    fn load_startup_pack_valid_logo_keeps_native_dimensions_without_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packs_root = dir.path().join("packs");
        let branded = packs_root.join("branded");
        write_pack_fixture(&branded, "Branded Pack");
        std::fs::write(
            branded.join("pack.toml"),
            "name = \"Branded Pack\"\n[menu]\nlogo = \"logo.png\"\noverlay_tint = [0, 0, 0, 120]\n",
        )
        .expect("write pack.toml");
        write_solid_png(&branded.join("logo.png"), [240, 20, 30, 255]);

        let (pack, warnings) = load_startup_pack(&packs_root, Some("branded"));

        let pack = pack.expect("pack loads");
        let pack_logo = pack.logo.expect("valid logo decodes");
        assert_eq!((pack_logo.width, pack_logo.height), (16, 16));
        assert_eq!(pack_logo.rgba.len(), 16 * 16 * 4);
        assert_eq!(pack.overlay_tint, Some([0, 0, 0, 120]));
        assert!(
            !warnings.iter().any(|w| w.field == "logo.png"),
            "a valid logo emits no warning: {warnings:?}"
        );
    }

    #[test]
    fn load_startup_pack_default_selection_is_none_without_warnings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let packs_root = dir.path().join("packs");

        let (pack, warnings) = load_startup_pack(&packs_root, None);

        assert!(pack.is_none(), "Default is the built-in procedural pack");
        assert!(warnings.is_empty(), "Default emits no warnings");
    }

    /// Loads the chunk at (0,0,0) plus its six face neighbours. `set_block_immediate`
    /// can only remesh a chunk whose full face neighbourhood is resident, so the
    /// headless collection/placement helpers need this halo to observe an edit.
    fn insert_test_chunk_with_face_neighbors(session: &mut WorldSession, block: BlockId) {
        session
            .world
            .insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(block));
        for pos in [
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(-1, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(0, -1, 0),
            ChunkPos::new(0, 0, 1),
            ChunkPos::new(0, 0, -1),
        ] {
            session
                .world
                .insert_chunk_for_test(pos, Chunk::new_uniform(BlockId::Air));
        }
    }

    /// Camera looking straight down at the stone placed by the placement tests.
    fn aim_placement_camera(session: &mut WorldSession) {
        session.camera.position = Vec3::new(0.5, 10.0, 0.5);
        session.camera.yaw = 0.0;
        session.camera.pitch = -std::f32::consts::FRAC_PI_2;
    }

    /// Unwraps an applied break so a rejection fails the test with its reason
    /// instead of a bare `None`.
    fn applied_break(outcome: BreakOutcome) -> (Vec<(ChunkPos, ChunkMesh)>, ToolWearOutcome) {
        match outcome {
            BreakOutcome::Applied { remeshed, wear } => (remeshed, wear),
            BreakOutcome::Rejected(reason) => panic!("expected an applied break, got {reason:?}"),
        }
    }

    #[test]
    fn mining_collects_broken_blocks_into_the_hotbar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session =
            WorldSession::new_session(7, dir.path().join("mined.mcvx"), DEFAULT_SEASON_LENGTH_DAYS);
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);

        let (first, wear) = applied_break(break_mined_block(
            &mut session,
            IVec3::new(0, 10, 0),
            BlockId::Stone,
        ));
        assert!(
            !first.is_empty(),
            "an applied break remeshes at least its own chunk"
        );
        assert_eq!(
            wear,
            ToolWearOutcome::Nothing,
            "a bare-hand break spends no durability"
        );

        session.world.set_block_immediate(0, 10, 0, BlockId::Stone);
        let (second, _) = applied_break(break_mined_block(
            &mut session,
            IVec3::new(0, 10, 0),
            BlockId::Stone,
        ));
        assert!(!second.is_empty());

        assert_eq!(session.world.get_block(0, 10, 0), BlockId::Air);
        // Stone's mined drop is Cobblestone (the stone-tier tool head material).
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Cobblestone,
                count: 2,
            })
        );
    }

    #[test]
    fn mining_collects_the_pre_break_block_not_the_now_air_cell() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session =
            WorldSession::new_session(8, dir.path().join("ore.mcvx"), DEFAULT_SEASON_LENGTH_DAYS);
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);
        session
            .world
            .set_block_immediate(0, 10, 1, BlockId::CoalOre);

        let (remeshed, _) = applied_break(break_mined_block(
            &mut session,
            IVec3::new(0, 10, 1),
            BlockId::CoalOre,
        ));

        assert!(!remeshed.is_empty());
        assert_eq!(session.world.get_block(0, 10, 1), BlockId::Air);
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::CoalOre,
                count: 1,
            })
        );
    }

    #[test]
    fn mining_an_unloaded_chunk_collects_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            9,
            dir.path().join("missing.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );

        let outcome = break_mined_block(&mut session, IVec3::new(80, 10, 0), BlockId::Stone);

        assert!(
            matches!(outcome, BreakOutcome::Rejected(BreakRejection::StaleTarget)),
            "no loaded chunk means no live target, so no edit"
        );
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(session.player.hotbar_stack(slot), None);
        }
    }

    #[test]
    fn crafted_table_placed_then_mined_with_a_matching_axe_wears_exactly_once() {
        use opencraft::player::inventory::{Inventory, Item, ItemId, ToolKind, ToolTier};
        use opencraft::player::physics::Player;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut session =
            WorldSession::new_session(11, dir.path().join("loop.mcvx"), DEFAULT_SEASON_LENGTH_DAYS);
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        session.player.position = Vec3::new(0.5, 10.0, 0.5);
        aim_placement_camera(&mut session);

        // Real crafting step: a 2x2 player grid of planks yields one table.
        let mut inventory = Inventory::empty();
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        let planks = Item::new(ItemId::Block(BlockId::OakPlanks), 1).expect("planks");
        for cell in 0..4 {
            player_grid.set(cell, Some(planks)).expect("grid cell");
        }
        let table = player_grid
            .take_result(&mut inventory)
            .expect("four planks craft one table");
        assert_eq!(table.id(), ItemId::Block(BlockId::CraftingTable));

        // A wood axe is the matching tool for a table and lands in slot 1.
        inventory
            .insert(
                Item::new(
                    ItemId::Tool {
                        kind: ToolKind::Axe,
                        tier: ToolTier::Wood,
                    },
                    1,
                )
                .expect("axe"),
            )
            .expect("axe fits");
        session.player = Player::with_inventory(
            session.player.position,
            inventory,
            player_grid,
            CraftingGrid::new(GridKind::Table),
        );
        assert!(session.player.select_slot(0));

        let placed = try_place_selected_block(&mut session).expect("crafted table places");
        assert!(!placed.is_empty(), "placement remeshes its chunk");
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::CraftingTable);
        assert_eq!(
            session.player.hotbar_stack(0),
            None,
            "placement consumed exactly the crafted table"
        );

        // Target the placed table through the same raycast the game loop uses.
        assert!(session.player.select_slot(1));
        let hit = raycast_voxels(
            session.camera.position,
            session.camera.forward(),
            8.0,
            |x, y, z| session.world.get_block(x, y, z),
        )
        .expect("the placed table is in reach");
        assert_eq!(hit.block_pos, IVec3::new(0, 6, 0));
        let target = MiningTarget::from_block(
            hit.block_pos,
            session
                .world
                .get_block(hit.block_pos.x, hit.block_pos.y, hit.block_pos.z),
            session.player.selected_item(),
        )
        .expect("the table is breakable");
        assert_eq!(
            target.duration(),
            0.8 / 2.0,
            "a wood axe halves the table's base duration"
        );

        // Full mining accumulator run: 0.4s at 0.1s per tick breaks on tick 4.
        let mut mining = MiningState::new();
        let mut ticks = 0;
        loop {
            ticks += 1;
            match mining.tick(0.1, Some(target)) {
                MiningTick::Broke { pos, block } => {
                    assert_eq!(pos, target.pos());
                    assert_eq!(block, BlockId::CraftingTable);
                    break;
                }
                MiningTick::Mining { progress } => {
                    assert!((0.0..1.0).contains(&progress), "progress stays partial");
                }
                MiningTick::Idle => panic!("a valid target must never idle"),
            }
            assert!(ticks < 100, "mining must terminate");
        }
        assert_eq!(
            ticks, 4,
            "a wood axe breaks the table at half base duration"
        );

        let (remeshed, wear) = applied_break(break_mined_block(
            &mut session,
            target.pos(),
            target.block(),
        ));
        assert!(!remeshed.is_empty(), "the applied break remeshes its chunk");
        assert_eq!(
            wear,
            ToolWearOutcome::Worn { remaining: 58 },
            "one applied break spends exactly one durability"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Air);
        assert_eq!(
            session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(58),
            "the selected axe kept its identity and lost one point"
        );
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::CraftingTable,
                count: 1,
            }),
            "the drop lands in the slot placement freed"
        );

        // The accumulator reset on completion, so the next tick starts over.
        assert!(matches!(
            mining.tick(0.1, Some(target)),
            MiningTick::Mining { .. }
        ));

        // A replayed break for the same (now Air) cell is stale: no second
        // wear and no duplicate drop.
        assert!(matches!(
            break_mined_block(&mut session, target.pos(), target.block()),
            BreakOutcome::Rejected(BreakRejection::StaleTarget)
        ));
        assert_eq!(
            session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(58),
            "a rejected replay spends no durability"
        );
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::CraftingTable,
                count: 1,
            }),
            "a rejected replay collects nothing"
        );
    }

    #[test]
    fn rejected_breaks_spend_no_durability_and_mutate_nothing() {
        use opencraft::player::inventory::{Inventory, Item, ToolKind, ToolTier, INVENTORY_SLOTS};
        use opencraft::player::physics::Player;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            12,
            dir.path().join("reject.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Stone, 10)
                .expect("worn pickaxe"),
        );
        session.player = Player::with_inventory(
            session.player.position,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        assert!(session.player.select_slot(0));

        // Stale target: the accumulator claims CoalOre, the world holds Stone.
        assert!(matches!(
            break_mined_block(&mut session, IVec3::new(0, 10, 0), BlockId::CoalOre),
            BreakOutcome::Rejected(BreakRejection::StaleTarget)
        ));
        assert_eq!(
            session.world.get_block(0, 10, 0),
            BlockId::Stone,
            "a stale target is not edited"
        );

        // Structurally unbreakable identities never reach the edit.
        for block in [BlockId::Air, BlockId::Bedrock] {
            assert!(matches!(
                break_mined_block(&mut session, IVec3::new(0, 10, 0), block),
                BreakOutcome::Rejected(BreakRejection::Unbreakable)
            ));
        }

        // Missing/unloaded chunk: the live block is Air, so the target is stale.
        assert!(matches!(
            break_mined_block(&mut session, IVec3::new(80, 10, 0), BlockId::Stone),
            BreakOutcome::Rejected(BreakRejection::StaleTarget)
        ));

        assert_eq!(
            session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(10),
            "every rejection left durability untouched"
        );
        for slot in 1..HOTBAR_SLOTS {
            assert_eq!(
                session.player.hotbar_stack(slot),
                None,
                "no rejection collected a drop"
            );
        }
    }

    #[test]
    fn last_use_break_frees_the_slot_before_collecting_the_drop() {
        use opencraft::player::inventory::{Inventory, Item, ToolKind, ToolTier, INVENTORY_SLOTS};
        use opencraft::player::physics::Player;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            13,
            dir.path().join("last-use.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session
            .world
            .set_block_immediate(0, 10, 0, BlockId::CraftingTable);

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] =
            Some(Item::tool_with_durability(ToolKind::Axe, ToolTier::Wood, 1).expect("last use"));
        session.player = Player::with_inventory(
            session.player.position,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        assert!(session.player.select_slot(0));

        let (remeshed, wear) = applied_break(break_mined_block(
            &mut session,
            IVec3::new(0, 10, 0),
            BlockId::CraftingTable,
        ));

        assert!(!remeshed.is_empty());
        assert_eq!(wear, ToolWearOutcome::Broke);
        assert_eq!(session.world.get_block(0, 10, 0), BlockId::Air);
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::CraftingTable,
                count: 1,
            }),
            "wear ran before collection, so the drop took the freed slot"
        );
        assert!(
            !session
                .player
                .inventory()
                .slots()
                .iter()
                .flatten()
                .any(|item| item.is_tool()),
            "the broken tool disappeared cleanly"
        );
        assert_eq!(
            session.player.inventory().main(0),
            None,
            "no drop spilled into main storage"
        );
    }

    #[test]
    fn hotbar_stacks_survive_save_and_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("stacks.mcvx");
        let mut session =
            WorldSession::new_session(4242, save_path.clone(), DEFAULT_SEASON_LENGTH_DAYS);
        session.player.collect_block(BlockId::CoalOre);
        session.player.collect_block(BlockId::IronOre);
        assert!(session.player.select_slot(1));

        save_current_world(&mut session).expect("save succeeds");
        let loaded = load_world_from_path(&save_path).expect("load succeeds");
        let restored = WorldSession::session_from_save(loaded, save_path.clone());

        assert_eq!(restored.world.seed(), 4242);
        assert_eq!(restored.player.selected_slot(), 1);
        assert_eq!(restored.player.inventory(), session.player.inventory());
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(
                restored.player.hotbar_stack(slot),
                session.player.hotbar_stack(slot)
            );
        }
    }

    #[test]
    fn inventory_tools_cursor_main_slots_and_grids_survive_named_world_restart() {
        use opencraft::player::inventory::{
            Item, ItemId, Material, ToolKind, ToolTier, INVENTORY_SLOTS,
        };
        use opencraft::player::physics::Player;

        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("full.mcvx");
        let mut session =
            WorldSession::new_session(909, save_path.clone(), DEFAULT_SEASON_LENGTH_DAYS);

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Block(BlockId::Stone), 5).expect("stone"));
        slots[HOTBAR_SLOTS + 4] =
            Some(Item::new(ItemId::Material(Material::Stick), 3).expect("sticks"));
        let axe =
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Stone, 77).expect("worn axe");
        let inventory = Inventory::from_parts(slots, Some(axe));

        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(
                1,
                Some(Item::new(ItemId::Material(Material::Stick), 1).expect("stick")),
            )
            .expect("player grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(
                7,
                Some(Item::new(ItemId::Block(BlockId::IronOre), 2).expect("ore")),
            )
            .expect("table grid cell");

        let mut player = Player::with_inventory(
            session.player.position,
            inventory.clone(),
            player_grid.clone(),
            table_grid.clone(),
        );
        player.yaw = 1.5;
        assert!(player.select_slot(1));
        session.player = player;

        save_current_world(&mut session).expect("save succeeds");
        let loaded = load_world_from_path(&save_path).expect("load succeeds");
        assert_eq!(loaded.player.selected_slot, 1);
        assert_eq!(loaded.player.inventory, inventory);
        assert_eq!(loaded.player.player_grid, player_grid);
        assert_eq!(loaded.player.table_grid, table_grid);

        let mut restored = WorldSession::session_from_save(loaded, save_path.clone());
        assert_eq!(restored.player.selected_slot(), 1);
        assert_eq!(restored.player.inventory(), &inventory);
        assert_eq!(restored.player.player_grid(), &player_grid);
        assert_eq!(restored.player.table_grid(), &table_grid);
        assert_eq!(
            restored
                .player
                .inventory()
                .cursor()
                .and_then(|item| item.durability()),
            Some(77),
            "worn tool durability survives the runtime/save boundary"
        );

        // Saving the restored session reproduces byte-identical state.
        let second_path = dir.path().join("full-again.mcvx");
        restored.save_path = second_path.clone();
        save_current_world(&mut restored).expect("re-save succeeds");
        assert_eq!(
            std::fs::read(&save_path).expect("read first save"),
            std::fs::read(&second_path).expect("read second save"),
            "session restore followed by save is byte-identical"
        );
    }

    #[test]
    fn placing_the_selected_stack_consumes_exactly_one_item() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            10,
            dir.path().join("place.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        session.player.position = Vec3::new(5.5, 10.0, 5.5);
        aim_placement_camera(&mut session);
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));

        let remeshed = try_place_selected_block(&mut session).expect("placement applies");

        assert!(!remeshed.is_empty(), "placement remeshes its chunk");
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Stone);
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 1,
            })
        );
    }

    #[test]
    fn placement_rejects_empty_slots_and_player_overlap_without_consuming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            12,
            dir.path().join("reject.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        session.player.position = Vec3::new(5.5, 10.0, 5.5);
        aim_placement_camera(&mut session);
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(5));

        assert!(
            try_place_selected_block(&mut session).is_none(),
            "an empty selected slot cannot place"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Air);
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 2,
            }),
            "nothing was consumed"
        );

        assert!(session.player.select_slot(0));
        session.player.position = Vec3::new(0.5, 6.0, 0.5);
        assert!(
            try_place_selected_block(&mut session).is_none(),
            "a placement overlapping the player AABB is rejected"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Air);
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 2,
            }),
            "rejected placement does not consume"
        );
    }

    /// Camera and player eyes looking straight down at `pos` from three
    /// blocks above, kept in sync so the session's reach check and raycast
    /// agree (unlike the placement tests, which only set the camera).
    fn aim_table_interaction_at(session: &mut WorldSession, pos: IVec3) {
        session.player.position =
            Vec3::new(pos.x as f32 + 0.5, pos.y as f32 + 3.0, pos.z as f32 + 0.5);
        session.player.yaw = 0.0;
        session.player.pitch = -std::f32::consts::FRAC_PI_2;
        session.sync_camera();
    }

    #[test]
    fn right_click_on_a_placed_table_opens_without_placing_or_consuming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            20,
            dir.path().join("table.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session
            .world
            .set_block_immediate(0, 5, 0, BlockId::CraftingTable);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));

        let outcome = right_click_interaction(&mut session);

        assert!(
            matches!(outcome, RightClickOutcome::TableOpened),
            "a targeted table must open"
        );
        assert_eq!(
            session.open_table_position(),
            Some(IVec3::new(0, 5, 0)),
            "the session exposes the active table position"
        );
        assert_eq!(
            session.world.get_block(0, 6, 0),
            BlockId::Air,
            "opening a table must not place the selected block"
        );
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 2,
            }),
            "opening a table must not consume the selected stack"
        );
    }

    #[test]
    fn right_click_on_a_table_opens_even_with_an_empty_selected_slot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            21,
            dir.path().join("table-empty.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session
            .world
            .set_block_immediate(0, 5, 0, BlockId::CraftingTable);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        assert!(session.player.select_slot(4));

        let outcome = right_click_interaction(&mut session);

        assert!(
            matches!(outcome, RightClickOutcome::TableOpened),
            "table interaction must not depend on the held stack"
        );
        assert_eq!(session.open_table_position(), Some(IVec3::new(0, 5, 0)));
        assert!(session.player.hotbar_stack(4).is_none());
    }

    #[test]
    fn right_click_on_a_non_table_falls_through_to_placement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            22,
            dir.path().join("fallback.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));

        let outcome = right_click_interaction(&mut session);

        assert!(
            matches!(outcome, RightClickOutcome::Placed(ref remeshed) if !remeshed.is_empty()),
            "a non-table target must use the placement path"
        );
        assert_eq!(
            session.world.get_block(0, 6, 0),
            BlockId::Stone,
            "placement is byte-compatible with the pre-table path"
        );
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 1,
            }),
            "placement consumes exactly one selected item"
        );
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn right_click_on_air_opens_and_places_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            23,
            dir.path().join("air-click.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));

        let outcome = right_click_interaction(&mut session);

        assert!(matches!(outcome, RightClickOutcome::Nothing));
        assert_eq!(session.world.get_block(0, 5, 0), BlockId::Air);
        assert_eq!(session.open_table_position(), None);
        assert_eq!(
            session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 1,
            }),
            "a click that applies nothing consumes nothing"
        );
    }

    #[test]
    fn right_click_placement_fallback_with_an_empty_slot_mutates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            24,
            dir.path().join("empty-slot.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        assert!(session.player.select_slot(0));

        let outcome = right_click_interaction(&mut session);

        assert!(matches!(outcome, RightClickOutcome::Nothing));
        assert_eq!(
            session.world.get_block(0, 6, 0),
            BlockId::Air,
            "no placement happened"
        );
        assert!(session.player.hotbar_stack(0).is_none());
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn a_replaced_table_closes_the_open_state_and_falls_through_to_placement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            25,
            dir.path().join("replaced.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session
            .world
            .set_block_immediate(0, 5, 0, BlockId::CraftingTable);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        assert!(matches!(
            right_click_interaction(&mut session),
            RightClickOutcome::TableOpened
        ));

        // An unrelated world edit replaces the station; the next click must
        // observe the stale open state as closed and place normally.
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));

        let outcome = right_click_interaction(&mut session);

        assert_eq!(
            session.open_table_position(),
            None,
            "a replaced table must not stay open"
        );
        assert!(
            matches!(outcome, RightClickOutcome::Placed(ref remeshed) if !remeshed.is_empty()),
            "the same click falls through to placement"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Stone);
    }

    #[test]
    fn a_placed_crafting_table_can_then_be_targeted_and_opened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            26,
            dir.path().join("place-then-open.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        session.player.collect_block(BlockId::CraftingTable);
        assert!(session.player.select_slot(0));

        // The first click targets the stone support, so the held crafting
        // table is placed above it through the normal placement path.
        assert!(
            matches!(
                right_click_interaction(&mut session),
                RightClickOutcome::Placed(ref remeshed) if !remeshed.is_empty()
            ),
            "placing a held crafting table must use the placement path"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::CraftingTable);
        assert!(
            session.player.hotbar_stack(0).is_none(),
            "the placed stack was consumed"
        );
        assert_eq!(session.open_table_position(), None);

        // Targeting the real placed block now opens that station instead of
        // stacking another block on top of it.
        aim_table_interaction_at(&mut session, IVec3::new(0, 6, 0));
        let outcome = right_click_interaction(&mut session);

        assert!(
            matches!(outcome, RightClickOutcome::TableOpened),
            "the placed station opens from its actual world coordinate"
        );
        assert_eq!(session.open_table_position(), Some(IVec3::new(0, 6, 0)));
        assert_eq!(
            session.world.get_block(0, 7, 0),
            BlockId::Air,
            "nothing was placed on top of the table"
        );
    }

    // -----------------------------------------------------------------------
    // S03-T03: inventory panel adapter (mode state, cursor ownership, clicks)
    // -----------------------------------------------------------------------

    /// Total item count across all 36 slots, the cursor, and both grids — the
    /// conservation invariant every UI command must preserve.
    fn inventory_units(player: &Player) -> u32 {
        let mut total = 0u32;
        for item in player.inventory().slots().iter().flatten() {
            total += u32::from(item.count());
        }
        if let Some(cursor) = player.inventory().cursor() {
            total += u32::from(cursor.count());
        }
        for grid in [player.player_grid(), player.table_grid()] {
            for index in 0..grid.kind().slot_count() {
                if let Some(item) = grid.slot(index) {
                    total += u32::from(item.count());
                }
            }
        }
        total
    }

    #[test]
    fn inventory_screen_toggles_to_player_mode_and_closed() {
        let mut screen = InventoryScreen::new();
        assert_eq!(screen.mode(None), InventoryMode::Closed);
        assert!(!screen.is_open());

        screen.toggle(None);
        assert!(screen.is_open());
        assert_eq!(screen.mode(None), InventoryMode::Player);
        assert_eq!(
            screen.mode(Some(IVec3::new(0, 5, 0))),
            InventoryMode::Player
        );
        assert!(
            !screen.table_attached(),
            "the E path never attaches a table by itself"
        );

        screen.toggle(None);
        assert_eq!(screen.mode(None), InventoryMode::Closed);
        assert!(!screen.is_open());
    }

    #[test]
    fn a_targeted_table_click_opens_table_mode_and_is_cleared_on_close() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            31,
            dir.path().join("ui-table.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session
            .world
            .set_block_immediate(0, 5, 0, BlockId::CraftingTable);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));
        let mut screen = InventoryScreen::new();

        assert!(matches!(
            right_click_interaction(&mut session),
            RightClickOutcome::TableOpened
        ));
        let table_open = session.open_table_position();
        assert_eq!(table_open, Some(IVec3::new(0, 5, 0)));

        // Exactly what the production TableOpened arm does (minus the winit
        // cursor-grab call): attach the panel to the live station.
        screen.open_table();
        assert_eq!(screen.mode(table_open), InventoryMode::Table);

        // Exactly what close_inventory does: clear the attachment and station.
        screen.close();
        session.close_table();
        assert_eq!(
            screen.mode(session.open_table_position()),
            InventoryMode::Closed
        );
        assert!(!screen.table_attached());
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn table_targets_are_rejected_without_a_live_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            32,
            dir.path().join("stale-table.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        session.player.collect_block(BlockId::Stone);
        let mut screen = InventoryScreen::new();
        // Attached, but the session has no live table: mode falls back to Player.
        screen.open_table();
        let before_inventory = session.player.inventory().clone();
        let before_player_grid = session.player.player_grid().clone();
        let before_table_grid = session.player.table_grid().clone();

        for target in [
            InventoryTarget::GridCell {
                kind: GridKind::Table,
                index: 0,
            },
            InventoryTarget::ResultSlot(GridKind::Table),
        ] {
            assert_eq!(
                apply_inventory_click(&mut session.player, &screen, None, target),
                Err(InventoryCommandError::TableUnavailable)
            );
        }
        assert_eq!(session.player.inventory(), &before_inventory);
        assert_eq!(session.player.player_grid(), &before_player_grid);
        assert_eq!(session.player.table_grid(), &before_table_grid);
    }

    #[test]
    fn apply_inventory_click_moves_a_slot_stack_without_touching_world_or_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            33,
            dir.path().join("slot-click.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));
        let mut screen = InventoryScreen::new();
        screen.open_player();
        let world_before = session.world.get_block(0, 10, 0);

        let outcome =
            apply_inventory_click(&mut session.player, &screen, None, InventoryTarget::Slot(0))
                .expect("a slot click moves the whole stack");

        assert_eq!(outcome, InventoryClickOutcome::Moved);
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(2)
        );
        assert!(session.player.inventory().hotbar(0).is_none());
        assert_eq!(session.player.selected_slot(), 0);
        assert_eq!(session.world.get_block(0, 10, 0), world_before);
    }

    #[test]
    fn apply_inventory_right_click_splits_a_stack_and_places_one_in_the_grid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            35,
            dir.path().join("right-click.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        for _ in 0..5 {
            session.player.collect_block(BlockId::OakPlanks);
        }
        let mut screen = InventoryScreen::new();
        screen.open_player();
        let units_before = inventory_units(&session.player);

        // Right click on the stack separates half (rounded up) onto the hand.
        let outcome = apply_inventory_click_button(
            &mut session.player,
            &screen,
            None,
            InventoryTarget::Slot(0),
            InventoryClickButton::Right,
        )
        .expect("right click splits the plank stack");
        assert_eq!(outcome, InventoryClickOutcome::Moved);
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(3)
        );
        assert_eq!(
            session
                .player
                .inventory()
                .hotbar(0)
                .map(|item| item.count()),
            Some(2)
        );
        assert_eq!(inventory_units(&session.player), units_before);

        // A right click on a grid cell places exactly one, which is the
        // gesture the player needs for a single-item recipe slot.
        let cell = InventoryTarget::GridCell {
            kind: GridKind::Player,
            index: 0,
        };
        let outcome = apply_inventory_click_button(
            &mut session.player,
            &screen,
            None,
            cell,
            InventoryClickButton::Right,
        )
        .expect("right click places one plank in the grid");
        assert_eq!(outcome, InventoryClickOutcome::Moved);
        assert_eq!(
            session
                .player
                .player_grid()
                .slot(0)
                .map(|item| item.count()),
            Some(1)
        );
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(2)
        );
        assert_eq!(inventory_units(&session.player), units_before);
    }

    #[test]
    fn apply_inventory_click_crafts_once_and_the_second_click_is_no_recipe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            34,
            dir.path().join("craft-click.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        session.player.collect_block(BlockId::OakLog);
        let mut screen = InventoryScreen::new();
        screen.open_player();

        // Pick the log out of the hotbar and drop it into the 2x2 grid.
        assert_eq!(
            apply_inventory_click(&mut session.player, &screen, None, InventoryTarget::Slot(0)),
            Ok(InventoryClickOutcome::Moved)
        );
        assert_eq!(
            apply_inventory_click(
                &mut session.player,
                &screen,
                None,
                InventoryTarget::GridCell {
                    kind: GridKind::Player,
                    index: 0,
                },
            ),
            Ok(InventoryClickOutcome::Moved)
        );
        assert!(session.player.player_grid().slot(0).is_some());

        let crafted = apply_inventory_click(
            &mut session.player,
            &screen,
            None,
            InventoryTarget::ResultSlot(GridKind::Player),
        )
        .expect("one log crafts four planks");
        match crafted {
            InventoryClickOutcome::Crafted(item) => {
                assert_eq!(item.id(), ItemId::Block(BlockId::OakPlanks));
                assert_eq!(item.count(), 4);
            }
            InventoryClickOutcome::Moved => panic!("a result click must report the crafted item"),
        }
        assert!(session.player.player_grid().is_empty());

        // The grid is empty now: the same click consumes nothing.
        let before = session.player.inventory().clone();
        assert_eq!(
            apply_inventory_click(
                &mut session.player,
                &screen,
                None,
                InventoryTarget::ResultSlot(GridKind::Player),
            ),
            Err(InventoryCommandError::Craft(CraftError::NoRecipe))
        );
        assert_eq!(session.player.inventory(), &before);
    }

    #[test]
    fn inventory_should_close_only_for_an_attached_screen_losing_its_table() {
        let live = Some(IVec3::new(0, 5, 0));
        let closed = InventoryScreen::new();
        assert!(!inventory_should_close(&closed, live));
        assert!(!inventory_should_close(&closed, None));

        let mut player_screen = InventoryScreen::new();
        player_screen.open_player();
        assert!(!inventory_should_close(&player_screen, None));
        assert!(!inventory_should_close(&player_screen, live));

        let mut table_screen = InventoryScreen::new();
        table_screen.open_table();
        assert!(!inventory_should_close(&table_screen, live));
        assert!(inventory_should_close(&table_screen, None));
    }

    #[test]
    fn inventory_status_text_names_success_and_every_error_shape() {
        let stone = Item::new(ItemId::Block(BlockId::Stone), 1).expect("stone is carryable");

        assert_eq!(
            inventory_status_text(Ok(InventoryClickOutcome::Moved)),
            "Moved"
        );
        assert_eq!(
            inventory_status_text(Ok(InventoryClickOutcome::Crafted(stone))),
            "Crafted Stone"
        );
        assert_eq!(
            inventory_status_text(Err(InventoryCommandError::Inventory(
                InventoryError::EmptySlot
            ))),
            "Inventory: EmptySlot"
        );
        assert_eq!(
            inventory_status_text(Err(InventoryCommandError::Craft(CraftError::NoRecipe))),
            "Craft: NoRecipe"
        );
        assert_eq!(
            inventory_status_text(Err(InventoryCommandError::TableUnavailable)),
            "Table unavailable"
        );
    }

    #[test]
    fn drag_release_target_only_routes_to_a_different_target() {
        let slot_one = InventoryTarget::Slot(1);
        let cell = InventoryTarget::GridCell {
            kind: GridKind::Player,
            index: 0,
        };

        assert_eq!(drag_release_target(None, Some(slot_one)), None);
        assert_eq!(drag_release_target(Some(slot_one), Some(slot_one)), None);
        assert_eq!(drag_release_target(Some(slot_one), None), None);
        assert_eq!(drag_release_target(Some(slot_one), Some(cell)), Some(cell));
    }

    #[test]
    fn a_press_release_on_the_same_slot_leaves_the_stack_on_the_cursor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            35,
            dir.path().join("same-cell.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        session.player.collect_block(BlockId::Stone);
        let mut screen = InventoryScreen::new();
        screen.open_player();
        let target = InventoryTarget::Slot(0);

        apply_inventory_click(&mut session.player, &screen, None, target).expect("pick up");
        let destination = drag_release_target(Some(target), Some(target));

        assert_eq!(destination, None, "a same-cell release is inert");
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(1),
            "the picked stack stays in hand"
        );
    }

    #[test]
    fn a_press_drag_release_between_slots_moves_and_conserves_the_whole_stack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session =
            WorldSession::new_session(36, dir.path().join("drag.mcvx"), DEFAULT_SEASON_LENGTH_DAYS);
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        let mut screen = InventoryScreen::new();
        screen.open_player();
        let total_before = inventory_units(&session.player);
        let origin = InventoryTarget::Slot(0);

        apply_inventory_click(&mut session.player, &screen, None, origin).expect("pick up");
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(2)
        );

        let destination = drag_release_target(Some(origin), Some(InventoryTarget::Slot(1)))
            .expect("a different target receives the drop");
        let outcome = apply_inventory_click(&mut session.player, &screen, None, destination)
            .expect("drop the whole stack");

        assert_eq!(outcome, InventoryClickOutcome::Moved);
        assert!(session.player.inventory().cursor().is_none());
        assert_eq!(
            session
                .player
                .inventory()
                .hotbar(1)
                .map(|item| item.count()),
            Some(2),
            "the destination holds the original whole stack"
        );
        assert_eq!(inventory_units(&session.player), total_before);
    }

    #[test]
    fn dragging_into_a_table_cell_without_a_live_table_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            37,
            dir.path().join("table-drag.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        let mut screen = InventoryScreen::new();
        screen.open_table();
        let origin = InventoryTarget::Slot(0);

        apply_inventory_click(&mut session.player, &screen, None, origin).expect("pick up");
        let before_inventory = session.player.inventory().clone();
        let before_player_grid = session.player.player_grid().clone();
        let before_table_grid = session.player.table_grid().clone();
        let destination = drag_release_target(
            Some(origin),
            Some(InventoryTarget::GridCell {
                kind: GridKind::Table,
                index: 0,
            }),
        )
        .expect("a table cell differs from the origin");

        assert_eq!(
            apply_inventory_click(&mut session.player, &screen, None, destination),
            Err(InventoryCommandError::TableUnavailable)
        );
        assert_eq!(session.player.inventory(), &before_inventory);
        assert_eq!(session.player.player_grid(), &before_player_grid);
        assert_eq!(session.player.table_grid(), &before_table_grid);
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(2),
            "the rejected drop leaves the stack on the cursor"
        );
    }

    #[test]
    fn drag_arms_only_when_the_cursor_was_empty_before_the_press() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            38,
            dir.path().join("drag-arm.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        // Two different block ids sit in slots 0 and 1, so a press on slot 1
        // while holding slot 0's stack is a genuine swap (not a merge).
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Dirt);
        let mut screen = InventoryScreen::new();
        screen.open_player();
        assert_eq!(
            session.player.inventory().hotbar(0).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone))
        );
        assert_eq!(
            session.player.inventory().hotbar(1).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Dirt))
        );
        let total_before = inventory_units(&session.player);

        // Gesture 1: a pure pick-up from an empty cursor DOES arm a drag.
        let origin = InventoryTarget::Slot(0);
        let cursor_empty_before = session.player.inventory().cursor().is_none();
        assert!(cursor_empty_before, "the cursor starts empty");
        let result = apply_inventory_click(&mut session.player, &screen, None, origin);
        assert_eq!(result, Ok(InventoryClickOutcome::Moved));
        assert_eq!(
            drag_origin_after_press(
                cursor_empty_before,
                result,
                session.player.inventory().cursor().is_some(),
                origin,
            ),
            Some(origin),
            "a pick-up press arms the drag"
        );

        // Swap path: pressing slot 1 while holding slot 0's stack swaps the
        // two whole stacks. The command is `Moved` and the cursor is occupied
        // afterwards, so the old rule armed a drag here; the S04-T03 rule must
        // not, because the press did not pick anything UP.
        let swapped = InventoryTarget::Slot(1);
        let cursor_empty_before = session.player.inventory().cursor().is_none();
        assert!(!cursor_empty_before, "the cursor still holds the stone");
        let result = apply_inventory_click(&mut session.player, &screen, None, swapped);
        assert_eq!(result, Ok(InventoryClickOutcome::Moved));
        assert!(session.player.inventory().cursor().is_some());
        let armed = drag_origin_after_press(
            cursor_empty_before,
            result,
            session.player.inventory().cursor().is_some(),
            swapped,
        );
        assert_eq!(armed, None, "a swap does not arm a drag");
        assert_eq!(
            session.player.inventory().hotbar(1).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone)),
            "the swap really happened"
        );

        // The release over slot 2 therefore routes nowhere: no second move and
        // no mutation of the release target.
        let slot_two_before = session.player.inventory().hotbar(2);
        let release_target = Some(InventoryTarget::Slot(2));
        assert_eq!(drag_release_target(armed, release_target), None);
        if let Some(destination) = drag_release_target(armed, release_target) {
            apply_inventory_click(&mut session.player, &screen, None, destination)
                .expect("the release would drop the stack");
        }
        assert_eq!(
            session.player.inventory().hotbar(2),
            slot_two_before,
            "the unintended release leaves slot 2 untouched"
        );
        assert_eq!(
            inventory_units(&session.player),
            total_before,
            "items stay conserved across the swap gesture"
        );

        // Positive control: place the held stack down first (a place is not a
        // pick-up either), then a fresh empty-cursor pick-up arms a drag and a
        // release on a different slot issues exactly one move.
        let place = InventoryTarget::Slot(2);
        let cursor_empty_before = session.player.inventory().cursor().is_none();
        assert!(!cursor_empty_before);
        let result = apply_inventory_click(&mut session.player, &screen, None, place);
        assert_eq!(
            drag_origin_after_press(
                cursor_empty_before,
                result,
                session.player.inventory().cursor().is_some(),
                place,
            ),
            None,
            "a place does not arm a drag"
        );
        assert!(session.player.inventory().cursor().is_none());

        let pick_up = InventoryTarget::Slot(1);
        let cursor_empty_before = session.player.inventory().cursor().is_none();
        assert!(cursor_empty_before);
        let result = apply_inventory_click(&mut session.player, &screen, None, pick_up);
        assert_eq!(result, Ok(InventoryClickOutcome::Moved));
        let armed = drag_origin_after_press(
            cursor_empty_before,
            result,
            session.player.inventory().cursor().is_some(),
            pick_up,
        );
        assert_eq!(armed, Some(pick_up), "a pick-up press still arms a drag");
        let destination = drag_release_target(armed, Some(InventoryTarget::Slot(3)))
            .expect("a different slot receives the drop");
        let outcome = apply_inventory_click(&mut session.player, &screen, None, destination)
            .expect("drop the whole stack");
        assert_eq!(outcome, InventoryClickOutcome::Moved);
        assert_eq!(
            session.player.inventory().hotbar(3).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone)),
            "the drag moved the stack exactly once"
        );
        assert!(session.player.inventory().hotbar(1).is_none());
        assert!(session.player.inventory().cursor().is_none());
        assert_eq!(inventory_units(&session.player), total_before);
    }

    // -----------------------------------------------------------------------
    // S03-T04: panel rendering (pure quads/labels over live gameplay state)
    // -----------------------------------------------------------------------

    fn test_stack(block: BlockId, count: u16) -> Item {
        Item::new(ItemId::Block(block), count).expect("test stacks are valid")
    }

    #[test]
    fn inventory_quads_cover_every_slot_grid_cell_and_result_and_are_empty_when_closed() {
        use opencraft::player::inventory::{INVENTORY_SLOTS, MAX_GRID_SLOTS};

        let empty_inventory = Inventory::empty();
        let empty_player_grid = CraftingGrid::new(GridKind::Player);
        let empty_table_grid = CraftingGrid::new(GridKind::Table);
        let layout = layout(1280, 720, InventoryMode::Player);

        // A closed screen contributes no geometry and no text at all.
        assert!(screen_quads(
            InventoryMode::Closed,
            &layout,
            &empty_inventory,
            &empty_player_grid,
            &empty_table_grid,
            Some(test_stack(BlockId::Stone, 3)),
            Some((100.0, 100.0)),
        )
        .is_empty());
        assert!(screen_labels(
            InventoryMode::Closed,
            &layout,
            &empty_inventory,
            &empty_player_grid,
            &empty_table_grid,
            Some("closed"),
        )
        .is_empty());

        // Empty player mode: backdrop + 36 slot outlines + 4 grid cells + the
        // player result slot. Table mode adds the 16 table cells and result.
        let player_quads = screen_quads(
            InventoryMode::Player,
            &layout,
            &empty_inventory,
            &empty_player_grid,
            &empty_table_grid,
            None,
            None,
        );
        assert_eq!(player_quads.len(), 1 + INVENTORY_SLOTS + 4 + 1);
        let table_quads = screen_quads(
            InventoryMode::Table,
            &layout,
            &empty_inventory,
            &empty_player_grid,
            &empty_table_grid,
            None,
            None,
        );
        assert_eq!(
            table_quads.len(),
            1 + INVENTORY_SLOTS + 4 + 1 + MAX_GRID_SLOTS + 1
        );

        // Occupied state adds exactly one fill per occupied cell plus the
        // cursor ghost. Player mode never draws a table cell fill.
        let stone = test_stack(BlockId::Stone, 3);
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(stone);
        let inventory = Inventory::from_parts(slots, Some(stone));
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid.set(0, Some(stone)).expect("player cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid.set(15, Some(stone)).expect("table cell");
        let cursor = Some((200.0, 150.0));

        let player_quads = screen_quads(
            InventoryMode::Player,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            Some(stone),
            cursor,
        );
        assert_eq!(
            player_quads.len(),
            1 + INVENTORY_SLOTS + 4 + 1 // chrome
                + 1 // slot 0 fill
                + 1 // player cell 0 fill
                + 1 // cursor ghost
        );
        let table_quads = screen_quads(
            InventoryMode::Table,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            Some(stone),
            cursor,
        );
        assert_eq!(
            table_quads.len(),
            1 + INVENTORY_SLOTS + 4 + 1 + MAX_GRID_SLOTS + 1
                + 1 // slot 0 fill
                + 1 // player cell 0 fill
                + 1 // table cell 15 fill
                + 1 // cursor ghost
        );
    }

    #[test]
    fn inventory_labels_report_slot_counts_tool_durability_result_preview_cursor_and_status() {
        use opencraft::player::inventory::{ToolKind, ToolTier, INVENTORY_SLOTS};

        let pickaxe = Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 200)
            .expect("worn iron pickaxe");
        let plank = test_stack(BlockId::OakPlanks, 1);
        let holding = test_stack(BlockId::Stone, 7);
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(pickaxe);
        let inventory = Inventory::from_parts(slots, Some(holding));
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        for cell in 0..4 {
            player_grid.set(cell, Some(plank)).expect("plank cell");
        }
        let table_grid = CraftingGrid::new(GridKind::Table);
        let layout = layout(1280, 720, InventoryMode::Player);

        let labels = screen_labels(
            InventoryMode::Player,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            Some("Crafted Crafting Table"),
        );
        let texts: Vec<&str> = labels.iter().map(|label| label.text.as_str()).collect();
        assert!(texts.contains(&"Player Inventory"));
        assert!(texts.contains(&"1"), "slot count for the pickaxe");
        assert!(texts.contains(&"200/250"), "remaining/max durability");
        assert!(
            texts.contains(&"Crafting Table x1"),
            "the 2x2 result preview is named"
        );
        assert!(texts.contains(&"Holding: Stone x7"));
        assert!(texts.contains(&"Left click moves a stack, right click splits or places one"));
        assert!(texts.contains(&"Crafted Crafting Table"));
        assert!(labels.len() <= MAX_LABELS);

        // Every label anchors inside the panel so nothing renders off-screen.
        for label in &labels {
            assert!(label.left >= layout.panel.0);
            assert!(label.top >= layout.panel.1);
            assert!(label.left <= layout.panel.0 + layout.panel.2);
            assert!(label.top <= layout.panel.1 + layout.panel.3);
        }
    }

    #[test]
    fn player_grid_crafting_flow_through_clicks_crafts_a_table_item_exactly_once() {
        use opencraft::player::inventory::{RecipeId, INVENTORY_SLOTS};

        let plank = test_stack(BlockId::OakPlanks, 1);
        let mut slots = [None; INVENTORY_SLOTS];
        for slot in slots.iter_mut().take(4) {
            *slot = Some(plank);
        }
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        let mut screen = InventoryScreen::new();
        screen.open_player();

        // Four slot picks and four grid drops: the UI never computes counts.
        for cell in 0..4 {
            assert_eq!(
                apply_inventory_click(&mut player, &screen, None, InventoryTarget::Slot(cell)),
                Ok(InventoryClickOutcome::Moved)
            );
            assert_eq!(
                apply_inventory_click(
                    &mut player,
                    &screen,
                    None,
                    InventoryTarget::GridCell {
                        kind: GridKind::Player,
                        index: cell,
                    },
                ),
                Ok(InventoryClickOutcome::Moved)
            );
        }
        assert_eq!(
            player.player_grid().preview().map(|preview| (
                preview.recipe,
                preview.output,
                preview.output_count
            )),
            Some((
                RecipeId::CraftingTable,
                ItemId::Block(BlockId::CraftingTable),
                1
            ))
        );

        let crafted = apply_inventory_click(
            &mut player,
            &screen,
            None,
            InventoryTarget::ResultSlot(GridKind::Player),
        )
        .expect("four planks craft one table");
        match crafted {
            InventoryClickOutcome::Crafted(item) => {
                assert_eq!(item.id(), ItemId::Block(BlockId::CraftingTable));
                assert_eq!(item.count(), 1);
            }
            InventoryClickOutcome::Moved => panic!("the result click must report the crafted item"),
        }
        assert!(player.player_grid().is_empty());
        let tables: Vec<Item> = player
            .inventory()
            .slots()
            .iter()
            .flatten()
            .filter(|item| item.id() == ItemId::Block(BlockId::CraftingTable))
            .copied()
            .collect();
        assert_eq!(tables.len(), 1, "exactly one table landed in the inventory");
        assert_eq!(tables[0].count(), 1);

        // The second result click has no recipe and changes nothing.
        let inventory_before = player.inventory().clone();
        let grid_before = player.player_grid().clone();
        assert_eq!(
            apply_inventory_click(
                &mut player,
                &screen,
                None,
                InventoryTarget::ResultSlot(GridKind::Player),
            ),
            Err(InventoryCommandError::Craft(CraftError::NoRecipe))
        );
        assert_eq!(player.inventory(), &inventory_before);
        assert_eq!(player.player_grid(), &grid_before);
    }

    #[test]
    fn placed_table_mode_reaches_cell_fifteen_through_clicks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            41,
            dir.path().join("table-4x4.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session
            .world
            .set_block_immediate(0, 5, 0, BlockId::CraftingTable);
        aim_table_interaction_at(&mut session, IVec3::new(0, 5, 0));

        assert!(
            matches!(
                right_click_interaction(&mut session),
                RightClickOutcome::TableOpened
            ),
            "a real targeted table opens its station"
        );
        let table_open = session.open_table_position();
        assert_eq!(table_open, Some(IVec3::new(0, 5, 0)));

        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));
        let target = InventoryTarget::GridCell {
            kind: GridKind::Table,
            index: 15,
        };

        // A live table alone never grants the 4x4 grid: a Player-mode screen
        // rejects the same target before any mutation.
        let mut player_screen = InventoryScreen::new();
        player_screen.open_player();
        let units_before = inventory_units(&session.player);
        assert_eq!(
            apply_inventory_click(&mut session.player, &player_screen, table_open, target),
            Err(InventoryCommandError::TableUnavailable)
        );
        assert_eq!(inventory_units(&session.player), units_before);
        assert!(session.player.inventory().cursor().is_none());

        // Table mode accepts cell 15 and moves the whole stack there.
        let mut screen = InventoryScreen::new();
        screen.open_table();
        assert_eq!(screen.mode(table_open), InventoryMode::Table);
        assert_eq!(
            apply_inventory_click(
                &mut session.player,
                &screen,
                table_open,
                InventoryTarget::Slot(0)
            ),
            Ok(InventoryClickOutcome::Moved)
        );
        assert_eq!(
            apply_inventory_click(&mut session.player, &screen, table_open, target),
            Ok(InventoryClickOutcome::Moved)
        );
        assert_eq!(
            session.player.table_grid().slot(15).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone))
        );
        assert!(session.player.inventory().cursor().is_none());
        assert!(session.player.inventory().hotbar(0).is_none());
        assert_eq!(
            session.world.get_block(0, 5, 0),
            BlockId::CraftingTable,
            "the UI click touched no world block"
        );
    }

    #[test]
    fn station_invalidated_between_redraws_rejects_or_closes_the_panel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(
            47,
            dir.path().join("stale-station.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        );
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        let table_pos = IVec3::new(0, 5, 0);
        session.world.set_block_immediate(
            table_pos.x,
            table_pos.y,
            table_pos.z,
            BlockId::CraftingTable,
        );
        aim_table_interaction_at(&mut session, table_pos);
        assert!(
            matches!(
                right_click_interaction(&mut session),
                RightClickOutcome::TableOpened
            ),
            "a real targeted table opens its station"
        );

        // The screen half of the table-open path (`open_inventory`); the cursor
        // grab needs a live Window and is not part of this seam.
        let mut screen = InventoryScreen::new();
        screen.open_table();
        assert_eq!(
            screen.mode(session.open_table_position()),
            InventoryMode::Table
        );

        // Park a stack in the high 4x4 cell so a stale click is observable as a
        // grid mutation (the pickup would take it back onto the cursor).
        session.player.collect_block(BlockId::Stone);
        session.player.collect_block(BlockId::Stone);
        assert!(session.player.select_slot(0));
        let table_target = InventoryTarget::GridCell {
            kind: GridKind::Table,
            index: 15,
        };
        let table_open = session.open_table_position();
        assert_eq!(
            apply_inventory_click(
                &mut session.player,
                &screen,
                table_open,
                InventoryTarget::Slot(0)
            ),
            Ok(InventoryClickOutcome::Moved)
        );
        assert_eq!(
            apply_inventory_click(&mut session.player, &screen, table_open, table_target),
            Ok(InventoryClickOutcome::Moved)
        );
        assert_eq!(
            session.player.table_grid().slot(15).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone))
        );

        // Invalidate the station WITHOUT a redraw: remove the table block
        // directly. Chunk streaming can do the same while the panel is open.
        session
            .world
            .set_block_immediate(table_pos.x, table_pos.y, table_pos.z, BlockId::Air);
        assert_eq!(
            session
                .world
                .get_block(table_pos.x, table_pos.y, table_pos.z),
            BlockId::Air
        );
        assert_eq!(
            session.open_table_position(),
            Some(table_pos),
            "until the click boundary syncs, the session still reports the stale station"
        );

        // The exact pixel a press on table cell 15 uses, from the real layout.
        let table_layout = layout(1280, 720, InventoryMode::Table);
        let (cell_x, cell_y, cell_w, cell_h) = table_layout.table_grid[15];
        let cell_cursor = (cell_x + cell_w * 0.5, cell_y + cell_h * 0.5);
        assert_eq!(
            hit_test(InventoryMode::Table, &table_layout, cell_cursor),
            Some(table_target),
            "the pixel really routes to the 4x4 grid's high cell"
        );

        let grid_before = session.player.table_grid().clone();
        let units_before = inventory_units(&session.player);

        // The click boundary's first step (both mouse arms call this): the lost
        // station closes the panel and its session station before any command.
        assert!(
            revalidate_inventory_station(&mut screen, &mut session),
            "the invalidated station closes the panel at the click boundary"
        );
        assert!(!screen.is_open(), "the panel is closed");
        assert_eq!(
            session.open_table_position(),
            None,
            "the stale station is closed too"
        );

        // The production adapter therefore rejects the same click, and nothing
        // in either grid, the slots, or the cursor moved.
        let closed_table = session.open_table_position();
        assert_eq!(
            apply_inventory_click(&mut session.player, &screen, closed_table, table_target),
            Err(InventoryCommandError::TableUnavailable)
        );
        assert_eq!(session.player.table_grid(), &grid_before);
        assert!(session.player.inventory().cursor().is_none());
        assert_eq!(inventory_units(&session.player), units_before);
        assert_eq!(
            session
                .world
                .get_block(table_pos.x, table_pos.y, table_pos.z),
            BlockId::Air,
            "the boundary sync touched no world block"
        );

        // A Player-mode panel has no attachable station: revalidation never
        // closes it, so the E-opened 2x2 screen keeps working.
        let mut player_screen = InventoryScreen::new();
        player_screen.open_player();
        assert!(!revalidate_inventory_station(
            &mut player_screen,
            &mut session
        ));
        assert!(player_screen.is_open());
        assert_eq!(player_screen.mode(None), InventoryMode::Player);

        // Out-of-reach is the other realistic between-redraw invalidation: the
        // same boundary sync closes a station the player walked away from.
        session.world.set_block_immediate(
            table_pos.x,
            table_pos.y,
            table_pos.z,
            BlockId::CraftingTable,
        );
        aim_table_interaction_at(&mut session, table_pos);
        assert!(
            session.open_table_at(table_pos),
            "the restored table reopens within reach"
        );
        let mut walked_screen = InventoryScreen::new();
        walked_screen.open_table();
        session.player.position = Vec3::new(60.5, 10.0, 60.5);
        session.sync_camera();
        assert!(
            revalidate_inventory_station(&mut walked_screen, &mut session),
            "walking out of reach between redraws closes the panel"
        );
        assert!(!walked_screen.is_open());
        assert_eq!(session.open_table_position(), None);
    }

    #[test]
    fn inventory_screen_does_not_mutate_the_world_when_a_click_is_consumed() {
        use opencraft::player::inventory::INVENTORY_SLOTS;

        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("ui-inert.mcvx");
        let mut session =
            WorldSession::new_session(42, save_path.clone(), DEFAULT_SEASON_LENGTH_DAYS);
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);
        // Two separate stacks: slot 0 is selected and must never be touched by
        // a click on the unrelated slot 1.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(test_stack(BlockId::Stone, 2));
        slots[1] = Some(test_stack(BlockId::Stone, 5));
        session.player = Player::with_inventory(
            session.player.position,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        assert!(session.player.select_slot(0));
        aim_placement_camera(&mut session);

        let hit = raycast_voxels(
            session.camera.position,
            session.camera.forward(),
            8.0,
            |x, y, z| session.world.get_block(x, y, z),
        )
        .expect("the stone below is targeted");
        let targeted_before =
            session
                .world
                .get_block(hit.block_pos.x, hit.block_pos.y, hit.block_pos.z);
        let selected_before = session.player.hotbar_stack(0);
        assert_eq!(selected_before.map(|stack| stack.count), Some(2));

        let mut screen = InventoryScreen::new();
        screen.open_player();
        assert_eq!(
            apply_inventory_click(&mut session.player, &screen, None, InventoryTarget::Slot(1)),
            Ok(InventoryClickOutcome::Moved)
        );

        assert_eq!(
            session
                .world
                .get_block(hit.block_pos.x, hit.block_pos.y, hit.block_pos.z),
            targeted_before,
            "a consumed UI click edits no world block"
        );
        assert_eq!(
            session.player.hotbar_stack(0),
            selected_before,
            "the selected stack is untouched by a click on another slot"
        );
        assert_eq!(
            session.player.inventory().cursor().map(|item| item.count()),
            Some(5),
            "the clicked stack moved to the cursor"
        );
        assert_eq!(session.save_path, save_path, "the save path is unchanged");
        assert!(!save_path.exists(), "a UI click never writes the save file");
    }

    #[test]
    fn valid_create_starts_a_world_session_and_enters_playing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("Deep Dark".into()));
        app.new_game.cycle_focus();
        app.new_game.apply(TextInputAction::InsertText("42".into()));
        app.attempt_create();

        // A valid create dispatches StartNewGame and enters Playing.
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        // The form's seed text resolves through catalog::resolve_seed_or.
        let expected_seed = resolve_seed_or("42", 0);
        assert_eq!(app.session.world.seed(), expected_seed);
        // The session owns the catalog-validated saves/<Name>.mcvx path.
        assert_eq!(app.session.save_path, dir.path().join("Deep Dark.mcvx"));
        // The player spawns on the resolved seed's land-search column (the
        // session was built via session::new_session, not the old one), 4
        // blocks above the surface.
        let (spawn_x, spawn_z) = find_spawn_column(expected_seed);
        let expected_y = (terrain_height_at_seeded(spawn_x, spawn_z, expected_seed) + 4) as f32;
        assert_eq!(
            app.session.player.position,
            Vec3::new(spawn_x as f32 + 0.5, expected_y, spawn_z as f32 + 0.5)
        );
        // Playing semantics: cursor grabbed, no form message.
        assert!(app.cursor_grabbed);
        assert!(app.new_game.message().is_none());
        // No save is written at create time — persistence rides the
        // normal save sites once the world accumulates state.
        assert!(!dir.path().join("Deep Dark.mcvx").exists());
    }

    #[test]
    fn new_game_season_length_row_feeds_the_session_it_creates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("Slow Seasons".into()));
        // Default create: one game day per season, matching persistence.
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.season_length_days, 1);

        // Cycling the row twice (1 -> 2 -> 4) before Create is what the
        // new session persists; the row's value is the single source.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("Long Seasons".into()));
        assert_eq!(app.new_game.cycle_season_length(), 2);
        assert_eq!(app.new_game.cycle_season_length(), 4);
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.season_length_days, 4);
    }

    #[test]
    fn blank_seed_create_uses_a_fresh_seed_and_still_enters_playing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("Blank Seed".into()));
        // Seed field left blank: the create resolves a fresh time-derived
        // seed (session::random_seed). The value itself is not predictable,
        // so assert the session is internally consistent with its own seed
        // (built via new_session: the player spawns on that seed's terrain)
        // plus the catalog-validated save path.
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.save_path, dir.path().join("Blank Seed.mcvx"));
        let seed = app.session.world.seed();
        let (spawn_x, spawn_z) = find_spawn_column(seed);
        let expected_y = (terrain_height_at_seeded(spawn_x, spawn_z, seed) + 4) as f32;
        assert_eq!(
            app.session.player.position,
            Vec3::new(spawn_x as f32 + 0.5, expected_y, spawn_z as f32 + 0.5)
        );
    }

    #[test]
    fn duplicate_and_invalid_name_creates_stay_on_new_game_with_inline_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Duplicate: attempt_create fetches FRESH rows, so a row saved
        // after the screen opened still collides (case-insensitively).
        let catalog = WorldCatalogFs::new(dir.path());
        catalog
            .save_world("Alpha", &sample_save(9))
            .expect("save alpha");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("alpha".into()));
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::NewGame);
        let message = app.new_game.message().expect("duplicate surfaces inline");
        assert!(
            message.text.contains("already exists"),
            "unexpected duplicate text: {}",
            message.text
        );
        assert_eq!(message.ticks_left, menu::MESSAGE_TICKS);
        assert_eq!(app.session.world.seed(), 1, "session not swapped");

        // Invalid name: the typed WorldNameError displays inline too.
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("bad/name".into()));
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::NewGame);
        let message = app
            .new_game
            .message()
            .expect("invalid name surfaces inline");
        assert!(
            message.text.contains("not allowed in a world name"),
            "unexpected invalid-name text: {}",
            message.text
        );
    }

    #[test]
    fn loadable_play_rebuilds_the_session_from_the_row_and_enters_playing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::LoadGame);

        // A save with one persisted chunk edit, so Play must restore edits.
        let mut inner: FxHashMap<u16, BlockId> = FxHashMap::default();
        inner.insert(5, BlockId::Stone);
        let mut edits = ChunkEditMap::default();
        edits.insert(ChunkPos { x: 1, y: 0, z: -2 }, inner);
        let mut data = sample_save(31337);
        data.chunk_edits = edits.clone();
        app.catalog.save_world("ghost", &data).expect("save ghost");
        app.refresh_load_rows();
        let index = app
            .load
            .rows()
            .iter()
            .position(|r| r.name == "ghost")
            .expect("ghost row listed");

        app.play_row(index);

        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.world.seed(), 31337);
        assert_eq!(
            app.session.player.position,
            Vec3::from_array(data.player.position)
        );
        assert_eq!(app.session.player.yaw, 0.5);
        assert_eq!(app.session.player.pitch, -0.25);
        assert!(app.session.player.fly_mode);
        assert_eq!(app.session.player.selected_slot(), 2);
        assert_eq!(app.session.world.chunk_edits(), &edits);
        assert_eq!(
            app.session.save_path,
            dir.path().join("ghost.mcvx"),
            "session owns the row's own file path"
        );
        assert!(app.cursor_grabbed, "Playing semantics grab the cursor");
        assert!(app.load_message.is_none());
    }

    #[test]
    fn corrupt_and_vanished_plays_stay_on_load_game_with_inline_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::LoadGame);
        app.catalog
            .save_world("ghost", &sample_save(5))
            .expect("save ghost");
        std::fs::write(dir.path().join("broken.mcvx"), b"NOPE!1234").expect("write broken");
        app.refresh_load_rows();
        let ghost = app
            .load
            .rows()
            .iter()
            .position(|r| r.name == "ghost")
            .expect("ghost row listed");
        let broken = app
            .load
            .rows()
            .iter()
            .position(|r| r.name == "broken")
            .expect("broken row listed");

        // A corrupt row surfaces its inline non-loadable display and the
        // screen stays.
        app.play_row(broken);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::LoadGame);
        let message = app.load_message.as_ref().expect("corrupt surfaces inline");
        assert!(
            message.text.contains("cannot be loaded"),
            "unexpected corrupt-row text: {}",
            message.text
        );
        assert_eq!(message.ticks_left, menu::MESSAGE_TICKS);
        assert_eq!(app.session.world.seed(), 1, "session not swapped");

        // Vanished between the listing and the Play click:
        // CatalogLoadError::Load surfaces inline, never swallowed.
        std::fs::remove_file(dir.path().join("ghost.mcvx")).expect("remove ghost");
        app.play_row(ghost);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::LoadGame);
        let message = app.load_message.as_ref().expect("vanished surfaces inline");
        assert!(
            message.text.contains("Could not load world \"ghost\""),
            "unexpected vanished-row text: {}",
            message.text
        );
    }

    #[test]
    fn save_and_quit_failure_composes_a_main_menu_message_and_keeps_the_retry_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Force a save failure: the save path's parent is a FILE, so the
        // atomic save's create_dir_all cannot succeed.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("write blocker");
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(9, blocker.join("deep.mcvx"), DEFAULT_SEASON_LENGTH_DAYS),
        );
        // An unsaved chunk edit: the flag the failure must preserve.
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, 9));
        app.session
            .world
            .set_block_immediate(0, 40, 5, BlockId::Brick);
        assert!(app.session.world.has_unsaved_changes());

        // Pause -> SaveAndQuit through the extracted, loop-free decision
        // apply (no &ActiveEventLoop, no exit requested).
        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        let exit_requested = app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit);
        assert!(!exit_requested);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);

        // The failure surfaces as the main menu's transient message and
        // composes as the main menu's last line.
        let message = app
            .main_menu_message
            .as_ref()
            .expect("save failure surfaces on the main menu");
        assert!(
            message.text.contains("could not save world"),
            "unexpected failure text: {}",
            message.text
        );
        assert_eq!(message.ticks_left, menu::MESSAGE_TICKS);
        let text = compose_screen_text(
            LifecycleScreen::MainMenu,
            &app.new_game,
            &app.load,
            None,
            app.main_menu_message.as_ref(),
        );
        assert_eq!(text.lines().count(), 4 + 1, "four rows plus the message");
        assert!(text.ends_with(message.text.as_str()));

        // The unsaved-changes flag stays set for the next save opportunity
        // (retry), and nothing was written.
        assert!(app.session.world.has_unsaved_changes());
        assert!(!app.session.save_path.exists());
    }

    #[test]
    fn save_and_quit_success_returns_to_the_main_menu_with_no_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("kept.mcvx");
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(9, save_path.clone(), DEFAULT_SEASON_LENGTH_DAYS),
        );
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, 9));
        app.session
            .world
            .set_block_immediate(0, 40, 5, BlockId::Brick);

        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        let exit_requested = app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit);
        assert!(!exit_requested);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert!(app.main_menu_message.is_none(), "no failure, no message");
        assert!(!app.session.world.has_unsaved_changes(), "flag cleared");
        assert!(save_path.is_file(), "the session's own path was written");
        // The main menu still lists exactly its four rows with no message.
        assert_eq!(
            compose_screen_text(
                LifecycleScreen::MainMenu,
                &app.new_game,
                &app.load,
                None,
                None
            ),
            "New Game\nLoad Game\nSettings\nExit Game"
        );
    }

    #[test]
    fn save_and_quit_from_pause_returns_to_a_vista_menu_with_the_save_flag_cleared() {
        // S02-T04: the menu-return interaction point where the vista takes
        // over the frame. The session GPU store clear is intentionally not
        // asserted here (headless GameApp has `gpu: None`); this locks the
        // lifecycle/flag side of the handoff so the vista is guaranteed to
        // be the screen that renders next.
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("menu-return.mcvx");
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(9, save_path, DEFAULT_SEASON_LENGTH_DAYS),
        );
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, 9));
        app.session
            .world
            .set_block_immediate(0, 40, 5, BlockId::Brick);
        assert!(app.session.world.has_unsaved_changes());

        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        assert!(
            !vista::vista_visible(app.lifecycle.screen()),
            "pause keeps rendering the frozen player world"
        );

        let exit_requested = app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit);

        assert!(!exit_requested);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert!(
            vista::vista_visible(app.lifecycle.screen()),
            "the main menu renders the vista, not the saved world"
        );
        assert!(
            app.main_menu_message.is_none(),
            "successful save, no message"
        );
        assert!(!app.session.world.has_unsaved_changes(), "flag cleared");
    }

    #[test]
    fn exit_game_saves_world_and_settings_and_requests_exit() {
        // S04-T02: the ExitGame decision arm (save the session's world,
        // save settings, return exit-requested — the caller owns
        // `event_loop.exit()`) made headless by the injected
        // `settings_path`: both save targets live inside a tempdir, so
        // the crate root is never written. No event loop, window, or GPU.
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("exit-game.mcvx");
        let settings_path = dir.path().join("settings.toml");
        let cli = CliConfig::parse_args(Vec::<String>::new());
        // Bare CliConfig: GameApp::new starts at the main menu over the
        // loaded default session (the same S03 test pattern as the
        // SaveAndQuit tests).
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(9, save_path.clone(), DEFAULT_SEASON_LENGTH_DAYS),
        );
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        // Inject the settings save path AFTER construction (runtime keeps
        // DEFAULT_SETTINGS_PATH; the test points it at the tempdir).
        app.settings_path = settings_path.clone();
        // An unsaved chunk edit: proof the world save actually wrote the
        // session's edited world, not just an empty file.
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, 9));
        app.session
            .world
            .set_block_immediate(0, 40, 5, BlockId::Brick);
        assert!(app.session.world.has_unsaved_changes());

        let exit_requested = app.apply_lifecycle_decision(LifecycleDecision::ExitGame);

        // The decision arm returns true: exit-requested, and the caller
        // owns `event_loop.exit()` (mirroring CloseRequested).
        assert!(
            exit_requested,
            "ExitGame requests exit; the caller owns event_loop.exit()"
        );
        // The session's world was saved at its own tempdir save path and
        // the unsaved-changes flag cleared.
        assert!(
            save_path.is_file(),
            "world saved at the session's save path"
        );
        assert!(!app.session.world.has_unsaved_changes());
        // Settings were durably written at the injected tempdir path.
        assert!(
            settings_path.is_file(),
            "settings saved at the injected settings path"
        );
        // The written settings parse back as Settings (durable, loadable).
        let (reloaded, warnings) = load_with_warnings(&settings_path);
        assert!(warnings.is_empty(), "reloaded settings parse cleanly");
        assert_eq!(reloaded.fov_degrees, app.settings.fov_degrees);
    }

    #[test]
    fn start_world_session_swaps_the_session_and_resets_input() {
        // GameApp::new with a bare CliConfig and a directly-built session:
        // no window/GPU needed (start_world_session tolerates gpu: None,
        // which is exactly the headless case; the GPU-clear leg rides on
        // the launch smoke script and S04's human UAT — see the honest
        // limitation documented on `ChunkRenderStore::clear`).
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(
                1,
                PathBuf::from("/nonexistent/initial.mcvx"),
                DEFAULT_SEASON_LENGTH_DAYS,
            ),
        );
        assert_eq!(app.session.world.seed(), 1);
        assert_eq!(
            app.session.save_path,
            PathBuf::from("/nonexistent/initial.mcvx")
        );

        // Held inputs as if mid-gameplay; the boundary must reset them.
        app.input.forward = true;
        app.input.jump_or_up = true;
        app.input.sprint = true;
        // The settings value is authoritative across a swap, including over
        // a session that arrived with a different saved render distance.
        app.settings.render_distance = 22;

        app.start_world_session(WorldSession::new_session(
            42,
            PathBuf::from("/nonexistent/replacement.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        ));

        assert_eq!(app.session.world.seed(), 42, "seed swapped");
        assert_eq!(
            app.session.save_path,
            PathBuf::from("/nonexistent/replacement.mcvx"),
            "save path swapped"
        );
        let (spawn_x, spawn_z) = find_spawn_column(42);
        let expected_y = (terrain_height_at_seeded(spawn_x, spawn_z, 42) + 4) as f32;
        assert_eq!(
            app.session.player.position,
            Vec3::new(spawn_x as f32 + 0.5, expected_y, spawn_z as f32 + 0.5),
            "player swapped to the new world's land-search spawn"
        );
        assert_eq!(
            app.session.camera.position,
            app.session.player.eye_position(),
            "camera synced to the new player by the constructor"
        );
        assert_eq!(app.session.camera.yaw, app.session.player.yaw);
        assert_eq!(app.session.camera.pitch, app.session.player.pitch);
        assert_eq!(
            app.session.world.render_distance(),
            22,
            "the settings render distance applies at the swap boundary"
        );
        assert!(
            !app.input.forward && !app.input.jump_or_up && !app.input.sprint,
            "input flags reset at the boundary"
        );
    }

    /// S02-T03: entering a non-Playing screen drops both the held-button
    /// latch and any partial mining progress — a menu must never inherit a
    /// hold or show a stale bar.
    #[test]
    fn enter_screen_clears_held_mouse_and_mining_progress() {
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(
                1,
                PathBuf::from("/nonexistent/initial.mcvx"),
                DEFAULT_SEASON_LENGTH_DAYS,
            ),
        );
        app.left_mouse_held = true;
        app.mining.tick(
            0.5,
            MiningTarget::from_block(IVec3::new(0, 40, 0), BlockId::Stone, None),
        );
        assert!(
            app.mining.progress().is_some(),
            "0.5s on Stone leaves partial progress"
        );

        app.enter_screen(LifecycleScreen::MainMenu);

        assert!(!app.left_mouse_held, "held latch cleared on screen entry");
        assert!(
            app.mining.progress().is_none(),
            "partial mining progress cleared on screen entry"
        );
    }

    /// S02-T03: a world swap clears the hold/progress too, so a new world
    /// can never start with progress inherited from the outgoing session.
    #[test]
    fn start_world_session_clears_held_mouse_and_mining_progress() {
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(
                1,
                PathBuf::from("/nonexistent/initial.mcvx"),
                DEFAULT_SEASON_LENGTH_DAYS,
            ),
        );
        app.left_mouse_held = true;
        app.mining.tick(
            0.5,
            MiningTarget::from_block(IVec3::new(0, 40, 0), BlockId::Stone, None),
        );
        assert!(app.mining.progress().is_some());

        app.start_world_session(WorldSession::new_session(
            42,
            PathBuf::from("/nonexistent/replacement.mcvx"),
            DEFAULT_SEASON_LENGTH_DAYS,
        ));

        assert!(!app.left_mouse_held, "held latch cleared on session swap");
        assert!(
            app.mining.progress().is_none(),
            "partial mining progress cleared on session swap"
        );
    }

    #[test]
    fn startup_session_loads_an_existing_save_file_at_the_cli_save_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("keep.mcvx");
        let save_path_str = save_path.to_str().expect("utf-8 tempdir path").to_string();
        let data = sample_save(31337);
        save_world_to_path(&save_path, &data).expect("write save file");

        let cli = CliConfig::parse_args(["--save", save_path_str.as_str()]);
        let session = startup_session(&cli);
        assert_eq!(session.world.seed(), 31337);
        assert_eq!(
            session.player.position,
            Vec3::from_array(data.player.position)
        );
        assert_eq!(session.player.yaw, 0.5);
        assert_eq!(session.player.pitch, -0.25);
        assert!(session.player.fly_mode);
        assert_eq!(session.player.selected_slot(), 2);
        assert_eq!(session.world.render_distance(), 16);
        assert_eq!(
            session.save_path, save_path,
            "session owns the cli save path"
        );
        assert_eq!(
            session.camera.position,
            session.player.eye_position(),
            "camera synced from the loaded player"
        );
    }

    #[test]
    fn startup_session_yields_a_fresh_session_when_the_save_is_missing_or_new_forced() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Missing file + seed override: fresh session at the override seed.
        let missing = dir.path().join("missing.mcvx");
        let cli = CliConfig::parse_args(["--save", missing.to_str().unwrap(), "--seed", "777"]);
        let session = startup_session(&cli);
        assert_eq!(session.world.seed(), 777);
        assert_eq!(session.save_path, missing);
        let (spawn_x, spawn_z) = find_spawn_column(777);
        let expected_y = (terrain_height_at_seeded(spawn_x, spawn_z, 777) + 4) as f32;
        assert_eq!(
            session.player.position,
            Vec3::new(spawn_x as f32 + 0.5, expected_y, spawn_z as f32 + 0.5),
            "spawn mirrors main()'s fresh branch"
        );

        // Missing file, no override: the default seed.
        let missing2 = dir.path().join("other.mcvx");
        let cli = CliConfig::parse_args(["--save", missing2.to_str().unwrap()]);
        let session = startup_session(&cli);
        assert_eq!(session.world.seed(), DEFAULT_WORLD_SEED);
        assert_eq!(session.save_path, missing2);

        // Corrupt save file: falls back to a fresh session (legacy
        // behavior — main() matched Err(_) the same way).
        let corrupt = dir.path().join("corrupt.mcvx");
        std::fs::write(&corrupt, b"NOPE!1234").expect("write corrupt save");
        let cli = CliConfig::parse_args(["--save", corrupt.to_str().unwrap()]);
        let session = startup_session(&cli);
        assert_eq!(session.world.seed(), DEFAULT_WORLD_SEED);
        let (default_x, default_z) = find_spawn_column(DEFAULT_WORLD_SEED);
        assert_eq!(
            session.player.position.y,
            (terrain_height_at_seeded(default_x, default_z, DEFAULT_WORLD_SEED) + 4) as f32,
            "corrupt save falls back to the default-seed land-search spawn"
        );

        // --new forces a fresh session even when a save file exists
        // (legacy direct-launch behavior, byte-identical).
        let existing = dir.path().join("ignored.mcvx");
        save_world_to_path(&existing, &sample_save(999)).expect("write existing save");
        let cli =
            CliConfig::parse_args(["--save", existing.to_str().unwrap(), "--new", "--seed", "5"]);
        let session = startup_session(&cli);
        assert_eq!(
            session.world.seed(),
            5,
            "force_new ignores the existing save"
        );
        let (spawn_x, spawn_z) = find_spawn_column(5);
        let expected_y = (terrain_height_at_seeded(spawn_x, spawn_z, 5) + 4) as f32;
        assert_eq!(
            session.player.position,
            Vec3::new(spawn_x as f32 + 0.5, expected_y, spawn_z as f32 + 0.5)
        );
        assert_eq!(session.save_path, existing);
    }

    /// S03-T04 integrated loop proof — the slice's demo, driven entirely
    /// through the real flows with no window/GPU/event-loop: create a
    /// named seeded world from the form, edit it and move the player,
    /// Escape-to-pause semantics + Save and Quit back to the main menu
    /// (writing the session's own save path), then reload the same row
    /// and prove seed, edited-time player state, and chunk edits intact.
    #[test]
    fn integrated_loop_create_edit_save_quit_and_reload_keeps_everything_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);

        // The create flow: name "alpha", seed text "42".
        app.new_game
            .apply(TextInputAction::InsertText("alpha".into()));
        app.new_game.cycle_focus();
        app.new_game.apply(TextInputAction::InsertText("42".into()));
        app.attempt_create();

        let expected_seed = resolve_seed_or("42", 0);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.world.seed(), expected_seed);
        assert_eq!(app.session.save_path, dir.path().join("alpha.mcvx"));

        // Edit the world: load the spawn chunk and place a block (marking
        // unsaved changes), and move the player / pick a hotbar slot so
        // the edited-time player state must survive the round trip.
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, expected_seed));
        app.session
            .world
            .set_block_immediate(0, 40, 5, BlockId::Brick);
        assert_eq!(app.session.world.get_block(0, 40, 5), BlockId::Brick);
        assert!(app.session.world.has_unsaved_changes());
        let edits_before = app.session.world.chunk_edits().clone();
        app.session.player.position = Vec3::new(12.5, 70.0, -3.5);
        app.session.player.yaw = 1.25;
        app.session.player.pitch = -0.5;
        app.session.player.fly_mode = true;
        assert!(app.session.player.select_slot(5));

        // Escape to pause, then Save and Quit through the real, loop-free
        // decision apply: back to the main menu with the session saved to
        // its own save path.
        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        let exit_requested = app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit);
        assert!(!exit_requested);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert!(
            app.main_menu_message.is_none(),
            "save succeeded, no message"
        );
        assert!(!app.session.world.has_unsaved_changes(), "flag cleared");

        // The save file exists with exactly the session's own data.
        let save_path = dir.path().join("alpha.mcvx");
        let data = load_world_from_path(&save_path).expect("alpha.mcvx written and loadable");
        assert_eq!(data.seed, expected_seed);
        assert_eq!(data.player.position, [12.5, 70.0, -3.5]);
        assert_eq!(data.player.yaw, 1.25);
        assert_eq!(data.player.pitch, -0.5);
        assert!(data.player.fly_mode);
        assert_eq!(data.player.selected_slot, 5);
        assert_eq!(data.chunk_edits, edits_before);

        // Load the same row: enter the load screen (refreshes rows from
        // the tempdir catalog), Play, and prove the round trip.
        app.enter_screen(LifecycleScreen::LoadGame);
        let index = app
            .load
            .rows()
            .iter()
            .position(|r| r.name == "alpha")
            .expect("alpha row listed");
        app.play_row(index);

        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.world.seed(), expected_seed, "seed round-trips");
        assert_eq!(
            app.session.save_path, save_path,
            "session owns the row's own file path"
        );
        assert_eq!(
            app.session.player.position,
            Vec3::new(12.5, 70.0, -3.5),
            "edited-time player position round-trips"
        );
        assert_eq!(app.session.player.yaw, 1.25);
        assert_eq!(app.session.player.pitch, -0.5);
        assert!(app.session.player.fly_mode);
        assert_eq!(app.session.player.selected_slot(), 5);
        assert_eq!(
            app.session.world.chunk_edits(),
            &edits_before,
            "chunk edits round-trip"
        );
        assert!(app.cursor_grabbed, "Playing semantics grab the cursor");

        // The persisted edit re-applies when the chunk regenerates (the
        // same bounded streaming pump the WorldManager
        // unload/regeneration test uses — headless, rayon-driven).
        for _ in 0..40 {
            app.session.world.update_streaming(cpos);
            if app.session.world.get_block(0, 40, 5) == BlockId::Brick {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            app.session.world.get_block(0, 40, 5),
            BlockId::Brick,
            "the edited block is restored after the reload"
        );
    }

    // -----------------------------------------------------------------------
    // S04-T01: named-world crafting continuity (Final Integrated Acceptance)
    // -----------------------------------------------------------------------

    /// One inventory click through the production adapter: the session's live
    /// table station is the mode authority, exactly as the event-loop arms
    /// pass `session.open_table_position()`.
    fn click_inventory(
        app: &mut GameApp,
        screen: &InventoryScreen,
        target: InventoryTarget,
    ) -> Result<InventoryClickOutcome, InventoryCommandError> {
        let table_open = app.session.open_table_position();
        apply_inventory_click(&mut app.session.player, screen, table_open, target)
    }

    /// Asserts a click was one accepted whole-stack move.
    fn expect_moved(app: &mut GameApp, screen: &InventoryScreen, target: InventoryTarget) {
        assert_eq!(
            click_inventory(app, screen, target),
            Ok(InventoryClickOutcome::Moved),
            "expected a whole-stack move for {target:?}"
        );
    }

    /// Asserts a result-slot click crafted exactly `id` and returns the item.
    fn expect_crafted(
        app: &mut GameApp,
        screen: &InventoryScreen,
        kind: GridKind,
        id: ItemId,
    ) -> Item {
        match click_inventory(app, screen, InventoryTarget::ResultSlot(kind)) {
            Ok(InventoryClickOutcome::Crafted(item)) => {
                assert_eq!(item.id(), id, "unexpected crafted item");
                item
            }
            other => panic!("expected a craft of {id:?} from {kind:?}, got {other:?}"),
        }
    }

    /// Total count of one identity across the slots, the cursor, and both
    /// grids: the conservation invariant every click path must preserve.
    fn inventory_count_of(app: &GameApp, id: ItemId) -> u32 {
        let mut total = 0u32;
        for item in app.session.player.inventory().slots().iter().flatten() {
            if item.id() == id {
                total += u32::from(item.count());
            }
        }
        if let Some(cursor) = app.session.player.inventory().cursor() {
            if cursor.id() == id {
                total += u32::from(cursor.count());
            }
        }
        for grid in [
            app.session.player.player_grid(),
            app.session.player.table_grid(),
        ] {
            for index in 0..grid.kind().slot_count() {
                if let Some(item) = grid.slot(index) {
                    if item.id() == id {
                        total += u32::from(item.count());
                    }
                }
            }
        }
        total
    }

    /// S04-T01 — the milestone's Final Integrated Acceptance in executable
    /// form. A named world is created from the real form, played through the
    /// real material chain (logs -> planks -> sticks -> table item -> wood
    /// pickaxe), the table is placed and opened through the production
    /// right-click path, a high 4x4 cell (15), the 2x2 grid, a worn tool, and
    /// a held cursor stack are all left unfinished, then Save-and-Quit writes
    /// the world and Load Game -> Play restores it through the real catalog.
    /// Every piece of inventory/grid/durability/world state must come back
    /// exactly, the table edit must re-apply when its chunk regenerates, a
    /// restore-then-save must be byte-identical, and the restored tool must
    /// break on its last point and be recrafted from the restored materials.
    ///
    /// Seeding note: two fixtures are injected, and both are called out inline
    /// below. (a) The flat test chunk plus its face halo — the same headless
    /// seam every mining/placement test uses, because terrain streaming cannot
    /// run without a live loop. (b) A one-time redistribution of the sixteen
    /// crafted planks into four stacks: the panel moves whole stacks and has no
    /// split gesture, so the 2x2 table recipe's four same-identity cells are
    /// unreachable through clicks. Conservation is asserted across that seam;
    /// gathering, every craft, placement, wear point, save, restore, tool
    /// breakage, and recraft below runs through production code.
    #[test]
    fn named_world_continuity_restores_inventory_grids_durability_and_placed_table() {
        use opencraft::player::inventory::{
            Material, RecipeId, ToolKind, ToolTier, INVENTORY_SLOTS,
        };
        use opencraft::world::chunk::linear_index;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);

        // 1. Create the named world through the real form + Create flow.
        app.new_game
            .apply(TextInputAction::InsertText("continuity".into()));
        app.new_game.cycle_focus();
        app.new_game
            .apply(TextInputAction::InsertText("4242".into()));
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        let save_path = dir.path().join("continuity.mcvx");
        assert_eq!(app.session.save_path, save_path);
        assert_eq!(app.session.world.seed(), resolve_seed_or("4242", 0));

        // 2. Gather six oak logs through the real break path.
        insert_test_chunk_with_face_neighbors(&mut app.session, BlockId::Air);
        let log_positions: Vec<IVec3> = (1..=6).map(|x| IVec3::new(x, 10, 0)).collect();
        for pos in &log_positions {
            app.session
                .world
                .set_block_immediate(pos.x, pos.y, pos.z, BlockId::OakLog);
        }
        for pos in &log_positions {
            let (remeshed, wear) =
                applied_break(break_mined_block(&mut app.session, *pos, BlockId::OakLog));
            assert!(!remeshed.is_empty(), "a real log break remeshes its chunk");
            assert_eq!(
                wear,
                ToolWearOutcome::Nothing,
                "bare hands cost no durability"
            );
        }
        assert_eq!(
            app.session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::OakLog,
                count: 6,
            }),
            "the real gather path collected every log into the hotbar"
        );

        let mut screen = InventoryScreen::new();
        screen.open_player();
        let player_cell = |index: usize| InventoryTarget::GridCell {
            kind: GridKind::Player,
            index,
        };
        let table_cell = |index: usize| InventoryTarget::GridCell {
            kind: GridKind::Table,
            index,
        };

        // 3a. Logs -> planks through real clicks. The 1x1 log recipe requires
        // the grid to hold the log stack alone, so all four crafts deposit
        // their four-plank results into the inventory, where insertion merges
        // them into one stack exactly as specified.
        expect_moved(&mut app, &screen, InventoryTarget::Slot(0));
        expect_moved(&mut app, &screen, player_cell(0));
        for _ in 0..4 {
            let planks = expect_crafted(
                &mut app,
                &screen,
                GridKind::Player,
                ItemId::Block(BlockId::OakPlanks),
            );
            assert_eq!(planks.count(), 4);
        }
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
            16,
            "four real log crafts produce sixteen planks"
        );

        // Park the two leftover logs in main storage.
        expect_moved(&mut app, &screen, player_cell(0));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(5));

        // Documented fixture seam — the one place the click surface cannot
        // express the domain: the panel moves whole stacks and exposes no
        // split gesture, so the four separate plank stacks the 2x2 table
        // recipe requires are seeded here through the same validated
        // constructor session restore uses. Conservation is asserted before
        // and after; every craft, placement, wear point, save, and load below
        // still runs through production code.
        let planks_before = inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks));
        let logs_before = inventory_count_of(&app, ItemId::Block(BlockId::OakLog));
        assert_eq!((planks_before, logs_before), (16, 2));
        let mut seeded_slots = [None; INVENTORY_SLOTS];
        for slot in seeded_slots.iter_mut().take(4) {
            *slot = Some(Item::new(ItemId::Block(BlockId::OakPlanks), 4).expect("plank stack"));
        }
        seeded_slots[5] = Some(Item::new(ItemId::Block(BlockId::OakLog), 2).expect("log stack"));
        let position = app.session.player.position;
        let player_grid = app.session.player.player_grid().clone();
        let table_grid = app.session.player.table_grid().clone();
        app.session.player = Player::with_inventory(
            position,
            Inventory::from_parts(seeded_slots, None),
            player_grid,
            table_grid,
        );
        app.session.player.restore_selected_slot(0);
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
            planks_before,
            "seeding redistributed the crafted planks without changing totals"
        );
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakLog)),
            logs_before
        );

        // 3b. Four plank stacks make the crafting-table item in the 2x2 grid.
        for cell in 0..4 {
            expect_moved(&mut app, &screen, InventoryTarget::Slot(cell));
            expect_moved(&mut app, &screen, player_cell(cell));
        }
        let table_item = expect_crafted(
            &mut app,
            &screen,
            GridKind::Player,
            ItemId::Block(BlockId::CraftingTable),
        );
        assert_eq!(table_item.count(), 1);
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
            12,
            "the table recipe consumed exactly four planks"
        );

        // 4. Place the table through the production right-click path and open
        // it through the same path (the screen attach mirrors `open_inventory`,
        // which needs a live Window for the cursor grab).
        assert!(app.session.player.select_slot(0));
        app.session.player.position = Vec3::new(5.5, 10.0, 5.5);
        app.session
            .world
            .set_block_immediate(0, 5, 0, BlockId::Stone);
        aim_placement_camera(&mut app.session);
        match right_click_interaction(&mut app.session) {
            RightClickOutcome::Placed(remeshed) => {
                assert!(!remeshed.is_empty(), "placement remeshes its chunk")
            }
            _ => panic!("the targeted stone face must accept the table placement"),
        }
        let table_pos = IVec3::new(0, 6, 0);
        assert_eq!(
            app.session
                .world
                .get_block(table_pos.x, table_pos.y, table_pos.z),
            BlockId::CraftingTable,
            "the placed table is a real world block"
        );
        assert!(
            app.session.player.hotbar_stack(0).is_none(),
            "placement consumed the crafted table"
        );
        aim_table_interaction_at(&mut app.session, table_pos);
        assert!(
            matches!(
                right_click_interaction(&mut app.session),
                RightClickOutcome::TableOpened
            ),
            "the real right-click path opens the placed table"
        );
        assert_eq!(app.session.open_table_position(), Some(table_pos));
        screen.close();
        screen.open_table();
        assert_eq!(
            screen.mode(app.session.open_table_position()),
            InventoryMode::Table
        );

        // 3c. Complete the pickaxe head row and two separate stick stacks in
        // the 4x4 grid. Cells 1 and 3 leave the 2x2 grid so the vertical stick
        // recipe is the only match there; the first stick batch is parked in
        // the table grid so the second craft cannot merge with it.
        for (from, to) in [(1usize, 0usize), (3, 1)] {
            expect_moved(&mut app, &screen, player_cell(from));
            expect_moved(&mut app, &screen, table_cell(to));
        }
        for cell in [5usize, 9usize] {
            let sticks = expect_crafted(
                &mut app,
                &screen,
                GridKind::Player,
                ItemId::Material(Material::Stick),
            );
            assert_eq!(sticks.count(), 4);
            expect_moved(&mut app, &screen, InventoryTarget::Slot(0));
            expect_moved(&mut app, &screen, table_cell(cell));
        }
        expect_moved(&mut app, &screen, player_cell(0));
        expect_moved(&mut app, &screen, table_cell(2));

        let pickaxe = expect_crafted(
            &mut app,
            &screen,
            GridKind::Table,
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            },
        );
        assert_eq!(pickaxe.durability(), Some(59));
        assert!(app.session.player.select_slot(0));

        // 5. Leave the 4x4 grid with unfinished materials, including cell 15
        // (index >= 9) to prove the full 4x4 extent persists. The remaining
        // plank stack from the 2x2 grid lands in cell 2 as its own unfinished
        // cell.
        expect_moved(&mut app, &screen, table_cell(0));
        expect_moved(&mut app, &screen, table_cell(15));
        expect_moved(&mut app, &screen, player_cell(2));
        expect_moved(&mut app, &screen, table_cell(2));

        // 7 (late batch). Craft the parked log's planks and leave two
        // unfinished cells in the 2x2 grid.
        expect_moved(&mut app, &screen, InventoryTarget::Slot(5));
        expect_moved(&mut app, &screen, player_cell(0));
        let planks = expect_crafted(
            &mut app,
            &screen,
            GridKind::Player,
            ItemId::Block(BlockId::OakPlanks),
        );
        assert_eq!(planks.count(), 4);
        // The fresh planks land in the first empty slot (slot 1): the pickaxe
        // owns slot 0 and the log slot was just emptied by the pick-up.
        expect_moved(&mut app, &screen, InventoryTarget::Slot(1));
        expect_moved(&mut app, &screen, player_cell(3));

        // 6. Wear the selected tool through the real break path (edit -> remesh
        // proof -> exactly one wear -> collect) down to its last point. The
        // quarry reuses one cell; the headless chunk's column is incomplete, so
        // no light recompute runs and each point costs one remesh.
        let wear_pos = IVec3::new(2, 8, 0);
        let mut remaining = 59u16;
        while remaining > 1 {
            let placed = app.session.world.set_block_immediate(
                wear_pos.x,
                wear_pos.y,
                wear_pos.z,
                BlockId::Stone,
            );
            assert!(!placed.is_empty(), "the quarry edit remeshes");
            let (remeshed, wear) = applied_break(break_mined_block(
                &mut app.session,
                wear_pos,
                BlockId::Stone,
            ));
            assert!(!remeshed.is_empty(), "the wear break remeshes");
            remaining -= 1;
            assert_eq!(
                wear,
                ToolWearOutcome::Worn { remaining },
                "exactly one durability point per applied break"
            );
        }
        assert_eq!(
            app.session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(1),
            "the worn tool is recorded at its last point"
        );
        assert_eq!(
            app.session
                .player
                .inventory()
                .slot(1)
                .map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Cobblestone), 58)),
            "every break folded its mined drop into the first free hotbar slot"
        );

        // 7. Leave a held cursor stack: pick the cobblestone up and do not
        // place it.
        expect_moved(&mut app, &screen, InventoryTarget::Slot(1));
        assert_eq!(
            app.session
                .player
                .inventory()
                .cursor()
                .map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Cobblestone), 58)),
            "the stack rides the cursor"
        );

        // The saved aim points straight down at the table from three blocks up.
        // Persist an air shaft so the raycast stays clear once the chunk
        // regenerates from terrain seed data on reload.
        for y in 7..=10 {
            app.session.world.set_block_immediate(0, y, 0, BlockId::Air);
        }

        // 8. Close the panel the way `close_inventory` does (it needs a live
        // Window for the cursor grab), pause, and Save-and-Quit.
        screen.close();
        app.session.close_table();
        assert_eq!(
            screen.mode(app.session.open_table_position()),
            InventoryMode::Closed
        );
        assert_eq!(app.session.open_table_position(), None);

        let expected_inventory = app.session.player.inventory().clone();
        let expected_player_grid = app.session.player.player_grid().clone();
        let expected_table_grid = app.session.player.table_grid().clone();
        let expected_units = inventory_units(&app.session.player);
        let expected_edits = app.session.world.chunk_edits().clone();

        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        assert!(!app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit));
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert!(app.main_menu_message.is_none(), "the save succeeded");
        assert!(!app.session.world.has_unsaved_changes());
        let first_bytes = std::fs::read(&save_path).expect("the save file exists");
        assert!(!first_bytes.is_empty());

        // The file itself carries the exact state and the table edit.
        let persisted = load_world_from_path(&save_path).expect("the save decodes");
        assert_eq!(persisted.player.inventory, expected_inventory);
        assert_eq!(persisted.player.player_grid, expected_player_grid);
        assert_eq!(persisted.player.table_grid, expected_table_grid);
        assert_eq!(persisted.player.selected_slot, 0);
        let (edit_cpos, (lx, ly, lz)) =
            world_to_chunk_and_local(table_pos.x, table_pos.y, table_pos.z);
        assert_eq!(
            persisted.chunk_edits[&edit_cpos][&(linear_index(lx, ly, lz) as u16)],
            BlockId::CraftingTable,
            "the placed table is persisted as a chunk edit"
        );

        // 9. Reload through the real catalog: Load Game refreshes rows from
        // the tempdir catalog, Play rebuilds the session from the file.
        app.enter_screen(LifecycleScreen::LoadGame);
        let index = app
            .load
            .rows()
            .iter()
            .position(|row| row.name == "continuity")
            .expect("the continuity row is listed");
        app.play_row(index);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.save_path, save_path);

        // 10. Exact restoration: slots, cursor, durability, selection, both
        // grids cell-by-cell, and the played position.
        assert_eq!(
            app.session.player.inventory(),
            &expected_inventory,
            "every slot, the cursor, and tool durability are restored exactly"
        );
        assert_eq!(
            app.session.player.player_grid(),
            &expected_player_grid,
            "the 2x2 grid is restored cell-by-cell"
        );
        assert_eq!(
            app.session.player.table_grid(),
            &expected_table_grid,
            "the 4x4 grid is restored cell-by-cell"
        );
        assert_eq!(
            inventory_units(&app.session.player),
            expected_units,
            "no item was lost or duplicated across the round trip"
        );
        assert_eq!(app.session.player.selected_slot(), 0);
        let restored_tool = app
            .session
            .player
            .inventory()
            .slot(0)
            .expect("the tool is restored in its slot");
        assert_eq!(
            restored_tool.id(),
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            }
        );
        assert_eq!(
            restored_tool.durability(),
            Some(1),
            "the remaining durability point survives exactly"
        );
        assert_eq!(
            app.session
                .player
                .inventory()
                .cursor()
                .map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Cobblestone), 58)),
            "the held cursor stack is restored"
        );
        assert_eq!(
            app.session
                .player
                .table_grid()
                .slot(15)
                .map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::OakPlanks), 2)),
            "high table cell 15 persists"
        );
        assert_eq!(
            app.session
                .player
                .table_grid()
                .slot(9)
                .map(|item| item.id()),
            Some(ItemId::Material(Material::Stick))
        );
        assert_eq!(
            app.session
                .player
                .player_grid()
                .slot(0)
                .map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakLog))
        );
        assert_eq!(
            app.session
                .player
                .player_grid()
                .slot(3)
                .map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakPlanks))
        );
        assert_eq!(
            app.session.player.position,
            Vec3::new(0.5, 9.0, 0.5),
            "the played position round-trips"
        );
        // The station is intentionally ephemeral (S02 design): a restart always
        // starts with no table open, which is a positive assertion here.
        assert_eq!(app.session.open_table_position(), None);
        assert_eq!(
            InventoryScreen::new().mode(app.session.open_table_position()),
            InventoryMode::Closed
        );

        // 13. Negative: a table-grid click without a live station is rejected
        // in Player mode and mutates nothing.
        let inventory_before = app.session.player.inventory().clone();
        let player_grid_before = app.session.player.player_grid().clone();
        let table_grid_before = app.session.player.table_grid().clone();
        let mut player_screen = InventoryScreen::new();
        player_screen.open_player();
        assert_eq!(
            apply_inventory_click(
                &mut app.session.player,
                &player_screen,
                None,
                InventoryTarget::GridCell {
                    kind: GridKind::Table,
                    index: 15,
                },
            ),
            Err(InventoryCommandError::TableUnavailable)
        );
        assert_eq!(app.session.player.inventory(), &inventory_before);
        assert_eq!(app.session.player.player_grid(), &player_grid_before);
        assert_eq!(app.session.player.table_grid(), &table_grid_before);

        // 11. The table edit re-applies when its chunk regenerates from the
        // seed plus the persisted edits (the reloaded session is unstreamed).
        assert_ne!(
            app.session
                .world
                .get_block(table_pos.x, table_pos.y, table_pos.z),
            BlockId::CraftingTable,
            "nothing is resident before the streaming pump runs"
        );
        for _ in 0..120 {
            app.session.world.update_streaming(ChunkPos::new(0, 0, 0));
            if app
                .session
                .world
                .get_block(table_pos.x, table_pos.y, table_pos.z)
                == BlockId::CraftingTable
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            app.session
                .world
                .get_block(table_pos.x, table_pos.y, table_pos.z),
            BlockId::CraftingTable,
            "the placed table edit survives chunk regeneration"
        );
        for _ in 0..120 {
            if app
                .session
                .world
                .has_all_face_neighbors(ChunkPos::new(0, 0, 0))
            {
                break;
            }
            app.session.world.update_streaming(ChunkPos::new(0, 0, 0));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.session
                .world
                .has_all_face_neighbors(ChunkPos::new(0, 0, 0)),
            "the regenerated chunk has its face halo resident for edits"
        );
        assert_eq!(
            app.session.world.chunk_edits(),
            &expected_edits,
            "streaming applies edits without rewriting the edit map"
        );

        // 12a. A restore followed by a save with no intervening play is
        // byte-identical to the original file.
        let second_path = dir.path().join("continuity-again.mcvx");
        app.session.save_path = second_path.clone();
        save_current_world(&mut app.session).expect("the restored session re-saves");
        assert_eq!(
            std::fs::read(&save_path).expect("read first save"),
            std::fs::read(&second_path).expect("read second save"),
            "session restore followed by save is byte-identical"
        );
        app.session.save_path = save_path.clone();

        // 12b. The restored tool is spent on its last point through the real
        // break path, then a replacement is recrafted from the restored
        // materials. The held stack is put down first.
        screen.open_player();
        expect_moved(&mut app, &screen, InventoryTarget::Slot(20));
        assert!(app.session.player.inventory().cursor().is_none());

        app.session
            .world
            .set_block_immediate(wear_pos.x, wear_pos.y, wear_pos.z, BlockId::Stone);
        let (remeshed, wear) = applied_break(break_mined_block(
            &mut app.session,
            wear_pos,
            BlockId::Stone,
        ));
        assert!(!remeshed.is_empty(), "the last wear break remeshes");
        assert_eq!(
            wear,
            ToolWearOutcome::Broke,
            "the restored last point breaks the tool"
        );
        assert!(
            app.session.player.inventory().slot(0).is_none(),
            "the last use freed the selected slot before collection (MEM195 ordering)"
        );
        assert!(
            app.session
                .player
                .inventory()
                .slots()
                .iter()
                .flatten()
                .all(|item| !item.is_tool()),
            "the broken tool is gone from every slot"
        );

        // Re-open the restored table through the production right-click path
        // using the restored aim, then recraft from the restored grid.
        assert!(
            matches!(
                right_click_interaction(&mut app.session),
                RightClickOutcome::TableOpened
            ),
            "the restored table is targetable again"
        );
        assert_eq!(app.session.open_table_position(), Some(table_pos));
        screen.close();
        screen.open_table();
        expect_moved(&mut app, &screen, table_cell(15));
        expect_moved(&mut app, &screen, table_cell(0));
        assert_eq!(
            app.session
                .player
                .table_grid()
                .preview()
                .map(|preview| preview.recipe),
            Some(RecipeId::WoodPickaxe),
            "the restored materials complete the pickaxe recipe"
        );
        let recrafted = expect_crafted(
            &mut app,
            &screen,
            GridKind::Table,
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            },
        );
        assert_eq!(recrafted.durability(), Some(59), "a recrafted tool is full");
        assert!(app.session.player.select_slot(0));
        assert_eq!(
            app.session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(59)
        );

        // Re-save the played-forward world through the real save site and read
        // the file back: the state legitimately changed, so this is a fresh
        // save that must decode with the recrafted tool and consumed grids.
        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        assert!(!app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit));
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert!(app.main_menu_message.is_none());
        let final_bytes = std::fs::read(&save_path).expect("the final save exists");
        assert_ne!(
            final_bytes, first_bytes,
            "play legitimately changed the persisted state"
        );
        let final_data = load_world_from_path(&save_path).expect("the final save decodes");
        assert_eq!(
            final_data.player.inventory.slot(0).map(|item| item.id()),
            Some(ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            })
        );
        assert_eq!(
            final_data
                .player
                .inventory
                .slot(0)
                .and_then(|item| item.durability()),
            Some(59),
            "the recrafted tool is persisted at full durability"
        );
        assert_eq!(
            final_data
                .player
                .inventory
                .slots()
                .iter()
                .flatten()
                .filter(|item| item.is_tool())
                .count(),
            1,
            "exactly the recrafted tool remains"
        );
        assert_eq!(
            final_data
                .player
                .inventory
                .slot(20)
                .map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Cobblestone), 59)),
            "the restored held stack plus the last mined drop are conserved"
        );
        assert_eq!(
            final_data
                .player
                .table_grid
                .slot(5)
                .map(|item| item.count()),
            Some(2),
            "the restored stick stack supplied the recipe and kept its remainder"
        );
    }

    /// S03-T04 startup regression: a world-intent CLI (the three-flag
    /// disjunction both smoke scripts' launches rely on) starts a session
    /// that owns `cli.save_path` and GameApp::new enters Playing directly
    /// (the direct-launch bypass); a bare launch targets the default save
    /// path and loads `saves/default.mcvx` when it exists (legacy
    /// byte-identical behavior — read-only, the test never writes there).
    #[test]
    fn startup_regression_direct_launch_owns_cli_path_and_bare_launch_targets_the_default_save() {
        // Direct launch: --save + --seed is a world intent; the startup
        // session owns the CLI save path at the override seed.
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("smoke.mcvx");
        let cli = CliConfig::parse_args(["--save", save_path.to_str().unwrap(), "--seed", "7"]);
        assert!(cli_has_world_intent(&cli));
        let session = startup_session(&cli);
        assert_eq!(session.save_path, cli.save_path);
        assert_eq!(session.world.seed(), 7);

        // GameApp::new with a world-intent CLI enters gameplay directly,
        // bypassing every menu screen (both smoke scripts' launches).
        let app = GameApp::new(cli, session);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert!(app.cursor_grabbed, "direct launch grabs the cursor");
        assert_eq!(app.session.save_path, save_path);

        // Bare launch: no world intent, the session targets the default
        // save path, and startup_session loads `saves/default.mcvx` when
        // it is present and loadable (asserting against the file's own
        // content, so the test tracks the fixture instead of pinning it);
        // a missing/corrupt default save falls back to the fresh default
        // seed. Read-only either way.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert!(!cli_has_world_intent(&bare));
        assert_eq!(bare.save_path, Path::new(DEFAULT_SAVE_PATH));
        let session = startup_session(&bare);
        assert_eq!(
            session.save_path,
            Path::new(DEFAULT_SAVE_PATH),
            "bare launch targets the default save path"
        );
        match load_world_from_path(Path::new(DEFAULT_SAVE_PATH)) {
            Ok(data) => {
                assert_eq!(session.world.seed(), data.seed, "default save loaded");
                assert_eq!(session.world.chunk_edits(), &data.chunk_edits);
                assert_eq!(
                    session.player.position,
                    Vec3::from_array(data.player.position)
                );
            }
            Err(_) => {
                assert_eq!(
                    session.world.seed(),
                    DEFAULT_WORLD_SEED,
                    "no default save: fresh session at the default seed"
                );
            }
        }
    }

    #[test]
    fn tick_transient_message_expires_by_tick_counter_purely() {
        let mut message = Some(MenuMessage {
            text: "note".to_string(),
            ticks_left: 2,
        });
        assert!(tick_transient_message(&mut message));
        assert_eq!(message.as_ref().unwrap().ticks_left, 1);
        assert!(
            !tick_transient_message(&mut message),
            "the expiring tick is false"
        );
        assert!(message.is_none(), "expires at zero");
        assert!(
            !tick_transient_message(&mut message),
            "no message stays false"
        );
    }

    #[test]
    fn compose_screen_text_lists_main_menu_and_pause_rows_in_order() {
        let new_game = NewGameScreen::new();
        let load = LoadScreen::new();
        assert_eq!(
            compose_screen_text(LifecycleScreen::MainMenu, &new_game, &load, None, None),
            "New Game\nLoad Game\nSettings\nExit Game"
        );
        assert_eq!(
            compose_screen_text(LifecycleScreen::Pause, &new_game, &load, None, None),
            "Resume\nSettings\nSave and Quit"
        );
        // With no message the main menu lists exactly its four rows; a
        // transient main-menu message (the S03-T03 Save-and-Quit save
        // failure note) appends as the main menu's last line when present.
        let message = MenuMessage {
            text: "could not save world: blocked".to_string(),
            ticks_left: menu::MESSAGE_TICKS,
        };
        assert_eq!(
            compose_screen_text(
                LifecycleScreen::MainMenu,
                &new_game,
                &load,
                None,
                Some(&message)
            ),
            "New Game\nLoad Game\nSettings\nExit Game\ncould not save world: blocked"
        );
    }

    #[test]
    fn compose_screen_text_shows_new_game_fields_and_appends_messages() {
        let mut new_game = NewGameScreen::new();
        let load = LoadScreen::new();
        // default focus Name: marker prefix, caret after the typed name,
        // unfocused blank seed renders the random hint
        new_game.apply(TextInputAction::InsertText("Deep Dark".into()));
        assert_eq!(
            compose_screen_text(LifecycleScreen::NewGame, &new_game, &load, None, None),
            "> World Name: Deep Dark|\n  Seed: (random)\nRandomize Seed\nSeason Length: 1 day\nCreate World\nBack"
        );
        // Tab moves the marker to the seed; its caret follows the text
        new_game.cycle_focus();
        new_game.apply(TextInputAction::InsertText("42".into()));
        assert_eq!(
            compose_screen_text(LifecycleScreen::NewGame, &new_game, &load, None, None),
            "  World Name: Deep Dark\n> Seed: 42|\nRandomize Seed\nSeason Length: 1 day\nCreate World\nBack"
        );
        // Inline form errors append as the final line (warnings-as-data).
        new_game.set_message_for_ticks("World name cannot be empty.", 1);
        let text = compose_screen_text(LifecycleScreen::NewGame, &new_game, &load, None, None);
        assert!(text.ends_with("World name cannot be empty."));
    }

    #[test]
    fn repeated_lines_composes_one_label_line_per_catalog_row() {
        // Empty catalog: the empty string (the render path skips the label
        // areas entirely when row_count == 0, so this is a pinned
        // convention, not a rendered state).
        assert_eq!(repeated_lines("Play", 0), "");
        assert_eq!(repeated_lines("Delete", 0), "");
        // One line per catalog row, every line exactly the label, nothing
        // beyond the single newline between lines (a trailing newline
        // would render an empty line on the row below the catalog).
        for &(label, count) in &[("Play", 1), ("Delete", 1), ("Play", 3), ("Delete", 3)] {
            let text = repeated_lines(label, count);
            let lines: Vec<&str> = text.split('\n').collect();
            assert_eq!(lines.len(), count);
            assert!(lines.iter().all(|line| *line == label));
        }
        assert_eq!(repeated_lines("Play", 3), "Play\nPlay\nPlay");
        assert_eq!(repeated_lines("Delete", 2), "Delete\nDelete");
    }

    #[test]
    fn compose_screen_text_lists_load_rows_back_prompt_and_message_in_geometry_order() {
        let new_game = NewGameScreen::new();
        let mut load = LoadScreen::new();
        // Empty catalog (fresh install / missing saves dir): not an error —
        // displayed as such, then Back.
        assert_eq!(
            compose_screen_text(LifecycleScreen::LoadGame, &new_game, &load, None, None),
            "(no saved worlds)\nBack"
        );

        // Rows in injected (newest-first) order with corrupt marking, then
        // Back (line order matches T01 geometry so highlights align).
        load.set_rows(vec![
            CatalogRow::loadable("alpha".into(), "alpha.mcvx".into(), Some(100)),
            CatalogRow::corrupt(
                "broken".into(),
                "broken.mcvx".into(),
                None,
                RowProblem::InvalidFormat,
            ),
        ]);
        assert_eq!(
            compose_screen_text(LifecycleScreen::LoadGame, &new_game, &load, None, None),
            "alpha — 1970-01-01\nbroken — unknown — (cannot load)\nBack"
        );

        // A pending delete appends the prompt-naming confirm row and cancel.
        assert!(load.request_delete(1));
        assert_eq!(
            compose_screen_text(LifecycleScreen::LoadGame, &new_game, &load, None, None),
            "alpha — 1970-01-01\nbroken — unknown — (cannot load)\nBack\nDelete 'broken'?\nCancel"
        );

        // A transient load-screen message (inline Play errors) appends last.
        let message = MenuMessage {
            text: "Could not load world \"alpha\"".to_string(),
            ticks_left: 1,
        };
        let text = compose_screen_text(
            LifecycleScreen::LoadGame,
            &new_game,
            &load,
            Some(&message),
            None,
        );
        assert!(text.ends_with("Could not load world \"alpha\""));

        // Settings composes through compose_menu_text instead; Playing
        // renders no screen text at all.
        assert_eq!(
            compose_screen_text(
                LifecycleScreen::Settings(SettingsCaller::MainMenu),
                &new_game,
                &load,
                None,
                None
            ),
            String::new()
        );
        assert_eq!(
            compose_screen_text(LifecycleScreen::Playing, &new_game, &load, None, None),
            String::new()
        );
    }

    #[test]
    fn screen_panel_and_hovered_rects_follow_t01_geometry() {
        let (sw, sh) = (1280u32, 720u32);
        let empty = LoadScreen::new();
        // Every lifecycle screen's panel is its T01 panel (reflows by size).
        assert_eq!(
            screen_panel_rect(LifecycleScreen::MainMenu, &empty, sw, sh),
            main_menu_panel_rect(sw, sh)
        );
        assert_eq!(
            screen_panel_rect(LifecycleScreen::Pause, &empty, sw, sh),
            pause_panel_rect(sw, sh)
        );
        assert_eq!(
            screen_panel_rect(LifecycleScreen::NewGame, &empty, sw, sh),
            new_game_panel_rect(sw, sh)
        );
        assert_eq!(
            screen_panel_rect(LifecycleScreen::LoadGame, &empty, sw, sh),
            load_panel_rect(sw, empty.row_count())
        );

        // Hover highlight: the center of main-menu row 2 (Settings) resolves
        // to that row's rect; misses and no-cursor resolve to None.
        let (x, y, w, h) = main_menu_row_rect(sw, sh, 2).unwrap();
        assert_eq!(
            hovered_target_rect(
                LifecycleScreen::MainMenu,
                &empty,
                Some((x + w / 2.0, y + h / 2.0)),
                sw,
                sh
            ),
            main_menu_row_rect(sw, sh, 2)
        );
        assert_eq!(
            hovered_target_rect(LifecycleScreen::MainMenu, &empty, Some((0.0, 0.0)), sw, sh),
            None,
            "above the panel"
        );
        assert_eq!(
            hovered_target_rect(LifecycleScreen::MainMenu, &empty, None, sw, sh),
            None,
            "cursor outside the window"
        );

        // Load screen: confirm/cancel highlight only while a delete is
        // pending (their geometry always exists — pending state decides).
        let mut load = LoadScreen::new();
        load.set_rows(vec![CatalogRow::loadable(
            "a".into(),
            "a.mcvx".into(),
            Some(1),
        )]);
        let confirm = load_confirm_rect(sw, 1);
        let center = |rect: (f32, f32, f32, f32)| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        assert_eq!(
            hovered_target_rect(
                LifecycleScreen::LoadGame,
                &load,
                Some(center(confirm)),
                sw,
                sh
            ),
            None,
            "confirm highlight requires a pending delete"
        );
        assert!(load.request_delete(0));
        assert_eq!(
            hovered_target_rect(
                LifecycleScreen::LoadGame,
                &load,
                Some(center(confirm)),
                sw,
                sh
            ),
            Some(confirm)
        );
    }

    #[test]
    fn screen_title_tag_names_every_screen_and_nothing_in_gameplay() {
        assert_eq!(screen_title_tag(LifecycleScreen::Playing), "");
        assert_eq!(screen_title_tag(LifecycleScreen::MainMenu), " | Main Menu");
        assert_eq!(screen_title_tag(LifecycleScreen::NewGame), " | New Game");
        assert_eq!(screen_title_tag(LifecycleScreen::LoadGame), " | Load Game");
        assert_eq!(screen_title_tag(LifecycleScreen::Pause), " | Paused");
        assert_eq!(
            screen_title_tag(LifecycleScreen::Settings(SettingsCaller::Pause)),
            " | Settings"
        );
    }

    /// A valid sample world save for catalog-wiring tests (same shape
    /// the catalog_fs tests use).
    fn sample_save(seed: u32) -> WorldSaveData {
        WorldSaveData {
            seed,
            time_of_day: opencraft::world::daycycle::DEFAULT_TIME_OF_DAY,
            day_index: 0,
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            player: SavedPlayerState {
                position: [1.0, 64.0, -2.0],
                yaw: 0.5,
                pitch: -0.25,
                fly_mode: true,
                selected_slot: 2,
                render_distance: 16,
                inventory: Inventory::empty(),
                player_grid: CraftingGrid::new(GridKind::Player),
                table_grid: CraftingGrid::new(GridKind::Table),
            },
            chunk_edits: ChunkEditMap::default(),
        }
    }

    #[test]
    fn apply_confirmed_delete_refreshes_rows_and_surfaces_delete_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = WorldCatalogFs::new(dir.path());
        catalog
            .save_world("doomed", &sample_save(1))
            .expect("save doomed");
        catalog
            .save_world("spared", &sample_save(2))
            .expect("save spared");

        // Confirmed request deletes exactly the named file and the
        // refreshed rows drop it; success shows no message.
        let mut confirm = DeleteConfirmation::new();
        let doomed = catalog
            .list_rows()
            .into_iter()
            .find(|r| r.name == "doomed")
            .expect("doomed row listed");
        confirm.request(&doomed);
        let ConfirmOutcome::Confirmed(pending) = confirm.confirm() else {
            panic!("expected a confirmed delete");
        };
        let (rows, message) = apply_confirmed_delete(&catalog, &pending);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["spared"]);
        assert!(!dir.path().join("doomed.mcvx").exists());
        assert!(dir.path().join("spared.mcvx").is_file());
        assert!(message.is_none(), "a successful delete shows no message");

        // S01 idempotency: deleting the already-vanished file again is a
        // success — rows stay truthful, no error surfaced.
        let (rows, message) = apply_confirmed_delete(&catalog, &pending);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["spared"]);
        assert!(message.is_none(), "vanished-entry redelete is a success");

        // A delete failure surfaces inline as the screen message and the
        // still-present row stays listed. The real wiring only ever passes
        // machine-confirmed pendings (whose file names come from listed
        // *.mcvx rows), so the error arm is staged with an unsafe pending
        // directly — the adapter refuses it before touching the disk.
        let unsafe_pending = PendingDelete {
            name: "escape".to_string(),
            file_name: "../escape.mcvx".to_string(),
        };
        let (rows, message) = apply_confirmed_delete(&catalog, &unsafe_pending);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["spared"], "nothing was deleted");
        assert!(dir.path().join("spared.mcvx").is_file());
        let message = message.expect("delete failure surfaces inline");
        assert!(
            message.text.contains("Refusing to delete"),
            "unexpected delete-failure text: {}",
            message.text
        );
        assert_eq!(message.ticks_left, menu::MESSAGE_TICKS);
        assert!(!dir.path().parent().unwrap().join("escape.mcvx").exists());
    }

    #[test]
    fn refresh_interval_matches_common_display_rates() {
        assert_eq!(
            refresh_interval(60_000),
            Some(Duration::from_nanos(16_666_666)),
            "60 Hz is one frame every ~16.67 ms"
        );
        assert_eq!(
            refresh_interval(144_000),
            Some(Duration::from_nanos(6_944_444)),
            "144 Hz is one frame every ~6.94 ms"
        );
    }

    #[test]
    fn refresh_interval_rejects_missing_or_implausible_rates() {
        assert_eq!(refresh_interval(0), None);
        assert_eq!(refresh_interval(999), None, "below 1 Hz");
        assert_eq!(refresh_interval(1_000_001), None, "above 1000 Hz");
    }

    #[test]
    fn sky_params_probe_pins_dusk_midnight_and_dawn_srgb_values() {
        // These are the exact values the sky pass clears the sRGB surface with
        // (`LoadOp::Clear` of `clear_color`) and the fog uniform terrain fades
        // into. Dusk/dawn carry the warm DUSK/DAWN keyframe; midnight must be
        // the NIGHT keyframe, not a lingering dusk lerp (UAT-H2 / S07-T01).
        let cases = [
            (12_000u32, [0.96f32, 0.66, 0.42], [0.92f32, 0.68, 0.48]),
            (18_000u32, [0.035f32, 0.05, 0.12], [0.05f32, 0.07, 0.15]),
            (0u32, [0.96f32, 0.66, 0.42], [0.92f32, 0.68, 0.48]),
        ];
        for (tick, sky, fog) in cases {
            let params = sky_params(daycycle::sky_palette(tick), tick);
            assert_eq!(params.time_of_day, tick);
            assert_eq!(params.clear_color.r, sky[0] as f64);
            assert_eq!(params.clear_color.g, sky[1] as f64);
            assert_eq!(params.clear_color.b, sky[2] as f64);
            assert_eq!(params.clear_color.a, 1.0);
            assert_eq!(params.fog_color, [fog[0], fog[1], fog[2], 1.0]);
        }

        // The midnight clear/fog must equal the NIGHT keyframe exactly.
        let midnight = sky_params(daycycle::sky_palette(18_000), 18_000);
        let night = daycycle::sky_palette(18_000);
        assert_eq!(midnight.clear_color.r, night.sky_color[0] as f64);
        assert_eq!(midnight.clear_color.g, night.sky_color[1] as f64);
        assert_eq!(midnight.clear_color.b, night.sky_color[2] as f64);
        assert_eq!(
            midnight.fog_color,
            [
                night.fog_color[0],
                night.fog_color[1],
                night.fog_color[2],
                1.0
            ]
        );
        assert_eq!(midnight.day_factor, night.day_factor);
        assert_eq!(night.day_factor, daycycle::MOONLIT_FLOOR);
    }

    // -----------------------------------------------------------------------
    // S05-T02: stone-tier reachability acceptance
    // -----------------------------------------------------------------------

    /// The single inventory slot holding `id`, or a panic naming the absence.
    fn inventory_slot_of(app: &GameApp, id: ItemId) -> usize {
        app.session
            .player
            .inventory()
            .slots()
            .iter()
            .position(|slot| slot.map(|item| item.id()) == Some(id))
            .unwrap_or_else(|| panic!("expected {id:?} in the inventory"))
    }

    /// S05-T02 — the stone head material only exists because real gameplay
    /// makes it reachable. Six seeded oak logs are gathered through the
    /// production break path and crafted through the real inventory clicks
    /// (logs -> four 4-plank stacks -> table item -> sticks -> wood pickaxe at
    /// the placed 4x4 table). That pickaxe then mines a real Stone block: the
    /// exact 0.6s wood-adjusted duration is ticked to one `Broke`, the applied
    /// break spends exactly one durability point (59 -> 58), and the drop is
    /// the Cobblestone head material rather than a Stone item. The positive
    /// close crafts a stone shovel from that Cobblestone through the same click
    /// seams, and the negative proves a Stone item cannot stand in for the
    /// recipe's 'C' head cell.
    ///
    /// Seeding note: only the world fixture is seeded (the flat test chunk plus
    /// its face halo and the log/stone blocks, the same headless seam every
    /// mining/placement test uses), plus the four separate 4-plank stacks the
    /// 2x2 table recipe requires (the same documented, conservation-asserted
    /// seam S04 uses: the panel moves whole stacks and has no split gesture).
    /// Every other material transform below runs through production clicks.
    #[test]
    fn mining_stone_with_a_wood_pickaxe_yields_the_stone_head_material() {
        use opencraft::player::inventory::{Material, ToolKind, ToolTier, INVENTORY_SLOTS};

        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game
            .apply(TextInputAction::InsertText("stonehead".into()));
        app.new_game.cycle_focus();
        app.new_game
            .apply(TextInputAction::InsertText("4141".into()));
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);

        // 1. Gather six oak logs through the real break path.
        insert_test_chunk_with_face_neighbors(&mut app.session, BlockId::Air);
        let log_positions: Vec<IVec3> = (1..=6).map(|x| IVec3::new(x, 10, 0)).collect();
        for pos in &log_positions {
            app.session
                .world
                .set_block_immediate(pos.x, pos.y, pos.z, BlockId::OakLog);
        }
        for pos in &log_positions {
            let (remeshed, wear) =
                applied_break(break_mined_block(&mut app.session, *pos, BlockId::OakLog));
            assert!(!remeshed.is_empty(), "a real log break remeshes its chunk");
            assert_eq!(
                wear,
                ToolWearOutcome::Nothing,
                "bare hands cost no durability"
            );
        }
        assert_eq!(
            app.session.player.hotbar_stack(0),
            Some(ItemStack {
                block: BlockId::OakLog,
                count: 6,
            }),
            "the real gather path collected every log into the hotbar"
        );

        let mut screen = InventoryScreen::new();
        screen.open_player();
        let player_cell = |index: usize| InventoryTarget::GridCell {
            kind: GridKind::Player,
            index,
        };
        let table_cell = |index: usize| InventoryTarget::GridCell {
            kind: GridKind::Table,
            index,
        };

        // 2. Logs -> planks through real clicks, then the documented fixture
        // seam (S04 precedent): the panel moves whole stacks and exposes no
        // split gesture, so the four separate 4-plank stacks the 2x2 table
        // recipe requires are seeded through the same validated constructor
        // session restore uses. Conservation is asserted before and after;
        // every craft below is a real click.
        expect_moved(&mut app, &screen, InventoryTarget::Slot(0));
        expect_moved(&mut app, &screen, player_cell(0));
        for _ in 0..4 {
            let planks = expect_crafted(
                &mut app,
                &screen,
                GridKind::Player,
                ItemId::Block(BlockId::OakPlanks),
            );
            assert_eq!(planks.count(), 4);
        }
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
            16,
            "four real log crafts produce sixteen planks"
        );
        assert_eq!(inventory_count_of(&app, ItemId::Block(BlockId::OakLog)), 2);

        // Park the two leftover logs so the grid is empty before the seam.
        expect_moved(&mut app, &screen, player_cell(0));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(5));
        assert!(app.session.player.player_grid().is_empty());

        let planks_before = inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks));
        let logs_before = inventory_count_of(&app, ItemId::Block(BlockId::OakLog));
        assert_eq!((planks_before, logs_before), (16, 2));
        let mut seeded_slots = [None; INVENTORY_SLOTS];
        for slot in seeded_slots.iter_mut().take(4) {
            *slot = Some(Item::new(ItemId::Block(BlockId::OakPlanks), 4).expect("plank stack"));
        }
        seeded_slots[5] = Some(Item::new(ItemId::Block(BlockId::OakLog), 2).expect("log stack"));
        let position = app.session.player.position;
        let player_grid = app.session.player.player_grid().clone();
        let table_grid = app.session.player.table_grid().clone();
        app.session.player = Player::with_inventory(
            position,
            Inventory::from_parts(seeded_slots, None),
            player_grid,
            table_grid,
        );
        app.session.player.restore_selected_slot(0);
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
            planks_before,
            "seeding redistributed the crafted planks without changing totals"
        );
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakLog)),
            logs_before
        );

        // 3. Four plank stacks make the crafting-table item in the 2x2 grid.
        for slot in 0..4 {
            expect_moved(&mut app, &screen, InventoryTarget::Slot(slot));
            expect_moved(&mut app, &screen, player_cell(slot));
        }
        let table_item = expect_crafted(
            &mut app,
            &screen,
            GridKind::Player,
            ItemId::Block(BlockId::CraftingTable),
        );
        assert_eq!(table_item.count(), 1);
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
            12,
            "the table recipe consumed exactly four planks"
        );

        // 4. Park two spare plank stacks so the 2x2 grid is exactly the
        // vertical stick pair, then run two real stick crafts. The first batch
        // rides the cursor so the second batch cannot merge into it.
        expect_moved(&mut app, &screen, player_cell(1));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(1));
        expect_moved(&mut app, &screen, player_cell(3));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(2));
        let sticks = expect_crafted(
            &mut app,
            &screen,
            GridKind::Player,
            ItemId::Material(Material::Stick),
        );
        assert_eq!(sticks.count(), 4);
        expect_moved(&mut app, &screen, InventoryTarget::Slot(3));
        let sticks = expect_crafted(
            &mut app,
            &screen,
            GridKind::Player,
            ItemId::Material(Material::Stick),
        );
        assert_eq!(sticks.count(), 4);
        expect_moved(&mut app, &screen, InventoryTarget::Slot(4));

        // Empty the 2x2 grid: both stick batches and the pickaxe row now live
        // in the inventory / table grid.
        expect_moved(&mut app, &screen, player_cell(0));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(6));
        expect_moved(&mut app, &screen, player_cell(2));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(7));
        assert!(
            app.session.player.player_grid().is_empty(),
            "the 2x2 grid is cleared before the table opens"
        );

        // 5. Place the crafted table through the production right-click path
        // and open it through the same path.
        assert!(app.session.player.select_slot(0));
        assert_eq!(
            app.session.player.selected_block(),
            Some(BlockId::CraftingTable)
        );
        app.session.player.position = Vec3::new(5.5, 10.0, 5.5);
        assert!(
            !app.session
                .world
                .set_block_immediate(0, 5, 0, BlockId::Stone)
                .is_empty(),
            "the placement support block remeshes"
        );
        aim_placement_camera(&mut app.session);
        match right_click_interaction(&mut app.session) {
            RightClickOutcome::Placed(remeshed) => {
                assert!(!remeshed.is_empty(), "placement remeshes its chunk")
            }
            _ => panic!("the targeted stone face must accept the table placement"),
        }
        let table_pos = IVec3::new(0, 6, 0);
        assert_eq!(
            app.session.world.get_block(0, 6, 0),
            BlockId::CraftingTable,
            "the placed table is a real world block"
        );
        assert!(
            app.session.player.hotbar_stack(0).is_none(),
            "placement consumed the crafted table"
        );
        aim_table_interaction_at(&mut app.session, table_pos);
        assert!(
            matches!(
                right_click_interaction(&mut app.session),
                RightClickOutcome::TableOpened
            ),
            "the real right-click path opens the placed table"
        );
        assert_eq!(app.session.open_table_position(), Some(table_pos));
        screen.close();
        screen.open_table();
        assert_eq!(
            screen.mode(app.session.open_table_position()),
            InventoryMode::Table
        );

        // 6. Craft the wood pickaxe in the 4x4 table grid: three plank cells
        // in the head row and two separate stick stacks down the middle.
        for (from, to) in [(1usize, 0usize), (2, 1), (6, 2)] {
            expect_moved(&mut app, &screen, InventoryTarget::Slot(from));
            expect_moved(&mut app, &screen, table_cell(to));
        }
        for (from, to) in [(3usize, 5usize), (4, 9)] {
            expect_moved(&mut app, &screen, InventoryTarget::Slot(from));
            expect_moved(&mut app, &screen, table_cell(to));
        }
        let wood_pickaxe = ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Wood,
        };
        let pickaxe = expect_crafted(&mut app, &screen, GridKind::Table, wood_pickaxe);
        assert_eq!(pickaxe.durability(), Some(59));
        assert_eq!(inventory_count_of(&app, wood_pickaxe), 1);
        let pickaxe_slot = inventory_slot_of(&app, wood_pickaxe);
        assert!(app.session.player.select_slot(pickaxe_slot));
        assert_eq!(
            app.session.player.selected_item().map(|item| item.id()),
            Some(wood_pickaxe)
        );

        // 7. Mine a real Stone block with the crafted wood pickaxe. The target
        // carries the exact tier-adjusted duration, the accumulator emits one
        // Broke, and the applied break spends exactly one durability point
        // before collecting the Cobblestone head material.
        let stone_pos = IVec3::new(2, 8, 0);
        assert!(
            !app.session
                .world
                .set_block_immediate(2, 8, 0, BlockId::Stone)
                .is_empty(),
            "the stone target remeshes"
        );
        let target = MiningTarget::from_block(
            stone_pos,
            BlockId::Stone,
            app.session.player.selected_item(),
        )
        .expect("a wood pickaxe can mine stone");
        assert_eq!(target.duration(), 0.6, "1.2s base / 2x wood tier");
        let mut mining = MiningState::new();
        let mut broke_after = None;
        for tick in 1..=20usize {
            match mining.tick(0.1, Some(target)) {
                MiningTick::Mining { .. } => {}
                MiningTick::Broke { pos, block } => {
                    assert_eq!((pos, block), (stone_pos, BlockId::Stone));
                    broke_after = Some(tick);
                    break;
                }
                MiningTick::Idle => panic!("a held stone target must not go idle"),
            }
        }
        let broke_after = broke_after.expect("stone must break after the wood-pickaxe hold");
        assert!(
            broke_after.abs_diff(6) <= 1,
            "broke after {broke_after} ticks, expected 6"
        );
        let (remeshed, wear) = applied_break(break_mined_block(
            &mut app.session,
            stone_pos,
            BlockId::Stone,
        ));
        assert!(!remeshed.is_empty(), "the mining break remeshes");
        assert_eq!(
            app.session
                .world
                .get_block(stone_pos.x, stone_pos.y, stone_pos.z),
            BlockId::Air
        );
        assert_eq!(
            wear,
            ToolWearOutcome::Worn { remaining: 58 },
            "59 max, exactly one point per applied break"
        );
        assert_eq!(
            app.session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(58)
        );
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::Cobblestone)),
            1,
            "Stone yields the Cobblestone head material"
        );
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::Stone)),
            0,
            "the break path must not collect a Stone item"
        );

        // 8. Positive close: the collected Cobblestone crafts a stone tool.
        // Clear the pickaxe leftovers out of the 4x4 grid, move the two stick
        // stacks into the 1-wide shovel column, and use the real click seams.
        // The pickaxe craft consumed the third head stack entirely, so only
        // cells 0 and 1 still hold leftover planks; clear them out of the grid.
        for (from, to) in [(0usize, 2usize), (1, 3)] {
            expect_moved(&mut app, &screen, table_cell(from));
            expect_moved(&mut app, &screen, InventoryTarget::Slot(to));
        }
        expect_moved(&mut app, &screen, table_cell(5));
        expect_moved(&mut app, &screen, table_cell(4));
        expect_moved(&mut app, &screen, table_cell(9));
        expect_moved(&mut app, &screen, table_cell(8));
        let cobblestone_slot = inventory_slot_of(&app, ItemId::Block(BlockId::Cobblestone));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(cobblestone_slot));
        expect_moved(&mut app, &screen, table_cell(0));
        let stone_shovel = ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Stone,
        };
        let shovel = expect_crafted(&mut app, &screen, GridKind::Table, stone_shovel);
        assert_eq!(shovel.durability(), Some(131));
        assert_eq!(inventory_count_of(&app, stone_shovel), 1);

        // 9. Negative: a Stone item is not the 'C' head material. The same
        // shovel-shaped grid with Stone in the head cell must be rejected as
        // no recipe and mutate neither the grid nor the inventory.
        assert!(
            app.session.player.collect_block(BlockId::Stone).is_some(),
            "a Stone item stack is seeded through the inventory command"
        );
        let stone_slot = inventory_slot_of(&app, ItemId::Block(BlockId::Stone));
        expect_moved(&mut app, &screen, InventoryTarget::Slot(stone_slot));
        expect_moved(&mut app, &screen, table_cell(0));
        let inventory_before = app.session.player.inventory().clone();
        let table_before = app.session.player.table_grid().clone();
        assert_eq!(
            click_inventory(
                &mut app,
                &screen,
                InventoryTarget::ResultSlot(GridKind::Table)
            ),
            Err(InventoryCommandError::Craft(CraftError::NoRecipe)),
            "Stone cannot satisfy the cobblestone head cell"
        );
        assert_eq!(
            app.session.player.inventory(),
            &inventory_before,
            "a rejected craft mutates nothing"
        );
        assert_eq!(
            app.session.player.table_grid(),
            &table_before,
            "the wrong head stays in the grid"
        );
        assert_eq!(inventory_count_of(&app, ItemId::Block(BlockId::Stone)), 1);
        assert_eq!(
            inventory_count_of(&app, ItemId::Block(BlockId::Cobblestone)),
            0,
            "the crafted shovel consumed the only Cobblestone"
        );
    }

    // -----------------------------------------------------------------------
    // S05-T03: all-nine-tool matrix and the break-at-zero generalization
    // -----------------------------------------------------------------------

    use opencraft::player::inventory::{
        Material, ToolKind, ToolTier, INVENTORY_SLOTS, MAX_STACK_COUNT,
    };

    /// Table-grid cells the head row of `kind` occupies.
    fn tool_head_cells(kind: ToolKind) -> &'static [usize] {
        match kind {
            ToolKind::Pickaxe => &[0, 1, 2],
            ToolKind::Axe => &[0, 1, 4],
            ToolKind::Shovel => &[0],
        }
    }

    /// Table-grid cells the two Stick stacks occupy for `kind`.
    fn tool_stick_cells(kind: ToolKind) -> &'static [usize] {
        match kind {
            ToolKind::Pickaxe | ToolKind::Axe => &[5, 9],
            ToolKind::Shovel => &[4, 8],
        }
    }

    /// One headless named world with a placed, open 4x4 crafting table and the
    /// material stock the S04/S05 chain produces: eight Oak Planks, two
    /// separate 4-Stick stacks, and two leftover logs.
    ///
    /// Gathering, every craft, placement, table open, break, wear point, and
    /// drop in [`ToolLab::new`] runs through production seams: the real break
    /// path, real inventory clicks, and the real placement/table-open
    /// interactions. One fixture step does not: the 2x2 table recipe needs four
    /// same-identity cells and the panel moves whole stacks with no split
    /// gesture, so the sixteen crafted planks are redistributed through the
    /// documented, conservation-asserted `Player::with_inventory` seam S04/S05
    /// established (totals asserted unchanged).
    struct ToolLab {
        /// Keeps the tempdir world catalog alive for the app's lifetime.
        _dir: tempfile::TempDir,
        app: GameApp,
        screen: InventoryScreen,
    }

    impl ToolLab {
        fn player_cell(index: usize) -> InventoryTarget {
            InventoryTarget::GridCell {
                kind: GridKind::Player,
                index,
            }
        }

        fn table_cell(index: usize) -> InventoryTarget {
            InventoryTarget::GridCell {
                kind: GridKind::Table,
                index,
            }
        }

        /// Two production clicks: pick the whole stack up, put it down.
        fn move_stack(&mut self, from: InventoryTarget, to: InventoryTarget) {
            expect_moved(&mut self.app, &self.screen, from);
            expect_moved(&mut self.app, &self.screen, to);
        }

        fn craft(&mut self, kind: GridKind, id: ItemId) -> Item {
            expect_crafted(&mut self.app, &self.screen, kind, id)
        }

        fn count(&self, id: ItemId) -> u32 {
            inventory_count_of(&self.app, id)
        }

        fn slot_of(&self, id: ItemId) -> usize {
            inventory_slot_of(&self.app, id)
        }

        /// The first empty inventory slot: where a parked stack can land
        /// without displacing anything already held.
        fn empty_slot(&self) -> usize {
            self.app
                .session
                .player
                .inventory()
                .slots()
                .iter()
                .position(Option::is_none)
                .expect("an empty inventory slot must exist")
        }

        /// Seeds `block` at `pos` and drives the real mining accumulator with
        /// the selected item to exactly one `Broke`, then applies the
        /// production break. Asserts the exact tier-adjusted duration, the
        /// tick count the duration implies, and that the cell became Air.
        fn mine(&mut self, pos: IVec3, block: BlockId, expected_duration: f32) -> ToolWearOutcome {
            assert!(
                !self
                    .app
                    .session
                    .world
                    .set_block_immediate(pos.x, pos.y, pos.z, block)
                    .is_empty(),
                "seeding {block:?} at {pos:?} must remesh"
            );
            let selected = self.app.session.player.selected_item();
            let target = MiningTarget::from_block(pos, block, selected)
                .unwrap_or_else(|| panic!("the selected item must be able to target {block:?}"));
            assert!(
                (target.duration() - expected_duration).abs() < 1e-6,
                "{block:?} should take {expected_duration}s but the target says {}s",
                target.duration()
            );
            let mut mining = MiningState::new();
            let mut broke_after = None;
            for tick in 1..=64usize {
                match mining.tick(0.1, Some(target)) {
                    MiningTick::Mining { .. } => {}
                    MiningTick::Broke {
                        pos: at,
                        block: broken,
                    } => {
                        assert_eq!(
                            (at, broken),
                            (pos, block),
                            "the accumulator broke the wrong target"
                        );
                        broke_after = Some(tick);
                        break;
                    }
                    MiningTick::Idle => panic!("a held {block:?} target must not go idle"),
                }
            }
            let broke_after = broke_after.expect("the held target must break");
            let expected_ticks = (expected_duration / 0.1).ceil() as usize;
            assert!(
                broke_after.abs_diff(expected_ticks) <= 1,
                "{block:?} broke after {broke_after} ticks, expected {expected_ticks}"
            );
            let (remeshed, wear) =
                applied_break(break_mined_block(&mut self.app.session, pos, block));
            assert!(!remeshed.is_empty(), "the mining break must remesh");
            assert_eq!(
                self.app.session.world.get_block(pos.x, pos.y, pos.z),
                BlockId::Air,
                "the applied break clears the cell"
            );
            wear
        }

        /// Crafts the wood pickaxe from the lab's plank/stick stock through real
        /// clicks and selects it, then clears the leftover cells so the next
        /// craft starts from an empty 4x4 grid. Returns the pickaxe identity and
        /// the hotbar slot holding it.
        fn craft_wood_pickaxe(&mut self) -> (ItemId, usize) {
            for cell in [0usize, 1, 2] {
                let slot = self.slot_of(ItemId::Block(BlockId::OakPlanks));
                self.move_stack(InventoryTarget::Slot(slot), Self::table_cell(cell));
            }
            for cell in [5usize, 9] {
                let slot = self.slot_of(ItemId::Material(Material::Stick));
                self.move_stack(InventoryTarget::Slot(slot), Self::table_cell(cell));
            }
            let wood_pickaxe = ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            };
            let pickaxe = self.craft(GridKind::Table, wood_pickaxe);
            assert_eq!(pickaxe.durability(), Some(59));
            let slot = self.slot_of(wood_pickaxe);
            assert!(self.app.session.player.select_slot(slot));
            assert_eq!(
                self.app
                    .session
                    .player
                    .selected_item()
                    .map(|item| item.id()),
                Some(wood_pickaxe)
            );
            assert_eq!(
                self.count(ItemId::Block(BlockId::OakPlanks)),
                5,
                "the wood pickaxe consumed three planks"
            );
            assert_eq!(
                self.count(ItemId::Material(Material::Stick)),
                6,
                "the wood pickaxe consumed two sticks"
            );
            for (cell, slot) in [(0usize, 1usize), (1, 2), (5, 3), (9, 4)] {
                self.move_stack(Self::table_cell(cell), InventoryTarget::Slot(slot));
            }
            (wood_pickaxe, slot)
        }

        /// Mines `count` Stone blocks with the selected wood pickaxe through the
        /// real accumulator, parking each Cobblestone drop as a one-item stack in
        /// its own 2x2 cell so the 3-wide stone head rows get separate stacks
        /// without any split gesture.
        fn mine_stone_heads(&mut self, count: usize) {
            for (mined, cell) in (0..count).enumerate() {
                let pos = IVec3::new(3 + mined as i32, 8, 0);
                let wear = self.mine(pos, BlockId::Stone, 0.6);
                assert_eq!(
                    wear,
                    ToolWearOutcome::Worn {
                        remaining: 59 - 1 - mined as u16
                    },
                    "the crafted wood pickaxe spends exactly one point per stone break"
                );
                let slot = self.slot_of(ItemId::Block(BlockId::Cobblestone));
                self.move_stack(InventoryTarget::Slot(slot), Self::player_cell(cell));
            }
            assert_eq!(self.count(ItemId::Block(BlockId::Stone)), 0);
            assert_eq!(
                self.count(ItemId::Block(BlockId::Cobblestone)),
                count as u32,
                "every mined Stone yielded exactly one Cobblestone"
            );
        }

        fn new(name: &str) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
            app.new_game.apply(TextInputAction::InsertText(name.into()));
            app.new_game.cycle_focus();
            app.new_game
                .apply(TextInputAction::InsertText("4141".into()));
            app.attempt_create();
            assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);

            // World fixture: the flat test chunk plus its face halo (the same
            // headless seam every mining/placement test uses) and six seeded
            // oak logs gathered through the real break path.
            insert_test_chunk_with_face_neighbors(&mut app.session, BlockId::Air);
            for x in 1..=6 {
                assert!(
                    !app.session
                        .world
                        .set_block_immediate(x, 10, 0, BlockId::OakLog)
                        .is_empty(),
                    "seeding each log remeshes"
                );
            }
            for x in 1..=6 {
                let (remeshed, wear) = applied_break(break_mined_block(
                    &mut app.session,
                    IVec3::new(x, 10, 0),
                    BlockId::OakLog,
                ));
                assert!(!remeshed.is_empty(), "a real log break remeshes");
                assert_eq!(
                    wear,
                    ToolWearOutcome::Nothing,
                    "bare hands cost no durability"
                );
            }
            assert_eq!(
                app.session
                    .player
                    .hotbar_stack(0)
                    .map(|stack| (stack.block, stack.count)),
                Some((BlockId::OakLog, 6)),
                "the real gather path collected every log into the hotbar"
            );

            let mut screen = InventoryScreen::new();
            screen.open_player();

            // Logs into the 2x2 grid and four real plank crafts. The panel
            // moves whole stacks and exposes no split gesture, so the four
            // separate 4-plank stacks the table recipe needs cannot be produced
            // by clicks: redistribute the sixteen crafted planks through the
            // same documented, conservation-asserted seam S04/S05 use.
            expect_moved(&mut app, &screen, InventoryTarget::Slot(0));
            expect_moved(&mut app, &screen, Self::player_cell(0));
            for _ in 0..4 {
                let planks = expect_crafted(
                    &mut app,
                    &screen,
                    GridKind::Player,
                    ItemId::Block(BlockId::OakPlanks),
                );
                assert_eq!(planks.count(), 4);
            }
            assert_eq!(
                inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
                16,
                "four real log crafts produce sixteen planks"
            );
            assert_eq!(inventory_count_of(&app, ItemId::Block(BlockId::OakLog)), 2);
            // Park the two leftover logs so the grid is empty before the seam.
            expect_moved(&mut app, &screen, Self::player_cell(0));
            expect_moved(&mut app, &screen, InventoryTarget::Slot(5));
            assert!(app.session.player.player_grid().is_empty());

            let planks_before = inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks));
            let logs_before = inventory_count_of(&app, ItemId::Block(BlockId::OakLog));
            let mut seeded_slots = [None; INVENTORY_SLOTS];
            for slot in seeded_slots.iter_mut().take(4) {
                *slot = Some(Item::new(ItemId::Block(BlockId::OakPlanks), 4).expect("plank stack"));
            }
            seeded_slots[5] =
                Some(Item::new(ItemId::Block(BlockId::OakLog), 2).expect("log stack"));
            let position = app.session.player.position;
            let player_grid = app.session.player.player_grid().clone();
            let table_grid = app.session.player.table_grid().clone();
            app.session.player = Player::with_inventory(
                position,
                Inventory::from_parts(seeded_slots, None),
                player_grid,
                table_grid,
            );
            app.session.player.restore_selected_slot(0);
            assert_eq!(
                inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
                planks_before,
                "seeding redistributed the crafted planks without changing totals"
            );
            assert_eq!(
                inventory_count_of(&app, ItemId::Block(BlockId::OakLog)),
                logs_before
            );

            // Four plank stacks make the crafting-table item in the 2x2 grid.
            for slot in 0..4 {
                expect_moved(&mut app, &screen, InventoryTarget::Slot(slot));
                expect_moved(&mut app, &screen, Self::player_cell(slot));
            }
            let table = expect_crafted(
                &mut app,
                &screen,
                GridKind::Player,
                ItemId::Block(BlockId::CraftingTable),
            );
            assert_eq!(table.count(), 1);

            // Two separate Stick stacks from the vertical plank pair.
            expect_moved(&mut app, &screen, Self::player_cell(1));
            expect_moved(&mut app, &screen, InventoryTarget::Slot(1));
            expect_moved(&mut app, &screen, Self::player_cell(3));
            expect_moved(&mut app, &screen, InventoryTarget::Slot(2));
            let sticks = expect_crafted(
                &mut app,
                &screen,
                GridKind::Player,
                ItemId::Material(Material::Stick),
            );
            assert_eq!(sticks.count(), 4);
            expect_moved(&mut app, &screen, InventoryTarget::Slot(3));
            let sticks = expect_crafted(
                &mut app,
                &screen,
                GridKind::Player,
                ItemId::Material(Material::Stick),
            );
            assert_eq!(sticks.count(), 4);
            expect_moved(&mut app, &screen, InventoryTarget::Slot(4));
            expect_moved(&mut app, &screen, Self::player_cell(0));
            expect_moved(&mut app, &screen, InventoryTarget::Slot(6));
            expect_moved(&mut app, &screen, Self::player_cell(2));
            expect_moved(&mut app, &screen, InventoryTarget::Slot(7));
            assert!(
                app.session.player.player_grid().is_empty(),
                "the 2x2 grid is cleared before the table opens"
            );
            assert_eq!(
                inventory_count_of(&app, ItemId::Block(BlockId::OakPlanks)),
                8,
                "the whole setup produced eight planks"
            );
            assert_eq!(
                inventory_count_of(&app, ItemId::Material(Material::Stick)),
                8,
                "the whole setup produced eight sticks"
            );
            assert_eq!(
                inventory_count_of(&app, ItemId::Block(BlockId::OakLog)),
                2,
                "the whole setup left two spare logs"
            );

            // Place the crafted table through the production right-click path
            // and open it through the same path.
            assert!(app.session.player.select_slot(0));
            assert_eq!(
                app.session.player.selected_block(),
                Some(BlockId::CraftingTable)
            );
            app.session.player.position = Vec3::new(5.5, 10.0, 5.5);
            assert!(
                !app.session
                    .world
                    .set_block_immediate(0, 5, 0, BlockId::Stone)
                    .is_empty(),
                "the placement support block remeshes"
            );
            aim_placement_camera(&mut app.session);
            match right_click_interaction(&mut app.session) {
                RightClickOutcome::Placed(remeshed) => {
                    assert!(!remeshed.is_empty(), "placement remeshes its chunk")
                }
                _ => panic!("the targeted stone face must accept the table placement"),
            }
            let table_pos = IVec3::new(0, 6, 0);
            assert_eq!(
                app.session.world.get_block(0, 6, 0),
                BlockId::CraftingTable,
                "the placed table is a real world block"
            );
            assert!(
                app.session.player.hotbar_stack(0).is_none(),
                "placement consumed the crafted table"
            );
            aim_table_interaction_at(&mut app.session, table_pos);
            assert!(
                matches!(
                    right_click_interaction(&mut app.session),
                    RightClickOutcome::TableOpened
                ),
                "the real right-click path opens the placed table"
            );
            assert_eq!(app.session.open_table_position(), Some(table_pos));
            screen.close();
            screen.open_table();
            assert_eq!(
                screen.mode(app.session.open_table_position()),
                InventoryMode::Table
            );

            Self {
                _dir: dir,
                app,
                screen,
            }
        }
    }

    /// S05-T03 — the milestone's full tool matrix, end to end. Each of the nine
    /// tier x kind combinations gets a fresh named world and is crafted and used
    /// through production seams: the head material is gathered by real breaks
    /// (wood = seeded oak logs, stone = a crafted wood pickaxe mining Stone,
    /// iron = seeded ore broken bare-handed), the tool is crafted at the placed
    /// 4x4 table with real inventory clicks, and one class-matching block is
    /// mined through the real accumulator to prove the exact tier-adjusted
    /// duration, exactly one durability point against 59/131/250 maxima, and
    /// the expected drop. Break-at-zero beyond the wood tier and the recraft
    /// live in `stone_tool_breaks_at_zero_and_is_recrafted_through_the_real_break_path`.
    ///
    /// Seeding notes: the flat chunk + face halo and the seeded log/stone/ore
    /// blocks are the same headless world fixture every mining test uses, and
    /// only the four separate 4-plank stacks are redistributed through the
    /// documented, conservation-asserted S04/S05 seam (the panel has no split
    /// gesture). Every craft, placement, table open, break, wear point, and drop
    /// runs through production code.
    #[test]
    fn all_nine_tools_craft_and_wear_exactly_once_on_matching_blocks() {
        let combinations = [
            (ToolTier::Wood, ToolKind::Pickaxe),
            (ToolTier::Wood, ToolKind::Axe),
            (ToolTier::Wood, ToolKind::Shovel),
            (ToolTier::Stone, ToolKind::Pickaxe),
            (ToolTier::Stone, ToolKind::Axe),
            (ToolTier::Stone, ToolKind::Shovel),
            (ToolTier::Iron, ToolKind::Pickaxe),
            (ToolTier::Iron, ToolKind::Axe),
            (ToolTier::Iron, ToolKind::Shovel),
        ];

        for (tier, kind) in combinations {
            let name = format!("matrix-{tier:?}-{kind:?}");
            let mut lab = ToolLab::new(&name);
            let head_count = tool_head_cells(kind).len();

            // 1. Reach the head material through real breaks. Wood planks are
            // already separate stacks; stone and iron heads are gathered one
            // unit at a time and parked one per 2x2 cell so the 3-wide head
            // rows can be filled with separate whole stacks.
            match tier {
                ToolTier::Wood => {
                    for cell in 0..head_count {
                        let slot = lab.slot_of(ItemId::Block(BlockId::OakPlanks));
                        lab.move_stack(InventoryTarget::Slot(slot), ToolLab::player_cell(cell));
                    }
                    assert_eq!(
                        lab.count(ItemId::Block(BlockId::OakPlanks)),
                        8,
                        "parking the plank heads conserves every plank"
                    );
                }
                ToolTier::Stone => {
                    // Stone-tier heads only exist because a crafted wood pickaxe
                    // mines Stone; build that pickaxe through real clicks first.
                    // The shovel pass needs one spare Cobblestone for its
                    // break-at-zero recraft; every other stone tool mines exactly
                    // the head row it spends.
                    let (_, _) = lab.craft_wood_pickaxe();
                    let needed = if kind == ToolKind::Shovel {
                        head_count + 1
                    } else {
                        head_count
                    };
                    lab.mine_stone_heads(needed);
                }
                ToolTier::Iron => {
                    assert!(
                        lab.app.session.player.selected_item().is_none(),
                        "iron ore is gathered bare-handed"
                    );
                    for cell in 0..head_count {
                        let pos = IVec3::new(3 + cell as i32, 8, 0);
                        let wear = lab.mine(pos, BlockId::IronOre, 2.2);
                        assert_eq!(
                            wear,
                            ToolWearOutcome::Nothing,
                            "bare hands break iron ore without durability"
                        );
                        let ore_slot = lab.slot_of(ItemId::Block(BlockId::IronOre));
                        lab.move_stack(InventoryTarget::Slot(ore_slot), ToolLab::player_cell(cell));
                    }
                    assert_eq!(
                        lab.count(ItemId::Block(BlockId::IronOre)),
                        head_count as u32,
                        "every mined ore is conserved"
                    );
                }
            }

            // 2. Craft the tool in the open 4x4 table from the separated head
            // stacks plus two whole Stick stacks.
            for (index, cell) in tool_head_cells(kind).iter().enumerate() {
                lab.move_stack(ToolLab::player_cell(index), ToolLab::table_cell(*cell));
            }
            for cell in tool_stick_cells(kind) {
                let slot = lab.slot_of(ItemId::Material(Material::Stick));
                lab.move_stack(InventoryTarget::Slot(slot), ToolLab::table_cell(*cell));
            }
            let tool_id = ItemId::Tool { kind, tier };
            let tool = lab.craft(GridKind::Table, tool_id);
            assert_eq!(
                tool.durability(),
                Some(tier.max_durability()),
                "a fresh craft is full durability"
            );
            assert_eq!(lab.count(tool_id), 1);
            let tool_slot = lab.slot_of(tool_id);
            assert!(
                lab.app.session.player.select_slot(tool_slot),
                "the crafted tool lands in the hotbar"
            );
            assert_eq!(
                lab.app.session.player.selected_item().map(|item| item.id()),
                Some(tool_id)
            );

            // 3. Negative surfaces: a tool outside its class never accelerates,
            // and structurally unbreakable blocks never become targets.
            let wrong_block = match kind {
                ToolKind::Pickaxe => BlockId::Dirt,
                ToolKind::Axe => BlockId::Stone,
                ToolKind::Shovel => BlockId::OakLog,
            };
            let wrong = MiningTarget::from_block(IVec3::new(7, 8, 0), wrong_block, Some(tool))
                .expect("a mismatched block is still breakable");
            assert_eq!(
                wrong.duration(),
                wrong_block
                    .break_duration_secs()
                    .expect("a mismatched block keeps its base hardness"),
                "a mismatched tool class must not shorten the break"
            );
            for unbreakable in [BlockId::Air, BlockId::Bedrock] {
                assert!(
                    MiningTarget::from_block(IVec3::new(7, 8, 0), unbreakable, Some(tool))
                        .is_none(),
                    "{unbreakable:?} must never become a target, even with a tool"
                );
            }

            // 4. One class-matching break: the exact tier-adjusted duration,
            // exactly one durability point, and the expected drop.
            let (target_block, base_duration, expected_duration) = match kind {
                ToolKind::Pickaxe => (
                    BlockId::Stone,
                    1.2,
                    match tier {
                        ToolTier::Wood => 0.6,
                        ToolTier::Stone => 0.3,
                        ToolTier::Iron => 0.2,
                    },
                ),
                ToolKind::Axe => (
                    BlockId::OakLog,
                    0.9,
                    match tier {
                        ToolTier::Wood => 0.45,
                        ToolTier::Stone => 0.225,
                        ToolTier::Iron => 0.15,
                    },
                ),
                ToolKind::Shovel => (
                    BlockId::Dirt,
                    0.45,
                    match tier {
                        ToolTier::Wood => 0.225,
                        ToolTier::Stone => 0.1125,
                        ToolTier::Iron => 0.075,
                    },
                ),
            };
            assert_eq!(target_block.break_duration_secs(), Some(base_duration));
            let drop_id = ItemId::Block(target_block.mined_drop());
            let drops_before = lab.count(drop_id);
            let wear = lab.mine(IVec3::new(6, 8, 0), target_block, expected_duration);
            assert_eq!(
                wear,
                ToolWearOutcome::Worn {
                    remaining: tier.max_durability() - 1
                },
                "{tier:?} {kind:?} spends exactly one point per applied break"
            );
            assert_eq!(lab.count(tool_id), 1, "one break leaves the tool held");
            assert_eq!(
                lab.count(drop_id),
                drops_before + 1,
                "the matching break yields {drop_id:?}"
            );
            assert_eq!(
                lab.app
                    .session
                    .player
                    .selected_item()
                    .and_then(|item| item.durability()),
                Some(tier.max_durability() - 1)
            );
        }
    }

    /// S05-T03 (break-at-zero half) — a stone-tier tool is driven to its last
    /// durability point through real Dirt breaks, the final applied break frees
    /// its hotbar slot *before* the drop is collected (MEM195 ordering), and a
    /// full-durability replacement is recrafted from the remaining grid
    /// materials through the real craft path. S04 proved this for the wood tier;
    /// this proves the same semantics one tier up, where the maxima are not the
    /// ones the wood chain exercised.
    ///
    /// Seeding notes: the world fixture and the four-way plank redistribution are
    /// the same documented seams as the matrix test. The near-full inventory at
    /// the final break is built through the documented
    /// `Player::with_inventory`/`Inventory::from_parts` seam S04/S05 established:
    /// only a saturated inventory can prove the final drop had exactly one home
    /// (the slot the break frees). The worn tool, its durability, and both
    /// recraft material stacks are asserted unchanged across that seam.
    #[test]
    fn stone_tool_breaks_at_zero_and_is_recrafted_through_the_real_break_path() {
        let mut lab = ToolLab::new("break-at-zero");

        // Stone-tier chain: crafted wood pickaxe -> two real Stone breaks (one
        // head, one spare for the recraft) -> stone shovel at the open table.
        let (_, _) = lab.craft_wood_pickaxe();
        lab.mine_stone_heads(2);
        lab.move_stack(ToolLab::player_cell(0), ToolLab::table_cell(0));
        for cell in [4usize, 8] {
            let slot = lab.slot_of(ItemId::Material(Material::Stick));
            lab.move_stack(InventoryTarget::Slot(slot), ToolLab::table_cell(cell));
        }
        let shovel_id = ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Stone,
        };
        let shovel = lab.craft(GridKind::Table, shovel_id);
        assert_eq!(
            shovel.durability(),
            Some(131),
            "a fresh stone shovel is full"
        );
        let tool_slot = lab.slot_of(shovel_id);
        assert!(lab.app.session.player.select_slot(tool_slot));

        // Spend the first point on a real Dirt break, then every point but the
        // last. Each applied break yields its drop.
        let wear = lab.mine(IVec3::new(7, 8, 0), BlockId::Dirt, 0.1125);
        assert_eq!(
            wear,
            ToolWearOutcome::Worn { remaining: 130 },
            "one applied break spends exactly one point"
        );

        // Pull the leftover Stick stacks out of the table grid so the recraft
        // starts from an empty grid, into genuinely empty slots (the worn tool
        // itself may sit where a stick stack used to be).
        let slot = lab.empty_slot();
        lab.move_stack(ToolLab::table_cell(4), InventoryTarget::Slot(slot));
        let slot = lab.empty_slot();
        lab.move_stack(ToolLab::table_cell(8), InventoryTarget::Slot(slot));
        assert_eq!(
            lab.slot_of(shovel_id),
            tool_slot,
            "moving the leftovers must not displace the worn tool"
        );

        let mut remaining = 130u16;
        while remaining > 1 {
            let wear = lab.mine(IVec3::new(7, 8, 0), BlockId::Dirt, 0.1125);
            remaining -= 1;
            assert_eq!(
                wear,
                ToolWearOutcome::Worn { remaining },
                "each applied break spends exactly one point"
            );
        }
        assert_eq!(remaining, 1, "the stone shovel is on its last point");
        assert_eq!(
            lab.count(ItemId::Block(BlockId::Dirt)),
            130,
            "every real Dirt break yielded its drop"
        );
        assert_eq!(
            lab.app
                .session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(1)
        );

        // Saturate the inventory through the documented S04/S05 seam so the
        // final drop has exactly one home: the slot the break frees.
        let position = lab.app.session.player.position;
        let player_grid = lab.app.session.player.player_grid().clone();
        let table_grid = lab.app.session.player.table_grid().clone();
        let cobble_before = lab.count(ItemId::Block(BlockId::Cobblestone));
        let sticks_before = lab.count(ItemId::Material(Material::Stick));
        let mut slots = [None; INVENTORY_SLOTS];
        for (slot, item) in slots
            .iter_mut()
            .zip(lab.app.session.player.inventory().slots())
        {
            *slot = match item {
                Some(item) if item.id() == ItemId::Block(BlockId::Dirt) => Some(
                    Item::new(ItemId::Block(BlockId::Dirt), MAX_STACK_COUNT).expect("full dirt"),
                ),
                other => *other,
            };
        }
        for slot in slots.iter_mut().filter(|slot| slot.is_none()) {
            *slot = Some(
                Item::new(ItemId::Block(BlockId::Brick), MAX_STACK_COUNT).expect("full brick"),
            );
        }
        lab.app.session.player = Player::with_inventory(
            position,
            Inventory::from_parts(slots, None),
            player_grid,
            table_grid,
        );
        lab.app.session.player.restore_selected_slot(tool_slot);
        assert_eq!(
            lab.slot_of(shovel_id),
            tool_slot,
            "the worn tool stays in its slot across the fixture seam"
        );
        assert_eq!(
            lab.app
                .session
                .player
                .selected_item()
                .and_then(|item| item.durability()),
            Some(1)
        );
        assert_eq!(
            lab.count(ItemId::Block(BlockId::Cobblestone)),
            cobble_before,
            "the recraft material survives the seam"
        );
        assert_eq!(
            lab.count(ItemId::Material(Material::Stick)),
            sticks_before,
            "the recraft sticks survive the seam"
        );
        assert!(
            lab.app
                .session
                .player
                .inventory()
                .slots()
                .iter()
                .all(|slot| slot.is_some()),
            "every slot is occupied before the final break"
        );
        assert!(
            lab.app
                .session
                .player
                .inventory()
                .slots()
                .iter()
                .flatten()
                .all(|item| item.id() != ItemId::Block(BlockId::Dirt)
                    || item.count() == MAX_STACK_COUNT),
            "no Dirt stack has room for the final drop"
        );

        let dirt_before = lab.count(ItemId::Block(BlockId::Dirt));
        let wear = lab.mine(IVec3::new(8, 8, 0), BlockId::Dirt, 0.1125);
        assert_eq!(
            wear,
            ToolWearOutcome::Broke,
            "the last point breaks the stone shovel"
        );
        assert_eq!(lab.count(shovel_id), 0, "the broken tool is gone");
        assert!(
            lab.app
                .session
                .player
                .inventory()
                .slots()
                .iter()
                .flatten()
                .all(|item| item.id() != shovel_id),
            "no slot still holds the broken tool identity"
        );
        assert_eq!(
            lab.app
                .session
                .player
                .inventory()
                .slot(tool_slot)
                .map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Dirt), 1)),
            "the freed slot received the drop (MEM195: wear precedes collect)"
        );
        assert_eq!(
            lab.count(ItemId::Block(BlockId::Dirt)),
            dirt_before + 1,
            "the final break still yields its drop"
        );

        // Recraft a full-durability replacement through the real craft path
        // from the remaining grid materials.
        lab.move_stack(ToolLab::player_cell(1), ToolLab::table_cell(0));
        for cell in [4usize, 8] {
            let slot = lab.slot_of(ItemId::Material(Material::Stick));
            lab.move_stack(InventoryTarget::Slot(slot), ToolLab::table_cell(cell));
        }
        let recrafted = lab.craft(GridKind::Table, shovel_id);
        assert_eq!(
            recrafted.durability(),
            Some(131),
            "a recrafted stone shovel is full durability"
        );
        assert_eq!(lab.count(shovel_id), 1);

        // Negative: the spent grid no longer matches any recipe, and a rejected
        // result click mutates neither the grid nor the inventory.
        let inventory_before = lab.app.session.player.inventory().clone();
        let table_before = lab.app.session.player.table_grid().clone();
        assert_eq!(
            click_inventory(
                &mut lab.app,
                &lab.screen,
                InventoryTarget::ResultSlot(GridKind::Table)
            ),
            Err(InventoryCommandError::Craft(CraftError::NoRecipe)),
            "a spent grid cannot mint a second tool"
        );
        assert_eq!(lab.app.session.player.inventory(), &inventory_before);
        assert_eq!(lab.app.session.player.table_grid(), &table_before);
    }

    /// Keybinds with jump_or_up rebound to ArrowUp (leaves Space free by
    /// rebinding nothing else) — a valid, conflict-free state.
    fn opencraft_keybinds_jump_arrow_up() -> opencraft::settings::Keybinds {
        opencraft::settings::Keybinds {
            jump_or_up: KeyCode::ArrowUp,
            ..opencraft::settings::Keybinds::default()
        }
    }
}
