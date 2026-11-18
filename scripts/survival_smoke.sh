#!/usr/bin/env bash
# Real-binary survival-loop smoke (M008/S08-T04).
#
# Proves the scripted `--demo-survival` seam through the production session
# seams with no menu automation and no test harness:
#
#   Run: `--new --seed 12345 --survival --demo-survival --save P`
#     (a) exit code 0 — the demo's own latch. A `timeout` kill surfaces as
#         124 and is an explicit FAILURE here, never the expected outcome.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) stdout contains the `survival_demo:` digest line with every proven
#         survival fact: fall damage 4, eat restore 4, drowning with air 0,
#         starvation death, respawn with inventory kept, Survival mode, and
#         the loaded v6 vitals at 20/20/300.
#     (d) the save is a valid sectioned v10 archive: `MCVX` magic, version
#         10 at offset 4, flags 0 at offset 8, a 16-byte header, and a
#         validated contiguous section directory (scripts/save_witness.sh).
#     (e) section 1 holds exactly the two kind-tagged names `2:apple` and
#         `2:stick`, proving the two kept inventory stacks were persisted by
#         name through the v10 content table.
#     (f) section 2 decodes to mode 0 (Survival), health 20, hunger 20, air
#         300, and a fresh day-0 calendar with one-day seasons; section 4
#         chests, section 5 mobs, section 8 crops, and section 9 chunk index
#         each hold a zero count, proving the survival demo performed no
#         chest, mob, crop, or chunk edits. Section 7 carries config schema 1
#         and the default effective-value digest.
#
# Why the old v9 flat offsets are gone: v10 replaces the single flat payload
# with a 16-byte header plus a 21-byte directory and section bodies. The
# survival demo is edit-free by construction (it never mines, places, or
# edits a chunk), so the smokes assert the fixed sections by id instead of
# pinning offsets that move whenever a section grows. The historical v9
# edit-free layout was 179 bytes (201 bytes with the two kept stacks); those
# offsets live only in docs/SAVE-NOTES.md and the frozen v9 witnesses.
#
# The script builds the debug binary itself with `cargo build --locked` and
# runs target/debug/opencraft. `SMOKE_PROFILE=release` is the opt-in
# real-binary release run: it builds with `cargo build --locked --release` and
# runs target/release/opencraft instead. Debug stays the default, so every
# existing gate command keeps its behavior. Everything runs inside a mktemp
# working directory, so the crate root's real saves/ and settings.toml are
# never touched.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch, so a typo can never silently
# exercise the wrong tree.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "SURVIVAL SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
        exit 1
        ;;
esac

if [[ "$PROFILE" == "release" ]]; then
    BUILD_ARGS=(build --locked --release)
else
    BUILD_ARGS=(build --locked)
fi

# Resolve the crate root and binary absolutely before changing directory (the
# build runs from the crate root), then source the shared v10 archive witness
# helper and attach to a live display (preferring a wayland-* socket).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/save_witness.sh
source "$SCRIPT_DIR/save_witness.sh"
smoke_detect_display
BIN="$CRATE_ROOT/target/$PROFILE/opencraft"
BUILD_LOG="$(mktemp /tmp/opencraft-survival-build.XXXXXX.log)" || {
    echo "SURVIVAL SMOKE FAIL: could not create a build log" >&2
    exit 1
}

cleanup_build_log() { rm -f "$BUILD_LOG"; }
trap cleanup_build_log EXIT

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "SURVIVAL SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "SURVIVAL SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-survival-smoke.XXXXXX)" || {
    echo "SURVIVAL SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() {
    rm -rf "$TEMP_DIR"
    rm -f "$BUILD_LOG"
}
trap cleanup EXIT

# Section 2 world/player layout (scripts/save_witness.sh documents the full
# section list): seed u32, position x/y/z f32, yaw f32, pitch f32, fly u8,
# selected_slot u8, render_distance u16, time_of_day u32, day_index u32,
# season_length_days u32, mode u8, health u16, hunger u16, air u16.
WORLD_DAY_OFFSET=32
WORLD_SEASON_OFFSET=36
WORLD_MODE_OFFSET=40
WORLD_HEALTH_OFFSET=41
WORLD_HUNGER_OFFSET=43
WORLD_AIR_OFFSET=45
# Section 7 config: u16 schema + u64 effective-value digest. The smokes run
# with built-in defaults (no config/ directory in the temp cwd), so the
# digest is the frozen default `0xeaeac8ddce568dec`.
CONFIG_SCHEMA_OFFSET=0
CONFIG_DIGEST_OFFSET=2
DEFAULT_CONFIG_DIGEST=16927563004400537068

# The demo's own wall-clock deadline is 120 s; the outer timeout is only a
# backstop for a hang, and a kill here is an explicit failure.
OUTER_TIMEOUT=180

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/survival.mcvx"
OUT="$TEMP_DIR/run.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed 12345 --survival --demo-survival \
    --save "$SAVE" >"$OUT" 2>&1
RC=$?
set -e

