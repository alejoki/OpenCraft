# S03 Research: UI foundation — glyphon text overlay + quad rendering

## Summary

S03 adds a `src/ui/` module rendering a live always-on stats overlay (FPS/position readout) with glyphon 0.5.0 text + a filled-quad renderer, in a **separate color-only render pass after the world pass**, and routes cursor position/enter/leave events into app state. The critical finding: **glyphon 0.5.0's API differs materially from latest docs** (per MEM002, only docs.rs/glyphon/0.5.0 is authoritative — verified below). The second critical finding: `VoxelRenderer::render_frame` in `src/render/pipeline.rs` currently acquires the surface texture, runs one depth pass, **submits and presents inside the same method** — it must be restructured so the UI pass draws between the world pass and `present()`. The existing `OverlayVertex` (clip_pos + color, alpha blend) line pipeline is directly reusable prior art for a TriangleList quad pipeline.

## Recommendation

Add `glyphon = "=0.5.0"` (or `~0.5`) to Cargo.toml — glyphon 0.5.0 re-exports all needed cosmic-text 0.10 types (FontSystem, SwashCache, Buffer, Metrics, Attrs, Family, Color, Shaping), so **no separate cosmic-text dependency is required**. Build `src/ui/` as a `UiRenderer` owning the glyphon stack + quad pipeline; hook it into the frame via a closure parameter or pass-split on `render_frame`; drive the overlay text from the existing 0.25 s title-stats cadence in `src/main.rs` (avoids per-frame `Buffer::set_text` cost, ~0.1–1.3 ms per call). Capture `CursorMoved`/`CursorLeft` in `window_event` (they currently fall into the `_ => {}` arm) into a `cursor_pos: Option<(f32,f32)>` field on `GameApp` (physical px — same space as cosmic-text buffers and `RenderContext.config`).

## Verified glyphon 0.5.0 API contract (use EXACTLY these — not latest docs)

Latest glyphon (0.6+) introduced a `Viewport` struct and different `prepare` signature. **0.5.0 has no Viewport at all** — the roadmap's mention of "Viewport" should be read as: pass `Resolution` to `prepare` and `TextBounds` on each `TextArea`.

```rust
// glyphon 0.5.0 — deps: wgpu ^0.19 (exact pin OK with our wgpu 0.19.4), cosmic-text ^0.10,
// etagere, lru. winit is a DEV-dependency only (0.29) — irrelevant to our winit 0.30.
// Re-exports: FontSystem, SwashCache, Buffer, Metrics, Attrs, Family, Color, Shaping, ...

// One-time init (in resumed(), after RenderContext::new):
let mut font_system = glyphon::FontSystem::new();      // scans system fonts (desktop Linux: fine)
let mut swash_cache = glyphon::SwashCache::new();
let mut atlas = glyphon::TextAtlas::with_color_mode(   // explicit ColorMode (new() default is ambiguous)
    &ctx.device, &ctx.queue, ctx.config.format, glyphon::ColorMode::Color);
let text_renderer = glyphon::TextRenderer::new(
    &mut atlas, &ctx.device, wgpu::MultisampleState::default(),
    None /* depth_stencil: MUST be None — UI pass has no depth attachment */);

// Per-frame text (overlay line):
let mut buffer = glyphon::Buffer::new(&mut font_system, glyphon::Metrics::new(20.0, 28.0));
buffer.set_size(&mut font_system, width_f32, height_f32);          // f32, f32 (cosmic-text 0.10)
buffer.set_text(&mut font_system, &stats_string,
    glyphon::Attrs::new().family(glyphon::Family::Monospace),      // Monospace suits a stats readout
    glyphon::Shaping::Advanced);
buffer.shape_until_scroll(&mut font_system);                        // 0.10: NO `prune` argument
let text_area = glyphon::TextArea {
    buffer: &buffer,
    left: 8.0, top: 8.0,                       // f32 pixel position (top-left)
    scale: 1.0,
    bounds: glyphon::TextBounds::default(),     // default = no clipping; or (l,t,r,b) px tuple field
    default_color: glyphon::Color::rgba(255, 255, 255, 255),
};
text_renderer.prepare(
    &ctx.device, &ctx.queue, &mut font_system, &mut atlas,
    glyphon::Resolution { width: w, height: h },   // u32 surface px — pass AFTER resize
    [text_area], &mut swash_cache)?;

// Inside the UI render pass (color-only, no depth attachment):
text_renderer.render(&atlas, &mut rpass)?;    // render<'pass>(&'pass self, atlas: &'pass TextAtlas,
                                             //   pass: &mut RenderPass<'pass>)
```

`TextArea` 0.5.0 fields: `buffer`, `left` (f32), `top` (f32), `scale` (f32), `bounds: TextBounds` (fields `left/top/right/bottom: i32`, `default()` = unclipped), `default_color: Color`. Borrow note: `render` takes `&'pass TextAtlas` — atlas and renderer are immutably borrowed for the whole pass scope; no mutation during the pass (fine in practice).

## Implementation Landscape (codebase seams)

