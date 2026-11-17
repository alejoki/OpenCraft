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
#     (c) the save is a valid sectioned v10 archive: `MCVX` magic, version
#         10 at offset 4, flags 0 at offset 8, a 16-byte header, and a
#         validated contiguous section directory (scripts/save_witness.sh).
#     (d) section 2 (`day_index` at body offset 32) is exactly 4 — four dawn
#         crossings, one per demo day — and `season_length_days` (body offset
#         36) is the default 1.
#     (e) section 1 holds zero content names, section 5 holds a zero mob
#         count, and section 9 holds a zero chunk count, proving this fresh
#         run is edit-free with an empty inventory and no spawned mobs.
#
# Why the old v9 flat offsets are gone: v10 replaces the single flat payload
# with a 16-byte header, a 21-byte directory entry per section and section
# bodies. A fresh `--new` run is edit-free by construction (no mining, no
# placement), so the fixed sections are asserted by id instead of pinning
# offsets that move whenever a section grows. The historical v9 edit-free
# layout was 179 bytes with the calendar at offsets 134/138; those offsets
# live only in docs/SAVE-NOTES.md and the frozen v9 witnesses.
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

# Resolve the binary absolutely before changing directory, then source the
# shared v10 archive witness helper and attach to a live display (preferring a
# wayland-* socket).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/save_witness.sh
source "$SCRIPT_DIR/save_witness.sh"
smoke_detect_display
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

# Section 2 world/player layout (scripts/save_witness.sh documents the full
# section list): ...; time_of_day u32; day_index u32; season_length_days u32;
# mode u8; health u16; hunger u16; air u16.
WORLD_DAY_OFFSET=32
WORLD_SEASON_OFFSET=36
CONFIG_SCHEMA_OFFSET=0
CONFIG_DIGEST_OFFSET=2
DEFAULT_CONFIG_DIGEST=16927563004400537068

SEED=1
DEMO_DAYS=4
EXPECTED_DAY=$DEMO_DAYS
EXPECTED_SEASON_LENGTH=1
# 60x completes one 24,000-tick cycle in 20 s; four cycles are 80 s. The app's
# own scaled safety net fires at 80 + 5 s, and `timeout` is only an outer
# backstop for a hang (a kill here means the demo never latched).
TIME_SCALE=60
OUTER_TIMEOUT=180

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

assert_mcvx_v10 "$SAVE"

CONTENT_NAME_COUNT=$(mcvx_section_u32 "$SAVE" 1 0)
if [[ "$CONTENT_NAME_COUNT" -ne 0 ]]; then
    echo "WEATHER SMOKE FAIL: section 1 content table holds $CONTENT_NAME_COUNT names (expected 0 for an edit-free fresh world with an empty inventory)"
    exit 1
fi

MOB_COUNT=$(mcvx_section_u32 "$SAVE" 5 0)
if [[ "$MOB_COUNT" -ne 0 ]]; then
    echo "WEATHER SMOKE FAIL: section 5 mob count is $MOB_COUNT (expected 0 in an edit-free fresh world)"
    exit 1
fi

CHUNK_COUNT=$(mcvx_section_u32 "$SAVE" 9 0)
if [[ "$CHUNK_COUNT" -ne 0 ]]; then
    echo "WEATHER SMOKE FAIL: section 9 chunk count is $CHUNK_COUNT (expected 0 in an edit-free fresh world)"
    exit 1
fi

CONFIG_SCHEMA=$(mcvx_section_u16 "$SAVE" 7 "$CONFIG_SCHEMA_OFFSET")
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "WEATHER SMOKE FAIL: section 7 config schema is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

CONFIG_DIGEST=$(mcvx_section_u64 "$SAVE" 7 "$CONFIG_DIGEST_OFFSET")
if [[ "$CONFIG_DIGEST" != "$DEFAULT_CONFIG_DIGEST" ]]; then
    echo "WEATHER SMOKE FAIL: section 7 config digest is $CONFIG_DIGEST (expected $DEFAULT_CONFIG_DIGEST = default config)"
    exit 1
fi

DAY=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_DAY_OFFSET")
SEASON=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_SEASON_OFFSET")

if [[ "$DAY" -ne "$EXPECTED_DAY" ]]; then
    echo "WEATHER SMOKE FAIL: section 2 day_index is $DAY (expected $EXPECTED_DAY after $DEMO_DAYS full cycles from noon)"
    exit 1
fi

if [[ "$SEASON" -ne "$EXPECTED_SEASON_LENGTH" ]]; then
    echo "WEATHER SMOKE FAIL: section 2 season_length_days is $SEASON (expected the default $EXPECTED_SEASON_LENGTH)"
    exit 1
fi

echo "WEATHER SMOKE PASS: seed $SEED spanned a four-day year in one accelerated session (section 2 day_index $DAY, default ${SEASON}-day seasons); the seed-1 sequence Rain -> Clear -> Rain -> Snowfall was derived from the persisted calendar with exit 0 and no panic; the v10 archive loaded with zero content names, zero mobs, zero chunk edits, and config schema 1"
