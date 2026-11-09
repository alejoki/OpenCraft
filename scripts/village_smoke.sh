#!/usr/bin/env bash
# Real-binary authored-worldgen smoke for M015/S07-T04.
#
# Proves the authored-prefab / village surface end to end through the production
# binary, with no test harness, no screenshot automation, and no window:
#
#   Docs gate: tracked docs/WORLDGEN.md and README.md carry the authoring CLI
#     entry points (--demo-capture, --validate-worldgen, --demo-village), the
#     content paths, the smoke-script invocation, the profile switch, socket
#     markers, and the TOML sections. Neither the git-ignored human-runbook doc
#     nor any other untracked file is required, so a fresh clone passes.
#
#   Validator matrix: --validate-worldgen pins clean, invalid-TOML, oversize,
#     duplicate-name, and missing-tree runs whole-line, including the exit code
#     (0/1/1/1/0) and the sorted-by-filename diagnostic order.
#
#   Capture: --demo-capture pins the ten-line success digest (region, 423-byte
#     write, roundtrip, validate, socket, ambiguous marker, catalog, captured-
#     prefab village, road), the exact on-disk tree, the socket TOML, and the
#     idempotent second run (capture_2.toml at 425 bytes, capture.toml
#     byte-identical). Failure legs pin the exit-2 usage error and the exit-1
#     "not a directory" write error.
#
#   Generate: --demo-village pins the twelve-line digest (catalog fingerprint,
#     site cell/anchor, 10-20 count, layout digest, order/single-chunk/empty/
#     outside identical proofs, fit, session cells) and proves the output is
#     byte-identical across two independent temp cwds.
#
#   Degradation: a corrupt prefab keeps --demo-village at exit 0 with two
#     warning lines and reduced counts; a missing worldgen/ tree exits 1 with
#     failed=site.
#
#   Isolation: the repository's tracked worldgen/ tree, packs/ and config/ are
#     byte-identical after every leg, and no launch cwd gains settings.toml,
#     saves/ or packs/. Only target/ (git-ignored) is written inside the repo.
#
# The whole-line pinned digests are mirrored in src/main.rs's demo tests. They
# must be re-pinned from a measured run of this binary; never relax a pin or
# substring-match a line to make a suspect binary pass. If a pin disagrees,
# fix the source/test first and re-measure both halves together.
#
# --demo-capture, --validate-worldgen and --demo-village all short-circuit
# before StartupContent, so player saves/, settings.toml and packs/ are never
# touched. Every leg runs in its own temp cwd; content legs run against a copy
# of worldgen/ so the tracked tree cannot be mutated.
#
# Dependencies: bash, cargo, cmp, mktemp, cp, grep, diff, mkdir, sed, awk, find.

set -u

# Profile switch (default debug). An invalid value fails closed before any
# build or launch.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "VILLAGE SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
        exit 1
        ;;
esac

if [[ "$PROFILE" == "release" ]]; then
    BUILD_ARGS=(build --locked --release)
else
    BUILD_ARGS=(build --locked)
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/$PROFILE/opencraft"
BUILD_LOG="$(mktemp /tmp/opencraft-village-build.XXXXXX.log)" || {
    echo "VILLAGE SMOKE FAIL: could not create a build log" >&2
    exit 1
}

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "VILLAGE SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    rm -f "$BUILD_LOG"
    exit 1
fi
rm -f "$BUILD_LOG"

if [[ ! -x "$BIN" ]]; then
    echo "VILLAGE SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

for tool in cmp mktemp cp grep diff mkdir sed awk find; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "VILLAGE SMOKE FAIL: required tool '$tool' not found on PATH"
        exit 1
    }
done

TEMP_DIR="$(mktemp -d /tmp/opencraft-village-smoke.XXXXXX)" || {
    echo "VILLAGE SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "VILLAGE SMOKE FAIL: $*" >&2
    exit 1
}

