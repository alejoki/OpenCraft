#!/usr/bin/env bash
# Real-binary calendar round-trip smoke (M007/S02-T05).
#
# Proves the v5 calendar tail through the real event loop with no menu
# automation and no test harness: create a fresh world with a chosen season
# length, let the bounded accelerated demo complete one full day cycle (the
# existing --time-scale seam saves and exits 0 on its own), quit, then
# relaunch the same save and assert the day index advanced by exactly one
# more with the season length byte-identical.
#
#   Run A: `--new --seed S --save P --time-scale 60 --season-length 4`
#     (a) exit code 0 (the accelerated demo latched its own shutdown; this is
#         not an external timeout kill).
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) the save exists, starts with the `MCVX` magic, and its version word
#         is 5 — the SAVE_VERSION that carries the calendar tail.
#     (d) day_index (u32 LE at byte offset 130) is exactly 1 and
#         season_length_days (u32 LE at byte offset 134) is the chosen 4.
#
#   Run B: `--save P --time-scale 60`
#     (a) exit code 0, no panic.
#     (b) day_index is exactly 2 — one more full cycle from the loaded day 1.
#     (c) season_length_days is byte-identical to run A (a load restores the
#         persisted calendar; no launch flag may change it).
#
# Why the offsets are fixed: a fresh `--new` run is edit-free by construction
# (no mining, no placement), so the variable-length chunk section writes a
# zero edit count and the layout is the header (36 + 27) + time_of_day (4) +
# chunk count (4) + v4 inventory extension (59) = 130 bytes before the v5
# tail. Those offsets are pinned by `V5_SEASON_OFFSET`/`V5_SEASON_LEN` in
# src/world/persistence.rs, so this script and the unit tests cannot drift
# apart silently.
#
# Why day 1 after run A: the default clock starts at noon (tick 6000) and one
# accelerated cycle is exactly 24,000 ticks, which returns to noon while
# crossing dawn (tick 0) exactly once — so the saved day index is 1. The
# second cycle resumes near noon and crosses dawn once more: day 2.
#
# The script builds nothing itself; it expects `cargo build --locked` to have
# produced ./target/debug/opencraft. Everything runs inside a mktemp working
# directory, so the crate root's saves/ and settings.toml are never touched.

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
    echo "SEASON SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-season-smoke.XXXXXX)"
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

# Calendar wire layout (see the header comment and persistence.rs).
V5_SEASON_OFFSET=130
V5_SEASON_LEN=8
V5_DAY_INDEX_OFFSET=$V5_SEASON_OFFSET
V5_SEASON_LENGTH_OFFSET=$((V5_SEASON_OFFSET + 4))

CHOSEN_SEASON_LENGTH=4
# 60x completes the 24,000-tick cycle in 20 s; the app's own wall-clock safety
# net fires at 25 s, and `timeout` is only an outer backstop for a hang.
TIME_SCALE=60
OUTER_TIMEOUT=90

read_u32_le() {
    od -An -tu4 --endian=little -j "$2" -N 4 "$1" | tr -d ' '
}

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/calendar.mcvx"
OUT_A="$TEMP_DIR/run_a.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed 4242 --save "$SAVE" \
    --time-scale "$TIME_SCALE" --season-length "$CHOSEN_SEASON_LENGTH" \
    >"$OUT_A" 2>&1
RC_A=$?
set -e

if [[ $RC_A -ne 0 ]]; then
    echo "SEASON SMOKE FAIL: run A exit code $RC_A (expected 0 from the bounded demo shutdown)"
    echo "--- captured output ---"
    cat "$OUT_A"
    exit 1
fi

if grep -q "panicked" "$OUT_A" || grep -q "RUST_BACKTRACE" "$OUT_A"; then
    echo "SEASON SMOKE FAIL: run A panic detected in output"
    echo "--- captured output ---"
    cat "$OUT_A"
    exit 1
fi

if [[ ! -f "$SAVE" ]]; then
    echo "SEASON SMOKE FAIL: run A exited 0 but wrote no save at $SAVE"
    exit 1
fi

SAVE_SIZE=$(wc -c <"$SAVE")
if [[ $SAVE_SIZE -lt $((V5_SEASON_OFFSET + V5_SEASON_LEN)) ]]; then
    echo "SEASON SMOKE FAIL: save is $SAVE_SIZE bytes, too short for the v5 calendar tail"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "SEASON SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 5 ]]; then
    echo "SEASON SMOKE FAIL: save version is $VERSION (expected 5)"
    exit 1
fi

DAY_A=$(read_u32_le "$SAVE" "$V5_DAY_INDEX_OFFSET")
SEASON_A=$(read_u32_le "$SAVE" "$V5_SEASON_LENGTH_OFFSET")

if [[ "$DAY_A" -ne 1 ]]; then
    echo "SEASON SMOKE FAIL: run A day_index is $DAY_A (expected 1 after one full cycle from noon)"
    exit 1
fi

if [[ "$SEASON_A" -ne "$CHOSEN_SEASON_LENGTH" ]]; then
    echo "SEASON SMOKE FAIL: run A season_length_days is $SEASON_A (expected $CHOSEN_SEASON_LENGTH)"
    exit 1
fi

# Keep the raw season-length bytes for the byte-identical comparison in run B.
SEASON_BYTES_A="$TEMP_DIR/season_length.run_a.bin"
dd if="$SAVE" of="$SEASON_BYTES_A" bs=1 skip="$V5_SEASON_LENGTH_OFFSET" count=4 status=none

echo "SEASON SMOKE: run A OK — created a day-1 world with $CHOSEN_SEASON_LENGTH-day seasons (exit 0)"

OUT_B="$TEMP_DIR/run_b.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --save "$SAVE" --time-scale "$TIME_SCALE" \
    >"$OUT_B" 2>&1
RC_B=$?
set -e

if [[ $RC_B -ne 0 ]]; then
    echo "SEASON SMOKE FAIL: run B exit code $RC_B (expected 0 from the bounded demo shutdown)"
    echo "--- captured output ---"
    cat "$OUT_B"
    exit 1
fi

if grep -q "panicked" "$OUT_B" || grep -q "RUST_BACKTRACE" "$OUT_B"; then
    echo "SEASON SMOKE FAIL: run B panic detected in output"
    echo "--- captured output ---"
    cat "$OUT_B"
    exit 1
fi

DAY_B=$(read_u32_le "$SAVE" "$V5_DAY_INDEX_OFFSET")
SEASON_B=$(read_u32_le "$SAVE" "$V5_SEASON_LENGTH_OFFSET")

if [[ "$DAY_B" -ne 2 ]]; then
    echo "SEASON SMOKE FAIL: run B day_index is $DAY_B (expected 2: the reloaded day 1 advanced by exactly one cycle)"
    exit 1
fi

if ! cmp -s "$SEASON_BYTES_A" <(dd if="$SAVE" bs=1 skip="$V5_SEASON_LENGTH_OFFSET" count=4 status=none); then
    echo "SEASON SMOKE FAIL: run B season_length_days bytes differ from run A (day $SEASON_A -> $SEASON_B)"
    exit 1
fi

if [[ "$SEASON_B" -ne "$CHOSEN_SEASON_LENGTH" ]]; then
    echo "SEASON SMOKE FAIL: run B season_length_days is $SEASON_B (expected $CHOSEN_SEASON_LENGTH)"
    exit 1
fi

echo "SEASON SMOKE PASS: create(day 0, ${CHOSEN_SEASON_LENGTH}-day seasons) -> demo cycle saved day 1 -> relaunch resumed and advanced to day 2 with the season length byte-identical across both saves"
