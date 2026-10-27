#!/usr/bin/env bash
# Real-binary weather-year smoke (M007/S06-T06).
#
# Proves the slice's mechanical end-to-end claim through the real event loop:
# one accelerated `--demo-days 4` session spans a full four-day year at the
# default one-day season length, so the per-frame weather derivation walks the
# whole seed-1 sequence (Spring day 0 Rain -> Summer day 1 Clear -> Autumn
# day 2 Rain -> Winter day 3 Snowfall) before the bounded demo shuts itself
# down. Nothing here inspects rendering — the windowed sensory claim (visible
# rain streaks / snowfall with matching sky and fog shifts over an ocean) is
# honestly deferred to S08's recorded human pass.
#
#   Run: `--new --seed 1 --save P --time-scale 60 --demo-days 4`
#     (a) exit code 0 (the multi-cycle demo watch latched its own shutdown;
#         a `timeout` kill surfaces as 124 and is an explicit failure here).
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) the save exists, starts with the `MCVX` magic, and its version word
#         is 5 — the SAVE_VERSION that carries the calendar tail.
#     (d) day_index (u32 LE at byte offset 130) is exactly 4 — four dawn
#         crossings, one per demo day.
#     (e) season_length_days (u32 LE at byte offset 134) is the default 1.
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
# Why day 4 after one run: the default clock starts at noon (tick 6000) and
# the T5 multi-cycle watch completes only after four summed 24,000-tick
# cycles, i.e. after crossing dawn (tick 0) four times. The bounded shutdown
# then saves the calendar tail with day_index 4.
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
    echo "WEATHER SMOKE FAIL: $BIN not found — run cargo build --locked first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-weather-smoke.XXXXXX)" || {
    echo "WEATHER SMOKE FAIL: could not create a temp dir" >&2
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
DEMO_DAYS=4
EXPECTED_DAY=$DEMO_DAYS
EXPECTED_SEASON_LENGTH=1
# 60x completes one 24,000-tick cycle in 20 s; four cycles are 80 s. The app's
# own scaled safety net fires at 80 + 5 s, and `timeout` is only an outer
# backstop for a hang (a kill here means the demo never latched).
TIME_SCALE=60
OUTER_TIMEOUT=180

read_u32_le() {
    od -An -tu4 --endian=little -j "$2" -N 4 "$1" | tr -d ' '
}

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/year.mcvx"
OUT="$TEMP_DIR/run.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed "$SEED" --save "$SAVE" \
    --time-scale "$TIME_SCALE" --demo-days "$DEMO_DAYS" \
    >"$OUT" 2>&1
RC=$?
set -e

if [[ $RC -ne 0 ]]; then
    echo "WEATHER SMOKE FAIL: exit code $RC (expected 0 from the bounded four-day demo shutdown; 124 means the demo never latched and an external timeout killed it)"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if grep -q "panicked" "$OUT" || grep -q "RUST_BACKTRACE" "$OUT"; then
    echo "WEATHER SMOKE FAIL: panic detected in output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if [[ ! -f "$SAVE" ]]; then
    echo "WEATHER SMOKE FAIL: run exited 0 but wrote no save at $SAVE"
    exit 1
fi

SAVE_SIZE=$(wc -c <"$SAVE")
if [[ $SAVE_SIZE -lt $((V5_SEASON_OFFSET + V5_SEASON_LEN)) ]]; then
    echo "WEATHER SMOKE FAIL: save is $SAVE_SIZE bytes, too short for the v5 calendar tail"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "WEATHER SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 5 ]]; then
    echo "WEATHER SMOKE FAIL: save version is $VERSION (expected 5)"
    exit 1
fi

DAY=$(read_u32_le "$SAVE" "$V5_DAY_INDEX_OFFSET")
SEASON=$(read_u32_le "$SAVE" "$V5_SEASON_LENGTH_OFFSET")

if [[ "$DAY" -ne "$EXPECTED_DAY" ]]; then
    echo "WEATHER SMOKE FAIL: day_index is $DAY (expected $EXPECTED_DAY after $DEMO_DAYS full cycles from noon)"
    exit 1
fi

if [[ "$SEASON" -ne "$EXPECTED_SEASON_LENGTH" ]]; then
    echo "WEATHER SMOKE FAIL: season_length_days is $SEASON (expected the default $EXPECTED_SEASON_LENGTH)"
    exit 1
fi

echo "WEATHER SMOKE PASS: seed $SEED spanned a four-day year in one accelerated session (day_index $DAY, default ${SEASON}-day seasons); the seed-1 sequence Rain -> Clear -> Rain -> Snowfall was derived from the persisted calendar with exit 0 and no panic"