# Compare an actual captured log to pinned lines WHOLE-LINE, in order, using
# diff on two files. Never a substring match. Extra, missing, reordered or
# reformatted lines all fail.
pinned_lines() {
    local actual="$1"
    local label="$2"
    shift 2
    local expected_file="$TEMP_DIR/pinned-$label.txt"
    local diff_file="$TEMP_DIR/pinned-$label.diff"
    printf '%s\n' "$@" >"$expected_file" || fail "cannot write expected lines for $label"
    if ! diff -u "$expected_file" "$actual" >"$diff_file"; then
        echo "VILLAGE SMOKE FAIL: $label drifted from the pinned whole-line contract" >&2
        cat "$diff_file" >&2
        echo "--- captured $label output ---" >&2
        cat "$actual" >&2
        exit 1
    fi
}

copy_worldgen() {
    local dir="$1"
    mkdir -p "$dir" || fail "cannot create $dir"
    cp -R "$CRATE_ROOT/worldgen" "$dir/worldgen" || fail "cannot copy worldgen/ into $dir"
}

[[ -d "$CRATE_ROOT/worldgen" ]] || fail "tracked worldgen/ tree not found at $CRATE_ROOT/worldgen"

# Snapshot the tracked content and the optional repo content dirs before any
# leg, so the read-only contract can be compared afterwards. TEMP_DIR already
# exists, so this cp creates before-worldgen/ itself and keeps the directory
# name.
cp -R "$CRATE_ROOT/worldgen" "$TEMP_DIR/before-worldgen" || fail "cannot snapshot worldgen/"
if [[ -d "$CRATE_ROOT/packs" ]]; then
    cp -R "$CRATE_ROOT/packs" "$TEMP_DIR/before-packs" || fail "cannot snapshot packs/"
fi
if [[ -d "$CRATE_ROOT/config" ]]; then
    cp -R "$CRATE_ROOT/config" "$TEMP_DIR/before-config" || fail "cannot snapshot config/"
fi
if [[ -f "$CRATE_ROOT/settings.toml" ]]; then
    cp "$CRATE_ROOT/settings.toml" "$TEMP_DIR/before-settings.toml" || fail "cannot snapshot settings.toml"
fi

LAUNCH_DIRS=()

# --- Leg 1: docs gate (tracked files only, no launch) ---------------------
while IFS= read -r needle; do
    grep -qF -- "$needle" "$CRATE_ROOT/docs/WORLDGEN.md" \
        || fail "docs/WORLDGEN.md is missing the authoring needle '$needle'"
done <<'EOF'
--demo-capture
--validate-worldgen
--demo-village
worldgen/prefabs/
worldgen/villages/
scripts/village_smoke.sh
SMOKE_PROFILE=release
socket
[palette]
[[sockets]]
EOF

while IFS= read -r needle; do
    grep -qF -- "$needle" "$CRATE_ROOT/README.md" \
        || fail "README.md is missing the authoring needle '$needle'"
done <<'EOF'
village_smoke.sh
docs/WORLDGEN.md
EOF

# --- Leg 2: validator clean -----------------------------------------------
VALID_CLEAN="$TEMP_DIR/valid-clean"
copy_worldgen "$VALID_CLEAN"
LAUNCH_DIRS+=("$VALID_CLEAN")
( cd "$VALID_CLEAN" && "$BIN" --validate-worldgen ) >"$TEMP_DIR/valid-clean.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: validator clean exit code $RC (expected 0)"
    cat "$TEMP_DIR/valid-clean.log"
    exit 1
}
pinned_lines "$TEMP_DIR/valid-clean.log" "validator-clean" \
    "ok: prefab plains_hub" \
    "ok: prefab small_house" \
    "ok: prefab wheat_farm" \
    "validate-worldgen: 3 prefabs, 0 errors, 0 warnings"

# --- Leg 3: validator broken TOML ----------------------------------------
VALID_BROKEN="$TEMP_DIR/valid-broken"
copy_worldgen "$VALID_BROKEN"
printf 'format = [\n' >>"$VALID_BROKEN/worldgen/prefabs/small_house.toml" \
    || fail "cannot corrupt small_house.toml for the broken leg"
