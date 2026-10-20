use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{IVec3, Vec3};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use opencraft::mesh::quad::ChunkMesh;
use opencraft::player::inventory::HOTBAR_SLOTS;
use opencraft::player::mining::{MiningState, MiningTick};
use opencraft::player::physics::InputState;
use opencraft::player::raycast::raycast_voxels;
use opencraft::render::buffer::ChunkRenderStore;
use opencraft::render::context::RenderContext;
use opencraft::render::pipeline::{UiPass, VoxelRenderer};
use opencraft::settings::keybind::key_to_string;
use opencraft::settings::{
    load_with_warnings, Keybinds, Settings, SettingsWarning, DEFAULT_SETTINGS_PATH,
};
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
use opencraft::ui::hud::{
    hotbar_count_label, hotbar_count_text_origin, hotbar_slot_rects, mining_progress_quads,
    HOTBAR_COUNT_FONT_SIZE, HOTBAR_COUNT_LINE_HEIGHT,
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
use opencraft::{random_seed, WorldSession};
use opencraft::world::persistence::{
    load_world_from_path, save_world_to_path, CliConfig, PersistenceError, SavedPlayerState,
    WorldSaveData, DEFAULT_SAVE_PATH,
};
use opencraft::world::terrain::DEFAULT_WORLD_SEED;

/// Top-left corner of the stats overlay text, in physical pixels.
const OVERLAY_TEXT_LEFT: f32 = 8.0;
const OVERLAY_TEXT_TOP: f32 = 8.0;

/// Message-expiry tick interval: the loop runs in `ControlFlow::Poll`,
/// potentially far above 60 fps, while `menu::MESSAGE_TICKS` is calibrated
/// as ~10 seconds at ~60 ticks/s. Ticking the menu message at most this
/// often keeps the T01 pure tick counter time-sane without giving the menu
/// module a clock.
const MENU_TICK_INTERVAL: f32 = 1.0 / 60.0;

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

/// Apply the vsync setting to the surface's present mode and reconfigure
/// (the same set-present_mode-then-configure mechanism the resize path
/// uses; `ctx.config` is pub). `Fifo` (blocking, always supported) when
/// vsync is on; uncapped `Immediate` when off. NOTE: on Wayland the
/// compositor may not honor `Immediate` — presenting then degrades to the
/// compositor's schedule (an accepted visual no-op per plan; do not fight
/// the compositor).
fn apply_vsync(ctx: &mut RenderContext, vsync: bool) {
    ctx.config.present_mode = if vsync {
        wgpu::PresentMode::Fifo
    } else {
        wgpu::PresentMode::Immediate
    };
    ctx.surface.configure(&ctx.device, &ctx.config);
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
        player: SavedPlayerState {
            position: session.player.position.to_array(),
            yaw: session.player.yaw,
            pitch: session.player.pitch,
            fly_mode: session.player.fly_mode,
            selected_slot: session.player.selected_slot as u8,
            render_distance: session.world.render_distance() as u16,
            hotbar: session.player.hotbar.clone(),
        },
        chunk_edits: session.world.chunk_edits().clone(),
    };
    let result = save_world_to_path(&session.save_path, &data);
    if result.is_ok() {
        session.world.mark_saved();
    }
    result
}

/// Breaks `pos` and collects the caller-supplied pre-break `block` into the
/// session hotbar.
///
/// The world edit returns the chunks it remeshed. An empty result means the
/// edit did not apply (unloaded chunk or outside the vertical world range),
/// so nothing is collected — mining can never duplicate items through an
/// edit that never happened.
fn collect_mined_block(
    session: &mut WorldSession,
    pos: IVec3,
    block: BlockId,
) -> Vec<(ChunkPos, ChunkMesh)> {
    let remeshed = session
        .world
        .set_block_immediate(pos.x, pos.y, pos.z, BlockId::Air);
    if !remeshed.is_empty() {
        session.player.hotbar.collect(block, 1);
    }
    remeshed
}

