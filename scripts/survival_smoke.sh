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
#         6, and its exact length is 155 bytes (an edit-free v6 save with
#         zero chests and exactly two occupied inventory slots).
#     (e) the save's chunk count is 0, proving the demo performed no chunk
#         edits.
#     (f) the v6 survival header at the pinned offsets decodes to mode 0
#         (Survival), health 20, hunger 20, air 300, chest count 0.
#
# Why the offsets are fixed here: the survival demo is edit-free by
# construction (it never mines, places, or edits a chunk), so the
# variable-length chunk section writes a zero chunk count. With an EMPTY
# inventory the layout would be the header (36 + 27) + time_of_day (4) +
# chunk count (4) + v4 inventory extension (59) = 130 bytes before the v5
# calendar tail (8 bytes), then the v6 survival header (mode u8 + health u16 +
# hunger u16 + air u16 + chest count u32 = 11 bytes), i.e. exactly 149 bytes.
# Those empty-inventory offsets are pinned by the persistence.rs test
# constants (V5_SEASON_OFFSET/V5_SEASON_LEN/V6_*), and the script copies them:
#   V5_SEASON_OFFSET      = 130
#   V5_SEASON_LEN         = 8
#   V6_MODE_OFFSET        = 138
#   V6_HEALTH_OFFSET      = 139
#   V6_HUNGER_OFFSET      = 141
#   V6_AIR_OFFSET         = 143
#   V6_CHEST_COUNT_OFFSET = 145
#   V6_FIRST_CHEST_OFFSET = 149
# The survival demo, however, must prove `inventory_kept=true`: it eats one
# of two Apples and keeps the Stick marker, so at save time it holds exactly
# two stacks (Apple x1 and Stick x1). Each occupied 4-byte item record
# replaces a 1-byte empty record, so the v4 extension grows by 3 + 3 = 6
# bytes, the v5 calendar tail moves to 136, the v6 header to 144, and the
# edit-free save is exactly 155 bytes. This script asserts that shifted layout
# (base constants + SURVIVAL_INVENTORY_EXPANSION) together with the exact
# size, so a header move or an inventory change fails loudly. A future header
# move must update the persistence.rs test constants first, then this script,
# then the expansion shift here.
#
# The content smoke deliberately does not use fixed offsets: its chunk edits
# shift the variable-length chunk section (see scripts/content_smoke.sh).
#
# The script builds the debug binary itself with `cargo build --locked`, and
# everything runs inside a mktemp working directory, so the crate root's real
# saves/ and settings.toml are never touched.

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

# Resolve the crate root and binary absolutely before changing directory (the
# build runs from the crate root).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"
BUILD_LOG="$(mktemp /tmp/opencraft-survival-build.XXXXXX.log)" || {
    echo "SURVIVAL SMOKE FAIL: could not create a build log" >&2
    exit 1
}

cleanup_build_log() { rm -f "$BUILD_LOG"; }
trap cleanup_build_log EXIT

if ! (cd "$CRATE_ROOT" && cargo build --locked) >"$BUILD_LOG" 2>&1; then
    echo "SURVIVAL SMOKE FAIL: cargo build --locked failed"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "SURVIVAL SMOKE FAIL: $BIN not found after the debug build"
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
# V6_* constants below are the empty-inventory offsets pinned by the
# persistence.rs unit tests; the survival demo keeps two marker stacks, so
# every later offset shifts by the 6-byte inventory expansion.
V5_SEASON_OFFSET=130
V5_SEASON_LEN=8
V6_MODE_OFFSET=138
V6_HEALTH_OFFSET=139
V6_HUNGER_OFFSET=141
V6_AIR_OFFSET=143
V6_CHEST_COUNT_OFFSET=145
V6_FIRST_CHEST_OFFSET=149

# Two occupied inventory slots (Apple x1 + Stick x1) each add 3 bytes over the
# empty record: 4-byte item record vs the 1-byte empty tag.
SURVIVAL_INVENTORY_EXPANSION=6
CHUNK_COUNT_OFFSET=67

SAVE_MODE_OFFSET=$((V6_MODE_OFFSET + SURVIVAL_INVENTORY_EXPANSION))
SAVE_HEALTH_OFFSET=$((V6_HEALTH_OFFSET + SURVIVAL_INVENTORY_EXPANSION))
SAVE_HUNGER_OFFSET=$((V6_HUNGER_OFFSET + SURVIVAL_INVENTORY_EXPANSION))
SAVE_AIR_OFFSET=$((V6_AIR_OFFSET + SURVIVAL_INVENTORY_EXPANSION))
SAVE_CHEST_COUNT_OFFSET=$((V6_CHEST_COUNT_OFFSET + SURVIVAL_INVENTORY_EXPANSION))
SAVE_FIRST_CHEST_OFFSET=$((V6_FIRST_CHEST_OFFSET + SURVIVAL_INVENTORY_EXPANSION))
SURVIVAL_SAVE_LEN=$SAVE_FIRST_CHEST_OFFSET

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
    echo "SURVIVAL SMOKE FAIL: save is $SAVE_SIZE bytes (expected exactly $SURVIVAL_SAVE_LEN: an edit-free v6 save with zero chests and two occupied inventory slots)"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "SURVIVAL SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 6 ]]; then
    echo "SURVIVAL SMOKE FAIL: save version is $VERSION (expected 6)"
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

echo "SURVIVAL SMOKE PASS: seed 12345 walked fall/eat/drown/starve/respawn with exit 0 and wrote an edit-free ${SURVIVAL_SAVE_LEN}-byte v6 save (mode Survival, vitals 20/20/300, 0 chests, 0 chunk edits, 2 kept inventory stacks)"
echo "SURVIVAL SMOKE EVIDENCE: $SURVIVAL_LINE"
