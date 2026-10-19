---
id: S01
parent: M001
milestone: M001
provides:
  - winit 0.30 ApplicationHandler event loop (resumed/window_event/device_event/about_to_wait) with ControlFlow::Poll set once pre-run
  - Window creation via ActiveEventLoop::create_window returning Arc<Window> for the wgpu 0.19 surface (contract for S03/S04)
  - Preserved event routing: CursorMoved/CursorLeft reach window_event (new capability S03 consumes), ungrabbed-click regrab, Focused(false) input reset, close/OOM save-then-exit
  - scripts/launch_smoke.sh — reusable bounded launch smoke harness with display attachment (MEM006) for S03/S04 smokes
requires:
  []
affects:
  - S03
  - S04
key_files:
  - src/main.rs
  - Cargo.toml
  - Cargo.lock
  - scripts/launch_smoke.sh
key_decisions: []
patterns_established:
  - (none)
observability_surfaces:
  - none
drill_down_paths:
  - .gsd/phases/01-refactor-settings-foundation/S01-T01-SUMMARY.md
  - .gsd/phases/01-refactor-settings-foundation/S01-T02-SUMMARY.md
  - scripts/launch_smoke.sh
duration: ""
verification_result: passed
completed_at: 2026-10-19T07:43:05.390Z
blocker_discovered: false
---

# S01: winit 0.30 ApplicationHandler migration

**Migrated src/main.rs from winit 0.29's deprecated closure run() to winit 0.30.13's ApplicationHandler (GameApp/Ready structs, window+GPU init in resumed() via ActiveEventLoop::create_window, verbatim event arms) with a warning-free build, clippy-clean bin target, and a passing 10s launch smoke.**

## What Happened

src/main.rs was rewritten from winit 0.29's deprecated closure run() API to winit 0.30.13's ApplicationHandler trait with byte-identical behavior. State formerly captured by the closure moved into a GameApp struct (cli, world, player, camera, input, cursor_grabbed, timers, frame/badge counters) with a nested Option<Ready> holding the window-dependent GPU stack (RenderContext, VoxelRenderer, ChunkRenderStore) that initializes together in resumed(). resumed() creates the window via ActiveEventLoop::create_window(Window::default_attributes()...) wrapped in Arc (idempotent guard against refires), builds the wgpu 0.19 surface stack, derives the camera from the player, grabs the cursor, and initializes timers. Every WindowEvent/DeviceEvent arm moved verbatim: CloseRequested saves then event_loop.exit(), Focused(false) resets input, KeyboardInput keeps all !event.repeat guards and Digit1-9 hotbar arms, MouseInput keeps the ungrabbed-click regrab early-return and raycast break/place with player-AABB placement rejection, RedrawRequested keeps the full per-frame update/streaming/render/title logic, and DeviceEvent::MouseMotion keeps the 0.0022 sensitivity and -1.54..1.54 pitch clamp gated on cursor_grabbed. request_redraw moved to about_to_wait(), and ControlFlow::Poll is set once before run_app(). src/render/context.rs and src/lib.rs required zero edits (rwh 0.6.2 shared by winit 0.30.13 and wgpu 0.19.4). scripts/launch_smoke.sh provides bounded proof the migrated loop actually runs: timeout-kills the game after 10s and asserts exit 124 with no panic in captured output. S01-T02 recorded the agent-shell display gotcha (Wayland socket, not Xwayland :0) as MEM006 for S03/S04 smokes.

## Verification

All slice-level checks passed via gsd_exec: (1) manifest probe — winit = "0.30" in Cargo.toml, Cargo.lock resolves winit 0.30.13, no winit = "0.29" remaining (0b3b004c, exit 0); (2) cargo build warning-free (99ba1ef3, BUILD_CLEAN); (3) cargo clippy — bin target/src/main.rs (the migration surface) has zero findings (c9b793a8, 002fa9ae); the 12 reported warnings are pre-existing lib lints in src/mesh/* and src/world/* files this slice never touched, left in place per the plan's explicit no-refactor scope (captured as MEM007); (4) bounded launch smoke — bash scripts/launch_smoke.sh exited 0 in 10.0s with "SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic" (5f8acce5): the game ran the full 10 seconds on the migrated ApplicationHandler loop under timeout (exit 124, not an early exit or panic). Full gameplay parity remains NEEDS-HUMAN per the plan's proof level and is documented in the UAT checklist.

## Requirements Advanced

None.

## Requirements Validated

None.

## New Requirements Surfaced

None.

## Requirements Invalidated or Re-scoped

None.

## Operational Readiness

None.

## Deviations

Full-workspace cargo clippy reports 12 pre-existing lib warnings (needless_range_loop and similar in src/mesh/greedy.rs, src/mesh/quad.rs, src/world/manager.rs, src/world/terrain.rs) that predate this slice — the slice plan's "cargo clippy is clean" must-have is satisfied on the migration surface (bin target / src/main.rs, zero findings; also zero rustc warnings across the whole crate), and the plan explicitly forbade refactoring any file outside src/main.rs, so the inherited lib lints were left untouched and documented as a known limitation (captured as MEM007/L002 for downstream slice closers). No other deviations.

## Known Limitations

Full-workspace clippy is not lint-clean: 12 pre-existing lib warnings in src/mesh/*, src/world/* predate this slice (see Deviations). Window creation in resumed() is platform-multiplexed (Wayland vs X11 per MEM006); the smoke script attaches the Wayland socket explicitly. Full gameplay parity is inherently manual and remains NEEDS-HUMAN follow-up (see UAT checklist).

## Follow-ups

None.

## Files Created/Modified

- `src/main.rs` — Migrated from winit 0.29 closure run() to 0.30 ApplicationHandler: GameApp + Ready structs, window/GPU init in resumed() with idempotent guard, verbatim event arms
- `Cargo.toml` — winit bumped from 0.29 to 0.30
- `Cargo.lock` — Resolves winit 0.30.13
- `scripts/launch_smoke.sh` — Bounded 10s launch smoke (timeout-kill assertion, no-panic assertion, /tmp-isolated save)
<!-- gsd:state-version=61:1 -->
