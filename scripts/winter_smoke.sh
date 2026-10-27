#!/usr/bin/env bash
# Real-binary mid-winter save round-trip smoke (M007/S08-T03).
#
# Proves the v5 calendar tail survives a cold process restart through the real
# event loop, and that the season derived from it really is Winter:
#
#   Run A: `--new --seed 1 --season-length 1 --save P --time-scale 60
#           --demo-days 3`
#     Seed 1 starts in Spring with one-day seasons, so after three bounded
#     accelerated day cycles the persisted calendar is day_index 3 = Winter.
#     (a) exit code 0 (the multi-cycle demo watch latched its own shutdown;
#         a `timeout` kill surfaces as 124 and is an explicit failure here).
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) the save exists, starts with the `MCVX` magic, and its version word
#         is 5 — the SAVE_VERSION that carries the calendar tail.
#     (d) day_index (u32 LE at byte offset 130) is exactly 3 — the mid-winter
#         save.
#     (e) season_length_days (u32 LE at byte offset 134) is the chosen 1.
#
#   Run B: `--save P --time-scale 60 --demo-days 1`
#     (a) exit code 0, no panic.
#     (b) day_index is exactly 4 — the reloaded day-3 calendar resumed and
#         advanced by exactly one more cycle.
#     (c) season_length_days is byte-identical to run A (`cmp`, not just the
#         decoded value): a load restores the persisted calendar and no launch
#         flag may change it.
#
# Why the offsets are fixed: a fresh `--new` run is edit-free by construction
# (no mining, no placement), so the variable-length chunk section writes a
# zero edit count and the layout is the header (36 + 27) + time_of_day (4) +
# chunk count (4) + v4 inventory extension (59) = 130 bytes before the v5
# tail. Those offsets are pinned by `V5_SEASON_OFFSET`/`V5_SEASON_LEN` in
# src/world/persistence.rs, so this script and the unit tests cannot drift
# apart silently. Any header change before the chunk section invalidates those
# offsets: update the persistence test consts first, then copy them here.
#
# Why day 3 after run A: the default clock starts at noon (tick 6000) and the
# T5 multi-cycle watch completes only after three summed 24,000-tick cycles,
# i.e. after crossing dawn (tick 0) three times, which lands the calendar in
# Winter at day 3. The bounded shutdown then saves the calendar tail.
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
    echo "WINTER SMOKE FAIL: $BIN not found — run cargo build --locked first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-winter-smoke.XXXXXX)" || {
    echo "WINTER SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

# Calendar wire layout (see the header comment and persistence.rs).
V5_SEASON_OFFSET=130
V5_SEASON_LEN=8
V5_DAY_INDEX_OFFSET=$V5_SEASON_OFFSET
V5_SEASON_LENGTH_OFFSET=$((V5_SEASON_OFFSET + 4))

SEED=1
SEASON_LENGTH=1
DEMO_DAYS=3
EXPECTED_DAY_A=$DEMO_DAYS
EXPECTED_DAY_B=$((DEMO_DAYS + 1))
# 60x completes one 24,000-tick cycle in 20 s; three cycles are 60 s. The app's
# own scaled safety net fires at 60 + 5 s, and `timeout` is only an outer
# backstop for a hang (a kill here means the demo never latched).
TIME_SCALE=60
OUTER_TIMEOUT=180

read_u32_le() {
    od -An -tu4 --endian=little -j "$2" -N 4 "$1" | tr -d ' '
}

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/winter.mcvx"
OUT_A="$TEMP_DIR/run_a.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed "$SEED" --save "$SAVE" \
    --season-length "$SEASON_LENGTH" --time-scale "$TIME_SCALE" \
    --demo-days "$DEMO_DAYS" >"$OUT_A" 2>&1
RC_A=$?
set -e

if [[ $RC_A -ne 0 ]]; then
    echo "WINTER SMOKE FAIL: run A exit code $RC_A (expected 0 from the bounded three-day demo shutdown; 124 means the demo never latched and an external timeout killed it)"
    echo "--- captured output ---"
    cat "$OUT_A"
    exit 1
fi

if grep -q "panicked" "$OUT_A" || grep -q "RUST_BACKTRACE" "$OUT_A"; then
    echo "WINTER SMOKE FAIL: run A panic detected in output"
    echo "--- captured output ---"
    cat "$OUT_A"
    exit 1
fi

if [[ ! -f "$SAVE" ]]; then
    echo "WINTER SMOKE FAIL: run A exited 0 but wrote no save at $SAVE"
    exit 1
fi

SAVE_SIZE=$(wc -c <"$SAVE")
if [[ $SAVE_SIZE -lt $((V5_SEASON_OFFSET + V5_SEASON_LEN)) ]]; then
    echo "WINTER SMOKE FAIL: save is $SAVE_SIZE bytes, too short for the v5 calendar tail"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "WINTER SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 5 ]]; then
    echo "WINTER SMOKE FAIL: save version is $VERSION (expected 5)"
    exit 1
fi

DAY_A=$(read_u32_le "$SAVE" "$V5_DAY_INDEX_OFFSET")
SEASON_A=$(read_u32_le "$SAVE" "$V5_SEASON_LENGTH_OFFSET")

if [[ "$DAY_A" -ne "$EXPECTED_DAY_A" ]]; then
    echo "WINTER SMOKE FAIL: run A day_index is $DAY_A (expected $EXPECTED_DAY_A after $DEMO_DAYS full cycles from noon — the mid-winter save)"
    exit 1
fi

if [[ "$SEASON_A" -ne "$SEASON_LENGTH" ]]; then
    echo "WINTER SMOKE FAIL: run A season_length_days is $SEASON_A (expected the chosen $SEASON_LENGTH)"
    exit 1
fi

# Keep the raw season-length bytes for the byte-identical comparison in run B.
SEASON_BYTES_A="$TEMP_DIR/season_length.run_a.bin"
dd if="$SAVE" of="$SEASON_BYTES_A" bs=1 skip="$V5_SEASON_LENGTH_OFFSET" \
    count=4 status=none

echo "WINTER SMOKE: run A OK — mid-winter save at day $DAY_A with ${SEASON_A}-day seasons (exit 0)"

OUT_B="$TEMP_DIR/run_b.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --save "$SAVE" --time-scale "$TIME_SCALE" \
    --demo-days 1 >"$OUT_B" 2>&1
RC_B=$?
set -e

if [[ $RC_B -ne 0 ]]; then
    echo "WINTER SMOKE FAIL: run B exit code $RC_B (expected 0 from the bounded demo shutdown; 124 means the demo never latched and an external timeout killed it)"
    echo "--- captured output ---"
    cat "$OUT_B"
    exit 1
fi

if grep -q "panicked" "$OUT_B" || grep -q "RUST_BACKTRACE" "$OUT_B"; then
    echo "WINTER SMOKE FAIL: run B panic detected in output"
    echo "--- captured output ---"
    cat "$OUT_B"
    exit 1
fi

DAY_B=$(read_u32_le "$SAVE" "$V5_DAY_INDEX_OFFSET")
SEASON_B=$(read_u32_le "$SAVE" "$V5_SEASON_LENGTH_OFFSET")

if [[ "$DAY_B" -ne "$EXPECTED_DAY_B" ]]; then
    echo "WINTER SMOKE FAIL: run B day_index is $DAY_B (expected $EXPECTED_DAY_B: the reloaded day-$DAY_A calendar advanced by exactly one cycle)"
    exit 1
fi

if ! cmp -s "$SEASON_BYTES_A" <(dd if="$SAVE" bs=1 skip="$V5_SEASON_LENGTH_OFFSET" count=4 status=none); then
    echo "WINTER SMOKE FAIL: run B season_length_days bytes differ from run A (decoded $SEASON_A -> $SEASON_B)"
    exit 1
fi

if [[ "$SEASON_B" -ne "$SEASON_LENGTH" ]]; then
    echo "WINTER SMOKE FAIL: run B season_length_days is $SEASON_B (expected $SEASON_LENGTH)"
    exit 1
fi

echo "WINTER SMOKE PASS: seed $SEED created a Winter world (day $DAY_A, ${SEASON_A}-day seasons) via the real binary, relaunched it, and resumed the calendar to day $DAY_B with the season-length bytes byte-identical across both saves"
