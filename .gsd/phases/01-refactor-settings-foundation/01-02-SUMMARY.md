---
id: S02
parent: M001
milestone: M001
provides:
  - Settings API (load_with_warnings/load, save, validate_keybinds, Keybinds, SettingsWarning, KeybindConflict, DEFAULT_SETTINGS_PATH)
  - keybind::key_to_string/parse_key stable-string conversion over the supported KeyCode subset
  - Documented settings.toml format guarded by the saved-format and round-trip tests
requires:
  []
affects:
  []
key_files:
  - src/settings/mod.rs
  - src/settings/keybind.rs
  - Cargo.toml
  - src/lib.rs
  - .gitignore
key_decisions:
  - D003: KeyCode<->stable-string hand-mapped table in src/settings/keybind.rs, decoupling the settings file format from winit's serde feature so future winit bumps cannot break saved settings
  - P001: two-phase TOML parse (toml::Value tree + per-field extraction) instead of serde derive, because #[serde(default)] aborts the whole struct on one bad field
  - P002: Serialize-only mirror struct for save + atomic write (tmp sibling + rename); loading deliberately bypasses serde
patterns_established:
  - P001 two-phase per-field TOML fallback parse in load_with_warnings
  - P002 Serialize-only mirror struct + atomic write for save, guarded by the round-trip test
observability_surfaces:
  - none
drill_down_paths:
  []
duration: ""
verification_result: passed
completed_at: 2026-10-19T08:05:16.601Z
blocker_discovered: false
---

# S02: Settings core with TOML persistence and tests

**Pure-logic src/settings module with two-phase per-field-fallback TOML load, atomic save, KeyCode string conversion, and duplicate-keybind validation — 31 settings tests pin every behavior, full suite 54/54 green, build and clippy clean, no GPU/event-loop dependency.**

## What Happened

Delivered a pure-logic settings layer: a new src/settings module (serde + toml + tempfile dev-dep) exposing Settings (7-action Keybinds struct, mouse_sensitivity, fov_degrees, fullscreen, vsync), load_with_warnings with a two-phase per-field-fallback parse, atomic save (create_dir_all + tmp sibling + rename) via a Serialize-only mirror struct, keybind::key_to_string/parse_key hand-mapped conversion per D003, and validate_keybinds returning structured KeybindConflict data. All decision data is returned by value (Vec<SettingsWarning> / Vec<KeybindConflict>) — no log/tracing dependency. Defaults equal today's hardcoded values (0.0022 sensitivity, 75.0 FOV, KeyW/KeyS/KeyA/KeyD/Space/ShiftLeft/ControlLeft, fullscreen=false, vsync=false) so S04's wiring swap is behavior-neutral. The module imports only std and winit::keyboard::KeyCode — no GPU, no event loop, no main.rs/camera.rs/context.rs modifications (wiring is S04 territory).

Verification (fresh, this unit, via gsd_exec): cargo build exit 0; cargo test 54 passed / 0 failed (23 pre-existing unregressed + 31 new settings tests covering every must-have: round-trip, defaults round-trip, missing file, invalid TOML, per-field fallback, unknown keys ignored, clamping both directions plus boundary acceptance, duplicate-keybind load + conflict data, defaults match hardcoded values, atomic save with no tmp residue, parent-dir creation, saved-format guard); cargo clippy exit 0 (remaining warnings are pre-existing world/terrain lint suggestions, untouched by this slice); settings module purity scan found no wgpu/EventLoop references; /settings.toml added to .gitignore; all test file I/O lives inside tempfile::tempdir().

## Operational Readiness

Health signal: `cargo test` green is the primary health check — the round-trip test (save -> load_with_warnings == original, zero warnings) locks load and save together, so any drift between the two halves fails the suite. Once S04 wires the module, a healthy runtime is a saved settings.toml that reloads with zero SettingsWarning entries and persists rebinds across restarts.

Failure signal: a bad settings.toml never blocks launch and never goes silent — load_with_warnings returns a SettingsWarning (field name + reason) for every fallback, which S04's menu will display so the player sees which field fell back and why. Save failures surface as std::io::Error to the caller rather than being swallowed.

Recovery procedure: delete or rename settings.toml to reset to defaults (missing file -> defaults + 1 warning, no panic). The two-phase parse guarantees valid fields survive an invalid sibling, so manual editing the file is always recoverable. The atomic save (tmp sibling + fs::rename) means a crash mid-save cannot corrupt an existing file — the old file stays intact.

Monitoring gaps: no logging framework in this slice by design (warnings/conflicts are structured data, not log lines), so fallback frequency is unobserved until S04 surfaces the warnings in the UI. No metrics or dashboards are applicable to a pure-logic library module.

## Verification

cargo build exit 0; cargo test 54 passed / 0 failed (23 pre-existing + 31 new settings tests, exit 0); cargo clippy exit 0; settings module purity scan: imports only std + winit::keyboard::KeyCode, no wgpu/EventLoop references; /settings.toml in .gitignore; all tests use tempfile::tempdir(). All 9 slice must-have behaviors pinned by named tests (round-trip, defaults, missing file, corrupt TOML, per-field fallback, unknown keys ignored, clamping, duplicate-keybind data, atomic save).

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

None.

## Follow-ups

None.

## Files Created/Modified

None.
<!-- gsd:state-version=90:1 -->
