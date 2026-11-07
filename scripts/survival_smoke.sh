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
#     (d) the save exists, starts with the `MCVX` magic, its version word is
#         9, and its exact length is 201 bytes (an edit-free v9 save with
#         zero chests, exactly two occupied inventory slots, a two-name
#         content table, and a zero mob count).
#     (e) the save's chunk count is 0, proving the demo performed no chunk
#         edits.
#     (f) the v6 survival header at the pinned offsets decodes to mode 0
#         (Survival), health 20, hunger 20, air 300, chest count 0, and the
#         v7 mob count at offset 175 is 0; the v8 pack count at 179 is 0, the
#         v9 config schema at 191 is 1, and the config digest 193..201 ends
#         at EOF.
#
# Why the offsets are fixed here: the survival demo is edit-free by
# construction (it never mines, places, or edits a chunk), so the
# variable-length chunk section writes a zero chunk count. With an EMPTY
# inventory the v9 layout would be the header (36) + content table (4) +
# hotbar (27) + time_of_day (4) + chunk count (4) + v4 inventory extension
# (59) = 134 bytes before the v5 calendar tail (8 bytes), then the v6
# survival header (11 bytes), the v7 mob count (4 bytes), the v8-format
# pack tail (u32 pack count + u64 digest = 12 bytes), and the 10-byte v9
# config tail (u16 schema + u64 digest), i.e. exactly 179 bytes.
# The survival demo, however, must prove `inventory_kept=true`: it eats one
# of two Apples and keeps the Stick marker, so at save time it holds exactly
# two stacks (Apple x1 and Stick x1). The save-local content table then holds
# two names (`apple` and `stick`): 4-byte count plus (2 + 5) + (2 + 5) = 18
# bytes. Each occupied v8 item record is 5 bytes (tag + u16 name index + u16
# payload) instead of the legacy 4, so the v4 extension grows by 2. The v5
# calendar tail therefore moves to 130 + 18 + 8 = 156, the v6 header to 164,
# the v7 mob count to 175, the pack tail to 191, and the v9 config tail
# ends at 201.
# This script asserts that shifted layout together with the exact size, so a
# header move or an inventory change fails loudly. A future header move must
# update the persistence.rs test constants first, then this script.
#
# The content smoke deliberately does not use fixed offsets: its chunk edits
# shift the variable-length chunk section (see scripts/content_smoke.sh).
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

# Resolve the crate root and binary absolutely before changing directory (the
# build runs from the crate root).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
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

# Survival wire layout (see the header comment and persistence.rs). The
# LEGACY_V5_SEASON_OFFSET is the empty-inventory offset pinned by the
# persistence.rs unit tests. v8 inserts an 18-byte content table (`apple` and
# `stick`) after render_distance, and the two occupied items each grow by one
# byte over the legacy record, so every later offset shifts by 26. v9 appends
# the 10-byte config tail after the pack-identity tail.
LEGACY_V5_SEASON_OFFSET=130
V5_SEASON_LEN=8
V8_CONTENT_TABLE_OFFSET=36
# Measured: count u32 + "apple" (2+5) + "stick" (2+5).
V8_CONTENT_TABLE_LEN=18
# Two occupied v8 item records at 5 bytes each instead of the legacy 4.
SURVIVAL_INVENTORY_EXPANSION=8
V5_SEASON_OFFSET=$((LEGACY_V5_SEASON_OFFSET + V8_CONTENT_TABLE_LEN + SURVIVAL_INVENTORY_EXPANSION))
V6_MODE_OFFSET=$((V5_SEASON_OFFSET + V5_SEASON_LEN))
V6_HEALTH_OFFSET=$((V6_MODE_OFFSET + 1))
V6_HUNGER_OFFSET=$((V6_HEALTH_OFFSET + 2))
V6_AIR_OFFSET=$((V6_HUNGER_OFFSET + 2))
V6_CHEST_COUNT_OFFSET=$((V6_AIR_OFFSET + 2))
V6_FIRST_CHEST_OFFSET=$((V6_CHEST_COUNT_OFFSET + 4))

# The v7 mob tail appends a u32 count after the last chest record. The
# survival demo never spawns a mob, so the count is 0.
V7_MOB_COUNT_LEN=4
# v9 tail: the v8 u32 pack count plus u64 FNV-1a digest (12 bytes), then the
# v9 u16 config schema plus u64 effective-value digest (10 bytes).
V9_TAIL_LEN=22
# The content table precedes the hotbar, so it shifts the chunk count too.
CHUNK_COUNT_OFFSET=$((67 + V8_CONTENT_TABLE_LEN))

