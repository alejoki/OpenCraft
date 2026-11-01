#!/usr/bin/env bash
# Real-binary ruin/farm/chest content-chain smoke (M008/S08-T04).
#
# Proves the scripted `--demo-content` seam through the production session
# seams with no menu automation and no test harness:
#
#   Run: `--new --seed 1 --demo-content --save P`
#     (a) exit code 0 — the demo's own latch. A `timeout` kill surfaces as
#         124 and is an explicit FAILURE here, never the expected outcome.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) the save exists, starts with the `MCVX` magic, and its version word
#         is 7.
#     (d) the save is longer than the 153-byte edit-free v7 baseline, proving
#         the demo's chunk edits were persisted.
#     (e) stdout contains the `content_demo:` digest line with the ruin loot,
#         chest-break, craft, farm, winter-pause, stage3, harvest, bread,
#         chest-place/store, and save/reload facts plus the chest and farm
#         coordinates.
#
# Why this smoke does NOT pin the fixed calendar/v6 offsets: the content demo
# edits chunks (removing the loot chest, tilling farmland, planting and
# breaking crops, placing a chest), and the variable-length chunk-edit section
# is written between the fixed header and the trailing records. Every edit
# shifts the following bytes, so a fixed offset is meaningless here. This
# script asserts magic/version and the digest's decoded reload facts
# (`reload_chests=1`, `reload_stage=1`) instead. The survival smoke is the one
# that pins the edit-free 153-byte layout (scripts/survival_smoke.sh).
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
        echo "CONTENT SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
BUILD_LOG="$(mktemp /tmp/opencraft-content-build.XXXXXX.log)" || {
    echo "CONTENT SMOKE FAIL: could not create a build log" >&2
    exit 1
}

cleanup_build_log() { rm -f "$BUILD_LOG"; }
trap cleanup_build_log EXIT

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "CONTENT SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "CONTENT SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-content-smoke.XXXXXX)" || {
    echo "CONTENT SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() {
    rm -rf "$TEMP_DIR"
    rm -f "$BUILD_LOG"
}
trap cleanup EXIT

# The edit-free v7 save ends exactly after the trailing u32 mob count (see
# persistence.rs V7_FIRST_MOB_OFFSET). Anything longer proves the demo's chunk
# edits are present.
EDIT_FREE_V7_LEN=153

# The demo's own wall-clock deadline is 240 s; the outer timeout is only a
# backstop for a hang, and a kill here is an explicit failure.
OUTER_TIMEOUT=300

read_u32_le() {
    od -An -tu4 --endian=little -j "$2" -N 4 "$1" | tr -d ' '
}

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/content.mcvx"
OUT="$TEMP_DIR/run.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed 1 --demo-content \
    --save "$SAVE" >"$OUT" 2>&1
RC=$?
set -e

if [[ $RC -ne 0 ]]; then
    echo "CONTENT SMOKE FAIL: exit code $RC (expected 0 from the content demo's own latch; 124 means an external timeout killed the demo before it finished)"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if grep -q "panicked" "$OUT" || grep -q "RUST_BACKTRACE" "$OUT"; then
    echo "CONTENT SMOKE FAIL: panic detected in output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if [[ ! -f "$SAVE" ]]; then
    echo "CONTENT SMOKE FAIL: exited 0 but wrote no save at $SAVE"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "CONTENT SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 7 ]]; then
    echo "CONTENT SMOKE FAIL: save version is $VERSION (expected 7)"
    exit 1
fi

SAVE_SIZE=$(wc -c <"$SAVE")
if [[ "$SAVE_SIZE" -le "$EDIT_FREE_V7_LEN" ]]; then
    echo "CONTENT SMOKE FAIL: save is $SAVE_SIZE bytes (expected more than the ${EDIT_FREE_V7_LEN}-byte edit-free baseline: the demo must persist chunk edits)"
    exit 1
fi

CONTENT_LINE=$(grep -m1 'content_demo:' "$OUT" || true)
if [[ -z "$CONTENT_LINE" ]]; then
    echo "CONTENT SMOKE FAIL: no content_demo: digest line in the output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

require_token() {
    local token="$1"
    if [[ "$CONTENT_LINE" != *"$token"* ]]; then
        echo "CONTENT SMOKE FAIL: digest is missing '$token' — observed: $CONTENT_LINE"
        exit 1
    fi
}

for token in \
    "ok=true" \
    "loot_taken=true" \
    "chest_broken=true" \
    "chest_item=1" \
    "tilled=true" \
    "planted=true" \
    "winter_paused=true" \
    "stage3=true" \
    "harvest_wheat=2" \
    "harvest_seeds=4" \
    "bread=1" \
    "chest_placed=true" \
    "stored_bread=true" \
    "saved=true" \
    "reload_chests=1" \
    "reload_stage=1" \
    "chest=" \
    "farm="
do
    require_token "$token"
done

echo "CONTENT SMOKE PASS [profile=$PROFILE]: seed 1 looted a ruin chest, broke it for 1 Chest, tilled/planted two crops through the Winter pause to stage 3, harvested 2 Wheat + 4 WheatSeeds, crafted Bread, placed and stored it in a chest, and reloaded a ${SAVE_SIZE}-byte v7 save with reload_chests=1 reload_stage=1"
echo "CONTENT SMOKE EVIDENCE: $CONTENT_LINE"
