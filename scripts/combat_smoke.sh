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
#     (e) the save is exactly the 317-byte v9 witness: magic `MCVX`, version 9
#         at offset 4, a 72-byte save-local content table starting at offset 36,
#         mob count 2 at offset 253, Zombie tag 2 @257 with health 11 @274,
#         Skeleton tag 3 @276 with health 14 @293, the two 19-byte records
#         ending at 295, then the 12-byte pack-identity + FNV-1a digest tail
#         (295..307) and the 10-byte v9 config tail (307..317) ending exactly
#         at EOF (253 + 4 + 2*19 + 22 == 317).
#
#   Reload leg: `--save c.mcvx --demo-combat` (no `--new`)
#     (f) exit code 0, no panic, the save still 317 bytes, and the exact
#         `combat_demo: seed=1 stage=restored ...` line.
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
SAVE_BYTES=317
MOB_COUNT_OFFSET=253
MOB_RECORD_LEN=19
MOB_COUNT=2
# Measured save-local content table: 7 names (arrow, bone, leather, raw_beef,
# raw_porkchop, wood_bow, wood_sword) = u32 count + 7 * (2-byte length + name).
CONTENT_TABLE_OFFSET=36
CONTENT_NAME_COUNT=7
# v9 tail: the v8 u32 pack count (zero for the built-in stack) + u64 FNV-1a
# digest (12 bytes), then the v9 u16 config schema + u64 effective-value
# digest (10 bytes).
SAVE_TAIL_BYTES=22

read_u32_le() {
    od -An -tu4 --endian=little -j "$2" -N 4 "$1" | tr -d ' '
}
read_u16_le() {
    od -An -tu2 --endian=little -j "$2" -N 2 "$1" | tr -d ' '
}
read_u8() {
    od -An -tu1 -j "$2" -N 1 "$1" | tr -d ' '
}

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

# Save witness: the exact 317-byte v9 layout the fresh leg writes.
if [[ ! -f "$SAVE" ]]; then
    echo "COMBAT SMOKE FAIL: fresh leg exited 0 but wrote no save at $SAVE"
    exit 1
fi

OBSERVED_BYTES=$(wc -c <"$SAVE" | tr -d ' ')
if [[ "$OBSERVED_BYTES" -ne "$SAVE_BYTES" ]]; then
    echo "COMBAT SMOKE FAIL: save is $OBSERVED_BYTES bytes (expected $SAVE_BYTES)"
    exit 1
fi

MAGIC=$(head -c 4 "$SAVE")
if [[ "$MAGIC" != "MCVX" ]]; then
    echo "COMBAT SMOKE FAIL: save magic is '$MAGIC' (expected MCVX)"
    exit 1
fi

VERSION=$(read_u32_le "$SAVE" 4)
if [[ "$VERSION" -ne 9 ]]; then
    echo "COMBAT SMOKE FAIL: save version is $VERSION (expected 9)"
    exit 1
fi

CONTENT_NAME_COUNT_OBSERVED=$(read_u32_le "$SAVE" "$CONTENT_TABLE_OFFSET")
if [[ "$CONTENT_NAME_COUNT_OBSERVED" -ne "$CONTENT_NAME_COUNT" ]]; then
    echo "COMBAT SMOKE FAIL: content table at offset $CONTENT_TABLE_OFFSET holds $CONTENT_NAME_COUNT_OBSERVED names (expected $CONTENT_NAME_COUNT)"
    exit 1
fi

MOB_COUNT_OBSERVED=$(read_u32_le "$SAVE" "$MOB_COUNT_OFFSET")
if [[ "$MOB_COUNT_OBSERVED" -ne "$MOB_COUNT" ]]; then
    echo "COMBAT SMOKE FAIL: mob count at offset $MOB_COUNT_OFFSET is $MOB_COUNT_OBSERVED (expected $MOB_COUNT)"
    exit 1
fi

ZOMBIE_TAG_OFFSET=$((MOB_COUNT_OFFSET + 4))
ZOMBIE_TAG=$(read_u8 "$SAVE" "$ZOMBIE_TAG_OFFSET")
if [[ "$ZOMBIE_TAG" -ne 2 ]]; then
    echo "COMBAT SMOKE FAIL: Zombie tag at offset $ZOMBIE_TAG_OFFSET is $ZOMBIE_TAG (expected 2)"
    exit 1
fi
ZOMBIE_HEALTH_OFFSET=$((ZOMBIE_TAG_OFFSET + 1 + 12 + 4))
ZOMBIE_HEALTH=$(read_u16_le "$SAVE" "$ZOMBIE_HEALTH_OFFSET")
if [[ "$ZOMBIE_HEALTH" -ne 11 ]]; then
    echo "COMBAT SMOKE FAIL: Zombie health at offset $ZOMBIE_HEALTH_OFFSET is $ZOMBIE_HEALTH (expected 11)"
    exit 1
