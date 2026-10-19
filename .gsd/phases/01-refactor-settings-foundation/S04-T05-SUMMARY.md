---
id: S04-T05
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

# S04-T05: Added settings smoke script proving seeded settings.toml survives a bounded game run byte-identical, and ran the full slice gate suite green (build, test, clippy, both smokes)

**Added settings smoke script proving seeded settings.toml survives a bounded game run byte-identical, and ran the full slice gate suite green (build, test, clippy, both smokes)**

## What Happened

Created scripts/settings_smoke.sh, cloning launch_smoke.sh's structure and environment discipline: display auto-detection (WAYLAND_DISPLAY=wayland-0 / XDG_RUNTIME_DIR attach when the shell lacks DISPLAY, per MEM006), bounded `timeout 10` run with exit-124-as-pass semantics, and panic-text scan. The key difference per the plan: the script creates a temp working directory (mktemp -d under /tmp, trap-based cleanup) and cds there before launching, because settings.toml is cwd-relative (DEFAULT_SETTINGS_PATH) — verified after both smoke runs that no crate-root settings.toml was created and all temp dirs were cleaned up. The seeded settings.toml pins exactly the documented saved format (as locked by `saved_file_matches_the_documented_format` in src/settings/mod.rs): jump_or_up = "ShiftLeft" (the roadmap proof rebind) with the other six keybinds at defaults, sensitivity = 0.0022, fov = 75.0, fullscreen = false, vsync = false. Note: the seed deliberately contains jump_or_up and crouch_or_down both on ShiftLeft — exactly "jump rebound, other six at defaults" per the plan — and the loader's duplicate-binding policy (keep both + warn, never block launch) handles it; the run stays read-only so the byte-identical assert holds.

Assertions in the script: (a) exit code 124 (timeout kill = survived the bound); (b) no "panicked"/"RUST_BACKTRACE" in captured output; (c) settings.toml still exists in the temp cwd and is byte-identical (cmp) to the seeded copy — the game must not rewrite an unchanged settings file; (d) prints SMOKE PASS / exits non-zero with captured output on any failure.

Full slice gate suite, all green: cargo build (current, warning-free on the slice surface), cargo test (83 lib + 11 settings = 94 passed, 0 failed — includes all 24 new src/ui/menu.rs tests), cargo clippy (exit 0; zero findings on src/ui/menu.rs, src/ui/mod.rs, src/settings/mod.rs, src/main.rs — the 12 findings are all in mesh/greedy.rs, mesh/quad.rs, world/manager.rs, world/persistence.rs, world/terrain.rs, inherited and out of scope per MEM007/MEM017), bash scripts/launch_smoke.sh (SMOKE PASS — menu wiring did not regress plain launch), bash scripts/settings_smoke.sh (SMOKE PASS).

## Failure Modes

External dependencies and their failure paths, each handled:
- Missing game binary: script checks -x on the absolute-resolved $BIN before anything else and fails with a clear message + exit 1 (never starts).
- Missing display server (agent/CI shell): auto-detection attaches to the live Wayland/X11 session; if neither socket exists, winit fails to open a window and the process exits non-124 → the script's exit-code check fails the run loudly (no false PASS possible).
- Game panic/crash mid-run: exits 101 (or any non-124 code) → assertion (a) fails; panic text in output → assertion (b) fails; both print captured output for diagnosis.
- settings.toml deleted or rewritten by the game: assertions (c) missing-file and cmp-byte-identity both fail with actual-vs-expected diffs printed.
- Save/load IO inside the game: the settings loader never blocks launch (missing/corrupt file yields defaults + one structured warning, verified by the negative tests in src/settings/mod.rs); save failures bubble and surface as an in-game menu message (save_settings_or_warn in src/main.rs), not a crash.

## Load Profile

## Negative Tests

Negative coverage protecting this slice, by file:
- src/settings/mod.rs tests: missing file → defaults + exactly one warning; syntactically invalid TOML → defaults + one warning; bad field type (fov = "wide") falls back to default while valid siblings survive with a scoped video.fov warning; out-of-range clamping on load; duplicate keybinds load with a conflict warning and both values kept; unknown keys warn.
- src/ui/menu.rs tests (24): hit-test boundaries are half-open like the pixel grid; clamping bounds are inclusive and match the load-path constants; rebind capture transitions (accept/cancel); duplicate-key rejection; unsupported-key rejection; accepted rebinds survive the Settings validate_keybinds scan; bind_action names match validate strings.
- scripts/settings_smoke.sh itself asserts the failure paths end-to-end: non-124 exit code, panic text in output, and missing/corrupted settings.toml each print SMOKE FAIL with diagnostic output and exit non-zero.

NEEDS-HUMAN items for slice UAT (run-uat disposition, same as S03's legibility items):
(a) Interactive press-to-rebind — open menu, click the jump row, press ShiftLeft, see binding accepted and shown; duplicate warning when pressing an already-bound key; unsupported-key rejection.
(b) Menu legibility and layout — text readable, rows hover-highlight, panel visible over gameplay.
(c) Live apply visible — FOV change visibly alters the projection, fullscreen toggle works, sensitivity change alters look speed.
(d) Restart persistence interactively — rebind, quit, relaunch, binding still in effect and shown in the menu.

## Verification

All five gates passed: (1) cargo build — exit 0, binary current; (2) cargo test — 94 tests passed, 0 failed (83 lib incl. 24 menu tests + 11 settings), 0 ignored; (3) cargo clippy — exit 0 with zero findings on the four in-slice files (all 12 findings are inherited mesh/world warnings, out of scope per MEM007/MEM017); (4) bash scripts/launch_smoke.sh — SMOKE PASS, menu wiring did not regress plain launch; (5) bash scripts/settings_smoke.sh — SMOKE PASS: 10s bounded run on the seeded settings.toml (jump = ShiftLeft), exit 124, no panic, file byte-identical after the run. Also verified post-run that no crate-root settings.toml was created and all temp dirs were cleaned up (trap EXIT).

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | pass | 198ms |
| 2 | `cargo test` | 0 | pass | 297ms |
| 3 | `cargo clippy` | 0 | pass | 221ms |
| 4 | `bash scripts/settings_smoke.sh` | 0 | pass | 10054ms |
| 5 | `bash scripts/launch_smoke.sh` | 0 | pass | 10043ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=192:1 -->
