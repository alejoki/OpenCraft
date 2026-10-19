---
id: S04-T01
parent: S04
milestone: M001
key_files:
  - (none)
key_decisions:
  - (none)
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# S04-T01: Added the pure-logic settings-menu state machine (row model, px layout, hit-test, clamped adjustments, rebind decision) in src/ui/menu.rs with 24 unit tests

**Added the pure-logic settings-menu state machine (row model, px layout, hit-test, clamped adjustments, rebind decision) in src/ui/menu.rs with 24 unit tests**

## What Happened

Retry of S04-T01 after a user-pause during setup: the implementation (written before the pause) was verified in place and required no repair — `gsd_task_recovery_resume` was correctly rejected by the abort-action guard because a `retry` action needs no separate authorization; the orchestrator's re-dispatch is the authorization.

Shipped (verified file-by-file against the task plan):
1. `src/settings/mod.rs`: `SENSITIVITY_MIN/MAX` and `FOV_MIN/MAX` are now `pub const` with doc comments (single source of truth, values unchanged: [0.0001, 0.01] and [30.0, 120.0] — load-path tests still pin them).
2. `src/ui/menu.rs` (new, 873 lines): `MenuState` (Closed / Open / AwaitingKey{action} / message-with-tick-counter warning mechanism — the module owns no clock; `MESSAGE_TICKS = 600` decremented by the caller); `MenuRow` row model covering the 7 keybinds (Forward, Backward, Left, Right, JumpOrUp, CrouchOrDown, Sprint) + MouseSensitivity + Fov + Fullscreen + Vsync + Close, with `label()` display strings; `row_rect(surface_w, surface_h, index) -> Option<Rect>` deterministic px layout (top-left origin, y down; rows LINE_HEIGHT=22.0 tall, centered panel MENU_WIDTH wide at MENU_TOP, returns None past the last row — reflows automatically when main.rs passes new surface dims per frame); `hit_test` pure point-in-rect with half-open pixel-grid boundaries; `adjust_sensitivity`/`adjust_fov` step helpers that clamp to the pub constants BEFORE save (in-memory Settings are unclamped, only load clamps); `evaluate_rebind_candidate -> RebindDecision` (Accept / RejectUnsupported — the mandatory filter because `save()` panics on keys lacking `key_to_string` / RejectDuplicate naming the conflicting action, aligned with `KeybindConflict` semantics and proven consistent with `Settings::validate_keybinds` by test); `format_warnings(Vec<SettingsWarning>) -> Vec<String>` field+reason lines per MEM009 warnings-are-data.
3. `src/ui/mod.rs`: `pub mod menu;` added (LINE_HEIGHT was already re-exported from text.rs).

Purity verified: src/ui/menu.rs imports only `winit::keyboard::KeyCode`, `super::LINE_HEIGHT`, and `crate::settings::*` (test module uses `super::*`) — no GPU, event loop, clock, or filesystem.

## Failure Modes

The module is pure CPU logic with no external runtime dependencies (no filesystem, network, or subprocess I/O). Its one external contract — the `key_to_string` mapping in `crate::settings::keybind` — is the documented failure path: `Settings::save()` PANICS on a KeyCode lacking a mapping, so `evaluate_rebind_candidate` checks `key_to_string(code).is_none()` FIRST and returns `RejectUnsupported` before any duplicate scan (test `unsupported_keys_are_rejected_before_any_duplicate_check`, menu.rs line ~674). Out-of-range row indices return `None` from `row_rect` instead of panicking (test `row_index_out_of_range_returns_none`). Both failure paths are handled inside the module, not bubbled.

## Load Profile

Per-frame CPU work is the only load dimension: main.rs (S04-T02) recomputes ~12 `row_rect` calls + hit-tests per frame — O(1) arithmetic each, microseconds. At 10x (e.g. 10x row count or 10x menu instances) nothing saturates; there is no allocation-heavy path, no unbounded loop, and no I/O. First-saturating resource would be CPU frame budget only at absurd scales irrelevant to this module. No protection needed beyond the fixed 12-row model.

## Negative Tests

24 unit tests in src/ui/menu.rs `#[cfg(test)]` cover the negative surface specifically: duplicate-key rejection (`rebind_to_another_actions_key_is_rejected_as_duplicate`, rejection messages name the culprit action); unsupported-key rejection before duplicate check; clamping in both directions plus inclusive boundary acceptance (`sensitivity_steps_move_by_the_step_and_clamp_both_directions`, `fov_steps_move_by_five_degrees_and_clamp_both_directions`, `clamping_bounds_are_inclusive_and_match_the_load_path_constants`); hit-test outside/false and half-open boundary edges (`hit_test_inside_is_true_and_outside_is_false`, `hit_test_boundaries_are_half_open_like_the_pixel_grid`); out-of-range row index returns None; hovered row returns None outside the panel; message expiry by pure tick counter; narrow-surface layout clamping (`row_rect_reflows_with_surface_width_and_clamps_at_narrow_surfaces`); idempotent same-key rebind treated as Accept. All pure — no GPU, no filesystem.

## Verification

cargo test: 83 passed, 0 failed (59 pre-existing + 24 new menu tests) in 0.06s. cargo build: clean, no warnings. Purity scan via import inspection: src/ui/menu.rs imports only winit::keyboard::KeyCode + crate types (super::LINE_HEIGHT, crate::settings::*); no GPU/event-loop/clock/filesystem dependencies. All 10 task-plan items verified present and exercised by the tests: layout determinism, hit-test edges, both-direction clamping with inclusive bounds, duplicate and unsupported-key rejection, rebind accept/cancel transitions, warning formatting, tick-based message expiry, action-name alignment with validate_keybinds strings.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo test` | 0 | pass: 83 passed, 0 failed (59 pre-existing + 24 new menu tests) | 315ms |
| 2 | `cargo build` | 0 | pass: clean dev build, no warnings | 149ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=157:1 -->
