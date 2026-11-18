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
#     (c) the save is a valid sectioned v10 archive: `MCVX` magic, version
#         10 at offset 4, flags 0 at offset 8, a 16-byte header, and a
#         validated contiguous section directory (scripts/save_witness.sh).
#     (d) section 9 holds a non-zero sorted chunk index and every indexed
#         chunk has its 16+i palette section, proving the demo's chunk edits
#         (chest removal, farmland, crop stages, placed chest) were persisted.
#     (e) section 4 holds the one placed chest, section 8 holds the one
#         immature crop edit, and section 5 holds zero mobs; section 7 carries
#         config schema 1 and the default digest; section 1's kind-tagged table
#         names the chest/farmland/crop/hoe content the demo saved.
#     (f) stdout contains the `content_demo:` digest line with the ruin loot,
#         chest-break, craft, farm, winter-pause, stage3, harvest, bread,
#         chest-place/store, and save/reload facts plus the chest and farm
#         coordinates.
#
# Why this smoke never pins flat offsets: the content demo edits chunks
# (removing the loot chest, tilling farmland, planting and breaking crops,
# placing a chest), so the v10 section sizes move with the world. The smoke
# asserts magic/version/structure and the section bodies it actually
# witnessed (chunk index + palette sections, chests, crops, content names)
# instead of old v9 flat offsets. The survival smoke pins the edit-free
# section counts (scripts/survival_smoke.sh).
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

# Resolve the crate root and binary absolutely before changing directory (the
# build runs from the crate root), then source the shared v10 archive witness
# helper and attach to a live display (preferring a wayland-* socket).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/save_witness.sh
source "$SCRIPT_DIR/save_witness.sh"
smoke_detect_display
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

# Section 7 config: u16 schema + u64 effective-value digest. These runs use
# built-in defaults (no config/ directory in the temp cwd), so the digest is
# the frozen default `0xeaeac8ddce568dec`.
DEFAULT_CONFIG_DIGEST=16927563004400537068

# The demo's own wall-clock deadline is 240 s; the outer timeout is only a
# backstop for a hang, and a kill here is an explicit failure.
OUTER_TIMEOUT=300

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

assert_mcvx_v10 "$SAVE"

CONTENT_NAMES_FILE="$TEMP_DIR/content-names.txt"
mcvx_content_table_names "$SAVE" >"$CONTENT_NAMES_FILE"
for expected_name in \
    "1:chest" \
    "1:farmland" \
    "1:wheat_stage_1" \
    "2:bread" \
    "3:wood_hoe"
do
    if ! grep -F -x -q -e "$expected_name" "$CONTENT_NAMES_FILE"; then
        echo "CONTENT SMOKE FAIL: section 1 content table is missing '$expected_name' — observed: $(tr '\n' ' ' <"$CONTENT_NAMES_FILE")"
        exit 1
    fi
done

CHEST_COUNT=$(mcvx_section_u32 "$SAVE" 4 0)
if [[ "$CHEST_COUNT" -ne 1 ]]; then
    echo "CONTENT SMOKE FAIL: section 4 chest count is $CHEST_COUNT (expected 1: the placed chest holding the bread)"
    exit 1
fi

MOB_COUNT=$(mcvx_section_u32 "$SAVE" 5 0)
if [[ "$MOB_COUNT" -ne 0 ]]; then
    echo "CONTENT SMOKE FAIL: section 5 mob count is $MOB_COUNT (expected 0 for the content demo)"
    exit 1
fi

CONFIG_SCHEMA=$(mcvx_section_u16 "$SAVE" 7 0)
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "CONTENT SMOKE FAIL: section 7 config schema is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

CONFIG_DIGEST=$(mcvx_section_u64 "$SAVE" 7 2)
if [[ "$CONFIG_DIGEST" != "$DEFAULT_CONFIG_DIGEST" ]]; then
    echo "CONTENT SMOKE FAIL: section 7 config digest is $CONFIG_DIGEST (expected $DEFAULT_CONFIG_DIGEST = default config)"
    exit 1
fi

CROP_COUNT=$(mcvx_section_u32 "$SAVE" 8 0)
if [[ "$CROP_COUNT" -ne 1 ]]; then
    echo "CONTENT SMOKE FAIL: section 8 crop count is $CROP_COUNT (expected 1 immature wheat edit left after the harvest)"
    exit 1
fi

CHUNK_COUNT=$(mcvx_section_u32 "$SAVE" 9 0)
if [[ "$CHUNK_COUNT" -lt 1 ]]; then
    echo "CONTENT SMOKE FAIL: section 9 chunk count is $CHUNK_COUNT (expected at least 1: the demo must persist chunk edits)"
    exit 1
fi

# Every chunk-index entry must have its matching 16+i palette section. The
# helper fails closed if an indexed chunk section is missing.
for ((chunk_ordinal = 0; chunk_ordinal < CHUNK_COUNT; chunk_ordinal++)); do
    chunk_section_id=$((16 + chunk_ordinal))
    chunk_section_len=$(mcvx_section_len "$SAVE" "$chunk_section_id")
    if [[ "$chunk_section_len" -eq 0 ]]; then
        echo "CONTENT SMOKE FAIL: chunk palette section $chunk_section_id is empty"
        exit 1
    fi
done

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

SAVE_SIZE=$(wc -c <"$SAVE")
echo "CONTENT SMOKE PASS [profile=$PROFILE]: seed 1 looted a ruin chest, broke it for 1 Chest, tilled/planted two crops through the Winter pause to stage 3, harvested 2 Wheat + 4 WheatSeeds, crafted Bread, placed and stored it in a chest, and reloaded a ${SAVE_SIZE}-byte v10 save with reload_chests=1 reload_stage=1, ${CHEST_COUNT} chest record, ${CROP_COUNT} crop record, ${CHUNK_COUNT} chunk sections and the named chest/farmland/crop/hoe content"
echo "CONTENT SMOKE EVIDENCE: $CONTENT_LINE"
