---
id: S03-T02
parent: S03
milestone: M001
key_files:
  - (none)
key_decisions:
  - (none)
duration:
verification_result: mixed
completed_at:
blocker_discovered: false
---

# S03-T02: Added pixel-space filled-quad rendering (Quad API + TriangleList alpha-blended pipeline in the color-only UI pass) and drew a translucent dark panel behind the live stats text

**Added pixel-space filled-quad rendering (Quad API + TriangleList alpha-blended pipeline in the color-only UI pass) and drew a translucent dark panel behind the live stats text**

## What Happened

Added the pixel-space filled-quad drawing API for the color-only UI pass and used it to draw a translucent dark panel behind the live stats text.

src/ui/quad.rs (new) provides: Quad { x, y, w, h, color } in physical pixels (top-left origin, y down); pure quad_to_vertices(x, y, w, h, color, surface_w, surface_h) -> Vec<OverlayVertex> doing CPU-side NDC conversion (x_ndc = 2x/w − 1, y_ndc = 1 − 2y/h with the y flip) and emitting 6 vertices per quad (triangles 0,1,2 and 2,3,0 expanded, no index buffer); quads_to_vertices(quads, ...) batching helper; and QuadRenderer, a TriangleList pipeline reusing the OverlayVertex pass-through WGSL pattern (shader defined locally in quad.rs per plan preference) with BlendState::ALPHA_BLENDING and depth_stencil None (color-only UI pass). set_quads(device, quads, w, h) rebuilds the per-frame vertex buffer before the pass (empty list clears the buffer); draw(pass) issues the draw inside the pass.

src/ui/mod.rs wires pub mod quad, adds the QuadRenderer to UiRenderer (built once in new alongside the glyphon stack), exposes set_quads, and reorders UiPass::draw_ui so quads draw FIRST, then glyphon text — the stats text sits on top of the panel. src/ui/text.rs makes FONT_SIZE/LINE_HEIGHT pub so the frame loop sizes the backing panel from the same metrics. src/main.rs builds the panel quad each frame before render_frame: a ~0.4-alpha black rect inset around OVERLAY_TEXT_LEFT/TOP (px origin (2,2), height LINE_HEIGHT+8), with width estimated from the stats string at ~0.6 em per monospace char.

Runtime behavior was proven by the 10 s bounded live smoke run: the quad pipeline validates in the color-only Load/Store UI pass on the same encoder/view (a mismatched depth-stencil or blend state would have panicked in wgpu validation), the game ran the full window with no panic, exit 124 as expected. All gameplay arms (movement, break/place, hotbar, fly, save) untouched.

One test iteration: the first cargo test run failed because the new unit test's vertex-order expectation array was miswritten as [0,1,2,3,2,0] instead of the actual vertex sequence c0,c1,c2,c2,c3,c0 = [0,1,2,2,3,0]; the implementation was correct (the failing assertion pinpointed vertex 3 carrying corner 2's x = +1). Fixed the expectation and re-ran: 59 tests pass, including the untouched overlay-vertex test.

## Verification

cargo build exit 0 warning-free (stderr shows only Compiling/Finished). cargo test exit 0: 59 tests passed, 0 failed (the first run failed one NEW test due to a wrong vertex-order expectation in the test itself; fixed and re-run green — the existing overlay-vertex pipeline test untouched and passing). bash scripts/launch_smoke.sh passed: 10 s bounded run on the live session, exit 124, no panic — proving the quad pipeline passes wgpu render-pass validation in the color-only UI pass and the panel code path executes every frame without breaking the render loop. Visual legibility (text readable over the game, panel visible behind text) is NEEDS-HUMAN, recorded in Known Issues for the slice UAT.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | pass | 2812ms |
| 2 | `cargo test` | 101 | fail | 1593ms |
| 3 | `cargo test` | 0 | pass | 1056ms |
| 4 | `bash scripts/launch_smoke.sh` | 0 | pass | 10043ms |

## Deviations

None.

## Known Issues

NEEDS-HUMAN (record for slice UAT): (1) stats text legible over the game, updating ~4 Hz; (2) translucent dark panel visibly rendered behind the stats text (the ~0.4 alpha black quad at px (2,2)); (3) resize keeps the text and panel positioned (stats overlay re-set_size on Resized is in place; the panel re-projects from ctx.config each frame, but visual confirmation needs human eyes). Panel width is a ~0.6 em/char heuristic — if the stats line is ever longer than the estimate, the panel could under-cover the text tail; acceptable for a stats backdrop and adjustable in one constant.

## Files Created/Modified

None.
<!-- gsd:state-version=105:1 -->