LAUNCH_DIRS+=("$VALID_BROKEN")
( cd "$VALID_BROKEN" && "$BIN" --validate-worldgen ) >"$TEMP_DIR/valid-broken.log" 2>&1
RC=$?
[[ $RC -eq 1 ]] || {
    echo "VILLAGE SMOKE FAIL: validator broken exit code $RC (expected 1)"
    cat "$TEMP_DIR/valid-broken.log"
    exit 1
}
pinned_lines "$TEMP_DIR/valid-broken.log" "validator-broken" \
    "ok: prefab plains_hub" \
    "error: worldgen/prefabs/small_house.toml: invalid TOML" \
    "ok: prefab wheat_farm" \
    "validate-worldgen: 2 prefabs, 1 errors, 0 warnings"

# --- Leg 4: validator oversize -------------------------------------------
VALID_OVERSIZE="$TEMP_DIR/valid-oversize"
copy_worldgen "$VALID_OVERSIZE"
sed -i 's/size = \[5, 4, 5\]/size = [33, 4, 5]/' "$VALID_OVERSIZE/worldgen/prefabs/small_house.toml" \
    || fail "cannot rewrite small_house.toml size for the oversize leg"
LAUNCH_DIRS+=("$VALID_OVERSIZE")
( cd "$VALID_OVERSIZE" && "$BIN" --validate-worldgen ) >"$TEMP_DIR/valid-oversize.log" 2>&1
RC=$?
[[ $RC -eq 1 ]] || {
    echo "VILLAGE SMOKE FAIL: validator oversize exit code $RC (expected 1)"
    cat "$TEMP_DIR/valid-oversize.log"
    exit 1
}
pinned_lines "$TEMP_DIR/valid-oversize.log" "validator-oversize" \
    "ok: prefab plains_hub" \
    "error: worldgen/prefabs/small_house.toml: size 33x4x5 exceeds cap 32x32x32" \
    "ok: prefab wheat_farm" \
    "validate-worldgen: 2 prefabs, 1 errors, 0 warnings"

# --- Leg 5: validator duplicate name -------------------------------------
VALID_DUPLICATE="$TEMP_DIR/valid-duplicate"
copy_worldgen "$VALID_DUPLICATE"
cp "$VALID_DUPLICATE/worldgen/prefabs/small_house.toml" "$VALID_DUPLICATE/worldgen/prefabs/copy_house.toml" \
    || fail "cannot duplicate small_house.toml for the duplicate-name leg"
LAUNCH_DIRS+=("$VALID_DUPLICATE")
( cd "$VALID_DUPLICATE" && "$BIN" --validate-worldgen ) >"$TEMP_DIR/valid-duplicate.log" 2>&1
RC=$?
[[ $RC -eq 1 ]] || {
    echo "VILLAGE SMOKE FAIL: validator duplicate exit code $RC (expected 1)"
    cat "$TEMP_DIR/valid-duplicate.log"
    exit 1
}
# copy_house.toml sorts before plains_hub.toml; the duplicate is rejected on
# the sorted-first file. Do not "fix" this order.
pinned_lines "$TEMP_DIR/valid-duplicate.log" "validator-duplicate" \
    "ok: prefab small_house" \
    "ok: prefab plains_hub" \
    "error: worldgen/prefabs/small_house.toml: duplicate prefab name 'small_house'" \
    "ok: prefab wheat_farm" \
    "validate-worldgen: 3 prefabs, 1 errors, 0 warnings"

# --- Leg 6: validator missing tree ---------------------------------------
MISSING_CWD="$TEMP_DIR/missing-cwd"
mkdir -p "$MISSING_CWD" || fail "cannot create $MISSING_CWD"
LAUNCH_DIRS+=("$MISSING_CWD")
( cd "$MISSING_CWD" && "$BIN" --validate-worldgen "$TEMP_DIR/absent" ) >"$TEMP_DIR/valid-missing.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: validator missing-tree exit code $RC (expected 0)"
    cat "$TEMP_DIR/valid-missing.log"
    exit 1
}
pinned_lines "$TEMP_DIR/valid-missing.log" "validator-missing" \
    "validate-worldgen: 0 prefabs, 0 errors, 0 warnings"
[[ ! -e "$TEMP_DIR/absent" ]] || fail "the validator created the absent tree $TEMP_DIR/absent"

