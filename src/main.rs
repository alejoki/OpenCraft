use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use minecraft_clone::player::camera::Camera;
use minecraft_clone::player::physics::{InputState, Player};
use minecraft_clone::player::raycast::raycast_voxels;
use minecraft_clone::render::buffer::ChunkRenderStore;
use minecraft_clone::render::context::RenderContext;
use minecraft_clone::render::pipeline::{UiPass, VoxelRenderer};
use minecraft_clone::settings::keybind::key_to_string;
use minecraft_clone::settings::{
    load_with_warnings, Keybinds, Settings, SettingsWarning, DEFAULT_SETTINGS_PATH,
};
use minecraft_clone::ui::menu::{self, AdjustDirection, MenuRow};
use minecraft_clone::ui::{Quad, StatsOverlay, UiRenderer, FONT_SIZE, LINE_HEIGHT};
use minecraft_clone::world::block::BlockId;
use minecraft_clone::world::chunk::world_to_chunk_and_local;
use minecraft_clone::world::manager::WorldManager;
use minecraft_clone::world::persistence::{
    load_world_from_path, save_world_to_path, ChunkEditMap, CliConfig, SavedPlayerState,
    WorldSaveData,
};
use minecraft_clone::world::terrain::{terrain_height_at_seeded, DEFAULT_WORLD_SEED};

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

