#!/usr/bin/env bash
# Bounded launch smoke for the winit 0.30 ApplicationHandler loop (M001/S01-T02).
#
# Runs the game for 10 seconds on a fresh deterministic world and asserts that
# the process survived on the event loop the whole time:
#   (a) exit code is 124 — `timeout` killed it, meaning it neither exited early
#       nor panicked (a panic exits with 101, a clean loop keeps running).
#   (b) captured output contains no "panicked" / "RUST_BACKTRACE" text.
#
# The script builds nothing itself; it expects `cargo build` to have produced
# ./target/debug/minecraft-clone. The /tmp save path keeps the player's real
# save file untouched.

set -u

# Display auto-detection: agent/CI shells can lack DISPLAY/WAYLAND_DISPLAY even
# when a desktop session is live. Attach to the running session so winit can
# create its window. Prefer the Wayland socket: Xwayland's :0 needs an xauth
# cookie the launching shell may not have, and winit picks the X11 backend
# whenever DISPLAY is set (it does not fall back cleanly to Wayland).
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    if [[ -e "/run/user/$(id -u)/wayland-0" ]]; then
        export WAYLAND_DISPLAY="wayland-0"
        export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    elif [[ -e "/tmp/.X11-unix/X0" ]]; then
        export DISPLAY=":0"
    fi
fi

BIN="./target/debug/minecraft-clone"
SMOKE_DIR="/tmp/minecraft-clone-smoke"
OUT="$SMOKE_DIR/output.log"

if [[ ! -x "$BIN" ]]; then
    echo "SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

mkdir -p "$SMOKE_DIR"

set +e
timeout 10 "$BIN" --new --seed 4242 --save "$SMOKE_DIR/smoke.mcvx" >"$OUT" 2>&1
RC=$?
set -e

if [[ $RC -ne 124 ]]; then
    echo "SMOKE FAIL: exit code $RC (expected 124 from timeout kill)"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if grep -q "panicked" "$OUT" || grep -q "RUST_BACKTRACE" "$OUT"; then
    echo "SMOKE FAIL: panic detected in output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

echo "SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic"
exit 0