/// Places the selected hotbar stack with the game's right-click semantics:
/// raycast 8 blocks from the camera, reject a placement that overlaps the
/// player, place the stack's block into the hit cell's adjacent cell, and
/// consume exactly one item only after the world edit actually applied.
///
/// Returns the remeshed chunks for the caller to upload, or `None` when the
/// ray misses, the cell is inside the player, the selected slot is empty, or
/// the edit did not apply (unloaded/out-of-range).
fn try_place_selected_block(
    session: &mut WorldSession,
) -> Option<Vec<(ChunkPos, ChunkMesh)>> {
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
    let stack = session.player.hotbar.stack(session.player.selected_slot)?;
    let remeshed = session
        .world
        .set_block_immediate(ap.x, ap.y, ap.z, stack.block);
    if remeshed.is_empty() {
        return None;
    }
    session.player.hotbar.consume(session.player.selected_slot);
    Some(remeshed)
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
/// line. Pure function of (settings, menu) so it is unit-testable without
/// GPU or events; called per frame while the menu is open, where
/// `StatsOverlay::update` skips the expensive re-shape unless the
/// composed string actually changed.
fn compose_menu_text(settings: &Settings, menu: &menu::SettingsMenu) -> String {
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
            MenuRow::Fullscreen => {
                format!("{}: {}", row.label(), on_off(settings.fullscreen))
            }
            MenuRow::Vsync => format!("{}: {}", row.label(), on_off(settings.vsync)),
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
    WorldSession::new_session(seed, cli.save_path.clone())
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
    ui: UiRenderer,
    stats: StatsOverlay,
    /// Settings-menu text buffer (same change-gated pattern as `stats`):
    /// drawn in the same UI pass, after the stats text area.
    menu_text: StatsOverlay,
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
}

struct GameApp {
    window: Option<Arc<Window>>,
    gpu: Option<Ready>,
    /// The active world session (S03-T02): world + player + camera + the
    /// save path every save site writes to. One always-present session,
    /// swapped atomically at `start_world_session`.
    session: WorldSession,
    input: InputState,
    cursor_grabbed: bool,
    /// The hold-to-mine accumulator (S02): pure state machine driven once per
    /// frame from the RedrawRequested pass. Any reset boundary below clears it
    /// so a menu/focus/session change never inherits partial progress.
    mining: MiningState,
    /// Whether the left mouse button is physically held from a gameplay
    /// press. Set only after the screen routing and click-to-grab early
    /// returns, cleared on release, focus loss, and screen/session changes.
    left_mouse_held: bool,
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
}

impl GameApp {
    fn new(cli: CliConfig, mut session: WorldSession) -> Self {
        // Startup settings load: never blocks launch — a missing or corrupt
        // settings.toml yields defaults plus structured warnings, surfaced
        // as menu text on the first open.
        let settings_path = Path::new(DEFAULT_SETTINGS_PATH).to_path_buf();
        let (settings, startup_warnings) = load_with_warnings(&settings_path);
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
        // The loaded Settings are the single source of truth for video
        // parameters: FOV applies to the session's camera at startup
        // (defaults are 75 degrees either way, so this is
        // behavior-neutral). The session's camera arrives already synced
        // to its player by the WorldSession constructors.
        session.camera.fov_y_radians = settings.fov_degrees.to_radians();
        Self {
            window: None,
            gpu: None,
            session,
            input: InputState::default(),
            cursor_grabbed: startup_screen == LifecycleScreen::Playing,
            mining: MiningState::new(),
            left_mouse_held: false,
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
        // same at startup; defaults are 75 degrees either way).
        session.camera.fov_y_radians = self.settings.fov_degrees.to_radians();
        self.session = session;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.render_store.clear();
        }
        self.input.reset();
        self.left_mouse_held = false;
        self.mining.reset();
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
        let session = WorldSession::new_session(seed, save_path);
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
                    NewGameTarget::Create => self.attempt_create(),
                    NewGameTarget::Back => {
                        self.dispatch_screen_intent(
                            event_loop,
                            LifecycleIntent::NewGame(NewGameIntent::Back),
                        );
                    }
                }
            }
            LifecycleScreen::LoadGame => {
                let row_count = self.load.row_count();
                let Some(target) = hovered_load_target(cursor, surface_w, surface_h, row_count)
                else {
                    return;
                };
                match target {
                    // The text portion of a row carries no click action
                    // (Play and Delete are the row's actions).
                    LoadTarget::Row(_) => {}
                    LoadTarget::Play(index) => self.play_row(index),
                    LoadTarget::Delete(index) => {
                        self.load.request_delete(index);
                    }
                    LoadTarget::Back => {
                        self.dispatch_screen_intent(
                            event_loop,
                            LifecycleIntent::LoadGame(LoadGameIntent::Back),
                        );
                    }
                    // Confirm/cancel are live only while a delete is
                    // pending (T01 geometry always exists; the pending
                    // state decides clickability, mirroring the hover
                    // rule in `hovered_target_rect`).
                    LoadTarget::ConfirmDelete => {
                        if self.load.is_delete_pending() {
                            self.confirm_pending_delete();
                        }
                    }
                    LoadTarget::CancelDelete => {
                        if self.load.is_delete_pending() {
                            self.load.cancel_delete();
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
        let render_store = ChunkRenderStore::new();
        let mut ui = UiRenderer::new(&ctx);
        let stats = StatsOverlay::new(&mut ui.font_system);
        let menu_text = StatsOverlay::new(&mut ui.font_system);
        let load_play_text = StatsOverlay::new(&mut ui.font_system);
        let load_delete_text = StatsOverlay::new(&mut ui.font_system);
        let hotbar_counts = std::array::from_fn(|_| {
            StatsOverlay::with_metrics(
                &mut ui.font_system,
                HOTBAR_COUNT_FONT_SIZE,
                HOTBAR_COUNT_LINE_HEIGHT,
            )
        });

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
            ui,
            stats,
            menu_text,
            load_play_text,
            load_delete_text,
            hotbar_counts,
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
                let _ = save_current_world(&mut self.session);
                // Crash-safety settings save on quit: every accepted change
                // already saved immediately, so this only catches anything
                // not yet persisted (e.g. a hand-edited in-memory state).
                // Save errors are ignored here — the process is exiting and
                // there is no menu left to display them in.
                let _ = self.settings.save(&self.settings_path);
                event_loop.exit();
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

                    match code {
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
                        KeyCode::BracketLeft if pressed && !event.repeat => {
                            let rd = (self.session.world.render_distance() - 2).max(4);
                            self.session.world.set_render_distance(rd);
                        }
                        KeyCode::BracketRight if pressed && !event.repeat => {
                            let rd = (self.session.world.render_distance() + 2).min(36);
                            self.session.world.set_render_distance(rd);
                        }
                        KeyCode::Digit1 if pressed => self.session.player.selected_slot = 0,
                        KeyCode::Digit2 if pressed => self.session.player.selected_slot = 1,
                        KeyCode::Digit3 if pressed => self.session.player.selected_slot = 2,
                        KeyCode::Digit4 if pressed => self.session.player.selected_slot = 3,
                        KeyCode::Digit5 if pressed => self.session.player.selected_slot = 4,
                        KeyCode::Digit6 if pressed => self.session.player.selected_slot = 5,
                        KeyCode::Digit7 if pressed => self.session.player.selected_slot = 6,
                        KeyCode::Digit8 if pressed => self.session.player.selected_slot = 7,
                        KeyCode::Digit9 if pressed => self.session.player.selected_slot = 8,
                        // Rebindable gameplay keys (S04-T03): press sets the
                        // InputState flag, release clears it — both
                        // transitions route through the loaded keybind map
                        // via the pure helper. The 7 rebindable routes sit
                        // AFTER the non-rebindable utility keys on purpose:
                        // Escape must always toggle the menu and F/F5/
                        // brackets/digits keep their hardcoded behavior even
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
                // a scroll action in this slice.
                if self.lifecycle.screen() != LifecycleScreen::Playing {
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
                    self.session.player.selected_slot =
                        (self.session.player.selected_slot as isize + step).rem_euclid(9) as usize;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
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
                    } else {
                        // Other lifecycle screens own the click (T01 hit
                        // tests); gpu is not touched after this call on
                        // this path.
                        self.screen_click(event_loop, button, surface_w, surface_h);
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
                        if let Some(remeshed) = try_place_selected_block(&mut self.session) {
                            for (cpos, mesh) in remeshed {
                                gpu.render_store.upload_mesh(
                                    &gpu.ctx.device,
                                    &gpu.renderer.chunk_bind_group_layout,
                                    cpos,
                                    &mesh,
                                );
                            }
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
                if playing {
                    self.session.player
                        .update(dt, &self.input, |x, y, z| self.session.world.get_block(x, y, z));
                    self.session.camera.position = self.session.player.eye_position();
                    self.session.camera.yaw = self.session.player.yaw;
                    self.session.camera.pitch = self.session.player.pitch;

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
                }

                let hit = raycast_voxels(
                    self.session.camera.position,
                    self.session.camera.forward(),
                    8.0,
                    |x, y, z| self.session.world.get_block(x, y, z),
                );
                // Hold-to-mine candidate: only a held Left button on the
                // grabbed gameplay surface feeds the accumulator. Every
                // other frame passes None, which resets stale progress
                // (released, paused, or cursor not grabbed).
                let candidate = if playing && self.cursor_grabbed && self.left_mouse_held {
                    hit.map(|h| {
                        (
                            h.block_pos,
                            self.session
                                .world
                                .get_block(h.block_pos.x, h.block_pos.y, h.block_pos.z),
                        )
                    })
                } else {
                    None
                };
                let mut target_block = hit.map(|h| h.block_pos);
                if let MiningTick::Broke { pos, block } = self.mining.tick(dt, candidate) {
                    // The only production break site (S02): hardness
                    // gating happens in MiningState, so Bedrock (no
                    // duration) can never reach this arm.
                    let remeshed = collect_mined_block(&mut self.session, pos, block);
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
                        compose_menu_text(&self.settings, &self.menu)
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
                let hotbar_rects =
                    hotbar_slot_rects(gpu.ctx.config.width, gpu.ctx.config.height);
                for (slot, counts) in gpu.hotbar_counts.iter_mut().enumerate() {
                    counts.update(
                        &mut gpu.ui.font_system,
                        gpu.ctx.config.width as f32,
                        gpu.ctx.config.height as f32,
                        &hotbar_count_label(self.session.player.hotbar.stack(slot)),
                    );
                }

                // UI overlay: prepare glyphs each frame; on prepare error
                // (glyph atlas full) the UI pass is skipped for that frame
                // (the crate is log-free by convention). The menu's text
                // area rides the same UI pass, AFTER the stats area, with
                // no pipeline change (MEM014).
                let stats_text_area = gpu.stats.text_area(OVERLAY_TEXT_LEFT, OVERLAY_TEXT_TOP);
                let mut areas = vec![stats_text_area];
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
                    let label = hotbar_count_label(self.session.player.hotbar.stack(slot));
                    if !label.is_empty() {
                        let (x, y) = hotbar_count_text_origin(hotbar_rects[slot]);
                        areas.push(counts.text_area(x, y));
                    }
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

                match gpu.renderer.render_frame(
                    &gpu.ctx,
                    &gpu.render_store,
                    &self.session.camera,
                    self.session.world.render_distance(),
                    target_block,
                    self.session.player.selected_slot,
                    ui_pass,
                ) {
                    Ok(visible_chunks) => {
                        self.frame_count += 1;
                        let elapsed = self.title_timer.elapsed().as_secs_f32();
                        if elapsed >= 0.25 {
                            let fps = (self.frame_count as f32 / elapsed).round() as u32;
                            let ms = (elapsed * 1000.0) / self.frame_count.max(1) as f32;
                            let vram_kb = gpu.render_store.total_vram_bytes / 1024;
                            let mode = if self.session.player.fly_mode { "Fly" } else { "Walk" };
                            let block_name = match self
                                .session
                                .player
                                .hotbar
                                .stack(self.session.player.selected_slot)
                            {
                                Some(stack) => format!("{} x{}", stack.block.name(), stack.count),
                                None => "Empty".to_string(),
                            };
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
                                "Minecraft Clone{} | {} FPS ({:.2} ms) | Chunks: {}/{} | Quads: {} ({} KB VRAM) | Pos: ({:.0}, {:.0}, {:.0}) [{}] | Block [{}]: {} | RD: {} | Seed: {}{}",
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
                                self.session.player.selected_slot + 1,
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
                        let _ =
                            save_current_world(&mut self.session);
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
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
    use opencraft::ui::SettingsCaller;
    use opencraft::world::catalog::{ConfirmOutcome, DeleteConfirmation, RowProblem};
    use opencraft::player::inventory::{ItemStack, HOTBAR_SLOTS};
    use opencraft::world::chunk::{Chunk, ChunkPos};
    use opencraft::world::terrain::generate_chunk_seeded;
    use rustc_hash::FxHashMap;
    use std::path::PathBuf;

    use opencraft::player::camera::Camera;
    use opencraft::world::persistence::ChunkEditMap;
    use opencraft::world::terrain::terrain_height_at_seeded;

    use glam::IVec3;

    #[test]
    fn menu_text_lists_every_row_with_its_live_value() {
        let settings = Settings::default();
        let mut menu = menu::SettingsMenu::new();
        menu.open();
        let text = compose_menu_text(&settings, &menu);
        assert!(text.contains("Move Forward: KeyW"));
        assert!(text.contains("Move Backward: KeyS"));
        assert!(text.contains("Move Left: KeyA"));
        assert!(text.contains("Move Right: KeyD"));
        assert!(text.contains("Jump / Up: Space"));
        assert!(text.contains("Crouch / Down: ShiftLeft"));
        assert!(text.contains("Sprint: ControlLeft"));
        assert!(text.contains("Mouse Sensitivity: 0.0022"));
        assert!(text.contains("Field of View: 75"));
        assert!(text.contains("Fullscreen: Off"));
        assert!(text.contains("VSync: Off"));
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
            fullscreen: true,
            vsync: true,
        };
        let menu = menu::SettingsMenu::new();
        let text = compose_menu_text(&settings, &menu);
        assert!(text.contains("Jump / Up: ArrowUp"));
        assert!(text.contains("Mouse Sensitivity: 0.0050"));
        assert!(text.contains("Field of View: 90"));
        assert!(text.contains("Fullscreen: On"));
        assert!(text.contains("VSync: On"));
    }

    #[test]
    fn awaiting_key_row_shows_capture_hint_and_message_line_appends() {
        let settings = Settings::default();
        let mut menu = menu::SettingsMenu::new();
        menu.open();
        menu.begin_rebind(menu::BindAction::JumpOrUp);
        let text = compose_menu_text(&settings, &menu);
        assert!(text.contains("Jump / Up: <press a key>"));
        assert!(text.contains("press a key for 'jump_or_up' (Escape cancels)"));

        // Transient messages append as the final line.
        menu.set_message_for_ticks("key already bound to 'forward'", 1);
        let text = compose_menu_text(&settings, &menu);
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
        assert!(cli_has_world_intent(&CliConfig::parse_args(["--new-world"])));
        assert!(cli_has_world_intent(&CliConfig::parse_args(["--seed", "7"])));
        assert!(cli_has_world_intent(&CliConfig::parse_args([
            "--save",
            "other.mcvx"
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

    /// A headless GameApp for seam tests: no window/GPU, a tempdir catalog,
    /// and the given screen entered through the real `enter_screen` path
    /// (LoadGame entry refreshes rows from the tempdir catalog).
    fn app_at_screen(saves_dir: &Path, screen: LifecycleScreen) -> GameApp {
        let cli = CliConfig::parse_args(Vec::<String>::new());
        let mut app = GameApp::new(
            cli,
            WorldSession::new_session(1, PathBuf::from("/nonexistent/initial.mcvx")),
        );
        app.catalog = WorldCatalogFs::new(saves_dir);
        app.enter_screen(screen);
        app
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

    #[test]
    fn mining_collects_broken_blocks_into_the_hotbar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(7, dir.path().join("mined.mcvx"));
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);

        let first = collect_mined_block(&mut session, IVec3::new(0, 10, 0), BlockId::Stone);
        assert!(
            !first.is_empty(),
            "an applied break remeshes at least its own chunk"
        );
        let second = collect_mined_block(&mut session, IVec3::new(0, 10, 0), BlockId::Stone);
        assert!(!second.is_empty());

        assert_eq!(session.world.get_block(0, 10, 0), BlockId::Air);
        assert_eq!(
            session.player.hotbar.stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 2,
            })
        );
    }

    #[test]
    fn mining_collects_the_pre_break_block_not_the_now_air_cell() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(8, dir.path().join("ore.mcvx"));
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Stone);

        let remeshed =
            collect_mined_block(&mut session, IVec3::new(0, 10, 1), BlockId::CoalOre);

        assert!(!remeshed.is_empty());
        assert_eq!(session.world.get_block(0, 10, 1), BlockId::Air);
        assert_eq!(
            session.player.hotbar.stack(0),
            Some(ItemStack {
                block: BlockId::CoalOre,
                count: 1,
            })
        );
    }

    #[test]
    fn mining_an_unloaded_chunk_collects_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(9, dir.path().join("missing.mcvx"));

        let remeshed = collect_mined_block(&mut session, IVec3::new(80, 10, 0), BlockId::Stone);

        assert!(remeshed.is_empty(), "no loaded chunk means no remesh");
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(session.player.hotbar.stack(slot), None);
        }
    }

    #[test]
    fn hotbar_stacks_survive_save_and_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let save_path = dir.path().join("stacks.mcvx");
        let mut session = WorldSession::new_session(4242, save_path.clone());
        session.player.hotbar.collect(BlockId::CoalOre, 2);
        session.player.hotbar.collect(BlockId::IronOre, 1);
        session.player.selected_slot = 1;

        save_current_world(&mut session).expect("save succeeds");
        let loaded = load_world_from_path(&save_path).expect("load succeeds");
        let restored = WorldSession::session_from_save(loaded, save_path.clone());

        assert_eq!(restored.world.seed(), 4242);
        assert_eq!(restored.player.selected_slot, 1);
        assert_eq!(restored.player.hotbar, session.player.hotbar);
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(restored.player.hotbar.stack(slot), session.player.hotbar.stack(slot));
        }
    }

    #[test]
    fn placing_the_selected_stack_consumes_exactly_one_item() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(10, dir.path().join("place.mcvx"));
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        session.player.position = Vec3::new(5.5, 10.0, 5.5);
        aim_placement_camera(&mut session);
        session.player.hotbar.collect(BlockId::Stone, 2);
        session.player.selected_slot = 0;

        let remeshed = try_place_selected_block(&mut session).expect("placement applies");

        assert!(!remeshed.is_empty(), "placement remeshes its chunk");
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Stone);
        assert_eq!(
            session.player.hotbar.stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 1,
            })
        );
    }

    #[test]
    fn placement_rejects_empty_slots_and_player_overlap_without_consuming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = WorldSession::new_session(12, dir.path().join("reject.mcvx"));
        insert_test_chunk_with_face_neighbors(&mut session, BlockId::Air);
        session.world.set_block_immediate(0, 5, 0, BlockId::Stone);
        session.player.position = Vec3::new(5.5, 10.0, 5.5);
        aim_placement_camera(&mut session);
        session.player.hotbar.collect(BlockId::Stone, 2);
        session.player.selected_slot = 5;

        assert!(
            try_place_selected_block(&mut session).is_none(),
            "an empty selected slot cannot place"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Air);
        assert_eq!(
            session.player.hotbar.stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 2,
            }),
            "nothing was consumed"
        );

        session.player.selected_slot = 0;
        session.player.position = Vec3::new(0.5, 6.0, 0.5);
        assert!(
            try_place_selected_block(&mut session).is_none(),
            "a placement overlapping the player AABB is rejected"
        );
        assert_eq!(session.world.get_block(0, 6, 0), BlockId::Air);
        assert_eq!(
            session.player.hotbar.stack(0),
            Some(ItemStack {
                block: BlockId::Stone,
                count: 2,
            }),
            "rejected placement does not consume"
        );
    }

    #[test]
    fn valid_create_starts_a_world_session_and_enters_playing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game.apply(TextInputAction::InsertText("Deep Dark".into()));
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
        // The player spawns on the resolved seed's terrain (the session
        // was built via session::new_session, not the old one).
        let expected_y = (terrain_height_at_seeded(0, 0, expected_seed) + 4) as f32;
        assert_eq!(app.session.player.position, Vec3::new(0.5, expected_y, 0.5));
        // Playing semantics: cursor grabbed, no form message.
        assert!(app.cursor_grabbed);
        assert!(app.new_game.message().is_none());
        // No save is written at create time — persistence rides the
        // normal save sites once the world accumulates state.
        assert!(!dir.path().join("Deep Dark.mcvx").exists());
    }

    #[test]
    fn blank_seed_create_uses_a_fresh_seed_and_still_enters_playing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game.apply(TextInputAction::InsertText("Blank Seed".into()));
        // Seed field left blank: the create resolves a fresh time-derived
        // seed (session::random_seed). The value itself is not predictable,
        // so assert the session is internally consistent with its own seed
        // (built via new_session: the player spawns on that seed's terrain)
        // plus the catalog-validated save path.
        app.attempt_create();
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::Playing);
        assert_eq!(app.session.save_path, dir.path().join("Blank Seed.mcvx"));
        let seed = app.session.world.seed();
        let expected_y = (terrain_height_at_seeded(0, 0, seed) + 4) as f32;
        assert_eq!(app.session.player.position, Vec3::new(0.5, expected_y, 0.5));
    }

    #[test]
    fn duplicate_and_invalid_name_creates_stay_on_new_game_with_inline_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Duplicate: attempt_create fetches FRESH rows, so a row saved
        // after the screen opened still collides (case-insensitively).
        let catalog = WorldCatalogFs::new(dir.path());
        catalog.save_world("Alpha", &sample_save(9)).expect("save alpha");
        let mut app = app_at_screen(dir.path(), LifecycleScreen::NewGame);
        app.new_game.apply(TextInputAction::InsertText("alpha".into()));
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
        app.new_game.apply(TextInputAction::InsertText("bad/name".into()));
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
        assert_eq!(app.session.player.selected_slot, 2);
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
        app.catalog.save_world("ghost", &sample_save(5)).expect("save ghost");
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
            WorldSession::new_session(9, blocker.join("deep.mcvx")),
        );
        // An unsaved chunk edit: the flag the failure must preserve.
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, 9));
        app.session.world.set_block_immediate(0, 40, 5, BlockId::Brick);
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
        let mut app = GameApp::new(cli, WorldSession::new_session(9, save_path.clone()));
        let cpos = ChunkPos { x: 0, y: 1, z: 0 };
        app.session
            .world
            .insert_chunk_for_test(cpos, generate_chunk_seeded(cpos, 9));
        app.session.world.set_block_immediate(0, 40, 5, BlockId::Brick);

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
        let mut app = GameApp::new(cli, WorldSession::new_session(9, save_path.clone()));
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
        app.session.world.set_block_immediate(0, 40, 5, BlockId::Brick);
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
        assert!(save_path.is_file(), "world saved at the session's save path");
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
            WorldSession::new_session(1, PathBuf::from("/nonexistent/initial.mcvx")),
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

        app.start_world_session(WorldSession::new_session(
            42,
            PathBuf::from("/nonexistent/replacement.mcvx"),
        ));

        assert_eq!(app.session.world.seed(), 42, "seed swapped");
        assert_eq!(
            app.session.save_path,
            PathBuf::from("/nonexistent/replacement.mcvx"),
            "save path swapped"
        );
        let expected_y = (terrain_height_at_seeded(0, 0, 42) + 4) as f32;
        assert_eq!(
            app.session.player.position,
            Vec3::new(0.5, expected_y, 0.5),
            "player swapped to the new world's spawn"
        );
        assert_eq!(
            app.session.camera.position,
            app.session.player.eye_position(),
            "camera synced to the new player by the constructor"
        );
        assert_eq!(app.session.camera.yaw, app.session.player.yaw);
        assert_eq!(app.session.camera.pitch, app.session.player.pitch);
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
            WorldSession::new_session(1, PathBuf::from("/nonexistent/initial.mcvx")),
        );
        app.left_mouse_held = true;
        app.mining
            .tick(0.5, Some((IVec3::new(0, 40, 0), BlockId::Stone)));
        assert!(
            app.mining.progress().is_some(),
            "0.5s on Stone leaves partial progress"
        );

        app.enter_screen(LifecycleScreen::MainMenu);

        assert!(
            !app.left_mouse_held,
            "held latch cleared on screen entry"
        );
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
            WorldSession::new_session(1, PathBuf::from("/nonexistent/initial.mcvx")),
        );
        app.left_mouse_held = true;
        app.mining
            .tick(0.5, Some((IVec3::new(0, 40, 0), BlockId::Stone)));
        assert!(app.mining.progress().is_some());

        app.start_world_session(WorldSession::new_session(
            42,
            PathBuf::from("/nonexistent/replacement.mcvx"),
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
        assert_eq!(session.player.selected_slot, 2);
        assert_eq!(session.world.render_distance(), 16);
        assert_eq!(session.save_path, save_path, "session owns the cli save path");
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
        let cli = CliConfig::parse_args([
            "--save",
            missing.to_str().unwrap(),
            "--seed",
            "777",
        ]);
        let session = startup_session(&cli);
        assert_eq!(session.world.seed(), 777);
        assert_eq!(session.save_path, missing);
        let expected_y = (terrain_height_at_seeded(0, 0, 777) + 4) as f32;
        assert_eq!(
            session.player.position,
            Vec3::new(0.5, expected_y, 0.5),
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
        assert_eq!(
            session.player.position.y,
            (terrain_height_at_seeded(0, 0, DEFAULT_WORLD_SEED) + 4) as f32,
            "corrupt save falls back to the default-seed spawn"
        );

        // --new forces a fresh session even when a save file exists
        // (legacy direct-launch behavior, byte-identical).
        let existing = dir.path().join("ignored.mcvx");
        save_world_to_path(&existing, &sample_save(999)).expect("write existing save");
        let cli = CliConfig::parse_args([
            "--save",
            existing.to_str().unwrap(),
            "--new",
            "--seed",
            "5",
        ]);
        let session = startup_session(&cli);
        assert_eq!(session.world.seed(), 5, "force_new ignores the existing save");
        let expected_y = (terrain_height_at_seeded(0, 0, 5) + 4) as f32;
        assert_eq!(session.player.position, Vec3::new(0.5, expected_y, 0.5));
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
        app.new_game.apply(TextInputAction::InsertText("alpha".into()));
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
        app.session.world.set_block_immediate(0, 40, 5, BlockId::Brick);
        assert_eq!(app.session.world.get_block(0, 40, 5), BlockId::Brick);
        assert!(app.session.world.has_unsaved_changes());
        let edits_before = app.session.world.chunk_edits().clone();
        app.session.player.position = Vec3::new(12.5, 70.0, -3.5);
        app.session.player.yaw = 1.25;
        app.session.player.pitch = -0.5;
        app.session.player.fly_mode = true;
        app.session.player.selected_slot = 5;

        // Escape to pause, then Save and Quit through the real, loop-free
        // decision apply: back to the main menu with the session saved to
        // its own save path.
        app.lifecycle = LifecycleState::from_screen(LifecycleScreen::Pause);
        let exit_requested = app.apply_lifecycle_decision(LifecycleDecision::SaveAndQuit);
        assert!(!exit_requested);
        assert_eq!(app.lifecycle.screen(), LifecycleScreen::MainMenu);
        assert!(app.main_menu_message.is_none(), "save succeeded, no message");
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
        assert_eq!(app.session.player.selected_slot, 5);
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
        let cli = CliConfig::parse_args([
            "--save",
            save_path.to_str().unwrap(),
            "--seed",
            "7",
        ]);
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
        assert!(!tick_transient_message(&mut message), "no message stays false");
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
            "> World Name: Deep Dark|\n  Seed: (random)\nRandomize Seed\nCreate World\nBack"
        );
        // Tab moves the marker to the seed; its caret follows the text
        new_game.cycle_focus();
        new_game.apply(TextInputAction::InsertText("42".into()));
        assert_eq!(
            compose_screen_text(LifecycleScreen::NewGame, &new_game, &load, None, None),
            "  World Name: Deep Dark\n> Seed: 42|\nRandomize Seed\nCreate World\nBack"
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
            player: SavedPlayerState {
                position: [1.0, 64.0, -2.0],
                yaw: 0.5,
                pitch: -0.25,
                fly_mode: true,
                selected_slot: 2,
                render_distance: 16,
                hotbar: opencraft::player::inventory::Hotbar::empty(),
            },
            chunk_edits: ChunkEditMap::default(),
        }
    }

    #[test]
    fn apply_confirmed_delete_refreshes_rows_and_surfaces_delete_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = WorldCatalogFs::new(dir.path());
        catalog.save_world("doomed", &sample_save(1)).expect("save doomed");
        catalog.save_world("spared", &sample_save(2)).expect("save spared");

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

    /// Keybinds with jump_or_up rebound to ArrowUp (leaves Space free by
    /// rebinding nothing else) — a valid, conflict-free state.
    fn opencraft_keybinds_jump_arrow_up() -> opencraft::settings::Keybinds {
        opencraft::settings::Keybinds {
            jump_or_up: KeyCode::ArrowUp,
            ..opencraft::settings::Keybinds::default()
        }
    }
}
