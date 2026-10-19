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
use winit::window::{CursorGrabMode, Window, WindowId};

use minecraft_clone::player::camera::Camera;
use minecraft_clone::player::physics::{InputState, Player};
use minecraft_clone::player::raycast::raycast_voxels;
use minecraft_clone::render::buffer::ChunkRenderStore;
use minecraft_clone::render::context::RenderContext;
use minecraft_clone::render::pipeline::{UiPass, VoxelRenderer};
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

/// Window-dependent GPU stack, initialized together in `resumed()`.
struct Ready {
    ctx: RenderContext,
    renderer: VoxelRenderer,
    render_store: ChunkRenderStore,
    ui: UiRenderer,
    stats: StatsOverlay,
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
}

impl GameApp {
    fn new(cli: CliConfig, world: WorldManager, player: Player) -> Self {
        let mut camera = Camera::new(player.eye_position());
        camera.yaw = player.yaw;
        camera.pitch = player.pitch;
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

        let ctx = RenderContext::new(window.clone());
        let renderer = VoxelRenderer::new(&ctx);
        let render_store = ChunkRenderStore::new();
        let mut ui = UiRenderer::new(&ctx);
        let stats = StatsOverlay::new(&mut ui.font_system);

        self.cursor_grabbed = true;
        set_cursor_grab(&window, true);

        self.window = Some(window);
        self.gpu = Some(Ready {
            ctx,
            renderer,
            render_store,
            ui,
            stats,
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
                    match code {
                        KeyCode::KeyW => self.input.forward = pressed,
                        KeyCode::KeyS => self.input.backward = pressed,
                        KeyCode::KeyA => self.input.left = pressed,
                        KeyCode::KeyD => self.input.right = pressed,
                        KeyCode::Space => self.input.jump_or_up = pressed,
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => {
                            self.input.crouch_or_down = pressed;
                        }
                        KeyCode::ControlLeft | KeyCode::ControlRight => {
                            self.input.sprint = pressed;
                        }
                        KeyCode::KeyF if pressed && !event.repeat => {
                            self.player.fly_mode = !self.player.fly_mode;
                            self.player.velocity = Vec3::ZERO;
                        }
                        KeyCode::F5 if pressed && !event.repeat => {
                            if save_current_world(&self.cli.save_path, &mut self.world, &self.player)
                            {
                                self.last_saved_badge = Some(Instant::now());
                            }
                        }
                        KeyCode::Escape if pressed && !event.repeat => {
                            self.cursor_grabbed = !self.cursor_grabbed;
                            set_cursor_grab(window, self.cursor_grabbed);
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
                        _ => {}
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
                                    self.world.set_block_immediate(bp.x, bp.y, bp.z, BlockId::Air);
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
                            if !self
                                .player
                                .aabb()
                                .intersects_block(ap.x, ap.y, ap.z)
                            {
                                let place_block = BlockId::HOTBAR[self.player.selected_slot];
                                let remeshed =
                                    self.world.set_block_immediate(ap.x, ap.y, ap.z, place_block);
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
                let panel = Quad {
                    x: OVERLAY_TEXT_LEFT - 6.0,
                    y: OVERLAY_TEXT_TOP - 4.0,
                    w: panel_w,
                    h: LINE_HEIGHT + 8.0,
                    color: [0.0, 0.0, 0.0, 0.4],
                };
                gpu.ui.set_quads(
                    &gpu.ctx.device,
                    std::slice::from_ref(&panel),
                    gpu.ctx.config.width,
                    gpu.ctx.config.height,
                );

                // UI overlay: prepare glyphs each frame; on prepare error
                // (glyph atlas full) the UI pass is skipped for that frame
                // (the crate is log-free by convention).
                let text_area = gpu.stats.text_area(OVERLAY_TEXT_LEFT, OVERLAY_TEXT_TOP);
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
                        [text_area],
                        &mut gpu.ui.swash_cache,
                    )
                    .is_ok();
                let ui_pass: Option<&dyn UiPass> = if prepare_ok {
                    Some(&gpu.ui)
                } else {
                    None
                };

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
                let sensitivity = 0.0022;
                self.player.yaw += delta.0 as f32 * sensitivity;
                self.player.pitch = (self.player.pitch - delta.1 as f32 * sensitivity)
                    .clamp(-1.54, 1.54);
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
    event_loop
        .run_app(&mut app)
        .expect("Event loop error");
}