# --- Leg 7: capture success ----------------------------------------------
CAPTURE_CWD="$TEMP_DIR/capture-cwd"
mkdir -p "$CAPTURE_CWD" || fail "cannot create $CAPTURE_CWD"
LAUNCH_DIRS+=("$CAPTURE_CWD")
CAP_ROOT="$TEMP_DIR/cap-root"
mkdir -p "$CAP_ROOT" || fail "cannot create $CAP_ROOT"
( cd "$CAPTURE_CWD" && "$BIN" --demo-capture "$CAP_ROOT" ) >"$TEMP_DIR/capture-1.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: capture success exit code $RC (expected 0)"
    cat "$TEMP_DIR/capture-1.log"
    exit 1
}
pinned_lines "$TEMP_DIR/capture-1.log" "capture-success" \
    "capture_demo: leg=region min=0,40,0 max=4,43,4 size=5x4x5 cells=100" \
    "capture_demo: leg=write name=capture path=prefabs/capture.toml bytes=423" \
    "capture_demo: leg=roundtrip identical=true" \
    "capture_demo: leg=validate ok=true prefabs=1 errors=0" \
    "capture_demo: leg=socket position=0,1,-2 direction=north sockets=1 markers=0" \
    "capture_demo: leg=ambiguous pos=2,41,0 wrote=false reason=socket marker at [2, 41, 0] is ambiguous" \
    "capture_demo: leg=catalog captured=true definition=true diagnostics=0" \
    "capture_demo: leg=village definition=capture_demo biome=plains cell=6,-6 anchor=1654,-1383 prefabs=2 socket_links=1 roads=2 prefab=capture" \
    "capture_demo: leg=road cobblestone=2/2 socket_clear=true prefab_block=true roads=2" \
    "capture_demo: ok=true"

( cd "$CAP_ROOT" && find . -type f | sort ) >"$TEMP_DIR/capture-tree.txt"
pinned_lines "$TEMP_DIR/capture-tree.txt" "capture-tree" \
    "./prefabs/capture.toml" \
    "./villages/capture_demo.toml"

CAPTURE_TOML="$CAP_ROOT/prefabs/capture.toml"
[[ -f "$CAPTURE_TOML" ]] || fail "capture did not write prefabs/capture.toml"
CAPTURE_BYTES="$(wc -c <"$CAPTURE_TOML")"
[[ "$CAPTURE_BYTES" -eq 423 ]] || fail "capture.toml is $CAPTURE_BYTES bytes (expected 423)"
[[ "$(grep -c '^\[\[sockets\]\]$' "$CAPTURE_TOML")" -eq 1 ]] \
    || fail "capture.toml must contain exactly one [[sockets]] entry"
grep -qF 'position = [0, 1, -2]' "$CAPTURE_TOML" \
    || fail "capture.toml is missing 'position = [0, 1, -2]'"
grep -qF 'direction = "north"' "$CAPTURE_TOML" \
    || fail "capture.toml is missing 'direction = \"north\"'"
if grep -qF 'socket_marker' "$CAPTURE_TOML"; then
    fail "capture.toml still contains socket_marker text"
fi

( cd "$CAPTURE_CWD" && "$BIN" --validate-worldgen "$CAP_ROOT" ) >"$TEMP_DIR/capture-validate-1.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: capture validate exit code $RC (expected 0)"
    cat "$TEMP_DIR/capture-validate-1.log"
    exit 1
}
pinned_lines "$TEMP_DIR/capture-validate-1.log" "capture-validate-1" \
    "ok: prefab capture" \
    "validate-worldgen: 1 prefabs, 0 errors, 0 warnings"

