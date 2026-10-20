#!/usr/bin/env bash
# Bounded settings smoke for the in-game settings menu (M001/S04-T05).
#
# Verifies the restart-persistence half of the roadmap proof automatically:
# settings.toml is loaded from the cwd (DEFAULT_SETTINGS_PATH), and the game
# must not corrupt or rewrite an unchanged settings file during a run.
# The interactive half (press-to-rebind in the live menu) is NEEDS-HUMAN UAT.
#
# Assertions:
#   (a) bounded run exits 124 — `timeout` killed it, meaning the process
#       survived on the event loop the whole time (no early exit, no panic).
#   (b) captured output contains no "panicked" / "RUST_BACKTRACE" text.
#   (c) settings.toml still exists in the run cwd and is byte-identical to
#       the seeded content (the game only saves on accepted changes).
#
# Unlike launch_smoke.sh, this runs the game inside a TEMP WORKING DIRECTORY
# so the crate-root settings.toml is never created or clobbered.

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

# Resolve the binary absolutely before changing directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"

if [[ ! -x "$BIN" ]]; then
    echo "SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-settings-smoke.XXXXXX)"
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

# Seeded settings.toml: the documented saved format exactly as pinned by the
# `saved_file_matches_the_documented_format` test in src/settings/mod.rs —
# jump rebound to ShiftLeft (the roadmap proof's rebind) plus known-good,
# in-range mouse/video values and the other six keybinds at defaults.
cat > "$TEMP_DIR/settings.toml" <<'EOF'
[keybinds]
forward = "KeyW"
backward = "KeyS"
left = "KeyA"
right = "KeyD"
jump_or_up = "ShiftLeft"
crouch_or_down = "ShiftLeft"
sprint = "ControlLeft"

[mouse]
sensitivity = 0.0022

[video]
fov = 75.0
fullscreen = false
vsync = false
EOF
EXPECTED="$TEMP_DIR/settings.toml.expected"
cp "$TEMP_DIR/settings.toml" "$EXPECTED"

# Run from the temp cwd so the game reads (and would write) settings.toml there.
cd "$TEMP_DIR"
OUT="$TEMP_DIR/output.log"

set +e
timeout 10 "$BIN" --new --seed 4242 --save "$TEMP_DIR/smoke.mcvx" >"$OUT" 2>&1
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

if [[ ! -f "$TEMP_DIR/settings.toml" ]]; then
    echo "SMOKE FAIL: settings.toml missing from run cwd after the run"
    exit 1
fi

if ! cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED"; then
    echo "SMOKE FAIL: settings.toml was rewritten or corrupted during the run"
    echo "--- actual ---"
    cat "$TEMP_DIR/settings.toml"
    echo "--- expected ---"
    cat "$EXPECTED"
    exit 1
fi

echo "SMOKE PASS: ran 10s on a seeded settings.toml (jump = ShiftLeft), exit 124, no panic, file byte-identical"
exit 0