if [[ $RC -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: exit code $RC (expected 0 from the survival demo's own latch; 124 means an external timeout killed the demo before it finished)"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if grep -q "panicked" "$OUT" || grep -q "RUST_BACKTRACE" "$OUT"; then
    echo "SURVIVAL SMOKE FAIL: panic detected in output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if [[ ! -f "$SAVE" ]]; then
    echo "SURVIVAL SMOKE FAIL: exited 0 but wrote no save at $SAVE"
    exit 1
fi

SURVIVAL_LINE=$(grep -m1 'survival_demo:' "$OUT" || true)
if [[ -z "$SURVIVAL_LINE" ]]; then
    echo "SURVIVAL SMOKE FAIL: no survival_demo: digest line in the output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

require_token() {
    local token="$1"
    if [[ "$SURVIVAL_LINE" != *"$token"* ]]; then
        echo "SURVIVAL SMOKE FAIL: digest is missing '$token' — observed: $SURVIVAL_LINE"
        exit 1
    fi
}

for token in \
    "ok=true" \
    "fall=true" \
    "fall_damage=4" \
    "eat=true" \
    "eat_restored=4" \
    "drown=true" \
    "air_empty=true" \
    "starve=true" \
    "death=true" \
    "respawn=true" \
    "inventory_kept=true" \
    "mode=survival" \
    "saved=true" \
    "health=20" \
    "hunger=20" \
    "air=300"
do
    require_token "$token"
done

assert_mcvx_v10 "$SAVE"

CONTENT_NAMES=()
mapfile -t CONTENT_NAMES < <(mcvx_content_table_names "$SAVE")
if [[ ${#CONTENT_NAMES[@]} -ne 2 || "${CONTENT_NAMES[0]}" != "2:apple" || "${CONTENT_NAMES[1]}" != "2:stick" ]]; then
    echo "SURVIVAL SMOKE FAIL: v10 content table is [${CONTENT_NAMES[*]}] (expected exactly 2:apple and 2:stick)"
    exit 1
fi

DAY_INDEX=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_DAY_OFFSET")
SEASON_LENGTH=$(mcvx_section_u32 "$SAVE" 2 "$WORLD_SEASON_OFFSET")
MODE=$(mcvx_section_u8 "$SAVE" 2 "$WORLD_MODE_OFFSET")
HEALTH=$(mcvx_section_u16 "$SAVE" 2 "$WORLD_HEALTH_OFFSET")
HUNGER=$(mcvx_section_u16 "$SAVE" 2 "$WORLD_HUNGER_OFFSET")
AIR=$(mcvx_section_u16 "$SAVE" 2 "$WORLD_AIR_OFFSET")
if [[ "$MODE" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 2 mode byte is $MODE (expected 0 = Survival)"
    exit 1
fi

if [[ "$DAY_INDEX" -ne 0 || "$SEASON_LENGTH" -ne 1 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 2 calendar is day $DAY_INDEX with $SEASON_LENGTH-day seasons (expected day 0 with 1-day seasons for a fresh world)"
    exit 1
fi

if [[ "$HEALTH" -ne 20 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 2 health is $HEALTH (expected 20)"
    exit 1
fi

if [[ "$HUNGER" -ne 20 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 2 hunger is $HUNGER (expected 20)"
    exit 1
fi

if [[ "$AIR" -ne 300 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 2 air is $AIR (expected 300)"
    exit 1
fi

CHEST_COUNT=$(mcvx_section_u32 "$SAVE" 4 0)
if [[ "$CHEST_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 4 chest count is $CHEST_COUNT (expected 0)"
    exit 1
fi

MOB_COUNT=$(mcvx_section_u32 "$SAVE" 5 0)
if [[ "$MOB_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 5 mob count is $MOB_COUNT (expected 0)"
    exit 1
fi

CROP_COUNT=$(mcvx_section_u32 "$SAVE" 8 0)
if [[ "$CROP_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 8 crop count is $CROP_COUNT (expected 0: the survival demo plants nothing)"
    exit 1
fi

CHUNK_COUNT=$(mcvx_section_u32 "$SAVE" 9 0)
if [[ "$CHUNK_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 9 chunk count is $CHUNK_COUNT (expected 0: the survival demo must be edit-free)"
    exit 1
fi

CONFIG_SCHEMA=$(mcvx_section_u16 "$SAVE" 7 "$CONFIG_SCHEMA_OFFSET")
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "SURVIVAL SMOKE FAIL: section 7 config schema is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

CONFIG_DIGEST=$(mcvx_section_u64 "$SAVE" 7 "$CONFIG_DIGEST_OFFSET")
if [[ "$CONFIG_DIGEST" != "$DEFAULT_CONFIG_DIGEST" ]]; then
    echo "SURVIVAL SMOKE FAIL: section 7 config digest is $CONFIG_DIGEST (expected $DEFAULT_CONFIG_DIGEST = default config)"
    exit 1
fi

SAVE_SIZE=$(wc -c <"$SAVE")
echo "SURVIVAL SMOKE PASS [profile=$PROFILE]: seed 12345 walked fall/eat/drown/starve/respawn with exit 0 and wrote a sectioned ${SAVE_SIZE}-byte v10 save (mode Survival, vitals 20/20/300, day 0 with 1-day seasons, 0 chests, 0 mobs, 0 crops, 0 chunk edits, 2 kept inventory stacks as 2:apple + 2:stick content names, config schema 1 with the default digest)"
echo "SURVIVAL SMOKE EVIDENCE: $SURVIVAL_LINE"
