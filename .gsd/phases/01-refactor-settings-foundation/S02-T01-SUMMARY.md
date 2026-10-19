---
id: T01
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

# T01: Added settings module skeleton with serde/toml deps, Keybinds struct (defaults = today's hardcoded bindings), and a hand-mapped 80-key KeyCode↔stable-string table with round-trip, Debug-spelling, and case-sensitivity tests green

**Added settings module skeleton with serde/toml deps, Keybinds struct (defaults = today's hardcoded bindings), and a hand-mapped 80-key KeyCode↔stable-string table with round-trip, Debug-spelling, and case-sensitivity tests green**

## What Happened

# T01: Settings foundation: deps, module skeleton, Keybinds struct, KeyCode string conversion

**Added the settings module skeleton: serde/toml/tempfile deps, `Keybinds` struct with today's hardcoded defaults, and a hand-mapped KeyCode↔string table (80 keys) with red-green-tested conversion functions.**

## What Happened

TDD red-green per the tdd skill:

1. Added `serde = { version = "1", features = ["derive"] }`, `toml = "1"`, and dev-dependency `tempfile = "3"` to Cargo.toml; registered `pub mod settings;` in src/lib.rs; added `/settings.toml` to .gitignore next to the existing `/saves` entry.
2. Created `src/settings/keybind.rs` with the full test suite first and an **empty** `KEY_TABLE` stub. RED run (`cargo test`, exit 101): the 4 parse tests failed against the empty table — 28 passed (23 pre-existing + default test + vacuous-loop/None tests), proving the tests bite.
3. GREEN: filled the hand-mapped `KEY_TABLE` (80 entries: KeyA–KeyZ, Digit0–Digit9, Space, Shift/Control/Alt L+R, Tab, Enter, Escape, Backquote, Arrow keys, F1–F12, Numpad0–Numpad9). `cargo build` exit 0; `cargo test` exit 0 with **32 passed, 0 failed** — 23 existing tests unregressed plus 9 new (8 in keybind.rs, 1 in mod.rs).
4. Created `src/settings/mod.rs`: `pub mod keybind;`, `Keybinds` with exactly the 7 fields mirroring `InputState` (forward, backward, left, right, jump_or_up, crouch_or_down, sprint), `Default` = KeyW/KeyS/KeyA/KeyD/Space/ShiftLeft(crouch)/ControlLeft(sprint) matching src/main.rs. The ShiftLeft|ShiftRight and ControlLeft|ControlRight pair-narrowing is documented in a doc comment on the struct (intentional: one KeyCode per action; the un-bound twin stops working when S04 wires it).
5. The `every_mapped_string_is_the_keycode_debug_spelling` test pins the whole table against `format!("{:?}", code)` — the hand-mapped strings are guaranteed to be the exact KeyCode Debug spellings without depending on winit's serde feature (decision D003/MEM003 honored: no winit serde feature enabled).

## Verification

`cargo build` exit 0 (dev profile, 2.57s). `cargo test` exit 0: 32 passed, 0 failed, 0 ignored — existing 23 tests unregressed (verified by reading full test output from the gsd_exec stdout file, not just exit code). RED phase observed before implementation (exit 101 with 4 expected failures against the stub table). No test reads .gitignore or any gitignored path: all T01 tests are pure in-memory string/enum assertions, no file I/O.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | cargo build | 0 | pass | 2613ms |
| 2 | cargo test | 0 | pass | 957ms |

(The RED-phase run `cargo test` against the stub table exited 101 with the 4 expected parse-test failures — intentional TDD red, not a verification failure.)

## Diagnostics

Behavior is inspectable via `cargo test settings` (runs the 9 settings tests). The conversion surface is `settings::keybind::{key_to_string, parse_key}` and `settings::Keybinds::default()`.

## Failure Modes

T01's runtime surface is a pure in-memory lookup table — no filesystem, network, GPU, or subprocess dependencies. Failure paths:

- **Unknown/unparsable key string at settings load (T02+)**: `parse_key` returns `None`, not an error or panic — pinned by `unsupported_strings_return_none` ("NotAKey", lowercase "keyw", uppercase "KEYW", empty string, real-but-unmapped "Semicolon", out-of-range "F13").
- **Key outside the supported subset at save time**: `key_to_string` returns `None` rather than guessing a spelling — pinned by `keys_outside_the_supported_subset_return_none_from_key_to_string` (Semicolon, Minus, Backslash).
- **winit version bump renaming KeyCode variants**: a compile error caught by `cargo build`, not a silent runtime failure; the Debug-spelling test would also catch any table drift at test time.
- External dependency added is build-time only (serde/toml/tempfile via Cargo); resolution failures would fail `cargo build` loudly.

## Load Profile

## Negative Tests

- `keybind::tests::unsupported_strings_return_none` — unknown string, case-sensitivity both directions ("keyw"/"KEYW"), empty string, real-but-unmapped key name, key beyond the supported F-range all return `None`.
- `keybind::tests::keys_outside_the_supported_subset_return_none_from_key_to_string` — unmapped KeyCodes return `None` from the string direction.
- `keybind::tests::every_supported_key_round_trips` / `every_mapped_string_is_the_keycode_debug_spelling` — table-integrity guards: any missing entry or non-Debug string fails the suite.
- `settings::tests::keybinds_defaults_match_todays_hardcoded_bindings` — pins that defaults stay behavior-identical to today's hardcoded bindings (regression guard for the S04 wiring swap).

## Deviations

Minor: added one small test in `src/settings/mod.rs` (`keybinds_defaults_match_todays_hardcoded_bindings`) beyond the plan's keybind.rs-only test scope — it pins the Default impl against today's hardcoded values, which T02's "defaults equal today's hardcoded values" test also relies on. No other deviations.

## Known Issues

None.

## Files Created/Modified

- `Cargo.toml` — added serde (derive), toml, and dev-dependency tempfile
- `src/lib.rs` — registered `pub mod settings;`
- `src/settings/mod.rs` — new: Keybinds struct (7 fields), Default matching today's bindings, pair-narrowing doc comment, default-pinning test
- `src/settings/keybind.rs` — new: hand-mapped 80-entry KeyCode↔string KEY_TABLE, key_to_string/parse_key, 8 tests (round-trip, Debug-spelling, case-sensitivity, unmapped-subset)
- `.gitignore` — added `/settings.toml` next to `/saves`

observability: none (pure logic, no logging framework this slice — warnings surface as structured data starting T02)

## Verification

cargo build exit 0; cargo test exit 0 with 32 passed / 0 failed (23 pre-existing tests unregressed + 9 new settings tests). RED phase observed first: cargo test against the empty-table stub exited 101 with the 4 parse tests failing, proving test coverage bites. No test reads gitignored paths — all T01 tests are in-memory assertions.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | pass | 2613ms |
| 2 | `cargo test` | 0 | pass | 957ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=93:1 -->