- **`src/main.rs` (GameApp/Ready)** — UI stack belongs in `Ready` (needs device/queue, created in `resumed()`). `RedrawRequested` arm already computes fps/ms/position stats at a 0.25 s cadence (`title_timer`) for `window.set_title` — reuse that exact data and cadence for the overlay string; store last stats so the overlay updates 4 Hz, not per frame. `CursorMoved`/`CursorLeft` currently hit `_ => {}`; add arms storing physical-pixel cursor position (winit 0.30 delivers `CursorMoved { position: PhysicalPosition<f64> }` inside `window_event` — S01 confirmed this routing works). Keep the ungrabbed-click regrab early-return in `MouseInput` untouched (S04 will add menu-open routing).
- **`src/render/pipeline.rs` (`render_frame`)** — current structure: `get_current_texture` → view → camera uniform → overlay verts → encoder → **one pass** (voxel + line overlay, depth attached) → `submit` → `present`. The UI pass must draw after the world pass **on the same encoder/texture before present**. Two viable restructures: (a) add a parameter `ui_pass: Option<&mut dyn FnMut(&mut wgpu::RenderPass)>` opened as a second `begin_render_pass` (color-only: no `depth_stencil_attachment`, `LoadOp::Load`, same view, alpha blend) before `submit`; (b) split `render_frame` into `render_world(...) -> (view, output)` and let main compose the UI pass then submit/present. Option (a) is the least-churn seam; option (b) is cleaner for S04. Either way: **do not create a second texture or encoder requirement — a second `begin_render_pass` on the existing encoder with `LoadOp::Load` is sufficient**.
- **Quad renderer (`src/ui/quad.rs`, new)** — reuse the existing pattern: `OverlayVertex { clip_pos: [f32;4], color: [f32;4] }` + the trivial pass-through WGSL already in `pipeline.rs` (`OVERLAY_SHADER_WGSL`), but `PrimitiveTopology::TriangleList` with a shared static index buffer (0,1,2, 2,3,0 pattern ×N). CPU-side pixel→NDC conversion (x_ndc = 2x/w − 1, y flipped) matches the codebase's hand-rolled CPU-vertex style (hotbar/crosshair already do this). Alpha blend, no depth. Draw quads with a small API: `Quad { x, y, w, h, color }` in px → vertex vec; S04's panels/buttons consume this.
- **`src/render/context.rs`** — no changes needed; surface format is sRGB (glyphon atlas handles it), resize already reconfigures surface + depth. UI only needs to see the new size (`ctx.config.width/height`) each frame.

## Natural seams / task order

1. **T1: glyphon dependency + `src/ui/` text wrapper + `render_frame` restructure + overlay live in-game.** (Highest risk: pass restructuring + version-API pitfalls together; first proof is seeing text on screen over the running game.) Files: `Cargo.toml`, `src/ui/mod.rs`, `src/ui/text.rs`, `src/render/pipeline.rs`, `src/main.rs`.
2. **T2: quad renderer + pixel-space drawing + hover plumbing.** Files: `src/ui/quad.rs`, `src/ui/mod.rs`, `src/main.rs` (cursor event arms). Independent of glyphon internals; needs T1's pass hook.
3. **T3: cursor events + smoke + clippy/build gate.** CursorMoved/CursorLeft arms can land with T2 (same match block); final gate is build + clippy (touched files only, per MEM007) + `scripts/launch_smoke.sh`.

## First proof

Text visible on screen over the world, in a separate pass, surviving the 10 s smoke without panic. That single artifact retires both the glyphon-0.5.0-API risk and the pass-restructuring risk; everything else (quads, cursor plumbing) is mechanical.

## Verification

- `cargo build` warning-free; `cargo clippy` scoped to touched files (src/ui/*, src/render/pipeline.rs, src/main.rs) — MEM007: ~12 pre-existing lib warnings in src/mesh/*, src/world/* are inherited, not regressions.
- `bash scripts/launch_smoke.sh` — 10 s bounded run, exit 124, no panic; attaches the Wayland socket per MEM006 (script auto-detects; agent shells lack DISPLAY/WAYLAND_DISPLAY).
- Visual: NEEDS-HUMAN — text overlay legible over the game, FPS/pos updating ~4 Hz, resize keeps text positioned (resize → buffer.set_size + new Resolution). If a screenshot mechanism is available (kwin DBus/grim on the live session), capture as runtime evidence; otherwise record as manual follow-up.
- `cargo test` — existing overlay-vertex unit test in pipeline.rs must still pass if `render_frame` signature changes; keep `build_overlay_line_vertices` pure and untouched.

## Risks / gotchas

- **glyphon version drift (MEM002)**: 0.6+ requires wgpu 0.20+/new numbering — incompatible with our pin. Pin `=0.5.0`/`~0.5`. Latest-docs signatures (Viewport, prepare-with-viewport, `&mut Device`) will not compile against 0.5.0; use only the contract above.
- **cosmic-text 0.10 signature specifics**: `set_size` takes f32s; `set_text` takes `(text, Attrs, Shaping)`; `shape_until_scroll` has **no prune arg**. All verified against docs.rs/cosmic-text/0.10.0.
- **`TextRenderer::new` depth_stencil must be `None`** — the UI pass has no depth attachment; a mismatched depth-stencil pipeline fails render-pass validation.
- **Per-frame `set_text` cost** (~0.1–1.3 ms, cosmic-text #245): drive overlay text at the existing 0.25 s cadence; reuse the Buffer between updates (set_text + reshape per update, 4 Hz).
- **FontSystem::new() startup cost**: scans system fonts once — create in `resumed()`, not per frame. If system fonts are a concern, cosmic-text 0.10 supports loading an embedded font via fontdb source, but desktop Linux with system fonts is fine for this milestone (keep it simple).
- **Pass ordering**: world pass writes depth; UI pass (color-only, `LoadOp::Load`) must come after it on the same encoder, before `submit`+`present`. Glyphon and quads can share one UI pass (quads first, text after).
- **DPI**: cursor events and surface config are physical pixels; cosmic-text works in that same pixel space, so no scale-factor math needed for S03 (S04 hover hit-testing in px also just works at scale 1; note scale_factor if S04 needs logical-space UI).

## Skills Discovered

None — no glyphon/cosmic-text skills available via `npx skills find`; API verified directly from pinned docs.rs pages instead.
<!-- gsd:state-version=90:1 -->
