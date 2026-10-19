---
id: T03
parent: S02
milestone: M001
key_files:
  - src/settings/mod.rs
key_decisions:
  - Conflict warnings use field "keybinds" with a reason string naming the duplicated key and all colliding actions (join(", ")) — structured enough for S04 to display and consistent with SettingsWarning
  - A KeyCode without a key_to_string mapping panics at save time with the action name (executor judgment per plan): every default is mapped and the round-trip/format tests guard it; save is the S04-controlled write path, so an unmapped key is a code bug, not user data
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# T03: Added atomic TOML save via a Serialize-only mirror struct, duplicate-keybind validation surfaced as structured conflicts (warned at load, never blocking), and round-trip/save-atomicity tests locking save and load together

**Added atomic TOML save via a Serialize-only mirror struct, duplicate-keybind validation surfaced as structured conflicts (warned at load, never blocking), and round-trip/save-atomicity tests locking save and load together**

## What Happened

## What Happened

Extended `src/settings/mod.rs` (the only file touched) with:

1. `pub struct KeybindConflict { pub actions: Vec<&'static str>, pub key: KeyCode }` naming the colliding action names and the duplicated key.
2. `Settings::validate_keybinds()` — one function, two call sites: groups the 7 actions by shared KeyCode (declaration order preserved) and reports one conflict per group larger than one. Empty for defaults; one conflict of 3 names for a triple duplicate.
3. `Settings::save(&self, path)` — serializes through a private Serialize-only mirror (`SettingsToml`/`KeybindsToml`/`MouseToml`/`VideoToml`, serde derive, Serialize only, never Deserialize) producing exactly the documented format ([keybinds] 7 actions as KeyCode Debug strings via `keybind::key_to_string`, [mouse] sensitivity, [video] fov/fullscreen/vsync; f32 cast to f64 so toml's shortest-roundtrip formatting preserves f32 precision). Atomic write follows `src/world/persistence.rs save_world_to_path`: `fs::create_dir_all` on non-empty parent, write to `path.with_extension("toml.tmp")` via BufWriter, flush + `sync_all`, then `fs::rename` over the target. Serialization errors map to `io::ErrorKind::InvalidData`; all IO errors bubble as `std::io::Result`. A key without a string mapping panics with the action name (save-time bug, guarded by the round-trip/format tests).
4. Load integration: after keybind extraction in `load_with_warnings`, each conflict appends one SettingsWarning (field "keybinds", reason names the duplicated key and all colliding actions). Settings still load — never blocks launch; both values are kept for S04's rebind flow to resolve.

8 new tests, all inline in `#[cfg(test)] mod tests` in `src/settings/mod.rs` using tempfile::tempdir (no gitignored paths): non-default round-trip (changed keybind + sensitivity 0.005, fov 90.0, fullscreen/vsync true) → zero warnings + exact equality; defaults round-trip; saved-file format pinned ([keybinds]/[mouse]/[video], "KeyW"/"Space"/"ShiftLeft" spellings, booleans); duplicate-keybind file loads both values with 1 conflict warning naming both actions + validate reports it; validate on defaults → empty; triple duplicate → one conflict listing all 3 actions; save creates missing parent dirs and overwrites existing; save leaves no .toml.tmp behind (directory contains exactly 1 file).

One compile fix during the red-green cycle: `keybind_string` returned `&str` where `String` was declared — added `.to_owned()` on the unwrap result.

## Verification

`cargo test` exit 0: 54 passed, 0 failed, 0 ignored (17 settings tests across T01–T03; prior tests unregressed). `cargo clippy` exit 0: 0 errors, 12 warnings all pre-existing in src/mesh/src/world (L002 — zero warnings originate in src/settings). All temp files live in tempfile::tempdir(); no test reads .gitignore or gitignored paths.

## Failure Modes

External dependency: the local filesystem only (no APIs, network, or GPU).
- Read path (load): missing/unreadable file → (defaults, exactly 1 warning), no panic (T02 test `missing_file_yields_defaults_and_single_warning`); syntactically invalid TOML → all defaults + 1 warning; per-field wrong types → per-field default + warning; out-of-range floats → clamp + warning (never reject). Every malformed-input path returns structured data instead of failing.
- Write path (save): `create_dir_all`/`File::create`/`write_all`/`sync_all`/`rename` failures bubble as `std::io::Result` to the caller (S04 decides UX); serialization failure maps to `io::ErrorKind::InvalidData`. Crash mid-save is contained by the tmp-sibling + `fs::rename` atomicity pattern: the target file is never half-written; a leftover .toml.tmp is inert (ignored by the loader as an unknown sibling, never read).
- Unmapped KeyCode at save time: panic with the action name — a code bug surfaced immediately rather than silently writing an unusable file; guarded by the round-trip and format tests.

## Load Profile

No runtime load dimension: the settings file is a ~15-line, ~300-byte TOML document loaded once at startup and written only on explicit user change (S04). Nothing saturates at 10x — the only "load" is a single-shot fs read + parse (microseconds) and an atomic rename. No pool/rate-limit/caching protection applicable; the atomic tmp+rename write is the only durability protection needed, and it is implemented and tested.

## Negative Tests

All inline in `src/settings/mod.rs` tests (plus T01's in `src/settings/keybind.rs`):
- Missing file → defaults + exactly 1 warning, no panic: `missing_file_yields_defaults_and_single_warning`
- Corrupt TOML → all defaults + 1 warning: `invalid_toml_yields_all_defaults_and_single_warning` (T02)
- Per-field fallback (bad sibling type survives valid fields): `bad_fov_type_falls_back_but_valid_fields_load` (T02)
- Unknown keys/sections silently ignored, zero warnings: `unknown_keys_and_sections_are_ignored` (T02)
- Out-of-range clamp boundaries: `out_of_range_values_are_clamped_with_warnings` — fov 500.0 → 120.0, sensitivity 0.5 → 0.01 (T02)
- Unparsable keybind string → that field's default + warning (T02)
- Case-sensitivity / unknown key strings → None (T01 keybind tests)
- Duplicate keybinds → loads with both kept + 1 conflict warning naming both actions, validate reports conflict: `duplicate_keybinds_load_with_conflict_warning_and_both_values_kept`
- No false positives: `validate_keybinds_on_defaults_reports_no_conflicts`; triple duplicate → exactly one conflict listing all 3 (`triple_duplicate_reports_one_conflict_listing_all_actions`)
- Atomicity residue: `save_leaves_no_tmp_file_behind_on_success` (renamed-away tmp path absent; temp dir contains exactly 1 entry)
- Overwrite semantics: `save_creates_missing_parent_directories_and_overwrites_existing_file`

## Known Issues

None. The 12 clippy lib warnings are pre-existing (L002, src/mesh/src/world) and untouched by this task.

## Files Created/Modified

- `src/settings/mod.rs` — added KeybindConflict, Settings::validate_keybinds, atomic Settings::save via Serialize-only mirror, load-time conflict warnings, and 8 new tests

## Verification

cargo test — exit 0, 54 passed / 0 failed / 0 ignored (includes all 8 new T03 tests: non-default round-trip, defaults round-trip, saved-format pinning, duplicate-keybind load warning, validate-on-defaults, triple duplicate, parent-dir creation + overwrite, no tmp residue). cargo clippy — exit 0, 0 errors; 12 warnings are the pre-existing mesh/world lib lints (L002), zero in src/settings. All file-based tests use tempfile::tempdir(); no test reads gitignored paths.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo test` | 0 | passed | 348ms |
| 2 | `cargo clippy` | 0 | passed | 244ms |

## Deviations

One deliberate test deviation from the plan's literal example: the round-trip test uses jump_or_up = KeyCode::ArrowUp instead of the plan's suggested ShiftLeft, because ShiftLeft collides with the default crouch_or_down and would produce a conflict warning, contradicting the plan's own zero-warnings assertion for that test. The non-default intent of the example (a changed keybind plus sensitivity 0.005, fov 90.0, vsync true) is preserved.

## Known Issues

None.

## Files Created/Modified

- `src/settings/mod.rs`
<!-- gsd:state-version=93:1 -->
