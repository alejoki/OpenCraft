---
id: T02
parent: S02
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

# T02: Added Settings struct with today's hardcoded defaults and a two-phase TOML loader with per-field fallback, range clamping, and structured warnings

**Added Settings struct with today's hardcoded defaults and a two-phase TOML loader with per-field fallback, range clamping, and structured warnings**

## What Happened

## What Happened

TDD red-green per behavior. RED first: wrote 15 tests against the public interface (`load`, `load_with_warnings`, `Settings::default`, `SettingsWarning`) in `src/settings/mod.rs` with a compiling `unimplemented!()` stub; `cargo test` failed 13 / passed 33 (existing suite unregressed). Then implemented the two-phase loader and went GREEN.

Implementation in `src/settings/mod.rs`:
- `Settings` struct + `Default` matching today's hardcoded values: sensitivity 0.0022 (main.rs `device_event`), FOV 75.0 (camera.rs `Camera::new`), fullscreen false, vsync false (context.rs prefers Immediate -> no vsync), keybinds = Keybinds::default().
- `SettingsWarning { field, reason }` returned by value (no logging dependency), and `DEFAULT_SETTINGS_PATH = "settings.toml"` for S04.
- `load_with_warnings`: phase 0 `fs::read_to_string` (missing/unreadable -> defaults + 1 warning, no panic); phase 1 `toml::from_str::<toml::Value>` (invalid TOML -> all defaults + 1 warning); phase 2 independent per-field extraction via `load_keybind_action` / `load_clamped_f32` / `load_bool` helpers — one bad field never aborts siblings. Keybinds parsed through `keybind::parse_key`; missing actions keep defaults with no warning; sensitivity clamped to [0.0001, 0.01], FOV to [30.0, 120.0] (clamp + warn, never reject); wrong types fall back per-field with warnings; unknown keys/sections/actions silently ignored. `load` is a thin warnings-discarding wrapper.

One mid-cycle fix: the first green run failed to compile because I referenced the four range constants before defining them (E0425 x4); defined them and the suite went green.

## Failure Modes (Q5)

External dependency: local filesystem only (single read of settings.toml); no network, API, or subprocess deps.
- Missing file -> defaults + exactly 1 warning (pinned: `missing_file_yields_defaults_with_exactly_one_warning`).
- Unreadable file (permissions, I/O error) -> same phase-0 io::Error arm: defaults + 1 warning, never panics.
- Syntactically invalid TOML -> all defaults + 1 warning (pinned: `syntactically_invalid_toml_yields_all_defaults_with_one_warning`).
- Per-field malformed values (wrong type, unparsable keybind string, out-of-range) -> that field's default/clamped value + per-field warning; valid siblings survive (pinned: `bad_fov_type_falls_back_while_valid_fields_survive`, `wrong_type_mouse_and_video_fields_fall_back_with_warnings`, `unparsable_keybind_string_falls_back_with_warning`, `wrong_type_keybind_falls_back_with_warning`).
All failure paths handled explicitly; a bad settings.toml never blocks launch.

## Load Profile (Q6)

No runtime load dimension: settings load is a one-time startup read of a single small TOML file on the main thread before the event loop; there is no per-frame, per-request, or concurrent load path through this code, so there is no resource that saturates at 10x and no pooling/rate-limiting to apply. Tests use `tempfile::tempdir()` so parallel test runs never collide.

## Negative Tests (Q7)

All inline in `src/settings/mod.rs` (`settings::tests`):
- `missing_file_yields_defaults_with_exactly_one_warning` — missing file -> defaults + exactly 1 warning.
- `syntactically_invalid_toml_yields_all_defaults_with_one_warning` — "not [valid" -> all defaults + 1 warning.
- `bad_fov_type_falls_back_while_valid_fields_survive` — per-field fallback: bad fov type, valid keybinds/sensitivity/fullscreen kept.
- `unknown_keys_sections_and_actions_are_silently_ignored` — unknown top-level key, unknown keybind action, unknown [audio] section -> zero warnings.
- `out_of_range_values_above_maximum_are_clamped_with_warnings` — fov 500.0 -> 120.0, sensitivity 0.5 -> 0.01, 2 warnings.
- `out_of_range_values_below_minimum_are_clamped_with_warnings` — fov 10.0 -> 30.0, sensitivity 0.000001 -> 0.0001, 2 warnings.
- `boundary_values_are_accepted_without_warnings` — inclusive range boundaries accepted, zero warnings.
- `wrong_type_mouse_and_video_fields_fall_back_with_warnings` — string sensitivity, integer fullscreen, string vsync -> defaults + warnings.
- `unparsable_keybind_string_falls_back_with_warning` / `wrong_type_keybind_falls_back_with_warning` — "NotAKey" and sprint = 3.
- `missing_keybind_actions_keep_defaults_without_warnings` — partial file is fine.
- `load_discards_warnings_and_never_panics` — wrapper never panics on missing/garbage files.
Boundary correctness of defaults pinned by `settings_defaults_match_todays_hardcoded_values` (0.0022 / 75.0 / false / false) and `keybinds_defaults_match_todays_hardcoded_bindings`.

## Deviations

None from the written task plan. (Additive beyond the plan's enumerated test list: below-minimum clamping, inclusive boundary acceptance, and wrong-type keybind/mouse/video tests — same behaviors the plan's phase-2 spec describes, just pinned more thoroughly.)

## Known Issues

None. (Note for S04: the loader treats TOML integers where floats are expected, e.g. `fov = 90`, as wrong type -> default + warning, per the plan's strict "wrong type -> default + warning" wording; if S04 wants integer-coercion it is a one-line change in `load_clamped_f32`.)

## Verification

cargo test via gsd_exec (exec id 585a24fe-30cf-4046-bf48-282ab1bbc7c9): exit 0, 46 passed / 0 failed / 0 ignored — 15 settings::tests (13 new T02 behaviors + 2 defaults tests) plus T01's 8 keybind tests and the pre-existing 23 project tests, all unregressed. RED observed first (13 failed / 33 passed) before implementation; fresh green run is the only evidence counted, and it ran after the last code change (no edits since).

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo test` | 0 | pass | 2813ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=136:1 -->