fi

SKELETON_TAG_OFFSET=$((ZOMBIE_TAG_OFFSET + MOB_RECORD_LEN))
SKELETON_TAG=$(read_u8 "$SAVE" "$SKELETON_TAG_OFFSET")
if [[ "$SKELETON_TAG" -ne 3 ]]; then
    echo "COMBAT SMOKE FAIL: Skeleton tag at offset $SKELETON_TAG_OFFSET is $SKELETON_TAG (expected 3)"
    exit 1
fi
SKELETON_HEALTH_OFFSET=$((SKELETON_TAG_OFFSET + 1 + 12 + 4))
SKELETON_HEALTH=$(read_u16_le "$SAVE" "$SKELETON_HEALTH_OFFSET")
if [[ "$SKELETON_HEALTH" -ne 14 ]]; then
    echo "COMBAT SMOKE FAIL: Skeleton health at offset $SKELETON_HEALTH_OFFSET is $SKELETON_HEALTH (expected 14)"
    exit 1
fi

MOB_TAIL_END=$((MOB_COUNT_OFFSET + 4 + MOB_COUNT * MOB_RECORD_LEN))
if [[ "$MOB_TAIL_END" -ne $((SAVE_BYTES - SAVE_TAIL_BYTES)) ]]; then
    echo "COMBAT SMOKE FAIL: mob records end at $MOB_TAIL_END (expected $((SAVE_BYTES - SAVE_TAIL_BYTES)): the 22-byte pack + config tails must end at EOF)"
    exit 1
fi

PACK_COUNT_OFFSET=$MOB_TAIL_END
PACK_COUNT=$(read_u32_le "$SAVE" "$PACK_COUNT_OFFSET")
if [[ "$PACK_COUNT" -ne 0 ]]; then
    echo "COMBAT SMOKE FAIL: pack count at offset $PACK_COUNT_OFFSET is $PACK_COUNT (expected 0 for the built-in stack)"
    exit 1
fi

# The config schema follows the u32 pack count and the u64 registry digest.
CONFIG_SCHEMA_OFFSET=$((PACK_COUNT_OFFSET + 4 + 8))
CONFIG_SCHEMA=$(read_u16_le "$SAVE" "$CONFIG_SCHEMA_OFFSET")
if [[ "$CONFIG_SCHEMA" -ne 1 ]]; then
    echo "COMBAT SMOKE FAIL: v9 config schema at offset $CONFIG_SCHEMA_OFFSET is $CONFIG_SCHEMA (expected 1)"
    exit 1
fi

TAIL_END=$((PACK_COUNT_OFFSET + SAVE_TAIL_BYTES))
if [[ "$TAIL_END" -ne "$SAVE_BYTES" ]]; then
    echo "COMBAT SMOKE FAIL: v9 tail ends at $TAIL_END (expected EOF at $SAVE_BYTES)"
    exit 1
fi

SAVE_LAYOUT_LINE="save: bytes=${OBSERVED_BYTES} magic=${MAGIC} version=${VERSION} content_names=${CONTENT_NAME_COUNT_OBSERVED} mob_count=${MOB_COUNT_OBSERVED} zombie_tag=${ZOMBIE_TAG} zombie_health=${ZOMBIE_HEALTH} skeleton_tag=${SKELETON_TAG} skeleton_health=${SKELETON_HEALTH} mob_tail_end=${MOB_TAIL_END} pack_count=${PACK_COUNT} config_schema=${CONFIG_SCHEMA} tail_end=${TAIL_END}"

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

RESTORED_BYTES=$(wc -c <"$SAVE" | tr -d ' ')
if [[ "$RESTORED_BYTES" -ne "$SAVE_BYTES" ]]; then
    echo "COMBAT SMOKE FAIL: reload leg changed the save size to $RESTORED_BYTES bytes (expected $SAVE_BYTES)"
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

echo "COMBAT SMOKE PASS [profile=$PROFILE]: seed 1 ran the full --demo-combat capstone (ambient spawn/burn, quartet walk, melee, drops, zombie death/respawn, bow shot, skeleton shot, Creative inertness, v9 save) and restored the 317-byte witness; frame p95 ${P95_MS}ms <= ${FRAME_P95_TARGET}ms with ${SAMPLES} samples"
for line in "${EXPECTED_LINES[@]}"; do
    echo "COMBAT SMOKE EVIDENCE: $line"
done
echo "COMBAT SMOKE EVIDENCE: $FRAME_LINE"
echo "COMBAT SMOKE EVIDENCE: $SAVE_LAYOUT_LINE"
echo "COMBAT SMOKE EVIDENCE: $RESTORE_LINE"
