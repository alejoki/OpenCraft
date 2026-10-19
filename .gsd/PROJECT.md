# minecraft-clone — Project

A Minecraft-style voxel sandbox game in Rust: chunked terrain, greedy-meshed rendering on wgpu, first-person player with block break/place and raycast, and — as of M001 — a winit 0.30 event loop, a persisted settings layer (settings.toml), an in-game settings/keybind menu, and a live stats overlay.

## Current State

**M001 closeout submitted for validation: Refactor + Settings Foundation (2026-10-19)**

- Event loop migrated from winit 0.29's deprecated closure `run()` to winit 0.30.13's `ApplicationHandler` (GameApp/Ready structs, window + GPU init in `resumed()` via `ActiveEventLoop::create_window`, `Arc<Window>` surface for wgpu 0.19). Gameplay parity was human-confirmed.
- `src/settings` is a pure-logic module with TOML load using two-phase per-field fallback, atomic save (tmp sibling + rename), hand-mapped KeyCode to stable-string conversion, duplicate-keybind validation, and defaults matching the pre-existing hardcoded values.
- `src/ui` provides the glyphon 0.5.0 text stack, pixel-space quad drawing, a color-only second render pass, cursor position plumbing, a live stats overlay, and the settings menu.
- The settings menu opens with cursor release and InputState reset; supports press-to-rebind for seven gameplay keybinds with duplicate/unsupported rejection; applies sensitivity, FOV, fullscreen, and vsync live; and persists accepted changes and quit state atomically.
- Test suite: 94/94 green (31 settings, 24 menu, 5 quad, plus pre-existing). Slice-surface build-warning-free and clippy-clean; full-tree cargo build reports 0 warnings.
- Smoke harnesses: `scripts/launch_smoke.sh` (bounded 10 s launch, exit-124/no-panic assertions) and `scripts/settings_smoke.sh` (seeded settings.toml survives a run byte-identically).
- Validation receipt: milestone validation passed in remediation round 3, with Contract, Integration, Operational, and UAT classes all passing. Interactive UAT on 2026-10-19 confirmed gameplay parity, menu behavior, live settings application, and relaunch persistence; durable evidence includes a game-written `crouch_or_down = "Tab"` binding.

## Architecture

- **Event loop / app:** `src/main.rs` — winit 0.30.13 `ApplicationHandler`; `GameApp` plus nested `Option<Ready>` for window-dependent GPU initialization in `resumed()`.
- **Rendering:** `src/render/` — wgpu 0.19.4 context and pipeline; world pass plus optional color-only UI pass (second `begin_render_pass`, LoadOp::Load, same encoder/view) for overlay and menu, driven through the lifetime-typed `UiPass` trait.
- **UI:** `src/ui/` — `UiRenderer` (glyphon 0.5.0), `StatsOverlay`, pure px-to-NDC quad API, and `menu.rs` pure immediate-mode menu state machine with unit tests.
- **Settings:** `src/settings/` — `Settings`, `load_with_warnings`, atomic `save`, `validate_keybinds`, `SettingsWarning` and `KeybindConflict` returned as data; no logging dependency.
- **World/mesh:** `src/world/`, `src/mesh/` — chunked terrain and greedy meshing, pre-existing and not changed by M001.

## Key Decisions (M001)

- D001: pin winit to 0.30.13 rather than 0.31.0-beta.
- D002: pin glyphon to `=0.5.0` for compatibility with wgpu 0.19.
- D003: store keybinds as hand-mapped stable strings, independent of winit serde.
- D004: execute foundations in risk order S01/S02, then UI S03, then end-to-end menu S04.
- D005: use two-phase per-field-fallback TOML loading rather than serde-derived loading.
- D006: return warnings and conflicts as structured data without adding a logging dependency.
- D007: draw UI through a second color-only render pass on the existing encoder/view, implemented by the lifetime-typed `UiPass` trait.
- D008: use loaded Settings as the single source of truth for video parameters at startup and during live changes.
- D009: atomically save on every accepted menu change and on quit.

## Known Limitations

- 13 pre-existing clippy lib warnings in `src/mesh/*`, `src/world/*`, and `src/render/texture.rs` predate M001 and remain out of scope; they are candidates for a future cleanup milestone.
- Wayland with vsync=false may fall back when PresentMode::Immediate is unavailable; this is an accepted visual no-op documented in `apply_vsync`.
- ShiftRight and ControlRight no longer crouch/sprint; this intentional keybind narrowing is documented in S02.
- Duplicate bindings in a hand-edited settings.toml load with a warning, never block launch, and resolve first-match in declaration order.

## Next

No further milestones are planned yet. Natural candidates are mesh/world lint cleanup, audio, more world generation, and inventory/crafting.

## Milestone Sequence

- [ ] M001: Refactor + Settings Foundation — winit 0.30.13 ApplicationHandler migration, resilient TOML settings persistence, UI overlay foundation, and interactive settings menu with live application and relaunch persistence.
