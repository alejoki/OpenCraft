---
id: S04-T04
parent: S04
milestone: M001
key_files:
  - src/main.rs
key_decisions:
  - Split live-apply by borrow reachability: FOV + fullscreen inside menu_click (window param, camera field), vsync surface reconfigure in the caller via a pre-computed clicked row
  - save_settings_or_warn became a free fn with disjoint (menu, settings) args so every save site works under live field borrows (P005)
  - Startup also applies fullscreen/vsync/FOV from the loaded Settings so they are the single source of truth for video parameters, not just menu-change triggers
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# S04-T04: Made the loaded Settings the live source of truth: settings-driven mouse sensitivity, menu FOV/fullscreen/vsync applying live, and save-on-every-accepted-change plus save-on-quit.

**Made the loaded Settings the live source of truth: settings-driven mouse sensitivity, menu FOV/fullscreen/vsync applying live, and save-on-every-accepted-change plus save-on-quit.**

## What Happened

Wired the loaded Settings as the live source of truth for video parameters in src/main.rs. (1) device_event MouseMotion now uses `self.settings.mouse_sensitivity` instead of the hardcoded 0.0022 (loaded value is clamped at load; menu adjustments clamp via the T01 helpers before every save). (2) FOV: menu FOV clicks clamp via `menu::adjust_fov`, store into `settings.fov_degrees`, and apply live to the camera's pub `fov_y_radians` (Camera reads it every frame in view_proj — no reconstruction); `GameApp::new` also applies the loaded FOV to the camera at construction so startup is settings-driven (defaults are 75 degrees either way — behavior-neutral, guarded by the new `default_settings_fov_matches_camera_default_fov` test). (3) Fullscreen: toggled live inside menu_click via `window.set_fullscreen(Some(Fullscreen::Borderless(None)))/None`; the resulting Resized flows through the existing ctx.resize + stats re-shape path, the menu buffer re-shapes on size change (StatsOverlay.update is size-gated), and row rects derive per frame from ctx.config — verified no cached menu rects survive a Resized; also applied at startup when the loaded setting is on. (4) Vsync: `apply_vsync` sets `ctx.config.present_mode` to Fifo (on) / Immediate (off) and reconfigures the surface (the resize path's mechanism). Because the whole-`self` menu_click cannot coexist with the live gpu borrow (P005), the MouseInput arm pre-hit-tests the clicked row, calls menu_click, and applies vsync afterwards by re-borrowing `self.gpu.as_mut()` — only when the Vsync row was clicked; startup applies Fifo when the loaded vsync is on (RenderContext::new's uncapped-Immediate default matches vsync=false). Wayland Immediate-unavailable is a documented accepted visual no-op in `apply_vsync`'s comment. (5) Persistence: every accepted change (rebind accept, sensitivity, FOV, fullscreen, vsync) saves immediately via the atomic `Settings::save`; `save_settings_or_warn` was converted from a `&mut self` method to a free fn with disjoint (menu, settings) args so all save sites work under live field borrows; the CloseRequested handler now saves once more for crash safety (errors deliberately ignored there — the process is exiting and no menu remains to display them; comment documents this). (6) Menu value display refresh: unchanged mechanism confirmed — compose_menu_text re-renders per frame from live settings and the size/text-gated buffer update re-shapes on any surface change. One compile fix during the task: the pre-call hit-test passed `Option<(f32,f32)>` where `hovered_row` takes `(f32,f32)` (E0308) — fixed with `cursor_pos.and_then(|pos| ...)` so a cursor-left-window click simply does nothing. Captured MEM020 (gotcha) documenting the borrow-split live-apply pattern.

## Failure Modes

External dependencies and their failure paths:
- **Filesystem (settings.toml writes)**: every in-menu accepted change saves via `Settings::save` (atomic tmp-sibling + `fs::rename`); an `io::Error` is surfaced as an in-menu message by `save_settings_or_warn` — never swallowed (D006 log-free failure display). The CloseRequested save ignores errors by design: the process is exiting and no menu can display them (code comment documents this).
- **Malformed settings at load**: unchanged from S02 — `load_with_warnings` does per-field TOML fallback + range clamping (P001); startup warnings surface in the menu (T02 path).
- **Compositor/platform (vsync=false → `PresentMode::Immediate` unsupported, e.g. Wayland)**: documented accepted visual no-op in `apply_vsync` (plan-directed: do not fight the compositor); Fifo for vsync=true is always supported.
- **Surface loss after reconfigure**: the existing `wgpu::SurfaceError::Lost` re-creation path in the render loop is unchanged; `apply_vsync` uses the same configure mechanism the resize path already exercises.
- No network or subprocess dependencies in this task's code.

## Load Profile

First-saturating resource at 10x load: `settings.toml` disk writes. Protection: writes are event-gated on accepted human-paced changes (one atomic write per click/keypress — never per frame, never per menu hover), and the per-frame menu cost is change-gated text composition (StatsOverlay::update dedupes on text+size, ~0.1–1.3 ms only when something changed). No pooling/rate-limiting is needed at any realistic load; a user click-spamming the menu produces at most a few dozen small atomic writes per second, which is trivial for any filesystem.

## Negative Tests

- **Boundary clamping**: `adjust_sensitivity`/`adjust_fov` clamp to [0.0001, 0.01] / [30.0, 120.0] at min and max — unit-tested in `src/ui/menu.rs` (adjust-helper clamp tests), so in-memory settings are never saved out of range.
- **Rebind negatives**: duplicate-key rejection and unsupported-key rejection (save panics on unmapped KeyCodes) — unit-tested in `src/ui/menu.rs` (capture accept/reject transitions) and guarded by the S02 save round-trip + unmapped-key panic tests in `src/settings/mod.rs`.
- **Source-of-truth neutrality**: new `default_settings_fov_matches_camera_default_fov` in `src/main.rs` asserts the default settings FOV equals Camera::new's hardcoded 75°, so the startup FOV wiring cannot silently change behavior.
- **Value display correctness**: `menu_text_lists_every_row_with_its_live_value` (src/main.rs tests) asserts the displayed values track live settings — covering plan item 6.
- **None-cursor click**: the pre-call hit-test uses `cursor_pos.and_then(...)`, so a click with the cursor outside the window (None) hits no row — matches menu_click's own None handling.

## Verification

cargo build, cargo test (full suite including the 11 main.rs menu/wiring tests), and cargo clippy all exit 0 — clippy shows only the 12 inherited lib warnings (mesh/world/texture loop-index, from_iter, range, too-many-args), zero on the slice surface (src/main.rs, src/ui/*). The absence check confirms the hardcoded `let sensitivity = 0.0022` is gone from src/main.rs. scripts/launch_smoke.sh passes: the binary ran 10 s on the event loop with routing + live-apply active, exit 124 (timeout kill, no panic), and no stray settings.toml was created in the cwd (no accepted changes during a headless smoke run — correct save-on-change-only behavior). The fullscreen→Resized path was verified by code: menu row rects derive per frame from ctx.config, and StatsOverlay::update re-shapes the menu buffer on any surface-size change, so no cached rects survive a Resized.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | pass | 1457ms |
| 2 | `cargo test` | 0 | pass | 1096ms |
| 3 | `cargo clippy` | 0 | pass | 450ms |
| 4 | `! grep -q 'let sensitivity = 0.0022' src/main.rs` | 0 | pass | 24ms |
| 5 | `bash scripts/launch_smoke.sh` | 0 | pass | 10045ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

- `src/main.rs`
<!-- gsd:state-version=192:1 -->
