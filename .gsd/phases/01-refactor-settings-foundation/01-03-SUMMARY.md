---
id: S03
parent: M001
milestone: M001
provides:
  - src/ui module: UiRenderer (glyphon FontSystem/SwashCache/TextAtlas/TextRenderer), StatsOverlay (reusable buffer, 4 Hz text updates, resize-aware)
  - Quad px-space drawing API: quad_to_vertices/quads_to_vertices (pure, 6 verts/quad, NDC conversion with y flip) + TriangleList alpha-blended color-only quad pipeline
  - render_frame optional UI pass hook: second color-only begin_render_pass on the existing encoder between world pass and submit/present
  - GameApp.cursor_pos: Option<(f32,f32)> physical-pixel cursor state fed by CursorMoved/CursorLeft through the migrated loop
  - A live always-on stats overlay rendered over the running game
requires:
  - slice: S01
    provides: the migrated ApplicationHandler event loop, Arc<Window> window, Ready GPU stack, and CursorMoved/CursorLeft delivery to window_event
affects:
  []
key_files:
  - src/ui/mod.rs
  - src/ui/text.rs
  - src/ui/quad.rs
  - src/render/pipeline.rs
  - src/main.rs
  - Cargo.toml
key_decisions:
  - D007: optional UI pass hook on render_frame — second begin_render_pass on the existing encoder (color-only, LoadOp::Load) between world pass and submit/present; no second surface texture or encoder, no world-pass changes
  - D002: glyphon pinned =0.5.0 (wgpu ^0.19 exact, winit dev-only) — docs referenced at 0.5.0, never latest
  - MEM017: clippy inherited-warning inventory refined — 13 unique warnings (mesh/world plus 1 pre-existing in src/render/texture.rs); slice surface clean
patterns_established:
  - Optional UI render pass seam on render_frame (D007): second begin_render_pass, color-only, LoadOp::Load, same encoder/view, between world pass and present
  - Pixel-space quad drawing API: quad_to_vertices/quads_to_vertices (px → NDC on CPU, 6 verts, y flip, pure and unit-testable) for S04 panels/buttons
  - Overlay text rebuilt only on string change at the 0.25 s cadence, never per frame (set_text ~0.1–1.3 ms)
observability_surfaces:
  - On-screen stats overlay (FPS, frame ms, position, mode, chunk counts) in the live game — the new user-facing observability surface
drill_down_paths:
  []
duration: ""
verification_result: passed
completed_at: 2026-10-19T08:42:48.349Z
blocker_discovered: false
---

# S03: UI foundation: glyphon text overlay + quad rendering

**A src/ui module with a glyphon 0.5.0 text stack and a pixel-space quad renderer draws a live stats overlay (FPS/position readout over a translucent panel) in a color-only second render pass on the existing wgpu 0.19 pipeline, plus CursorMoved/CursorLeft capture into GameApp state.**

## What Happened

Three tasks delivered the slice. T01 added glyphon = "=0.5.0" (Cargo.toml) and a src/ui module: UiRenderer owning the glyphon stack (FontSystem, SwashCache, TextAtlas with ColorMode::Color, TextRenderer with depth_stencil None — the UI pass has no depth attachment), constructed once in resumed() after RenderContext::new. StatsOverlay holds the reusable glyphon Buffer; update() re-shapes only when the stats string changed (set_text/shape_until_scroll at the 0.25 s cadence, never per frame) and re-set_size on resize so text never stretches. render_frame gained the optional UI pass hook (pipeline.rs): a second begin_render_pass on the same encoder and view, color-only (no depth_stencil_attachment), LoadOp::Load, drawn between the world pass and submit/present — per D007 the least-churn seam into the existing submit-and-present-inside-method structure. src/main.rs builds the overlay stats string from the existing title-timer stats block, calls ui.prepare each frame (Resolution from ctx.config), and passes a closure/UiPass that draws quads first, then glyphon text (text on top of panel). Prepare errors skip the UI pass that frame, log-free per D006.

T02 added src/ui/quad.rs: pure functions quad_to_vertices and quads_to_vertices converting physical-pixel top-left-origin quads (y down) to NDC on the CPU (y flip) via the existing OverlayVertex type, 6 vertices per quad, plus a TriangleList alpha-blended color-only pipeline reusing the pass-through WGSL pattern; a translucent dark panel quad is drawn behind the stats text each frame as the visible proof. Five pure unit tests cover full-surface mapping (corners at ±1, y-flip direction), partial quads, concatenation order, empty lists, and degenerate surface dims (no division by zero).

T03 added the cursor plumbing: GameApp gains cursor_pos: Option<(f32,f32)> (physical pixels), WindowEvent::CursorMoved stores position, WindowEvent::CursorLeft clears it — match arms placed before the existing catch-all with no existing-arm behavior change. This is the hover/click hit-testing surface S04 consumes.

Integration: the UI pass hooks into S01's ApplicationHandler loop (resumed constructs UiRenderer; RedrawRequested drives prepare+render; CursorMoved/CursorLeft route through the migrated window_event match). Downstream S04 consumes the src/ui module (quad px-space API, glyphon stack, cursor_pos) for the settings menu.

## Verification

All four slice gates passed via gsd_exec evidence: (1) cargo build warning-free — forced recompile of every slice-touched file, exit 0, 0 warnings [gsd_exec 5e238319]; (2) cargo test — 59/59 pass including 5 new ui::quad unit tests (full-surface NDC corners with y-flip, partial quads, concat order, empty list, degenerate dims) and the existing overlay-vertex pipeline test untouched [gsd_exec bea52dd7]; (3) cargo clippy — exit 0, zero findings on the slice surface (src/ui/*, src/render/pipeline.rs, src/main.rs); the 13 inherited lib warnings are in src/mesh/*, src/world/* plus one pre-existing in src/render/texture.rs (out of scope per MEM007; inventory recorded as MEM017) [gsd_exec af006edc]; (4) bash scripts/launch_smoke.sh — SMOKE PASS, 10 s bounded run on the live display with the UI pass active, bounded-run exit 124, zero real panics [gsd_exec 24bd79b7]. Wiring confirmed by code inspection: pipeline.rs "ui_pass" second color-only begin_render_pass before submit/present; main.rs UiRenderer in resumed(), ui_pass threaded each frame, cursor_pos updated by CursorMoved/CursorLeft [gsd_exec 1e3c761c].

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

None.

## Known Limitations

Visual legibility of the overlay (text readable over the game, panel visible, resize positioning) is NEEDS-HUMAN — deferred to run-uat. Per D006 the crate remains log-free: glyphon prepare errors silently skip the UI pass for that frame. cursor_pos is stored but unconsumed until S04. The 13 inherited clippy lib warnings (mesh/world/texture) remain by design (MEM007).

## Follow-ups

None.

## Files Created/Modified

None.
