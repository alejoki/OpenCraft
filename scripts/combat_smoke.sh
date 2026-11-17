#!/usr/bin/env bash
# Real-binary full mob/combat capstone smoke (M009/S10-T01).
#
# Proves the assembled `--demo-combat` seam end to end through the production
# session seams with no menu automation and no test harness:
#
#   Fresh leg: `--new --seed 1 --demo-combat --save c.mcvx`
#     (a) exit code 0 — the demo's own latch. A `timeout` kill surfaces as
#         124 and is an explicit FAILURE here, never the expected outcome.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) the six pinned digests appear EXACTLY, in order, with
#         `combat_creative:` last: the quartet walk, the ambient population
#         proof, the zombie melee/death/respawn proof, the bow shot proof, the
#         skeleton ranged/drop proof, and the Creative inertness proof.
#     (d) a `frame_budget:` line exists with `samples >= 126` and
#         `p95 <= 16.6 ms`.
#     (e) the save is a valid sectioned v10 archive: `MCVX` magic, version
#         10 at offset 4, flags 0 at offset 8, a 16-byte header, and a
#         validated contiguous section directory (scripts/save_witness.sh).
#         Section 1 holds the seven kind-tagged harness names, section 5 holds
#         mob count 2 with Zombie tag 2 @ +4 and health 11 @ +21, Skeleton tag
#         3 @ +23 and health 14 @ +40 (two 19-byte records), section 4 holds a
#         zero chest count, sections 8 and 9 hold zero crops and zero chunk
#         edits, and section 7 carries config schema 1 and the default digest.
#
#   Reload leg: `--save c.mcvx --demo-combat` (no `--new`)
#     (f) exit code 0, no panic, the save still validates as the same v10
#         archive with the same byte length, and the exact
#         `combat_demo: seed=1 stage=restored ...` line.
#
# Why the old v9 flat offsets are gone: v10 replaces the single flat payload
# with a 16-byte header, a 21-byte directory entry per section and section
# bodies. The smoke asserts the sections it actually witnessed (content table,
# world/player, chests, mobs, config, crops, chunk index) instead of pinning
# offsets that move whenever a section grows. The historical v9 combat save
# was 317 bytes: content table at 36, mob count at 253, two 19-byte records
# ending at 295, pack tail 295..307 and config tail 307..317. Those offsets
# live only in docs/SAVE-NOTES.md and the frozen v9 witnesses.
#
# The digest values are contractual: the script compares whole lines, never
# substrings, and asserts their order. A release-only abort is a FAILURE here
# (the observed `stage=... observed=...` line is printed); the assertion is
# never relaxed to make a profile pass.
#
# The script builds the debug binary itself with `cargo build --locked` and
# runs target/debug/opencraft. `SMOKE_PROFILE=release` is the opt-in
# real-binary release run: it builds with `cargo build --locked --release` and
# runs target/release/opencraft instead. Debug stays the default. Everything
# runs inside a mktemp working directory, so the crate root's real saves/ and
# settings.toml are never touched.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch, so a typo can never silently
# exercise the wrong tree.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "COMBAT SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
BUILD_LOG="$(mktemp /tmp/opencraft-combat-build.XXXXXX.log)" || {
    echo "COMBAT SMOKE FAIL: could not create a build log" >&2
    exit 1
}

cleanup_build_log() { rm -f "$BUILD_LOG"; }
trap cleanup_build_log EXIT

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "COMBAT SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "COMBAT SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-combat-smoke.XXXXXX)" || {
    echo "COMBAT SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() {
    rm -rf "$TEMP_DIR"
    rm -f "$BUILD_LOG"
}
trap cleanup EXIT

# The demo's own wall-clock deadline is 120 s; the outer timeout is only a
# backstop for a hang, and a kill here is an explicit failure.
OUTER_TIMEOUT=180
FRAME_P95_TARGET=16.6
FRAME_SAMPLES_MIN=126
# Section 5 mob body: u32 count plus 19-byte records {u8 kind, f32 x/y/z,
# f32 yaw, u16 health}. Section 1 content table entries are kind:name.
MOB_RECORD_LEN=19
MOB_COUNT=2
DEFAULT_CONFIG_DIGEST=16927563004400537068

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/c.mcvx"
OUT="$TEMP_DIR/fresh.log"

