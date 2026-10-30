#!/usr/bin/env bash
# Real-binary menu-background safety smoke (M012/S03).
#
# Proves the background save lifecycle on the real event loop, without menu
# automation:
#
#   Variant A (fresh install): an empty cwd bare-launches into the main menu
#     and survives 10 s. Launching must not create saves/background/ — the
#     deterministic demo scenery is read-only until the player edits it.
#
#   Variant B (corrupt file): a junk-byte saves/background/background.mcvx
#     must not block launch and must not be repaired, rewritten, or deleted.
#     The process survives 10 s and the bytes are identical afterwards.
#
#   Variant C (valid file): create a real v5 save at the background path with
#     the bounded accelerated demo (--new/--save), then bare-launch the menu
#     for 10 s and assert the file is byte-identical. The menu is a reader:
#     idling on it never writes the background.
#
# The script builds nothing itself; it expects `cargo build --locked` to have
# produced ./target/debug/opencraft. Every variant runs inside its own
# mktemp working directory, so the crate root's saves/ and settings.toml are
# never touched.

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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"
MENU_TIMEOUT=10
DEMO_TIMEOUT=60
# 120x completes the 24,000-tick demo day in 10 s; the app's own safety net
# fires a few seconds later, and `timeout` is only an outer backstop.
DEMO_TIME_SCALE=120

if [[ ! -x "$BIN" ]]; then
    echo "BACKGROUND SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-background-smoke.XXXXXX)"
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "BACKGROUND SMOKE FAIL: $*"
    if [[ -n "${LOG:-}" && -f "$LOG" ]]; then
        echo "--- captured output ($LOG) ---"
        cat "$LOG"
    fi
    exit 1
}

# Run a bare menu launch in `$1`; assert a timeout kill (124) and no panic.
run_menu() {
    local dir="$1"
    local label="$2"
    LOG="$dir/menu.log"
    (
        cd "$dir" || exit 1
        timeout "$MENU_TIMEOUT" "$BIN" >"$LOG" 2>&1
    )
    local rc=$?
    if [[ $rc -ne 124 ]]; then
        fail "$label: bare launch exit $rc (expected 124 from the menu timeout kill)"
    fi
    if grep -q "panicked" "$LOG" || grep -q "RUST_BACKTRACE" "$LOG"; then
        fail "$label: panic detected in the menu output"
    fi
    echo "BACKGROUND SMOKE: $label OK — menu survived ${MENU_TIMEOUT}s, no panic"
}

# --- Variant A: fresh install creates no background file -------------------
EMPTY_DIR="$TEMP_DIR/empty"
mkdir -p "$EMPTY_DIR"
run_menu "$EMPTY_DIR" "fresh install"
if [[ -e "$EMPTY_DIR/saves/background/background.mcvx" ]]; then
    fail "a fresh launch created $EMPTY_DIR/saves/background/background.mcvx"
fi
echo "BACKGROUND SMOKE: fresh install wrote no background save"

# --- Variant B: corrupt background is untouched ----------------------------
CORRUPT_DIR="$TEMP_DIR/corrupt"
mkdir -p "$CORRUPT_DIR/saves/background"
CORRUPT_BG="$CORRUPT_DIR/saves/background/background.mcvx"
CORRUPT_ORIG="$TEMP_DIR/corrupt.orig"
printf 'this is not a world file\n' >"$CORRUPT_BG"
cp "$CORRUPT_BG" "$CORRUPT_ORIG"
run_menu "$CORRUPT_DIR" "corrupt background"
if ! cmp -s "$CORRUPT_BG" "$CORRUPT_ORIG"; then
    fail "launch repaired or rewrote the corrupt background file"
fi
echo "BACKGROUND SMOKE: corrupt background left byte-identical"

# --- Variant C: valid background is not rewritten while on the menu --------
VALID_DIR="$TEMP_DIR/valid"
mkdir -p "$VALID_DIR"
VALID_BG="$VALID_DIR/saves/background/background.mcvx"
VALID_ORIG="$TEMP_DIR/valid.orig"
LOG="$VALID_DIR/demo.log"
(
    cd "$VALID_DIR" || exit 1
    timeout "$DEMO_TIMEOUT" "$BIN" --new --seed 4242 \
        --save saves/background/background.mcvx \
        --time-scale "$DEMO_TIME_SCALE" --demo-days 1 >"$LOG" 2>&1
)
RC_DEMO=$?
if [[ $RC_DEMO -ne 0 ]]; then
    fail "bounded demo exit $RC_DEMO (expected 0 from its own shutdown)"
fi
if grep -q "panicked" "$LOG" || grep -q "RUST_BACKTRACE" "$LOG"; then
    fail "panic detected while creating the valid background save"
fi
if [[ ! -f "$VALID_BG" ]]; then
    fail "bounded demo exited 0 but wrote no save at $VALID_BG"
fi
MAGIC="$(head -c 4 "$VALID_BG")"
if [[ "$MAGIC" != "MCVX" ]]; then
    fail "background save magic is '$MAGIC' (expected MCVX)"
fi
cp "$VALID_BG" "$VALID_ORIG"
run_menu "$VALID_DIR" "valid background"
if ! cmp -s "$VALID_BG" "$VALID_ORIG"; then
    fail "idling on the menu rewrote the valid background save"
fi
echo "BACKGROUND SMOKE: valid background left byte-identical while on the menu"

echo "BACKGROUND SMOKE PASS: no file on fresh launch, corrupt file untouched, valid file read-only"
exit 0
