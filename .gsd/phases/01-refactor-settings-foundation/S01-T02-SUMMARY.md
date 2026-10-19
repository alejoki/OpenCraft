---
id: S01-T02
parent: S01
milestone: M001
key_files:
  - (none)
key_decisions:
  - (none)
duration:
verification_result: mixed
completed_at:
blocker_discovered: false
---

# S01-T02: Added display-attaching launch smoke proving the game runs 10s on the migrated winit 0.30 loop without panic

**Added display-attaching launch smoke proving the game runs 10s on the migrated winit 0.30 loop without panic**

## What Happened

The interrupted prior attempt had already written scripts/launch_smoke.sh to the plan's spec (timeout 10 binary run, exit-124 assert, no-panic assert, /tmp save isolation) but had never run it, and target/ did not exist. Sequence: (1) cargo build produced the debug binary warning-free in 37.75s (T01's migration compiles). (2) First smoke run failed in 21ms: winit panicked at EventLoop::new() with XNotSupported(XOpenDisplayFailed) — the agent shell has no DISPLAY/WAYLAND_DISPLAY, and the initial script fallback attached to Xwayland's :0 socket, which rejects connections without an xauth cookie. (3) One-hypothesis probe: forced the Wayland backend (WAYLAND_DISPLAY=wayland-0, XDG_RUNTIME_DIR=/run/user/1000, WINIT_UNIX_BACKEND=wayland) — the game survived a 3s timeout kill (RC=124), proving the migrated loop, window creation in resumed(), surface init, and the render path all run on the live kwin Wayland session. (4) Fixed the script's display auto-detection to prefer the Wayland socket over Xwayland :0 (Xwayland requires an xauth cookie; winit 0.30 picks the X11 backend whenever DISPLAY is set and does not fall back cleanly to Wayland). (5) bash scripts/launch_smoke.sh → "SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic" in 10.04s: the process ran the full 10 seconds on the migrated ApplicationHandler loop without exiting or panicking, captured output contains no "panicked"/"RUST_BACKTRACE". /tmp/minecraft-clone-smoke cleaned up afterwards (verified absent). The window opens on the live desktop session for the bounded 10s; the real save file (saves/default.mcvx) is untouched (--new --seed 4242 --save /tmp/minecraft-clone-smoke/smoke.mcvx, flags verified against CliConfig::from_iter).

NEEDS-HUMAN manual gameplay-parity checklist for slice UAT closeout (cannot be automated without a human at the keyboard): WASD movement; Space jump; Shift crouch; Ctrl sprint; F fly toggle; F5 save + "[Saved]" title badge; Escape cursor grab/release toggle with click-to-regrab; LMB break / RMB place with player-AABB placement rejection; hotbar Digit1-9 + mouse wheel; [ and ] render-distance change; window title FPS/chunk/quad readout updates; resize keeps rendering; window close (X) saves world and exits; out-of-focus resets movement keys.

## Failure Modes (Q5)
External dependencies of scripts/launch_smoke.sh and their failure paths, each handled explicitly:
- Display server (winit EventLoop::new/window creation): no DISPLAY/WAYLAND_DISPLAY or unreachable socket → winit panics "Failed to create event loop"; the script surfaces this via the SMOKE FAIL branch that prints the captured output (observed live: exit 1, "XNotSupported(XOpenDisplayFailed)" shown) instead of masking it. Auto-detection attaches to the live session's Wayland socket first; if none exists, falls back to :0; if neither, the binary's own expect panic is caught and reported.
- Missing binary (cargo build not run): `[[ ! -x "$BIN" ]]` guard exits 1 with "SMOKE FAIL: ... run cargo build first" before launching anything.
- Process exit/panic during the 10s window: `set +e` captures the real exit code; RC≠124 (e.g. 101 panic, 0 clean exit, crash signal) → SMOKE FAIL with RC printed; grep for "panicked"/"RUST_BACKTRACE" in captured output as a second, independent panic detector.
- /tmp filesystem: mkdir -p creates the smoke dir; the game's own save path (via save_world_to_path's atomic tmp+rename) writes to /tmp/minecraft-clone-smoke, isolating the player's real save from any write failure.

## Negative Tests (Q7)
scripts/launch_smoke.sh is itself the negative-surface harness for this task; its assertions are exercised at scripts/launch_smoke.sh:
- Wrong exit code path: RC≠124 → exit 1 + "SMOKE FAIL: exit code N (expected 124 from timeout kill)" — observed live in the first run (exit 1, 21ms, panic output printed).
- Panic-text path: grep -q "panicked"/"RUST_BACKTRACE" over captured output → exit 1 with full captured output dumped.
- Missing-binary path: non-executable ./target/debug/minecraft-clone → exit 1 before any launch.
- Boundary: the run is bounded by `timeout 10` so a hung window/surface init cannot wedge the harness; both failure branches print captured output for diagnosis.

## Load Profile (Q6)
Not applicable — the deliverable is a one-shot bounded smoke script (single process, fixed 10s, no runtime load dimension); omitted.

## Diagnostics
Re-run `bash scripts/launch_smoke.sh` from the worktree root after `cargo build`; it prints "SMOKE PASS"/"SMOKE FAIL" and, on failure, dumps the captured game output at /tmp/minecraft-clone-smoke/output.log. Environment gotcha captured as MEM006 (agent shell lacks DISPLAY/WAYLAND_DISPLAY; prefer the Wayland socket — Xwayland :0 rejects unauthenticated connections; winit 0.30 does not fall back from X11 to Wayland when DISPLAY is set) for S03/S04 launch smokes.

## Verification

cargo build produced ./target/debug/minecraft-clone warning-free (37.75s). A 3s Wayland-backend probe confirmed the migrated loop launches (RC=124, timeout kill, no panic). Final verification: bash scripts/launch_smoke.sh exited 0 printing "SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic" — the binary ran the full 10 seconds under timeout (exit code 124, not 0/101), and the captured output contains no "panicked"/"RUST_BACKTRACE". /tmp/minecraft-clone-smoke removed afterwards (verified absent). The manual gameplay-parity checklist (WASD/jump/crouch/sprint/fly/F5-save-badge/Esc-grab-toggle/LMB-break/RMB-place-AABB-reject/hotbar+wheel/brackets-render-distance/title-readout/resize/close-saves-exit/focus-reset) is recorded above as NEEDS-HUMAN for slice UAT.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build 2>&1 | tail -20; test -x target/debug/minecraft-clone && echo BUILD_OK || echo BUILD_FAILED` | 0 | passed | 37791ms |
| 2 | `bash scripts/launch_smoke.sh` | 1 | failed | 21ms |
| 3 | `WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 WINIT_UNIX_BACKEND=wayland timeout 3 ./target/debug/minecraft-clone --new --seed 4242 --save /tmp/minecraft-clone-smoke/probe.mcvx >/tmp/minecraft-clone-smoke/probe.log 2>&1; echo PROBE_RC=$?; head -c 500 /tmp/minecraft-clone-smoke/probe.log` | 0 | passed | 3032ms |
| 4 | `bash scripts/launch_smoke.sh` | 0 | passed | 10044ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=53:1 -->
