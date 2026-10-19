# M001: Refactor + Settings Foundation

**Vision:**

## Slices

- [x] **S01: winit 0.30 ApplicationHandler migration** `risk:medium` `depends:[]`
  > After this: After this: cargo run launches the game on winit 0.30's ApplicationHandler loop with movement, block break/place, and raycast behaving identically to pre-migration (manual smoke check), and cargo build + cargo clippy are clean with winit = 0.30.

- [x] **S02: Settings core with TOML persistence and tests** `risk:medium` `depends:[]`
  > After this: After this: cargo test passes for settings round-trip (load → save → load equality), missing file → defaults, corrupt file → per-field fallback with valid fields kept, unknown keys ignored, and duplicate-keybind rejection — pure logic, no GPU required.

- [x] **S03: UI foundation: glyphon text overlay + quad rendering** `risk:medium` `depends:[]`
  > After this: After this: the running game renders a live always-on text overlay (e.g. FPS/position readout) in a separate color-only render pass on the existing wgpu 0.19 pipeline, and cursor position/enter/leave events reach the app through the migrated loop.

- [x] **S04: In-game settings menu: rebind, adjust, persist** `risk:medium` `depends:[]`
  > After this: After this: in-game settings menu opens (releases cursor grab, resets InputState), press-to-rebind works with a duplicate warning, sensitivity/FOV/fullscreen/vsync apply live — rebind jump from Space to Shift, quit the process, relaunch, and the binding is still in effect and shown in the menu.
<!-- gsd:state-version=27:1 -->
