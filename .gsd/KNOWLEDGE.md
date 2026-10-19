# Project Knowledge

Append-only register of project-specific rules, patterns, and lessons learned.
Agents read this before every unit. Add entries when you discover something worth remembering.

## Rules

| # | Scope | Rule | Why | Added |
|---|-------|------|-----|-------|

## Patterns

| # | Pattern | Where | Notes |
|---|---------|-------|-------|
| P001 | Per-field TOML fallback in src/settings/mod.rs requires a two-phase parse: first `toml::from_str::<toml::Value>` into a generic tree, then independent per-field extraction with type/range checks. Plain `#[serde(default)]` derive cannot be used for loading — serde aborts the whole struct on any field type error, losing valid siblings. Add new settings fields by extending the phase-2 lookups (see load_keybind_action / load_clamped_f32 / load_bool helpers). | — | — |
| P002 | Settings persistence (src/settings/mod.rs) uses a private Serialize-only mirror struct (SettingsToml/KeybindsToml/MouseToml/VideoToml, serde derive with Serialize only, never Deserialize) plus atomic write (create_dir_all on parent, write to <name>.toml.tmp sibling, sync_all, fs::rename over target). Loading deliberately bypasses serde (see P001 two-phase parse). When adding a settings field: extend BOTH the phase-2 lookups in load_with_warnings and the save mirror, and keep the round-trip test (save -> load_with_warnings == original, zero warnings) as the guard locking the two halves together. A KeyCode lacking a key_to_string mapping is treated as a save-time bug: panic with the action name, guarded by tests. | — | — |
| P003 | src/ui quad rendering (M001-S03) reuses the existing OverlayVertex { clip_pos: [f32;4], color: [f32;4] } + pass-through WGSL pattern with a TriangleList pipeline, depth_stencil None, alpha blend: quads are specified in physical pixels (top-left origin, y down) and converted to NDC on the CPU (x_ndc = 2x/w − 1, y flipped: y_ndc = 1 − 2y/h), emitting 6 vertices per quad (0,1,2, 2,3,0 expanded — no shared index buffer). Same pixel space as ctx.config, cosmic-text buffers, and the cursor_pos plumbing, so S04 hover hit-testing works in px with no scale-factor math at scale 1. | — | — |
| P004 | S04's settings menu consumes the pixel-space quad API from src/ui/quad.rs: build a `Quad { x, y, w, h, color }` list in physical pixels (top-left origin, y down), call `UiRenderer::set_quads(&device, &quads, surface_w, surface_h)` once per frame before render_frame, and quads draw first in the UI pass (before glyphon text) via UiPass. NDC conversion (with y-flip: y_ndc = 1 − 2y/h) is CPU-side in quads_to_vertices; pipelines in the UI pass must use depth_stencil None (color-only pass, LoadOp::Load). | — | — |

## Lessons Learned

| # | What Happened | Root Cause | Fix | Scope |
|---|--------------|------------|-----|-------|
| L001 | winit 0.30.13's ApplicationHandler trait methods take `&ActiveEventLoop` (not `&dyn ActiveEventLoop`); `EventLoop::set_control_flow(ControlFlow::Poll)` is called once before `run_app`, window creation moves into `resumed()` via `event_loop.create_window(Window::default_attributes()...)`, and the migrated `GameApp`/`Ready` struct pattern lives in src/main.rs of the M001 worktree (S03/S04 build on this loop; window+GPU stack is behind `Option`, guarded idempotently in resumed()). | — | — | project |
| L002 | The workspace has ~12 pre-existing clippy lib warnings (needless_range_loop etc. in src/mesh/greedy.rs, src/mesh/quad.rs, src/world/manager.rs, src/world/terrain.rs) that predate M001-S01; the bin target (src/main.rs) is clippy-clean. Downstream slice verification should not treat these lib lints as a regression — scope clippy checks to the files the slice touched, or record the inherited findings as known limitations. | — | — | project |
| L003 | In this wgpu 0.19 project, glyphon 0.5.0's `render<'pass>(&'pass self, atlas: &'pass TextAtlas, pass: &mut RenderPass<'pass>)` cannot be driven through a `dyn FnMut(&mut wgpu::RenderPass)` closure hook: wgpu 0.19's RenderPass carries a lifetime and the higher-ranked closure forces the captured atlas/renderer borrows to be 'static (E0521/E0502). The working pattern is a trait with a lifetime-typed method: `trait UiPass { fn draw_ui<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>); }` implemented on the owner of the glyphon stack, passed as `Option<&dyn UiPass>` to render_frame. Also: glyphon 0.5.0's ColorMode has only `Accurate`/`Web` variants (no `Color`), and cosmic-text 0.10's Buffer methods (set_size/set_text/shape_until_scroll) all take `&mut FontSystem` first. | — | — | minecraft-clone |