# The six contractual digest lines, in their required order. `combat_creative:`
# is last by construction.
EXPECTED_LINES=(
    "combat_demo: seed=1 mobs=4 resident=4 walked=4 verts=864 melee_hits=2 melee_damage=4 melee_blocked=1 killed=2 drops=5 drops_lost=0 saved=2 records=2 ok=true"
    "combat_spawn: seed=1 spawned=22 despawned=1 burned=21 cap_blocked=2 safe=0 peak=22 ok=true"
    "combat_hostile: seed=1 attacks=7 damage=20 cause=zombie respawn=spawn knockback=true inventory=kept ok=true"
    "combat_shot: seed=1 charged=true canceled=true fired=1 mob_hits=1 damage=4 knockback=true embed=true expired=true ammo=2 bow=57 ok=true"
    "combat_skeleton: seed=1 fired=1 player_hits=1 damage=2 cause=skeleton_arrow arc=true killed=1 bones=1 arrows=2 bow=true respawn=spawn ok=true"
    "combat_creative: seed=1 inert=true attacks=0 shots=0 damage=0 melee_damage=20 arrow_damage=20 kills=2 drops=2 drops_lost=0 inventory=kept ok=true"
)

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed 1 --demo-combat \
    --save "$SAVE" >"$OUT" 2>&1
RC=$?
set -e

