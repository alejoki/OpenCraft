---
id: S03-T01
parent: S03
milestone: M001
key_files:
  - Cargo.toml
  - Cargo.lock
  - src/lib.rs
  - src/ui/mod.rs
  - src/ui/text.rs
  - src/render/pipeline.rs
  - src/main.rs
key_decisions:
  - Used glyphon::ColorMode::Accurate (0.5.0 has no `Color` variant; only Accurate/Web)
  - Replaced the planned closure UI-pass hook with a lifetime-typed `UiPass` trait passed as `Option<&dyn UiPass>` — the closure design cannot compile against wgpu 0.19's lifetime-carrying RenderPass + glyphon's render<'pass> tie (E0521 forces 'static borrows); captured as MEM014/L003 for T02 and S04
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# S03-T01: Added glyphon 0.5.0 text overlay rendering a live 4 Hz FPS/position stats readout in a new color-only UI pass between the world pass and present

**Added glyphon 0.5.0 text overlay rendering a live 4 Hz FPS/position stats readout in a new color-only UI pass between the world pass and present**

## What Happened

Glyphon 0.5.0 was added pinned exactly (`glyphon = "=0.5.0"` in Cargo.toml, Cargo.lock regenerated, confirmed resolving 0.5.0 with cosmic-text 0.10.0); no separate cosmic-text dependency was added — glyphon re-exports FontSystem, SwashCache, Buffer, Metrics, Attrs, Family, Color, Shaping, Resolution, TextArea, TextBounds.

src/lib.rs gained `pub mod ui;`. src/ui/mod.rs defines `UiRenderer` owning the glyphon stack (font_system, swash_cache, atlas, text_renderer), constructed once in `resumed()` after RenderContext::new: `TextAtlas::with_color_mode(&ctx.device, &ctx.queue, ctx.config.format, ColorMode::Accurate)` and `TextRenderer::new(&mut atlas, &ctx.device, MultisampleState::default(), None)` — depth_stencil None because the UI pass has no depth attachment. src/ui/text.rs defines `StatsOverlay` holding a reusable `glyphon::Buffer` (18 px monospace) plus last-text/last-size fields; `update(&mut FontSystem, width, height, text)` calls set_size/set_text/shape_until_scroll (cosmic-text 0.10 signatures, all taking `&mut FontSystem`, no prune argument) and no-ops unless the text or size changed — so the expensive set_text/shape path runs only at the 0.25 s cadence, never per frame. `text_area(8.0, 8.0)` builds the unclipped glyphon::TextArea with white default color in physical pixels.

src/render/pipeline.rs `render_frame` gained a `ui_pass: Option<&dyn UiPass>` parameter and, between the world pass and submit/present, a second `begin_render_pass` on the SAME encoder and view — color-only (no depth_stencil_attachment), `LoadOp::Load` + `StoreOp::Store` on the color attachment — invoking `ui.draw_ui(&mut ui_rpass)` when Some. `build_overlay_line_vertices` and the world pass are untouched (existing unit test still passes).

src/main.rs: `Ready` gained `ui: UiRenderer` + `stats: StatsOverlay` (built in `resumed()`); `GameApp` gained `last_overlay_text: String`. The existing 0.25 s title-stats branch also formats "FPS {fps} | {ms:.1} ms | Pos (…) [{mode}] | Chunks {visible}/{loaded}" from the same data into `last_overlay_text` and feeds `stats.update`. Each frame, before `render_frame`, `text_renderer.prepare(...)` runs with `glyphon::Resolution { width, height }` from ctx.config; on Err the UI pass is skipped for that frame (log-free per D006). `Resized` re-shapes the overlay at the new size so text never stretches.

Two deviations from the plan text (both compile-driven): (1) glyphon 0.5.0 has no `ColorMode::Color` variant (only Accurate/Web) — used `Accurate`, which matches the project's sRGB surface format. (2) The planned `Option<&mut dyn FnMut(&mut wgpu::RenderPass)>` closure hook cannot compile on wgpu 0.19: glyphon's `render<'pass>(&'pass self, atlas: &'pass TextAtlas, pass: &mut RenderPass<'pass>)` ties the atlas/renderer borrows to the pass lifetime, and the higher-ranked closure forces those captured borrows to be 'static (E0521/E0502 — verified, then fixed). Replaced with a `UiPass` trait defined in pipeline.rs (keeps pipeline.rs glyphon-agnostic): `fn draw_ui<'pass>(&'pass self, pass: &mut RenderPass<'pass>)`, implemented for `UiRenderer`, passed as `Option<&dyn UiPass>`. The 'pass receiver matches glyphon's tie exactly, so no 'static borrow is needed. This is the pattern S03-T02's quad renderer will plug into (draw quads first, then text, inside the same draw_ui).

## Failure Modes (Q5) — filled above in failureModes field.
## Load Profile (Q6) — filled above in loadProfile field.
## Negative Tests (Q7) — filled above in negativeTests field.

Visual legibility is NEEDS-HUMAN for the slice UAT (not this task's gate): stats text legible over the running game and updating ~4 Hz; text stays positioned after resize (re-shape implemented + live-run smoke passed, human confirmation pending).

## Verification

Verification evidence recorded: `cargo build 2>&1 | tail -20` exited 0 (passed); `cargo test 2>&1 | tail -20` exited 0 (passed); `bash scripts/launch_smoke.sh` exited 0 (passed).

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build 2>&1 | tail -20` | 0 | passed | 3062ms |
| 2 | `cargo test 2>&1 | tail -20` | 0 | passed | 2277ms |
| 3 | `bash scripts/launch_smoke.sh` | 0 | passed | 10043ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

- `Cargo.toml`
- `Cargo.lock`
- `src/lib.rs`
- `src/ui/mod.rs`
- `src/ui/text.rs`
- `src/render/pipeline.rs`
- `src/main.rs`
<!-- gsd:state-version=102:1 -->
