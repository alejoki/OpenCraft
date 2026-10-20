#!/usr/bin/env bash
# Bounded menu-path smoke for the bare-launch main menu (M002/S04-T01).
#
# launch_smoke.sh and settings_smoke.sh both launch with --new/--seed/--save
# CLI flags, which bypass every menu screen (the cli_has_world_intent
# direct-launch boundary). This script covers the OTHER launch mode — a bare
# launch with no CLI flags opens the main menu — and proves on the real event
# loop:
#
#   Variant A (fresh fallback): an empty cwd (no saves/ directory, no
#     settings.toml) bare-launches into the main menu and survives 10s.
#       (a) exit code 124 — `timeout` killed it, meaning it neither exited
#           early nor panicked.
#       (b) captured output contains no "panicked" / "RUST_BACKTRACE" text.
#
#   Variant B (corrupt default degradation): a junk-byte saves/default.mcvx
#     degrades to a fresh menu session at startup without destroying the
#     user's file — startup degradation must not delete or rewrite the
#     corrupt file, and nothing may autosave the startup session while
#     sitting on the menu.
#       (a) exit code 124, no panic (as variant A).
#       (b) saves/default.mcvx still exists and is byte-identical to the
#           seeded junk (cmp against a saved copy).
#
# The script builds nothing itself; it expects `cargo build` to have produced
# ./target/debug/opencraft. Both variants run ONLY inside mktemp working
# directories, so the crate root's real saves/ and settings.toml are never
# touched.

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
    echo "MENU SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

TEMP_DIR_A="$(mktemp -d /tmp/opencraft-menu-smoke-a.XXXXXX)"
TEMP_DIR_B="$(mktemp -d /tmp/opencraft-menu-smoke-b.XXXXXX)"
cleanup() { rm -rf "$TEMP_DIR_A" "$TEMP_DIR_B"; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Variant A (fresh fallback): bare launch in an empty temp cwd — no saves/
# directory, no settings.toml — must sit on the main menu for the full 10s.
# ---------------------------------------------------------------------------
cd "$TEMP_DIR_A"
OUT_A="$TEMP_DIR_A/output.log"

set +e
timeout 10 "$BIN" >"$OUT_A" 2>&1
RC_A=$?
set -e

if [[ $RC_A -ne 124 ]]; then
    echo "MENU SMOKE FAIL: variant A exit code $RC_A (expected 124 from timeout kill)"
    echo "--- captured output ---"
    cat "$OUT_A"
    exit 1
fi

if grep -q "panicked" "$OUT_A" || grep -q "RUST_BACKTRACE" "$OUT_A"; then
    echo "MENU SMOKE FAIL: variant A panic detected in output"
    echo "--- captured output ---"
    cat "$OUT_A"
    exit 1
fi

echo "MENU SMOKE PASS: variant A (fresh fallback) — bare launch in an empty cwd opened the main menu and survived 10s on the event loop, exit 124, no panic"

# ---------------------------------------------------------------------------
# Variant B (corrupt default degradation): a junk-byte saves/default.mcvx
# degrades to a fresh menu session non-destructively — no rewrite, no
# menu-path autosave of the startup session.
# ---------------------------------------------------------------------------
cd "$TEMP_DIR_B"
mkdir saves
printf 'not an mcvx save' > saves/default.mcvx
EXPECTED_B="$TEMP_DIR_B/saves/default.mcvx.expected"
cp saves/default.mcvx "$EXPECTED_B"
OUT_B="$TEMP_DIR_B/output.log"

set +e
timeout 10 "$BIN" >"$OUT_B" 2>&1
RC_B=$?
set -e

if [[ $RC_B -ne 124 ]]; then
    echo "MENU SMOKE FAIL: variant B exit code $RC_B (expected 124 from timeout kill)"
    echo "--- captured output ---"
    cat "$OUT_B"
    exit 1
fi

if grep -q "panicked" "$OUT_B" || grep -q "RUST_BACKTRACE" "$OUT_B"; then
    echo "MENU SMOKE FAIL: variant B panic detected in output"
    echo "--- captured output ---"
    cat "$OUT_B"
    exit 1
fi

if [[ ! -f "$TEMP_DIR_B/saves/default.mcvx" ]]; then
    echo "MENU SMOKE FAIL: variant B — saves/default.mcvx missing after the run (startup degradation deleted the corrupt file)"
    exit 1
fi

if ! cmp -s "$TEMP_DIR_B/saves/default.mcvx" "$EXPECTED_B"; then
    echo "MENU SMOKE FAIL: variant B — saves/default.mcvx was rewritten during the run (destructive degradation or menu-path autosave)"
    echo "--- actual ---"
    cat "$TEMP_DIR_B/saves/default.mcvx"
    echo "--- expected ---"
    cat "$EXPECTED_B"
    exit 1
fi

echo "MENU SMOKE PASS: variant B (corrupt default degradation) — bare launch over a junk saves/default.mcvx degraded to a fresh menu session and survived 10s, exit 124, no panic, corrupt file byte-identical (no destructive rewrite, no menu-path autosave)"

exit 0