if [[ $RC -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: fresh leg exit code $RC (expected 0 from the demo's own latch; 124 means an external timeout killed it)"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if grep -q "panicked" "$OUT" || grep -q "RUST_BACKTRACE" "$OUT"; then
    echo "COMBAT SMOKE FAIL: panic detected in the fresh leg output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

# Exact whole-line match, never a substring. `grep -F -x` anchors both ends.
# The first occurrence's line number is used for the ordered assertion.
LINE_NUMBERS=()
for expected in "${EXPECTED_LINES[@]}"; do
    line_no=$(grep -n -F -x -e "$expected" "$OUT" | head -n1 | cut -d: -f1 || true)
    if [[ -z "$line_no" ]]; then
        prefix="${expected%% *}"
        observed=$(grep -m1 "^${prefix}" "$OUT" || true)
        echo "COMBAT SMOKE FAIL: missing exact digest line '$expected'"
        echo "COMBAT SMOKE OBSERVED: ${observed:-<no ${prefix} line>}"
        echo "--- captured output ---"
        cat "$OUT"
        exit 1
    fi
    LINE_NUMBERS+=("$line_no")
done

# Ordered assertion: each line must appear strictly after the previous one, so
# `combat_creative:` cannot drift ahead of any earlier proof.
previous=0
for i in "${!LINE_NUMBERS[@]}"; do
    current="${LINE_NUMBERS[$i]}"
    if (( current <= previous )); then
        echo "COMBAT SMOKE FAIL: digest lines are out of order at index $i (line $current after line $previous): ${EXPECTED_LINES[$i]}"
        exit 1
    fi
    previous="$current"
done

# Frame budget: the Walk stage requires 120 Playing observations, and the
# shutdown digest is sampled from those frames, so a healthy run is well above
# 126. The p95 claim is the operational target from the milestone.
FRAME_LINE=$(grep -m1 '^frame_budget:' "$OUT" || true)
if [[ -z "$FRAME_LINE" ]]; then
    echo "COMBAT SMOKE FAIL: no frame_budget: digest line in the fresh leg output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

SAMPLES_TOKEN=$(grep -o 'samples=[0-9]*' <<<"$FRAME_LINE" || true)
if [[ -z "$SAMPLES_TOKEN" ]]; then
    echo "COMBAT SMOKE FAIL: frame_budget digest has no parseable samples field — observed: $FRAME_LINE"
    exit 1
fi
SAMPLES=${SAMPLES_TOKEN#samples=}
if (( SAMPLES < FRAME_SAMPLES_MIN )); then
    echo "COMBAT SMOKE FAIL: frame budget has $SAMPLES samples (expected >= $FRAME_SAMPLES_MIN) — observed: $FRAME_LINE"
    exit 1
fi

P95_TOKEN=$(grep -o 'p95=[0-9.]*ms' <<<"$FRAME_LINE" || true)
if [[ -z "$P95_TOKEN" ]]; then
    echo "COMBAT SMOKE FAIL: frame_budget digest has no parseable p95 field — observed: $FRAME_LINE"
    exit 1
fi
P95_MS=${P95_TOKEN#p95=}
P95_MS=${P95_MS%ms}
if ! awk -v p95="$P95_MS" -v target="$FRAME_P95_TARGET" \
    'BEGIN { exit !((p95 + 0) <= (target + 0)) }'; then
    echo "COMBAT SMOKE FAIL: frame p95 is ${P95_MS}ms (expected <= ${FRAME_P95_TARGET}ms) — observed: $FRAME_LINE"
    exit 1
fi

# Save witness: the v10 sectioned archive the fresh leg writes.
if [[ ! -f "$SAVE" ]]; then
    echo "COMBAT SMOKE FAIL: fresh leg exited 0 but wrote no save at $SAVE"
    exit 1
fi

assert_mcvx_v10 "$SAVE"
OBSERVED_BYTES=$(wc -c <"$SAVE" | tr -d ' ')

CONTENT_NAMES=()
mapfile -t CONTENT_NAMES < <(mcvx_content_table_names "$SAVE")
EXPECTED_CONTENT_NAMES=(
    "2:arrow"
    "2:bone"
    "2:leather"
    "2:raw_beef"
    "2:raw_porkchop"
    "3:wood_bow"
    "3:wood_sword"
)
if [[ "${CONTENT_NAMES[*]}" != "${EXPECTED_CONTENT_NAMES[*]}" ]]; then
    echo "COMBAT SMOKE FAIL: section 1 content table is [${CONTENT_NAMES[*]}] (expected [${EXPECTED_CONTENT_NAMES[*]}], sorted by name then kind)"
    exit 1
fi
CONTENT_NAME_COUNT=${#CONTENT_NAMES[@]}

CHEST_COUNT=$(mcvx_section_u32 "$SAVE" 4 0)
if [[ "$CHEST_COUNT" -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: section 4 chest count is $CHEST_COUNT (expected 0: the combat demo places no chest)"
    exit 1
fi

MOB_COUNT_OBSERVED=$(mcvx_section_u32 "$SAVE" 5 0)
if [[ "$MOB_COUNT_OBSERVED" -ne "$MOB_COUNT" ]]; then
    echo "COMBAT SMOKE FAIL: section 5 mob count is $MOB_COUNT_OBSERVED (expected $MOB_COUNT)"
    exit 1
fi

MOB_SECTION_LEN=$(mcvx_section_len "$SAVE" 5)
MOB_SECTION_EXPECTED=$((4 + MOB_COUNT * MOB_RECORD_LEN))
if [[ "$MOB_SECTION_LEN" -ne "$MOB_SECTION_EXPECTED" ]]; then
    echo "COMBAT SMOKE FAIL: section 5 is $MOB_SECTION_LEN bytes (expected $MOB_SECTION_EXPECTED = u32 count + $MOB_COUNT * $MOB_RECORD_LEN-byte records)"
    exit 1
fi

ZOMBIE_TAG=$(mcvx_section_u8 "$SAVE" 5 4)
if [[ "$ZOMBIE_TAG" -ne 2 ]]; then
    echo "COMBAT SMOKE FAIL: Zombie tag at section 5 body offset 4 is $ZOMBIE_TAG (expected 2)"
    exit 1
fi
ZOMBIE_HEALTH=$(mcvx_section_u16 "$SAVE" 5 $((4 + 1 + 12 + 4)))
if [[ "$ZOMBIE_HEALTH" -ne 11 ]]; then
    echo "COMBAT SMOKE FAIL: Zombie health at section 5 body offset $((4 + 1 + 12 + 4)) is $ZOMBIE_HEALTH (expected 11)"
    exit 1
fi

SKELETON_TAG_OFFSET=$((4 + MOB_RECORD_LEN))
SKELETON_TAG=$(mcvx_section_u8 "$SAVE" 5 "$SKELETON_TAG_OFFSET")
if [[ "$SKELETON_TAG" -ne 3 ]]; then
    echo "COMBAT SMOKE FAIL: Skeleton tag at section 5 body offset $SKELETON_TAG_OFFSET is $SKELETON_TAG (expected 3)"
    exit 1
fi
SKELETON_HEALTH_OFFSET=$((SKELETON_TAG_OFFSET + 1 + 12 + 4))
SKELETON_HEALTH=$(mcvx_section_u16 "$SAVE" 5 "$SKELETON_HEALTH_OFFSET")
if [[ "$SKELETON_HEALTH" -ne 14 ]]; then
    echo "COMBAT SMOKE FAIL: Skeleton health at section 5 body offset $SKELETON_HEALTH_OFFSET is $SKELETON_HEALTH (expected 14)"
    exit 1
fi

PACK_COUNT=$(mcvx_section_u32 "$SAVE" 6 0)
if [[ "$PACK_COUNT" -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: section 6 pack count is $PACK_COUNT (expected 0 for the built-in stack)"
    exit 1
fi

CONFIG_SCHEMA=$(mcvx_section_u16 "$SAVE" 7 0)
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "COMBAT SMOKE FAIL: section 7 config schema is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

CONFIG_DIGEST=$(mcvx_section_u64 "$SAVE" 7 2)
if [[ "$CONFIG_DIGEST" != "$DEFAULT_CONFIG_DIGEST" ]]; then
    echo "COMBAT SMOKE FAIL: section 7 config digest is $CONFIG_DIGEST (expected $DEFAULT_CONFIG_DIGEST = default config)"
    exit 1
fi

CROP_COUNT=$(mcvx_section_u32 "$SAVE" 8 0)
if [[ "$CROP_COUNT" -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: section 8 crop count is $CROP_COUNT (expected 0: the combat demo plants nothing)"
    exit 1
fi

CHUNK_COUNT=$(mcvx_section_u32 "$SAVE" 9 0)
if [[ "$CHUNK_COUNT" -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: section 9 chunk count is $CHUNK_COUNT (expected 0: the combat demo edits no chunks)"
    exit 1
fi

SAVE_LAYOUT_LINE="save: bytes=${OBSERVED_BYTES} magic=MCVX version=10 sections=${MCVX_SECTION_COUNT} content_names=${CONTENT_NAME_COUNT} mob_count=${MOB_COUNT_OBSERVED} zombie_tag=${ZOMBIE_TAG} zombie_health=${ZOMBIE_HEALTH} skeleton_tag=${SKELETON_TAG} skeleton_health=${SKELETON_HEALTH} chests=${CHEST_COUNT} crops=${CROP_COUNT} chunks=${CHUNK_COUNT} config_schema=${CONFIG_SCHEMA}"

# Reload leg: the same save path, no `--new`, so the startup path loads the
# fresh leg's witness and the demo's one-call Restore proves it.
RESTORE_OUT="$TEMP_DIR/restore.log"
set +e
timeout "$OUTER_TIMEOUT" "$BIN" --save "$SAVE" --demo-combat >"$RESTORE_OUT" 2>&1
RESTORE_RC=$?
set -e

if [[ $RESTORE_RC -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: reload leg exit code $RESTORE_RC (expected 0 from the demo's own restore latch; 124 means an external timeout killed it)"
    echo "--- captured reload output ---"
    cat "$RESTORE_OUT"
    exit 1
fi

if grep -q "panicked" "$RESTORE_OUT" || grep -q "RUST_BACKTRACE" "$RESTORE_OUT"; then
    echo "COMBAT SMOKE FAIL: panic detected in the reload leg output"
    echo "--- captured reload output ---"
    cat "$RESTORE_OUT"
    exit 1
fi

assert_mcvx_v10 "$SAVE"
RESTORED_BYTES=$(wc -c <"$SAVE" | tr -d ' ')
if [[ "$RESTORED_BYTES" -ne "$OBSERVED_BYTES" ]]; then
    echo "COMBAT SMOKE FAIL: reload leg changed the save size to $RESTORED_BYTES bytes (expected the fresh leg's $OBSERVED_BYTES)"
    exit 1
fi

RESTORE_LINE="combat_demo: seed=1 stage=restored mobs=2 health=11/14 yaw=true positions=true items=8 ok=true"
if ! grep -q -F -x -e "$RESTORE_LINE" "$RESTORE_OUT"; then
    observed=$(grep -m1 '^combat_demo:' "$RESTORE_OUT" || true)
    echo "COMBAT SMOKE FAIL: missing exact restore line '$RESTORE_LINE'"
    echo "COMBAT SMOKE OBSERVED: ${observed:-<no combat_demo: line>}"
    echo "--- captured reload output ---"
    cat "$RESTORE_OUT"
    exit 1
fi

echo "COMBAT SMOKE PASS [profile=$PROFILE]: seed 1 ran the full --demo-combat capstone (ambient spawn/burn, quartet walk, melee, drops, zombie death/respawn, bow shot, skeleton shot, Creative inertness, v10 save) and restored the ${OBSERVED_BYTES}-byte v10 witness; frame p95 ${P95_MS}ms <= ${FRAME_P95_TARGET}ms with ${SAMPLES} samples"
for line in "${EXPECTED_LINES[@]}"; do
    echo "COMBAT SMOKE EVIDENCE: $line"
done
echo "COMBAT SMOKE EVIDENCE: $FRAME_LINE"
echo "COMBAT SMOKE EVIDENCE: $SAVE_LAYOUT_LINE"
echo "COMBAT SMOKE EVIDENCE: $RESTORE_LINE"
