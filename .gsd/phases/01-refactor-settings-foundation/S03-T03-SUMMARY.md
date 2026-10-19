---
id: S03-T03
parent: S03
milestone: M001
key_files:
  - src/main.rs
  - src/render/pipeline.rs
  - src/ui/mod.rs
  - src/ui/quad.rs
  - src/ui/text.rs
key_decisions:
  - Render_frame's 8th arg (ui_pass) kept flat with a rationale-commented #[allow(clippy::too_many_arguments)] instead of an args struct — frame-draw entry point, single call site, avoid churn
  - CursorLeft matched with `..` (winit 0.30 carries device_id on the variant) — captured as MEM016/L004 for S04
  - cursor_pos stored as Option<(f32,f32)> physical px — one shared px space across ctx.config, cosmic-text buffers, quad renderer, and S04 hit-testing
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# S03-T03: Added CursorMoved/CursorLeft capture into GameApp.cursor_pos (physical px) and closed all four slice gates clean: build, test, clippy, and 10 s live smoke

**Added CursorMoved/CursorLeft capture into GameApp.cursor_pos (physical px) and closed all four slice gates clean: build, test, clippy, and 10 s live smoke**

## What Happened

## What Happened

Added cursor event capture to GameApp: a `cursor_pos: Option<(f32, f32)>` field (physical pixels — the same px space as ctx.config, cosmic-text buffers, and the quad renderer's API, no scale-factor math at scale 1), initialized to None in `new()`, and two match arms placed before the existing `_ => {}` arm in `window_event`: `WindowEvent::CursorMoved { position, .. }` stores `(position.x as f32, position.y as f32)` (winit 0.30 delivers PhysicalPosition&lt;f64&gt;), and `WindowEvent::CursorLeft { .. }` resets to None. No existing arm's behavior changed (ungrabbed-click regrab untouched). No consumers in S03 — pure plumbing for S04's hover/click hit-testing, per plan.

One API adaptation from the plan text: the plan wrote the arm as bare `WindowEvent::CursorLeft`, but this winit 0.30 version carries a `device_id` field on the variant — the bare match failed E0533 and was fixed to `WindowEvent::CursorLeft { .. }`. Captured as MEM016/L004 (S04 will hit the same shape on CursorEntered).

Then ran all four final slice gates. One regression found and fixed: `cargo clippy` flagged `render_frame` (src/render/pipeline.rs:564) with too-many-arguments (8/7) — the `ui_pass` parameter T01 added pushed it over the threshold. Since it is a regression in a slice-touched file, fixed with a rationale-commented `#[allow(clippy::too_many_arguments)]` (frame-draw entry point with a single call site; grouping args into a struct would be churn for no gain). After the fix, all slice-touched files (src/ui/*, src/render/pipeline.rs, src/main.rs) have zero clippy findings; the remaining ~13 clippy warnings are the inherited src/mesh/* and src/world/* findings, out of scope per MEM007.

All four gates pass on the final code (clean unpiped evidence runs): cargo build warning-free, cargo test 59/59 passed (existing overlay-vertex test untouched + the five slice quad tests), cargo clippy exit 0 with zero slice-touched-file findings, and scripts/launch_smoke.sh — 10 s bounded run on the live Wayland session with the UI pass (stats overlay text + translucent panel) active, exit 124 from the timeout kill, no panic.

## Failure Modes

External dependencies and their failure paths for this task's surface:

- **winit event delivery** (in-process, no network): if CursorMoved stops arriving (window occluded, device loss), `cursor_pos` simply goes stale; CursorLeft deterministically resets it to None. Benign in S03 — the field is pure storage with no consumer; S04 owns any staleness policy.
- **GPU surface acquisition** in render_frame (unchanged by this task): existing explicit handling kept — `SurfaceError::Lost | Outdated` → resize; `OutOfMemory` → save world + exit loop; `Timeout` → ignore (skip frame).
- **glyphon prepare** (glyph atlas exhaustion): handled — `.is_ok()` guard skips the entire UI pass for that frame (world pass unaffected); crate is log-free by convention (D006).
- **World save on close/F5** (filesystem): existing failure path — `save_current_world` returns bool, caller either shows the [Saved] badge or silently skips; unchanged.
- **Toolchain/build failures**: compile errors surface directly (the E0533 CursorLeft mismatch was caught and fixed on first build).

No new network or filesystem dependencies were added; the cursor plumbing is pure state.

## Load Profile

- **This task's load dimension — cursor event rate**: CursorMoved can arrive at ~1 kHz from gaming mice (10x a typical 125 Hz mouse). The handler is an O(1) two-f32 store with no allocation — no saturation at any plausible rate.
- **First-saturating resource at 10x in the frame loop**: per-frame GPU buffer allocation churn (the line-overlay vertex buffer and the new quad vertex buffer are created every frame; wgpu does no buffer pooling here). At ~10x frame rate the driver-side buffer alloc/free would saturate before anything else. Protection applied: the quad upload is a tiny fixed-size buffer (one panel = 6 vertices = 48 B) following the established per-frame overlay-buffer pattern, and text shaping is rate-limited to 4 Hz by the title_timer cadence with a changed-string guard — the deliberate protection against glyphon's 0.1–1.3 ms set_text cost per call.

## Negative Tests

- `src/ui/quad.rs` test module (all passed in the 59-test run) covers the slice's negative surface: `degenerate_surface_dims_do_not_divide_by_zero` (0x0 surface cannot divide by zero), `empty_quad_list_yields_no_vertices` (empty list → no vertices, and `set_quads` clears the buffer so nothing is drawn), `full_surface_quad_maps_to_unit_corners_with_y_flip` (asserts w == 1.0 for all vertices and the y-flip direction), `partial_quad_maps_to_offset_corners`, and `quads_to_vertices_concatenates_in_input_order`.
- The existing pipeline test `test_overlay_vertices_include_wireframe_crosshair_and_hotbar` is untouched and passing.
- Cursor plumbing boundary paths (CursorLeft → None; stale position if events stop; glyphon prepare-error → UI pass skipped that frame) are not unit-testable: GameApp lives in the binary target and requires a live window/event loop. Verified by code review plus the bounded 10 s live smoke run instead — consistent with the plan's "verified by code review + gates, not by visual proof".

## Visual items recorded as NEEDS-HUMAN for the slice UAT

- Stats text legible over the running game, updating at the ~4 Hz cadence (white monospace at physical-pixel left/top 8/8).
- Translucent dark panel (alpha 0.4 black) visible behind the stats text.
- Resize keeps text correctly positioned and un-stretched — the code path is in place (`Resized` → `gpu.stats.update` with the new width/height re-runs buffer.set_size + shape_until_scroll; the per-frame glyphon `Resolution` uses ctx.config's live values), but visual confirmation needs a human resizing the live window.

## Verification

All four slice gates pass on the final code. Cursor plumbing verified in src/main.rs (field + init + match arms before `_ => {}`, no existing arm changed). Clippy: the one slice-touched regression (render_frame too-many-arguments from T01's ui_pass param) was fixed with a rationale-commented #[allow]; a scoped grep over clippy output (`cargo clippy --all-targets` filtered to src/ui/*, src/main.rs, src/render/pipeline.rs) returned zero findings afterward — the ~13 remaining warnings are the inherited src/mesh/* + src/world/* findings, out of scope per MEM007. Tests: 59/59 passed including the five quad unit tests and the untouched overlay-vertex pipeline test. Smoke: 10 s bounded live run on the auto-attached Wayland session with the UI pass (stats overlay + panel) active — exit 124 from timeout kill, no panic, per the script's internal assertions (build expectation + panic grep). Visual legibility items are recorded as NEEDS-HUMAN in the summary (per task plan).

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | passed | 206ms |
| 2 | `cargo test` | 0 | passed | 314ms |
| 3 | `cargo clippy --all-targets` | 0 | passed | 231ms |
| 4 | `bash scripts/launch_smoke.sh` | 0 | passed | 10047ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

- `src/main.rs`
- `src/render/pipeline.rs`
- `src/ui/mod.rs`
- `src/ui/quad.rs`
- `src/ui/text.rs`
<!-- gsd:state-version=136:1 -->