fn save_current_world(save_path: &Path, world: &mut WorldManager, player: &Player) -> bool {
    let data = WorldSaveData {
        seed: world.seed(),
        player: SavedPlayerState {
            position: player.position.to_array(),
            yaw: player.yaw,
            pitch: player.pitch,
            fly_mode: player.fly_mode,
            selected_slot: player.selected_slot as u8,
            render_distance: world.render_distance() as u16,
        },
        chunk_edits: world.chunk_edits().clone(),
    };
    if save_world_to_path(save_path, &data).is_ok() {
        world.mark_saved();
        true
    } else {
        false
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
fn save_settings_or_warn(menu: &mut menu::SettingsMenu, settings: &Settings) {
    if let Err(err) = settings.save(Path::new(DEFAULT_SETTINGS_PATH)) {
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
}

struct GameApp {
    window: Option<Arc<Window>>,
    gpu: Option<Ready>,
    cli: CliConfig,
    world: WorldManager,
    player: Player,
    camera: Camera,
    input: InputState,
    cursor_grabbed: bool,
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
    menu: menu::SettingsMenu,
    /// Timestamp of the last message-expiry tick (bounded at ~60 Hz; see
    /// `MENU_TICK_INTERVAL`).
    message_tick_timer: Instant,
}

impl GameApp {
    fn new(cli: CliConfig, world: WorldManager, player: Player) -> Self {
        let mut camera = Camera::new(player.eye_position());
        camera.yaw = player.yaw;
        camera.pitch = player.pitch;
        // Startup settings load: never blocks launch — a missing or corrupt
        // settings.toml yields defaults plus structured warnings, surfaced
        // as menu text on the first open.
        let (settings, startup_warnings) = load_with_warnings(Path::new(DEFAULT_SETTINGS_PATH));
        // The loaded Settings are the single source of truth for video
        // parameters: FOV applies to the camera at construction (defaults
        // are 75 degrees either way, so this is behavior-neutral).
        camera.fov_y_radians = settings.fov_degrees.to_radians();
        Self {
            window: None,
            gpu: None,
            cli,
            world,
            player,
            camera,
            input: InputState::default(),
            cursor_grabbed: true,
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
            message_tick_timer: Instant::now(),
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
    fn menu_click(
        &mut self,
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
                    save_settings_or_warn(&mut self.menu, &self.settings);
                }
            }
            MenuRow::Fov => {
                if let Some(direction) = adjust_direction(button) {
                    self.settings.fov_degrees =
                        menu::adjust_fov(self.settings.fov_degrees, direction);
                    // Live apply: Camera reads `fov_y_radians` every frame
                    // in view_proj, so updating the pub field updates the
                    // projection with no reconstruction.
                    self.camera.fov_y_radians = self.settings.fov_degrees.to_radians();
                    save_settings_or_warn(&mut self.menu, &self.settings);
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
                save_settings_or_warn(&mut self.menu, &self.settings);
            }
            MenuRow::Vsync => {
                self.settings.vsync = !self.settings.vsync;
                // The live present-mode swap happens in the caller (needs
                // the RenderContext; see the doc comment above).
                save_settings_or_warn(&mut self.menu, &self.settings);
            }
            MenuRow::Close => {
                self.menu.close();
                self.cursor_grabbed = true;
                set_cursor_grab(window, true);
            }
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

        self.cursor_grabbed = true;
        set_cursor_grab(&window, true);

        self.window = Some(window);
        self.gpu = Some(Ready {
            ctx,
            renderer,
            render_store,
            ui,
            stats,
            menu_text,
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
                let _ = save_current_world(&self.cli.save_path, &mut self.world, &self.player);
                // Crash-safety settings save on quit: every accepted change
                // already saved immediately, so this only catches anything
                // not yet persisted (e.g. a hand-edited in-memory state).
                // Save errors are ignored here — the process is exiting and
                // there is no menu left to display them in.
                let _ = self.settings.save(Path::new(DEFAULT_SETTINGS_PATH));
                event_loop.exit();
            }
            WindowEvent::Focused(false) => {
                self.input.reset();
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
                    // While the menu is open it owns the keyboard: every key
                    // routes to the menu FIRST and the gameplay match below
                    // never runs. Gameplay keys must not set InputState
                    // while the menu is open — especially in AwaitingKey,
                    // where ANY pressed KeyCode is a capture candidate.
                    if self.menu.is_open() {
                        if pressed && !event.repeat {
                            if code == KeyCode::Escape {
                                // In capture mode, Escape cancels the pending
                                // capture first (the menu stays open and the
                                // cursor stays released); otherwise it closes
                                // the menu and re-grabs the cursor.
                                if !self.menu.cancel_capture() {
                                    self.menu.close();
                                    self.cursor_grabbed = true;
                                    set_cursor_grab(window, true);
                                }
                            } else if let Some(action) = self.menu.awaiting_action() {
                                // Press-to-rebind: T01's pure decision — the
                                // unsupported-key filter runs first, then the
                                // duplicate check.
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
                                        save_settings_or_warn(&mut self.menu, &self.settings);
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

                    match code {
                        KeyCode::KeyF if pressed && !event.repeat => {
                            self.player.fly_mode = !self.player.fly_mode;
                            self.player.velocity = Vec3::ZERO;
                        }
                        KeyCode::F5 if pressed && !event.repeat => {
                            if save_current_world(
                                &self.cli.save_path,
                                &mut self.world,
                                &self.player,
                            ) {
                                self.last_saved_badge = Some(Instant::now());
                            }
                        }
                        KeyCode::Escape if pressed && !event.repeat => {
                            // The menu is closed here — an open menu returned
                            // above. Escape opens the settings menu
                            // (replacing the old raw cursor-grab toggle: an
                            // open menu always implies a released cursor).
                            if self.menu.open() {
                                self.cursor_grabbed = false;
                                set_cursor_grab(window, false);
                                // Roadmap requirement: opening the menu resets
                                // held inputs so no gameplay key sticks while
                                // the menu owns the input.
                                self.input.reset();
                                // Startup load warnings surface as the menu's
                                // initial message, once (MEM009/D006: data
                                // displayed in-menu, never logged).
                                if !self.startup_warnings_shown {
                                    self.startup_warnings_shown = true;
                                    if !self.startup_warnings.is_empty() {
                                        let lines = menu::format_warnings(&self.startup_warnings)
                                            .join(" | ");
                                        self.menu.set_message(lines);
                                    }
                                }
                            }
                        }
                        KeyCode::BracketLeft if pressed && !event.repeat => {
                            let rd = (self.world.render_distance() - 2).max(4);
                            self.world.set_render_distance(rd);
                        }
                        KeyCode::BracketRight if pressed && !event.repeat => {
                            let rd = (self.world.render_distance() + 2).min(36);
                            self.world.set_render_distance(rd);
                        }
                        KeyCode::Digit1 if pressed => self.player.selected_slot = 0,
                        KeyCode::Digit2 if pressed => self.player.selected_slot = 1,
                        KeyCode::Digit3 if pressed => self.player.selected_slot = 2,
                        KeyCode::Digit4 if pressed => self.player.selected_slot = 3,
                        KeyCode::Digit5 if pressed => self.player.selected_slot = 4,
                        KeyCode::Digit6 if pressed => self.player.selected_slot = 5,
                        KeyCode::Digit7 if pressed => self.player.selected_slot = 6,
                        KeyCode::Digit8 if pressed => self.player.selected_slot = 7,
                        KeyCode::Digit9 if pressed => self.player.selected_slot = 8,
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
                    self.player.selected_slot =
                        (self.player.selected_slot as isize + step).rem_euclid(9) as usize;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                // While the menu is open the cursor is released and every
                // click belongs to the menu: hit-test the row under the
                // cursor. The click-to-re-grab path below and the gameplay
                // raycast must not steal menu clicks.
                if self.menu.is_open() {
                    let (surface_w, surface_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                    // Hit-test the clicked row BEFORE the whole-`self`
                    // menu_click call (which must not overlap the live
                    // gpu/window field borrows): only the Vsync row needs a
                    // post-call surface reconfigure, and that needs gpu.
                    let clicked = self
                        .cursor_pos
                        .and_then(|pos| menu::hovered_row(pos, surface_w, surface_h));
                    let window = window.clone();
                    let cursor = self.cursor_pos;
                    self.menu_click(&window, button, cursor, surface_w, surface_h);
                    if clicked == Some(MenuRow::Vsync) {
                        if let Some(gpu) = self.gpu.as_mut() {
                            // Live apply: Fifo/Immediate present mode +
                            // surface reconfigure (the resize path's
                            // mechanism; see `apply_vsync`).
                            apply_vsync(&mut gpu.ctx, self.settings.vsync);
                        }
                    }
                    return;
                }

                if !self.cursor_grabbed {
                    self.cursor_grabbed = true;
                    set_cursor_grab(window, true);
                    return;
                }

                if let Some(hit) = raycast_voxels(
                    self.camera.position,
                    self.camera.forward(),
                    8.0,
                    |x, y, z| self.world.get_block(x, y, z),
                ) {
                    match button {
                        MouseButton::Left => {
                            let bp = hit.block_pos;
                            if self.world.get_block(bp.x, bp.y, bp.z) != BlockId::Bedrock {
                                let remeshed =
                                    self.world
                                        .set_block_immediate(bp.x, bp.y, bp.z, BlockId::Air);
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
                        MouseButton::Right => {
                            let ap = hit.adjacent_pos;
                            if !self.player.aabb().intersects_block(ap.x, ap.y, ap.z) {
                                let place_block = BlockId::HOTBAR[self.player.selected_slot];
                                let remeshed =
                                    self.world
                                        .set_block_immediate(ap.x, ap.y, ap.z, place_block);
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
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32();
                self.last_frame = now;

                // Message expiry at a bounded ~60 ticks/s (see
                // `MENU_TICK_INTERVAL`): the pure T01 tick counter decrements
                // only here, so warnings never stick forever and never flash
                // by at uncapped frame rates.
                if self.menu.message.is_some()
                    && self.message_tick_timer.elapsed().as_secs_f32() >= MENU_TICK_INTERVAL
                {
                    self.message_tick_timer = Instant::now();
                    self.menu.tick();
                }

                self.player
                    .update(dt, &self.input, |x, y, z| self.world.get_block(x, y, z));
                self.camera.position = self.player.eye_position();
                self.camera.yaw = self.player.yaw;
                self.camera.pitch = self.player.pitch;

                let (player_chunk, _) = world_to_chunk_and_local(
                    self.player.position.x.floor() as i32,
                    self.player.position.y.floor() as i32,
                    self.player.position.z.floor() as i32,
                );
                let (new_meshes, unloaded) = self.world.update_streaming(player_chunk);
                if !unloaded.is_empty()
                    && self.world.has_unsaved_changes()
                    && save_current_world(&self.cli.save_path, &mut self.world, &self.player)
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

                let target_block = raycast_voxels(
                    self.camera.position,
                    self.camera.forward(),
                    8.0,
                    |x, y, z| self.world.get_block(x, y, z),
                )
                .map(|h| h.block_pos);

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

                // While the menu is open: draw its dark panel plus a
                // translucent highlight over the hovered row, and shape the
                // menu text (labels + live values + transient message). All
                // geometry derives per frame from the current surface size,
                // so a fullscreen Resized reflows the menu automatically with
                // no cached rects; the text only re-shapes when the composed
                // string changed (StatsOverlay guards that).
                let mut menu_text_area = None;
                if self.menu.is_open() {
                    let (surface_w, surface_h) = (gpu.ctx.config.width, gpu.ctx.config.height);
                    let (mx, my, mw, mh) = menu::panel_rect(surface_w, surface_h);
                    quads.push(Quad {
                        x: mx,
                        y: my,
                        w: mw,
                        h: mh,
                        color: [0.0, 0.0, 0.0, 0.7],
                    });
                    if let Some(cursor) = self.cursor_pos {
                        if let Some(hover) = menu::hovered_row(cursor, surface_w, surface_h) {
                            let index = menu::ROWS
                                .iter()
                                .position(|&r| r == hover)
                                .expect("hovered row is one of ROWS");
                            if let Some((hx, hy, hw, hh)) =
                                menu::row_rect(surface_w, surface_h, index)
                            {
                                quads.push(Quad {
                                    x: hx,
                                    y: hy,
                                    w: hw,
                                    h: hh,
                                    color: [0.35, 0.55, 0.9, 0.3],
                                });
                            }
                        }
                    }
                    let text = compose_menu_text(&self.settings, &self.menu);
                    gpu.menu_text.update(
                        &mut gpu.ui.font_system,
                        surface_w as f32,
                        surface_h as f32,
                        &text,
                    );
                    menu_text_area = Some(
                        gpu.menu_text
                            .text_area(mx + menu::MENU_PAD, my + menu::MENU_PAD),
                    );
                }
                gpu.ui.set_quads(
                    &gpu.ctx.device,
                    &quads,
                    gpu.ctx.config.width,
                    gpu.ctx.config.height,
                );

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
                    &self.camera,
                    self.world.render_distance(),
                    target_block,
                    self.player.selected_slot,
                    ui_pass,
                ) {
                    Ok(visible_chunks) => {
                        self.frame_count += 1;
                        let elapsed = self.title_timer.elapsed().as_secs_f32();
                        if elapsed >= 0.25 {
                            let fps = (self.frame_count as f32 / elapsed).round() as u32;
                            let ms = (elapsed * 1000.0) / self.frame_count.max(1) as f32;
                            let vram_kb = gpu.render_store.total_vram_bytes / 1024;
                            let mode = if self.player.fly_mode { "Fly" } else { "Walk" };
                            let block_name = BlockId::HOTBAR[self.player.selected_slot].name();
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
                                "Minecraft Clone | {} FPS ({:.2} ms) | Chunks: {}/{} | Quads: {} ({} KB VRAM) | Pos: ({:.0}, {:.0}, {:.0}) [{}] | Block [{}]: {} | RD: {} | Seed: {}{}",
                                fps,
                                ms,
                                visible_chunks,
                                self.world.loaded_chunk_count(),
                                gpu.render_store.total_quads,
                                vram_kb,
                                self.player.position.x,
                                self.player.position.y,
                                self.player.position.z,
                                mode,
                                self.player.selected_slot + 1,
                                block_name,
                                self.world.render_distance(),
                                self.world.seed(),
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
                                self.player.position.x,
                                self.player.position.y,
                                self.player.position.z,
                                mode,
                                visible_chunks,
                                self.world.loaded_chunk_count(),
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
                            save_current_world(&self.cli.save_path, &mut self.world, &self.player);
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
                self.player.yaw += delta.0 as f32 * sensitivity;
                self.player.pitch =
                    (self.player.pitch - delta.1 as f32 * sensitivity).clamp(-1.54, 1.54);
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
    let cli = CliConfig::from_iter(std::env::args().skip(1));

    let (world, player) = if !cli.force_new_world && cli.save_path.exists() {
        match load_world_from_path(&cli.save_path) {
            Ok(save) => {
                let mut p = Player::new(Vec3::from_array(save.player.position));
                p.yaw = save.player.yaw;
                p.pitch = save.player.pitch;
                p.fly_mode = save.player.fly_mode;
                p.selected_slot = (save.player.selected_slot as usize).min(8);
                let w = WorldManager::with_seed_and_edits(
                    save.player.render_distance as i32,
                    save.seed,
                    save.chunk_edits,
                );
                (w, p)
            }
            Err(_) => {
                let seed = cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED);
                let spawn_y = (terrain_height_at_seeded(0, 0, seed) + 4) as f32;
                (
                    WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
                    Player::new(Vec3::new(0.5, spawn_y, 0.5)),
                )
            }
        }
    } else {
        let seed = cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED);
        let spawn_y = (terrain_height_at_seeded(0, 0, seed) + 4) as f32;
        (
            WorldManager::with_seed_and_edits(16, seed, ChunkEditMap::default()),
            Player::new(Vec3::new(0.5, spawn_y, 0.5)),
        )
    };

    let event_loop = EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = GameApp::new(cli, world, player);
    event_loop.run_app(&mut app).expect("Event loop error");
}

#[cfg(test)]
mod tests {
    use super::*;

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
            keybinds: minecraft_clone_keybinds_jump_arrow_up(),
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
        let binds = minecraft_clone_keybinds_jump_arrow_up();
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
        let mut binds = Keybinds::default();
        binds.backward = KeyCode::KeyW; // collides with forward
        let mut input = InputState::default();
        assert!(apply_gameplay_key(&mut input, &binds, KeyCode::KeyW, true));
        assert!(input.forward, "forward is declared first and wins");
        assert!(!input.backward, "the duplicate loser stays clear");
    }

    /// Keybinds with jump_or_up rebound to ArrowUp (leaves Space free by
    /// rebinding nothing else) — a valid, conflict-free state.
    fn minecraft_clone_keybinds_jump_arrow_up() -> minecraft_clone::settings::Keybinds {
        let mut binds = minecraft_clone::settings::Keybinds::default();
        binds.jump_or_up = KeyCode::ArrowUp;
        binds
    }
}
