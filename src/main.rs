use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use winit::dpi::PhysicalSize;
use winit::event::{
    DeviceEvent, ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, WindowBuilder};

use minecraft_clone::player::camera::Camera;
use minecraft_clone::player::physics::{InputState, Player};
use minecraft_clone::player::raycast::raycast_voxels;
use minecraft_clone::render::buffer::ChunkRenderStore;
use minecraft_clone::render::context::RenderContext;
use minecraft_clone::render::pipeline::VoxelRenderer;
use minecraft_clone::world::block::BlockId;
use minecraft_clone::world::chunk::world_to_chunk_and_local;
use minecraft_clone::world::manager::WorldManager;
use minecraft_clone::world::persistence::{
    load_world_from_path, save_world_to_path, ChunkEditMap, CliConfig, SavedPlayerState,
    WorldSaveData,
};
use minecraft_clone::world::terrain::{terrain_height_at_seeded, DEFAULT_WORLD_SEED};

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

fn main() {
    let cli = CliConfig::from_iter(std::env::args().skip(1));

    let (mut world, mut player) = if !cli.force_new_world && cli.save_path.exists() {
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
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Rust Voxel Engine — Initializing...")
            .with_inner_size(PhysicalSize::new(1280, 720))
            .build(&event_loop)
            .expect("Failed to create window"),
    );

    let mut ctx = RenderContext::new(window.clone());
    let renderer = VoxelRenderer::new(&ctx);
    let mut render_store = ChunkRenderStore::new();

    let mut camera = Camera::new(player.eye_position());
    camera.yaw = player.yaw;
    camera.pitch = player.pitch;
    let mut input = InputState::default();
    let mut cursor_grabbed = true;
    set_cursor_grab(&window, true);

    let mut last_frame = Instant::now();
    let mut title_timer = Instant::now();
    let mut last_saved_badge: Option<Instant> = None;
    let mut frame_count = 0u32;

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Poll);

            match event {
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => {
                        let _ = save_current_world(&cli.save_path, &mut world, &player);
                        elwt.exit();
                    }
                    WindowEvent::Focused(false) => {
                        input.reset();
                    }
                    WindowEvent::Resized(new_size) => {
                        ctx.resize(new_size);
                    }
                    WindowEvent::KeyboardInput { event, .. } => {
                        let pressed = event.state == ElementState::Pressed;
                        if let PhysicalKey::Code(code) = event.physical_key {
                            match code {
                                KeyCode::KeyW => input.forward = pressed,
                                KeyCode::KeyS => input.backward = pressed,
                                KeyCode::KeyA => input.left = pressed,
                                KeyCode::KeyD => input.right = pressed,
                                KeyCode::Space => input.jump_or_up = pressed,
                                KeyCode::ShiftLeft | KeyCode::ShiftRight => {
                                    input.crouch_or_down = pressed;
                                }
                                KeyCode::ControlLeft | KeyCode::ControlRight => {
                                    input.sprint = pressed;
                                }
                                KeyCode::KeyF if pressed && !event.repeat => {
                                    player.fly_mode = !player.fly_mode;
                                    player.velocity = Vec3::ZERO;
                                }
                                KeyCode::F5 if pressed && !event.repeat => {
                                    if save_current_world(&cli.save_path, &mut world, &player) {
                                        last_saved_badge = Some(Instant::now());
                                    }
                                }
                                KeyCode::Escape if pressed && !event.repeat => {
                                    cursor_grabbed = !cursor_grabbed;
                                    set_cursor_grab(&window, cursor_grabbed);
                                }
                                KeyCode::BracketLeft if pressed && !event.repeat => {
                                    let rd = (world.render_distance() - 2).max(4);
                                    world.set_render_distance(rd);
                                }
                                KeyCode::BracketRight if pressed && !event.repeat => {
                                    let rd = (world.render_distance() + 2).min(36);
                                    world.set_render_distance(rd);
                                }
                                KeyCode::Digit1 if pressed => player.selected_slot = 0,
                                KeyCode::Digit2 if pressed => player.selected_slot = 1,
                                KeyCode::Digit3 if pressed => player.selected_slot = 2,
                                KeyCode::Digit4 if pressed => player.selected_slot = 3,
                                KeyCode::Digit5 if pressed => player.selected_slot = 4,
                                KeyCode::Digit6 if pressed => player.selected_slot = 5,
                                KeyCode::Digit7 if pressed => player.selected_slot = 6,
                                KeyCode::Digit8 if pressed => player.selected_slot = 7,
                                KeyCode::Digit9 if pressed => player.selected_slot = 8,
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
                            player.selected_slot =
                                (player.selected_slot as isize + step).rem_euclid(9) as usize;
                        }
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button,
                        ..
                    } => {
                        if !cursor_grabbed {
                            cursor_grabbed = true;
                            set_cursor_grab(&window, true);
                            return;
                        }

                        if let Some(hit) = raycast_voxels(
                            camera.position,
                            camera.forward(),
                            8.0,
                            |x, y, z| world.get_block(x, y, z),
                        ) {
                            match button {
                                MouseButton::Left => {
                                    let bp = hit.block_pos;
                                    if world.get_block(bp.x, bp.y, bp.z) != BlockId::Bedrock {
                                        let remeshed = world.set_block_immediate(
                                            bp.x,
                                            bp.y,
                                            bp.z,
                                            BlockId::Air,
                                        );
                                        for (cpos, mesh) in remeshed {
                                            render_store.upload_mesh(
                                                &ctx.device,
                                                &renderer.chunk_bind_group_layout,
                                                cpos,
                                                &mesh,
                                            );
                                        }
                                    }
                                }
                                MouseButton::Right => {
                                    let ap = hit.adjacent_pos;
                                    if !player.aabb().intersects_block(ap.x, ap.y, ap.z) {
                                        let place_block = BlockId::HOTBAR[player.selected_slot];
                                        let remeshed = world.set_block_immediate(
                                            ap.x,
                                            ap.y,
                                            ap.z,
                                            place_block,
                                        );
                                        for (cpos, mesh) in remeshed {
                                            render_store.upload_mesh(
                                                &ctx.device,
                                                &renderer.chunk_bind_group_layout,
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
                        let dt = (now - last_frame).as_secs_f32();
                        last_frame = now;

                        player.update(dt, &input, |x, y, z| world.get_block(x, y, z));
                        camera.position = player.eye_position();
                        camera.yaw = player.yaw;
                        camera.pitch = player.pitch;

                        let (player_chunk, _) = world_to_chunk_and_local(
                            player.position.x.floor() as i32,
                            player.position.y.floor() as i32,
                            player.position.z.floor() as i32,
                        );
                        let (new_meshes, unloaded) = world.update_streaming(player_chunk);
                        if !unloaded.is_empty() && world.has_unsaved_changes() {
                            if save_current_world(&cli.save_path, &mut world, &player) {
                                last_saved_badge = Some(Instant::now());
                            }
                        }
                        for upos in unloaded {
                            render_store.remove_chunk(upos);
                        }
                        for (cpos, mesh) in new_meshes {
                            render_store.upload_mesh(
                                &ctx.device,
                                &renderer.chunk_bind_group_layout,
                                cpos,
                                &mesh,
                            );
                        }

                        let target_block = raycast_voxels(
                            camera.position,
                            camera.forward(),
                            8.0,
                            |x, y, z| world.get_block(x, y, z),
                        )
                        .map(|h| h.block_pos);

                        match renderer.render_frame(
                            &ctx,
                            &render_store,
                            &camera,
                            world.render_distance(),
                            target_block,
                            player.selected_slot,
                        ) {
                            Ok(visible_chunks) => {
                                frame_count += 1;
                                let elapsed = title_timer.elapsed().as_secs_f32();
                                if elapsed >= 0.25 {
                                    let fps = (frame_count as f32 / elapsed).round() as u32;
                                    let ms = (elapsed * 1000.0) / frame_count.max(1) as f32;
                                    let vram_kb = render_store.total_vram_bytes / 1024;
                                    let mode = if player.fly_mode { "Fly" } else { "Walk" };
                                    let block_name = BlockId::HOTBAR[player.selected_slot].name();
                                    let save_tag = if last_saved_badge
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
                                        world.loaded_chunk_count(),
                                        render_store.total_quads,
                                        vram_kb,
                                        player.position.x,
                                        player.position.y,
                                        player.position.z,
                                        mode,
                                        player.selected_slot + 1,
                                        block_name,
                                        world.render_distance(),
                                        world.seed(),
                                        save_tag,
                                    ));
                                    title_timer = Instant::now();
                                    frame_count = 0;
                                }
                            }
                            Err(
                                wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated,
                            ) => {
                                ctx.resize(ctx.size);
                            }
                            Err(wgpu::SurfaceError::OutOfMemory) => {
                                let _ = save_current_world(&cli.save_path, &mut world, &player);
                                elwt.exit();
                            }
                            Err(wgpu::SurfaceError::Timeout) => {}
                        }
                    }
                    _ => {}
                },
                Event::DeviceEvent {
                    event: DeviceEvent::MouseMotion { delta },
                    ..
                } => {
                    if cursor_grabbed {
                        let sensitivity = 0.0022;
                        player.yaw += delta.0 as f32 * sensitivity;
                        player.pitch = (player.pitch - delta.1 as f32 * sensitivity)
                            .clamp(-1.54, 1.54);
                    }
                }
                Event::AboutToWait => {
                    window.request_redraw();
                }
                _ => {}
            }
        })
        .expect("Event loop error");
}
