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
use minecraft_clone::world::terrain::terrain_height_at;

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

fn main() {
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
    let mut world = WorldManager::new(16);

    let spawn_y = (terrain_height_at(0, 0) + 4) as f32;
    let mut player = Player::new(Vec3::new(0.5, spawn_y, 0.5));
    let mut camera = Camera::new(player.eye_position());
    let mut input = InputState::default();
    let mut cursor_grabbed = true;
    set_cursor_grab(&window, true);

    let mut last_frame = Instant::now();
    let mut title_timer = Instant::now();
    let mut frame_count = 0u32;

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Poll);

            match event {
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => elwt.exit(),
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
                                    window.set_title(&format!(
                                        "Minecraft Clone | {} FPS ({:.2} ms) | Chunks: {}/{} | Quads: {} ({} KB VRAM) | Pos: ({:.0}, {:.0}, {:.0}) [{}] | Block [{}]: {} | RD: {}",
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
