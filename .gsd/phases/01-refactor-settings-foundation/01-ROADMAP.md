# M001: Refactor + Settings Foundation

**Vision:** The event loop migrates from winit 0.29's deprecated closure API to winit 0.30.13's ApplicationHandler trait (wgpu stays pinned at 0.19), and the game gains its first persisted player preferences: a serde/TOML settings layer covering keybinds, mouse sensitivity, FOV, and fullscreen/vsync, with per-field fallback so a bad settings.toml never blocks launch. A hand-rolled immediate-mode settings/keybind menu (new src/ui module on glyphon 0.5, rendering in a color-only pass over the existing pipeline) lets the player press-to-rebind keys with a duplicate warning and adjust video settings live. When done, a player can rebind jump from Space to Shift, quit, relaunch, and the binding is still in effect.

## Slices

- [x] **S01: winit 0.30 ApplicationHandler migration** `risk:high` `depends:[]`
  > After this: After this: cargo run launches the game on winit 0.30's ApplicationHandler loop with movement, block break/place, and raycast behaving identically to pre-migration (manual smoke check), and cargo build + cargo clippy are clean with winit = 0.30.

- [x] **S02: Settings core with TOML persistence and tests** `risk:medium` `depends:[]`
  > After this: After this: cargo test passes for settings round-trip (load → save → load equality), missing file → defaults, corrupt file → per-field fallback with valid fields kept, unknown keys ignored, and duplicate-keybind rejection — pure logic, no GPU required.

- [x] **S03: UI foundation: glyphon text overlay + quad rendering** `risk:high` `depends:[S01]`
  > After this: After this: the running game renders a live always-on text overlay (e.g. FPS/position readout) in a separate color-only render pass on the existing wgpu 0.19 pipeline, and cursor position/enter/leave events reach the app through the migrated loop.

- [ ] **S04: In-game settings menu: rebind, adjust, persist** `risk:medium` `depends:[S02,S03]`
  > After this: After this: in-game settings menu opens (releases cursor grab, resets InputState), press-to-rebind works with a duplicate warning, sensitivity/FOV/fullscreen/vsync apply live — rebind jump from Space to Shift, quit the process, relaunch, and the binding is still in effect and shown in the menu.

## Boundary Map

- S01 → S03, S04: provides the migrated ApplicationHandler event loop (window_event, device_event, about_to_wait, resumed) and window creation via ActiveEventLoop::create_window returning Arc<Window> for the wgpu 0.19 surface; S03 additionally consumes CursorMoved/CursorLeft events that the pre-migration loop never delivered.
- S02 → S04: provides the Settings module API — Settings struct (keybinds, mouse_sensitivity, fov, fullscreen, vsync), load with per-field fallback + logged warnings, save, defaults, KeyCode↔stable-string conversion, duplicate-keybind validation; consumes nothing upstream (pure logic, no GPU, no event loop dependency).
- S03 → S04: provides the src/ui module — glyphon 0.5 wrappers (FontSystem, SwashCache, TextAtlas, TextRenderer, Viewport), a minimal quad renderer for panels/buttons, color-only UI render pass after the world pass, and cursor hover/click plumbing; consumes S01's loop and cursor events.
- S04: consumes S01's input routing (menu-open key releases cursor grab and calls InputState::reset, gameplay keys route through the S02 keybind map), S02's Settings API, and S03's UI; provides the milestone's user-facing settings capability and its end-to-end restart-persistence proof.
<!-- gsd:state-version=136:1 -->