SAVE_MODE_OFFSET=$V6_MODE_OFFSET
SAVE_HEALTH_OFFSET=$V6_HEALTH_OFFSET
SAVE_HUNGER_OFFSET=$V6_HUNGER_OFFSET
SAVE_AIR_OFFSET=$V6_AIR_OFFSET
SAVE_CHEST_COUNT_OFFSET=$V6_CHEST_COUNT_OFFSET
SAVE_FIRST_CHEST_OFFSET=$V6_FIRST_CHEST_OFFSET
SAVE_MOB_COUNT_OFFSET=$SAVE_FIRST_CHEST_OFFSET
SAVE_PACK_COUNT_OFFSET=$((SAVE_MOB_COUNT_OFFSET + V7_MOB_COUNT_LEN))
# The config schema follows the u32 pack count and the u64 registry digest.
SAVE_CONFIG_SCHEMA_OFFSET=$((SAVE_PACK_COUNT_OFFSET + 4 + 8))
SURVIVAL_SAVE_LEN=$((SAVE_PACK_COUNT_OFFSET + V9_TAIL_LEN))

# The demo's own wall-clock deadline is 120 s; the outer timeout is only a
# backstop for a hang, and a kill here is an explicit failure.
OUTER_TIMEOUT=180

read_u8() {
    od -An -tu1 -j "$2" -N 1 "$1" | tr -d ' '
}

read_u16_le() {
    od -An -tu2 --endian=little -j "$2" -N 2 "$1" | tr -d ' '
}

read_u32_le() {
    od -An -tu4 --endian=little -j "$2" -N 4 "$1" | tr -d ' '
}

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

SAVE_SIZE=$(wc -c <"$SAVE")
if [[ "$SAVE_SIZE" -ne "$SURVIVAL_SAVE_LEN" ]]; then
    echo "SURVIVAL SMOKE FAIL: save is $SAVE_SIZE bytes (expected exactly $SURVIVAL_SAVE_LEN: an edit-free v9 save with zero chests, zero mobs, a two-name content table, and two occupied inventory slots)"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "SURVIVAL SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 9 ]]; then
    echo "SURVIVAL SMOKE FAIL: save version is $VERSION (expected 9)"
    exit 1
fi

CONTENT_NAME_COUNT=$(read_u32_le "$SAVE" "$V8_CONTENT_TABLE_OFFSET")
if [[ "$CONTENT_NAME_COUNT" -ne 2 ]]; then
    echo "SURVIVAL SMOKE FAIL: v8 content table holds $CONTENT_NAME_COUNT names (expected 2: apple and stick)"
    exit 1
fi

CHUNK_COUNT=$(read_u32_le "$SAVE" "$CHUNK_COUNT_OFFSET")
if [[ "$CHUNK_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: chunk count is $CHUNK_COUNT (expected 0: the survival demo must be edit-free)"
    exit 1
fi

MODE=$(read_u8 "$SAVE" "$SAVE_MODE_OFFSET")
if [[ "$MODE" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: v6 mode byte is $MODE (expected 0 = Survival)"
    exit 1
fi

HEALTH=$(read_u16_le "$SAVE" "$SAVE_HEALTH_OFFSET")
if [[ "$HEALTH" -ne 20 ]]; then
    echo "SURVIVAL SMOKE FAIL: v6 health is $HEALTH (expected 20)"
    exit 1
fi

HUNGER=$(read_u16_le "$SAVE" "$SAVE_HUNGER_OFFSET")
if [[ "$HUNGER" -ne 20 ]]; then
    echo "SURVIVAL SMOKE FAIL: v6 hunger is $HUNGER (expected 20)"
    exit 1
fi

AIR=$(read_u16_le "$SAVE" "$SAVE_AIR_OFFSET")
if [[ "$AIR" -ne 300 ]]; then
    echo "SURVIVAL SMOKE FAIL: v6 air is $AIR (expected 300)"
    exit 1
fi

CHEST_COUNT=$(read_u32_le "$SAVE" "$SAVE_CHEST_COUNT_OFFSET")
if [[ "$CHEST_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: v6 chest count is $CHEST_COUNT (expected 0)"
    exit 1
fi

MOB_COUNT=$(read_u32_le "$SAVE" "$SAVE_MOB_COUNT_OFFSET")
if [[ "$MOB_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: v7 mob count is $MOB_COUNT (expected 0)"
    exit 1
fi

PACK_COUNT=$(read_u32_le "$SAVE" "$SAVE_PACK_COUNT_OFFSET")
if [[ "$PACK_COUNT" -ne 0 ]]; then
    echo "SURVIVAL SMOKE FAIL: v8 pack count is $PACK_COUNT (expected 0 for a built-in-only stack)"
    exit 1
fi

CONFIG_SCHEMA=$(read_u16_le "$SAVE" "$SAVE_CONFIG_SCHEMA_OFFSET")
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "SURVIVAL SMOKE FAIL: v9 config schema is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

echo "SURVIVAL SMOKE PASS [profile=$PROFILE]: seed 12345 walked fall/eat/drown/starve/respawn with exit 0 and wrote an edit-free ${SURVIVAL_SAVE_LEN}-byte v9 save (mode Survival, vitals 20/20/300, 0 chests, 0 mobs, 0 chunk edits, 2 kept inventory stacks, 2 content names, 0 packs, config schema 1)"
echo "SURVIVAL SMOKE EVIDENCE: $SURVIVAL_LINE"
