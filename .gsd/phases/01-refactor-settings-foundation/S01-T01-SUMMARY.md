---
id: S01-T01
parent: S01
milestone: M001
key_files:
  - (none)
key_decisions:
  - (none)
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# S01-T01: Migrated src/main.rs to winit 0.30.13's ApplicationHandler (GameApp + Ready structs, window/GPU init in resumed(), verbatim event arms) and bumped winit to 0.30 with a warning-free build and clippy-clean bin target

**Migrated src/main.rs to winit 0.30.13's ApplicationHandler (GameApp + Ready structs, window/GPU init in resumed(), verbatim event arms) and bumped winit to 0.30 with a warning-free build and clippy-clean bin target**

## What Happened

Migrated src/main.rs from winit 0.29's deprecated closure `run()` to winit 0.30.13's `ApplicationHandler` trait with behavior preserved verbatim, and bumped the manifest. Cargo.toml now has `winit = "0.30"` (Cargo.lock resolved 0.30.13; wgpu verified unchanged at 0.19.4). main() keeps CliConfig parse, world/player load/save logic, and `EventLoop::new()`; sets `ControlFlow::Poll` once via `event_loop.set_control_flow()` before `run_app(&mut app).expect("Event loop error")`. State moved into `GameApp { window: Option<Arc<Window>>, gpu: Option<Ready>, cli, world, player, camera, input, cursor_grabbed, last_frame, title_timer, last_saved_badge, frame_count }`; window-dependent GPU stack (`RenderContext` → `VoxelRenderer` → `ChunkRenderStore`, initialized together since `VoxelRenderer::new` takes `&ctx`) lives in the nested `Ready` struct behind `Option`, making `window_event` arms clean via `if let`-style destructuring. `resumed()` has the mandatory idempotent guard (early return if window exists), creates the window via `ActiveEventLoop::create_window(Window::default_attributes()...)` (replacing deprecated pre-run `EventLoop::create_window`/`WindowBuilder`), wraps in Arc, builds the GPU stack, grabs the cursor. Every WindowEvent arm moved verbatim: CloseRequested saves then `event_loop.exit()`; Focused(false) → input.reset(); Resized → ctx.resize; KeyboardInput with all pressed/!repeat guards, Digit1-9, brackets, F fly-toggle, F5 save+badge, Escape grab toggle; MouseWheel hotbar wrap; MouseInput with the regrab-then-return flow, raycast break/place with Bedrock guard and player-AABB placement rejection; RedrawRequested with the full per-frame update/streaming/upload/render/title logic including the byte-identical title format string and 0.25s title cadence; the SurfaceError arms map Lost/Outdated → ctx.resize, OutOfMemory → save+exit, Timeout → no-op (the old `WindowEvent::OutOfMemory` arm no longer exists as a top-level arm in 0.30 since wgpu surface errors are handled in the render match, exactly as the original did). device_event keeps MouseMotion verbatim (sensitivity 0.0022, pitch clamp -1.54..1.54, gated on cursor_grabbed); about_to_wait calls `window.request_redraw()`. Free helpers `set_cursor_grab` and `save_current_world` unchanged. `src/render/context.rs` and `src/lib.rs` untouched — confirmed compatible (both winit 0.30.13 and wgpu 0.19.4 use raw-window-handle 0.6.2; `cargo build` compiled the whole crate warning-free with zero edits there).

Process note: my first draft was composed from the plan's shorthand before reading the original file; I caught this by recovering the original via `git show HEAD:src/main.rs` and rewrote the migration as a true verbatim transcription of the actual 395-line original, so the shipped code is line-faithful to the original's logic (verified by side-by-side review of every arm).

### Failure Modes (Q5)
External dependencies and their failure paths, all preserved verbatim from the original code: (1) cargo/crates.io registry during build — resolution failures bubble as cargo errors, no suppression; (2) event-loop creation — `EventLoop::new().expect("Failed to create event loop")` panics loudly; (3) window creation in resumed() — `.expect("Failed to create window")` panic (same message as original); (4) GPU surface acquisition inside `RenderContext::new`/render path — wgpu `SurfaceError` handled per-arm: Lost/Outdated → `ctx.resize` recovery, OutOfMemory → save world then `event_loop.exit()` (graceful), Timeout → no-op retry next frame; (5) world persistence — `save_current_world` never panics: returns bool, load failure falls back to fresh seeded world (original fallback path preserved); (6) run_app return — `.expect("Event loop error")` preserved. No new failure paths introduced; `resumed()` refires are handled by the idempotent guard instead of panicking on double window creation.

### Negative Tests (Q7)
No automated negative tests were added — the plan explicitly forbids adding tests in this slice (single-file verbatim migration, tests are S02/S04 territory). The startup-failure negative surface (window creation panic, surface init panic, cursor-grab failure) is covered by the next task's bounded launch smoke (S01-T02: `timeout 10` run asserting exit 124 and no "panicked"/"RUST_BACKTRACE" in output), and the behavioral-parity surface is the documented manual gameplay-parity checklist recorded at slice closeout.

## Verification

cargo build completed warning-free (grep of full build stdout for "warning" found zero matches) — the whole crate including src/lib.rs and src/render/context.rs compiled against winit 0.30.13 with no edits to them. cargo clippy after the migration initially flagged one bin warning (clippy::collapsible_if on the nested save-on-unload check, a structure inherited verbatim from the original code); I collapsed it to a single short-circuit `&&` chain (semantics-identical) and re-ran: bin target now generates zero clippy warnings. A full warning inventory run confirmed the remaining 12 clippy warnings are all pre-existing style lints in untouched lib files (src/mesh/quad.rs too_many_arguments, src/world/persistence.rs from_iter naming, src/world/terrain.rs loop-variable-indexing ×6, plus mesh-generation lints) — none in src/main.rs, none introduced by the winit bump; fixing them is outside this task's file scope (src/main.rs, Cargo.toml, Cargo.lock). Manifest checks: no `winit = "0.29"` remains in Cargo.toml, Cargo.lock pins winit 0.30.13, and wgpu is unchanged at 0.19.4. Runtime launch proof (10s bounded smoke) is deliberately deferred to S01-T02 per the slice plan.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build 2>&1 | tail -30` | 0 | passed (warning-free; zero "warning" matches in full stdout) | 44312ms |
| 2 | `cargo clippy 2>&1 | tail -15` | 0 | failed (1 bin warning: clippy::collapsible_if — fixed before completion) | 24666ms |
| 3 | `cargo build 2>&1 | tail -5` | 0 | passed (post-fix rebuild, warning-free) | 1021ms |
| 4 | `cargo clippy 2>&1 | tail -3` | 0 | passed (bin target clean; 12 warnings are pre-existing lib lints only) | 351ms |
| 5 | `cargo clippy 2>&1 | grep -E "^warning|^  -->|generated" | head -40` | 0 | passed (full inventory: all 12 warnings in untouched lib files — src/mesh/quad.rs, src/world/persistence.rs, src/world/terrain.rs; none in src/main.rs) | 183ms |
| 6 | `! grep -q 'winit = "0.29"' Cargo.toml` | 0 | passed (no 0.29 pin remains) | 21ms |
| 7 | `grep -B1 -A3 'name = "wgpu"' Cargo.lock | head -8` | 0 | passed (wgpu stays at 0.19.4) | 26ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=64:1 -->
