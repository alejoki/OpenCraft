# minecraft-clone — Project

A Minecraft-style voxel sandbox game in Rust: chunked terrain, greedy-meshed rendering on wgpu, first-person player with block break/place and raycast, and — as of M001 — a winit 0.30 event loop, a persisted settings layer (settings.toml), an in-game settings/keybind menu, and a live stats overlay.

## Current State

**Last completed milestone: M001 — Refactor + Settings Foundation (2026-10-19)**

- Event loop migrated from winit 0.29's deprecated closure `run()` to winit 0.30.13's `ApplicationHandler` (GameApp/Ready structs, window + GPU init in `resumed()` via `ActiveEventLoop::create_window`, `Arc<Window>` surface for wgpu 0.19). Gameplay parity human-confirmed.
- `src/settings` pure-logic module: TOML load with two-phase per-field fallback (a bad settings.toml never blocks launch), atomic save (tmp sibling + rename), hand-mapped KeyCode↔stable-string keybind conversion, duplicate-keybind validation. Defaults equal the pre-existing hardcoded values.
- `src/ui` module: glyphon 0.5.0 text stack, pixel-space quad drawing API, color-only second render pass over the world pass, cursor position plumbing — powering both a live always-on stats overlay (FPS/frame ms/position/chunks) and the settings menu.
- In-game settings menu (`src/ui/menu.rs`): Escape toggles with cursor release + InputState reset, menu-first input routing, press-to-rebind for the 7 gameplay keybinds with duplicate/unsupported rejection, live sensitivity/FOV/fullscreen/vsync apply, atomic save on every accepted change plus save-on-quit. Loaded Settings is the single source of truth for video parameters (applied at startup too).
- Test suite: 94/94 green (31 settings, 24 menu, 5 quad, plus pre-existing). Slice surface build-warning-free and clippy-clean.
- Smoke harnesses: `scripts/launch_smoke.sh` (bounded 10 s launch, exit-124/no-panic assertions) and `scripts/settings_smoke.sh` (seeded settings.toml survives a run byte-identical).

## Architecture

- **Event loop / app:** `src/main.rs` — winit 0.30.13 `ApplicationHandler`; `GameApp` (state) + nested `Option<Ready>` (window-dependent GPU stack) initialized in `resumed()`.
- **Rendering:** `src/render/` — wgpu 0.19.4 context and pipeline; world pass plus optional color-only UI pass (second `begin_render_pass`, LoadOp::Load, same encoder) for overlay and menu.
- **UI:** `src/ui/` — `UiRenderer` (glyphon 0.5.0), `StatsOverlay`, pure px→NDC quad API, `menu.rs` pure immediate-mode menu state machine + layout + hit-test (unit-tested before wiring).
- **Settings:** `src/settings/` — `Settings`, `load_with_warnings`, atomic `save`, `validate_keybinds`, `SettingsWarning`/`KeybindConflict` returned as data (crate is log-free by decision D006).
- **World/mesh:** `src/world/`, `src/mesh/` — chunked terrain, greedy meshing (pre-existing; not touched by M001).

## Key Decisions (M001)

- D001: winit pinned to 0.30.13 (not 0.31.0-beta).
- D002: glyphon pinned `=0.5.0` (its wgpu ^0.19 pin; 0.6+ requires the out-of-scope wgpu upgrade).
- D003: keybinds stored as hand-mapped stable strings, decoupled from winit's serde.
- D004: slice order S01→S02→S03→S04 (risk-first, S04 as end-to-end integration).
- D005: two-phase per-field-fallback TOML load; never serde-derive load.
- D006: warnings/conflicts returned as structured data, no logging dependency.
- D007: UI drawn via an optional render pass hook on `render_frame` (least-churn seam).

## Known Limitations

- 13 pre-existing clippy lib warnings in `src/mesh/*`, `src/world/*`, `src/render/texture.rs` predate M001 and are out of its scope (MEM007/MEM017) — candidates for a future cleanup milestone.
- Wayland with vsync=false may fall back (PresentMode::Immediate unavailable) — accepted visual no-op, documented in `apply_vsync`.
- ShiftRight/ControlRight no longer crouch/sprint — intentional keybind narrowing documented in S02's rationale.
- Duplicate bindings in a hand-edited settings.toml load with a warning (never block launch) and resolve first-match in declaration order.

## Next

No further milestones planned yet. Natural candidates: mesh/world lint cleanup, audio, more world generation, inventory/crafting.

## Milestone Sequence

- [ ] M001: Refactor + Settings Foundation — The event loop migrates from winit 0.29's deprecated closure API to winit 0.30.13's ApplicationHandler trait (wgpu stays pinned at 0.19), and the game gains its first persisted player preferences: a serde/TOML settings layer covering keybinds, mouse sensitivity, FOV, and fullscreen/vsync, with per-field fallback so a bad settings.toml never blocks launch.
