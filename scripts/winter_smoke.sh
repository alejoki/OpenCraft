#!/usr/bin/env bash
# Real-binary mid-winter save round-trip smoke (M007/S08-T03).
#
# Proves the v10 save's section 2 calendar survives a cold process restart
# through the real event loop, and that the season derived from it really is Winter:
#
#   Run A: `--new --seed 1 --season-length 1 --save P --time-scale 60
#           --demo-days 3`
#     Seed 1 starts in Spring with one-day seasons, so after three bounded
#     accelerated day cycles the persisted calendar is day_index 3 = Winter.
#     (a) exit code 0 (the multi-cycle demo watch latched its own shutdown;
#         a `timeout` kill surfaces as 124 and is an explicit failure here).
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) the save is a valid sectioned v10 archive: `MCVX` magic, version
#         10 at offset 4, flags 0 at offset 8, a 16-byte header, and a
#         validated contiguous section directory (scripts/save_witness.sh).
#     (d) section 2 `day_index` (body offset 32) is exactly 3 — the mid-winter
#         save — and `season_length_days` (body offset 36) is the chosen 1.
#
#   Run B: `--save P --time-scale 60 --demo-days 1`
#     (a) exit code 0, no panic. The same v10 archive validates.
#     (b) section 2 `day_index` is exactly 4 — the reloaded day-3 calendar
#         resumed and advanced by exactly one more cycle.
#     (c) the four `season_length_days` bytes inside section 2 are
#         byte-identical to run A (`cmp`, not just the decoded value): a load
#         restores the persisted calendar and no launch flag may change it.
#
# Why the old v9 flat offsets are gone: v10 replaces the single flat payload
# with a 16-byte header, a 21-byte directory entry per section and section
# bodies. A fresh `--new` run is edit-free by construction (no mining, no
# placement), so the fixed sections are asserted by id instead of pinning
# offsets that move whenever a section grows. The historical v9 calendar
# offsets were 134/138 in a 179-byte edit-free save; those offsets live only
# in docs/SAVE-NOTES.md and the frozen v9 witnesses.
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
    echo "WINTER SMOKE FAIL: $BIN not found — run cargo build --locked first" >&2
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-winter-smoke.XXXXXX)" || {
    echo "WINTER SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

# Section 2 world/player layout (scripts/save_witness.sh documents the full
# section list). `day_index` sits at body offset 32 and
# `season_length_days` at body offset 36.
WORLD_DAY_OFFSET=32
WORLD_SEASON_OFFSET=36
CONFIG_SCHEMA_OFFSET=0
CONFIG_DIGEST_OFFSET=2
DEFAULT_CONFIG_DIGEST=16927563004400537068

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

assert_mcvx_v10 "$SAVE"

CONTENT_NAME_COUNT=$(mcvx_section_u32 "$SAVE" 1 0)
if [[ "$CONTENT_NAME_COUNT" -ne 0 ]]; then
    echo "WINTER SMOKE FAIL: section 1 content table holds $CONTENT_NAME_COUNT names (expected 0 for an edit-free fresh world with an empty inventory)"
    exit 1
fi

MOB_COUNT=$(mcvx_section_u32 "$SAVE" 5 0)
if [[ "$MOB_COUNT" -ne 0 ]]; then
    echo "WINTER SMOKE FAIL: section 5 mob count is $MOB_COUNT (expected 0 in an edit-free fresh world)"
    exit 1
fi

CHUNK_COUNT=$(mcvx_section_u32 "$SAVE" 9 0)
if [[ "$CHUNK_COUNT" -ne 0 ]]; then
    echo "WINTER SMOKE FAIL: section 9 chunk count is $CHUNK_COUNT (expected 0 in an edit-free fresh world)"
    exit 1
fi

CONFIG_SCHEMA=$(mcvx_section_u16 "$SAVE" 7 "$CONFIG_SCHEMA_OFFSET")
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "WINTER SMOKE FAIL: section 7 config schema is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

CONFIG_DIGEST=$(mcvx_section_u64 "$SAVE" 7 "$CONFIG_DIGEST_OFFSET")
if [[ "$CONFIG_DIGEST" != "$DEFAULT_CONFIG_DIGEST" ]]; then
    echo "WINTER SMOKE FAIL: section 7 config digest is $CONFIG_DIGEST (expected $DEFAULT_CONFIG_DIGEST = default config)"
    exit 1
fi

DAY_A=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_DAY_OFFSET")
SEASON_A=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_SEASON_OFFSET")

if [[ "$DAY_A" -ne "$EXPECTED_DAY_A" ]]; then
    echo "WINTER SMOKE FAIL: run A section 2 day_index is $DAY_A (expected $EXPECTED_DAY_A after $DEMO_DAYS full cycles from noon — the mid-winter save)"
    exit 1
fi

if [[ "$SEASON_A" -ne "$SEASON_LENGTH" ]]; then
    echo "WINTER SMOKE FAIL: run A section 2 season_length_days is $SEASON_A (expected the chosen $SEASON_LENGTH)"
    exit 1
fi

# Keep the raw season-length field bytes for the byte-identical comparison in
# run B. The field sits at relative offset 36 inside section 2's body.
WORLD_BYTES_A="$TEMP_DIR/world.run_a.bin"
mcvx_section_to_file "$SAVE" 2 "$WORLD_BYTES_A"
SEASON_BYTES_A="$TEMP_DIR/season_length.run_a.bin"
dd if="$WORLD_BYTES_A" of="$SEASON_BYTES_A" bs=1 skip="$WORLD_SEASON_OFFSET" \
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

assert_mcvx_v10 "$SAVE"

DAY_B=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_DAY_OFFSET")
SEASON_B=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_SEASON_OFFSET")

if [[ "$DAY_B" -ne "$EXPECTED_DAY_B" ]]; then
    echo "WINTER SMOKE FAIL: run B section 2 day_index is $DAY_B (expected $EXPECTED_DAY_B: the reloaded day-$DAY_A calendar advanced by exactly one cycle)"
    exit 1
fi

WORLD_BYTES_B="$TEMP_DIR/world.run_b.bin"
mcvx_section_to_file "$SAVE" 2 "$WORLD_BYTES_B"
SEASON_BYTES_B="$TEMP_DIR/season_length.run_b.bin"
dd if="$WORLD_BYTES_B" of="$SEASON_BYTES_B" bs=1 skip="$WORLD_SEASON_OFFSET" \
    count=4 status=none

if ! cmp -s "$SEASON_BYTES_A" "$SEASON_BYTES_B"; then
    echo "WINTER SMOKE FAIL: run B season_length_days bytes differ from run A (decoded $SEASON_A -> $SEASON_B)"
    exit 1
fi

if [[ "$SEASON_B" -ne "$SEASON_LENGTH" ]]; then
    echo "WINTER SMOKE FAIL: run B season_length_days is $SEASON_B (expected $SEASON_LENGTH)"
    exit 1
fi

echo "WINTER SMOKE PASS: seed $SEED created a Winter world (day $DAY_A, ${SEASON_A}-day seasons) via the real binary, relaunched it, and resumed the calendar to day $DAY_B with the season-length bytes byte-identical across both saves"