# --- Leg 8: capture idempotence ------------------------------------------
cp "$CAPTURE_TOML" "$TEMP_DIR/capture-first.toml" || fail "cannot save the first capture.toml"
( cd "$CAPTURE_CWD" && "$BIN" --demo-capture "$CAP_ROOT" ) >"$TEMP_DIR/capture-2.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: capture idempotence exit code $RC (expected 0)"
    cat "$TEMP_DIR/capture-2.log"
    exit 1
}
pinned_lines "$TEMP_DIR/capture-2.log" "capture-idempotence" \
    "capture_demo: leg=region min=0,40,0 max=4,43,4 size=5x4x5 cells=100" \
    "capture_demo: leg=write name=capture_2 path=prefabs/capture_2.toml bytes=425" \
    "capture_demo: leg=roundtrip identical=true" \
    "capture_demo: leg=validate ok=true prefabs=2 errors=0" \
    "capture_demo: leg=socket position=0,1,-2 direction=north sockets=1 markers=0" \
    "capture_demo: leg=ambiguous pos=2,41,0 wrote=false reason=socket marker at [2, 41, 0] is ambiguous" \
    "capture_demo: leg=catalog captured=true definition=true diagnostics=0" \
    "capture_demo: leg=village definition=capture_demo biome=plains cell=6,-6 anchor=1654,-1383 prefabs=2 socket_links=1 roads=2 prefab=capture_2" \
    "capture_demo: leg=road cobblestone=2/2 socket_clear=true prefab_block=true roads=2" \
    "capture_demo: ok=true"
cmp -s "$TEMP_DIR/capture-first.toml" "$CAPTURE_TOML" \
    || fail "the idempotent second capture rewrote prefabs/capture.toml"

( cd "$CAPTURE_CWD" && "$BIN" --validate-worldgen "$CAP_ROOT" ) >"$TEMP_DIR/capture-validate-2.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: capture idempotence validate exit code $RC (expected 0)"
    cat "$TEMP_DIR/capture-validate-2.log"
    exit 1
}
pinned_lines "$TEMP_DIR/capture-validate-2.log" "capture-validate-2" \
    "ok: prefab capture" \
    "ok: prefab capture_2" \
    "validate-worldgen: 2 prefabs, 0 errors, 0 warnings"

# --- Leg 9: capture failure legs -----------------------------------------
( cd "$CAPTURE_CWD" && "$BIN" --demo-capture ) >"$TEMP_DIR/capture-no-value.log" 2>&1
RC=$?
[[ $RC -eq 2 ]] || {
    echo "VILLAGE SMOKE FAIL: capture with no directory exit code $RC (expected 2)"
    cat "$TEMP_DIR/capture-no-value.log"
    exit 1
}
pinned_lines "$TEMP_DIR/capture-no-value.log" "capture-no-value" \
    "error: --demo-capture requires a directory"

FILE_ROOT_PARENT="$TEMP_DIR/file-root-parent"
FILE_ROOT="$FILE_ROOT_PARENT/not-a-dir"
mkdir -p "$FILE_ROOT_PARENT" || fail "cannot create $FILE_ROOT_PARENT"
touch "$FILE_ROOT" || fail "cannot create $FILE_ROOT"
( cd "$CAPTURE_CWD" && "$BIN" --demo-capture "$FILE_ROOT" ) >"$TEMP_DIR/capture-file-root.log" 2>&1
RC=$?
[[ $RC -eq 1 ]] || {
    echo "VILLAGE SMOKE FAIL: capture with a file root exit code $RC (expected 1)"
    cat "$TEMP_DIR/capture-file-root.log"
    exit 1
}
pinned_lines "$TEMP_DIR/capture-file-root.log" "capture-file-root" \
    "capture_demo: leg=region min=0,40,0 max=4,43,4 size=5x4x5 cells=100" \
    "capture_demo: leg=write error=could not write worldgen/prefabs/capture.toml: not a directory" \
    "capture_demo: ok=false failed=write"
