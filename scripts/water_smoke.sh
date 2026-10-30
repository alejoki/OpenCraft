#!/usr/bin/env bash
# Bounded windowed water-pass smoke (M007/S04-T04).
#
# GPU work is not headless-provable, so this runs the real binary with the
# translucent water pipeline live on a deterministic ocean-bearing seed and
# asserts the event loop survived the whole bounded window:
#   (a) exit code is 124 — `timeout` killed it, meaning neither an early exit
#       (0/1) nor a panic (101) happened; the window stayed up.
#   (b) captured output contains no "panicked" / "RUST_BACKTRACE" text.
#
# Seed 12345: column (0,0) is real ocean at h=14 and the deterministic
# land-search spawn is (-60,-60) h=24 Forest (S01/S03 observed scans), so a
# directly-launched session renders real ocean water quads with no player
# input, well inside the default render distance (16 chunks).
#
# Best-effort frame capture (NEVER part of the PASS/FAIL contract): while the
# game is up, capture one screenshot into the temp dir when grim, import, or
# spectacle is available and a display was detected. grim covers wlroots
# compositors, import covers X11, and spectacle covers KDE Plasma (where grim
# is unavailable and ImageMagick's X11 grab sees only a black Xwayland root).
# The image is copied to a stable path before the temp dir is cleaned so the
# UAT record can reference it.
#
# The script builds nothing itself; it expects `cargo build --locked` to have
# produced ./target/debug/opencraft. Everything runs inside a mktemp temp dir,
# so the crate root's real saves/ and settings.toml are never touched.

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

DISPLAY_FOUND=0
if [[ -n "${DISPLAY:-}" || -n "${WAYLAND_DISPLAY:-}" ]]; then
    DISPLAY_FOUND=1
fi

# Resolve the binary absolutely before changing directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"

if [[ ! -x "$BIN" ]]; then
    echo "SMOKE FAIL: $BIN not found — run cargo build --locked first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-water-smoke.XXXXXX)" || {
    echo "SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
SHOT="$TEMP_DIR/water.png"
# Stable evidence path: the temp dir is removed on exit (saves included).
SHOT_KEEP="/tmp/opencraft-water-smoke-latest.png"
cleanup() {
    if [[ -s "$SHOT" ]]; then
        cp -f "$SHOT" "$SHOT_KEEP" 2>/dev/null || true
    fi
    rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

OUT="$TEMP_DIR/output.log"
SAVE="$TEMP_DIR/water.mcvx"

# Bounded window: 10s total, one capture attempt after the world has streamed.
RUN_SECONDS=10
CAPTURE_AFTER=7

# Best-effort only: never fatal, never affects the verdict.
capture_frame() {
    [[ $DISPLAY_FOUND -eq 1 ]] || return 1
    if command -v grim >/dev/null 2>&1; then
        timeout 5 grim "$SHOT" >/dev/null 2>&1 && [[ -s "$SHOT" ]] && return 0
    fi
    if command -v import >/dev/null 2>&1; then
        timeout 5 import -window root "$SHOT" >/dev/null 2>&1 && [[ -s "$SHOT" ]] && return 0
    fi
    if command -v spectacle >/dev/null 2>&1; then
        timeout 10 spectacle -b -n -f -o "$SHOT" >/dev/null 2>&1 && [[ -s "$SHOT" ]] && return 0
    fi
    return 1
}

cd "$TEMP_DIR"

set +e
timeout "$RUN_SECONDS" "$BIN" --new --seed 12345 --save "$SAVE" >"$OUT" 2>&1 &
RUN_PID=$!
sleep "$CAPTURE_AFTER"
capture_frame || true
wait "$RUN_PID"
RC=$?
set -e

if [[ $RC -ne 124 ]]; then
    echo "SMOKE FAIL: exit code $RC (expected 124 from the timeout kill)"
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

if [[ -s "$SHOT" ]]; then
    echo "SMOKE PASS: water pass live 10s (exit 124, no panic); frame captured: $SHOT_KEEP"
else
    echo "SMOKE PASS: water pass live 10s (exit 124, no panic); no frame captured (headless or capture tool unavailable)"
fi
exit 0