[[ -f "$FILE_ROOT" ]] || fail "the failed capture leg deleted or replaced the file root"
mapfile -t FILE_ROOT_ENTRIES < <(find "$FILE_ROOT_PARENT" -mindepth 1 | sort)
[[ ${#FILE_ROOT_ENTRIES[@]} -eq 1 && "${FILE_ROOT_ENTRIES[0]}" == "$FILE_ROOT" ]] \
    || fail "the failed capture leg created entries beside the file root"

# --- Leg 10: generate deterministic digest -------------------------------
GEN_A="$TEMP_DIR/generate-a"
GEN_B="$TEMP_DIR/generate-b"
copy_worldgen "$GEN_A"
copy_worldgen "$GEN_B"
LAUNCH_DIRS+=("$GEN_A" "$GEN_B")
( cd "$GEN_A" && "$BIN" --demo-village ) >"$TEMP_DIR/generate-a.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: generate exit code $RC (expected 0)"
    cat "$TEMP_DIR/generate-a.log"
    exit 1
}
pinned_lines "$TEMP_DIR/generate-a.log" "generate" \
    "village_demo: leg=catalog prefabs=3 villages=1 diagnostics=0 fingerprint=0x5c2b550240d34cfb" \
    "village_demo: leg=context rules=0 villages=1" \
    "village_demo: leg=site cell=6,-6 anchor=1654,-1383 definition=plains prefabs=13 socket_links=12 roads=48 unmatched=22" \
    "village_demo: leg=counts in_range=true min=10 max=20" \
    "village_demo: leg=layout digest=0x79663be18a213bab" \
    "village_demo: leg=order identical=true" \
    "village_demo: leg=single_chunk identical=true" \
    "village_demo: leg=empty_catalog identical=true" \
    "village_demo: leg=outside identical=true" \
    "village_demo: leg=fit foundations=true no_overlap=true no_orphan_road=true sockets_linked=true" \
    "village_demo: leg=session cell=-1,0 spawn=0,0 chunks=6 cells=2962 identical=true" \
    "village_demo: ok=true"

( cd "$GEN_B" && "$BIN" --demo-village ) >"$TEMP_DIR/generate-b.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: second generate exit code $RC (expected 0)"
    cat "$TEMP_DIR/generate-b.log"
    exit 1
}
cmp -s "$TEMP_DIR/generate-a.log" "$TEMP_DIR/generate-b.log" \
    || fail "two independent --demo-village runs produced different output"

# --- Leg 11: generate degradation ----------------------------------------
GEN_DEGRADED="$TEMP_DIR/generate-degraded"
copy_worldgen "$GEN_DEGRADED"
printf 'format = [\n' >>"$GEN_DEGRADED/worldgen/prefabs/small_house.toml" \
    || fail "cannot corrupt small_house.toml for the degradation leg"
LAUNCH_DIRS+=("$GEN_DEGRADED")
( cd "$GEN_DEGRADED" && "$BIN" --demo-village ) >"$TEMP_DIR/generate-degraded.log" 2>&1
RC=$?
[[ $RC -eq 0 ]] || {
    echo "VILLAGE SMOKE FAIL: degraded generate exit code $RC (expected 0)"
    cat "$TEMP_DIR/generate-degraded.log"
    exit 1
}
pinned_lines "$TEMP_DIR/generate-degraded.log" "generate-degraded" \
    "village_demo: warning worldgen/prefabs/small_house.toml: invalid TOML" \
    "village_demo: warning worldgen/villages/plains.toml: pool 'house' names unknown prefab 'small_house'" \
    "village_demo: leg=catalog prefabs=2 villages=1 diagnostics=2 fingerprint=0x6568c30e7fc2b8b9" \
    "village_demo: leg=context rules=0 villages=1" \
    "village_demo: leg=site cell=6,-6 anchor=1654,-1383 definition=plains prefabs=13 socket_links=12 roads=48 unmatched=22" \
    "village_demo: leg=counts in_range=true min=10 max=20" \
    "village_demo: leg=layout digest=0xe6bdd6a9da365441" \
    "village_demo: leg=order identical=true" \
    "village_demo: leg=single_chunk identical=true" \
    "village_demo: leg=empty_catalog identical=true" \
    "village_demo: leg=outside identical=true" \
    "village_demo: leg=fit foundations=true no_overlap=true no_orphan_road=true sockets_linked=true" \
    "village_demo: leg=session cell=-1,0 spawn=0,0 chunks=6 cells=2850 identical=true" \
    "village_demo: ok=true"

GEN_ABSENT="$TEMP_DIR/generate-absent"
mkdir -p "$GEN_ABSENT" || fail "cannot create $GEN_ABSENT"
LAUNCH_DIRS+=("$GEN_ABSENT")
( cd "$GEN_ABSENT" && "$BIN" --demo-village ) >"$TEMP_DIR/generate-absent.log" 2>&1
RC=$?
[[ $RC -eq 1 ]] || {
    echo "VILLAGE SMOKE FAIL: missing-worldgen generate exit code $RC (expected 1)"
    cat "$TEMP_DIR/generate-absent.log"
    exit 1
}
pinned_lines "$TEMP_DIR/generate-absent.log" "generate-absent" \
    "village_demo: leg=catalog prefabs=0 villages=0 diagnostics=0 fingerprint=0xaf64724c8602eb6e" \
    "village_demo: leg=context rules=0 villages=0" \
    "village_demo: leg=site cell=none anchor=none definition=none prefabs=0 socket_links=0 roads=0 unmatched=0" \
    "village_demo: leg=counts in_range=false min=10 max=20" \
    "village_demo: leg=layout digest=0x0000000000000000" \
    "village_demo: leg=order identical=false" \
    "village_demo: leg=single_chunk identical=false" \
    "village_demo: leg=empty_catalog identical=false" \
    "village_demo: leg=outside identical=false" \
    "village_demo: leg=fit foundations=false no_overlap=false no_orphan_road=false sockets_linked=false" \
    "village_demo: leg=session cell=none spawn=0,0 chunks=0 cells=0 identical=false" \
    "village_demo: ok=false failed=site"

# --- Leg 12: no repository writes / no player state ----------------------
diff -r "$TEMP_DIR/before-worldgen" "$CRATE_ROOT/worldgen" >"$TEMP_DIR/worldgen-diff.txt" 2>&1 \
    || {
        echo "VILLAGE SMOKE FAIL: the tracked worldgen/ tree changed during the smoke"
        cat "$TEMP_DIR/worldgen-diff.txt"
        exit 1
    }
if [[ -d "$CRATE_ROOT/packs" ]]; then
    diff -r "$TEMP_DIR/before-packs" "$CRATE_ROOT/packs" >"$TEMP_DIR/packs-diff.txt" 2>&1 \
        || {
            echo "VILLAGE SMOKE FAIL: the repo packs/ tree changed during the smoke"
            cat "$TEMP_DIR/packs-diff.txt"
            exit 1
        }
fi
if [[ -d "$CRATE_ROOT/config" ]]; then
    diff -r "$TEMP_DIR/before-config" "$CRATE_ROOT/config" >"$TEMP_DIR/config-diff.txt" 2>&1 \
        || {
            echo "VILLAGE SMOKE FAIL: the repo config/ tree changed during the smoke"
            cat "$TEMP_DIR/config-diff.txt"
            exit 1
        }
fi
if [[ -f "$CRATE_ROOT/settings.toml" ]]; then
    cmp -s "$TEMP_DIR/before-settings.toml" "$CRATE_ROOT/settings.toml" \
        || fail "the repo settings.toml changed during the smoke"
fi

for dir in "${LAUNCH_DIRS[@]}"; do
    [[ ! -e "$dir/settings.toml" ]] || fail "launch cwd $dir gained a settings.toml"
    [[ ! -e "$dir/saves" ]] || fail "launch cwd $dir gained a saves/ directory"
    [[ ! -e "$dir/packs" ]] || fail "launch cwd $dir gained a packs/ directory"
done

# --- Leg 13: PASS + one evidence line per pinned digest group -------------
echo "VILLAGE SMOKE PASS [profile=$PROFILE]"
echo "VILLAGE SMOKE EVIDENCE: validator matrix clean=0/broken=1/oversize=1/duplicate=1/missing=0 pinned whole-line"
echo "VILLAGE SMOKE EVIDENCE: capture success bytes=423 sockets=1 markers=0 ambiguous-write=false catalog+village+road ok"
echo "VILLAGE SMOKE EVIDENCE: capture idempotence capture.toml byte-identical second path=prefabs/capture_2.toml bytes=425"
echo "VILLAGE SMOKE EVIDENCE: generate digest=0x79663be18a213bab session chunks=6 cells=2962 deterministic across two temp cwds"
echo "VILLAGE SMOKE EVIDENCE: degradation prefabs=2 diagnostics=2 digest=0xe6bdd6a9da365441 session cells=2850; missing tree exit=1 failed=site"
echo "VILLAGE SMOKE EVIDENCE: worldgen/packs/config byte-identical; launch cwds free of settings.toml, saves/ and packs/"
exit 0
