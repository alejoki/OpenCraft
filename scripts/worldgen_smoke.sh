#!/usr/bin/env bash
# Real-binary declarative-worldgen digest + smoke for the reference pack
# (M010/S06-T04).
#
# Proves the S06 surface end to end through the production binary, with no test
# harness and no screenshot automation:
#
#   Digest leg: `"$BIN" --demo-worldgen` (with the checked-in test fixture at tests/fixtures/packs/mymod-ruby
#   copied into the temp cwd and selected in settings.toml)
#     (a) exit code 0 — the digest's own `worldgen_demo: ok=true` latch. A
#         timeout kill or panic is an explicit FAILURE.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) exactly the pinned non-empty lines, compared WHOLE-LINE (never
#         substrings), in order. The lines pin: the resolved rule count, the
#         registry-aware session construction, authored `mymod:ruby_ore` ore
#         cells strictly inside the authored y band, authored surface-patch
#         cells only in the authored biomes, coal/iron counts unchanged versus
#         an empty-rule regeneration, and byte-identical empty-rule chunks.
#     (d) settings.toml, pack.toml and every pack texture stay byte-identical
#         while the pack is present (the game is read-only on packs/ and
#         settings.toml here).
#
#   Bounded launch leg A (pack present): `timeout 10 "$BIN" --save fresh.mcvx`
#   must exit 124 with no panic — the real streaming path generates chunks with
#   the authored rules and the event loop survives.
#
#   Bounded launch leg B (pack removed): after moving the pack folder out of
#   packs/ and clearing the stack, the same launch must still exit 124 with no
#   panic — the empty-rule default is crash-free and the documented
#   pack-removal drift does not break startup.
#
# The pinned digest lines live in two places on purpose: here and in
# `src/main.rs::worldgen_demo_pins_the_checked_in_reference_pack_line_set`.
# Update both halves together when the reference pack changes. If the binary
# drifts from the pinned test, fix the source/test first; never relax this
# script to match a broken binary.
#
# Dependencies: bash, timeout, cmp, mktemp, od, tr, cp, mv.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "WORLDGEN SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
# create its window. Prefer the Wayland socket for the same reason as the other
# smokes.
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    if [[ -e "/run/user/$(id -u)/wayland-0" ]]; then
        export WAYLAND_DISPLAY="wayland-0"
        export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    elif [[ -e "/tmp/.X11-unix/X0" ]]; then
        export DISPLAY=":0"
    fi
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/$PROFILE/opencraft"
CRATE_PACK="$CRATE_ROOT/tests/fixtures/packs/mymod-ruby"
BUILD_LOG="$(mktemp /tmp/opencraft-worldgen-build.XXXXXX.log)" || {
    echo "WORLDGEN SMOKE FAIL: could not create a build log" >&2
    exit 1
}

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "WORLDGEN SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    rm -f "$BUILD_LOG"
    exit 1
fi
rm -f "$BUILD_LOG"

if [[ ! -x "$BIN" ]]; then
    echo "WORLDGEN SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

if [[ ! -f "$CRATE_PACK/pack.toml" ]]; then
    echo "WORLDGEN SMOKE FAIL: checked-in reference pack not found at $CRATE_PACK"
    exit 1
fi

for tool in timeout cmp mktemp od tr cp mv; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "WORLDGEN SMOKE FAIL: required tool '$tool' not found on PATH"
        exit 1
    }
done

TEMP_DIR="$(mktemp -d /tmp/opencraft-worldgen-smoke.XXXXXX)" || {
    echo "WORLDGEN SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "WORLDGEN SMOKE FAIL: $*" >&2
    exit 1
}

# The contractual digest lines, in their required order. Keep byte-for-byte
# identical to src/main.rs::worldgen_demo_pins_the_checked_in_reference_pack_line_set.
EXPECTED_LINES=(
    "worldgen_demo: leg=registry rules=2 ore=1 patch=1"
    "worldgen_demo: leg=session rules=2 matches=true"
    "worldgen_demo: leg=ore block=mymod:ruby_ore cells=2476 min_y=4 max_y=40 in_band=true"
    "worldgen_demo: leg=patch block=mymod:ruby_ore cells=334 biomes=plains,forest biomes_ok=true"
    "worldgen_demo: leg=coal_iron coal=4248 iron=3875 unchanged=true"
    "worldgen_demo: leg=empty_rules identical=true"
    "worldgen_demo: ok=true"
)

mkdir -p "$TEMP_DIR/packs" || fail "cannot create packs/ in the temp dir"
cp -R "$CRATE_PACK" "$TEMP_DIR/packs/mymod-ruby" || fail "cannot copy the checked-in reference pack"
PACK_DIR="$TEMP_DIR/packs/mymod-ruby"
FIXTURE="$TEMP_DIR/fresh.mcvx"
DIGEST_OUT="$TEMP_DIR/digest.log"
LAUNCH_A_OUT="$TEMP_DIR/launch-a.log"
LAUNCH_B_OUT="$TEMP_DIR/launch-b.log"

cat >"$TEMP_DIR/settings.toml" <<'EOF'
[packs]
stack = ["mymod-ruby"]
EOF

# Snapshot the authored files so the read-only contract can be compared after
# the digest and the authored launch.
EXPECTED_DIR="$TEMP_DIR/expected"
mkdir -p "$EXPECTED_DIR" || fail "cannot create expected/ dir"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "cannot snapshot settings.toml"
cp "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "cannot snapshot pack.toml"
mapfile -t PACK_TEXTURES < <(cd "$PACK_DIR" && find textures -type f | sort)
if [[ ${#PACK_TEXTURES[@]} -eq 0 ]]; then
    fail "the reference pack ships no textures"
fi
for relative in "${PACK_TEXTURES[@]}"; do
    mkdir -p "$EXPECTED_DIR/$(dirname "$relative")" || fail "cannot create expected asset dir"
    cp "$PACK_DIR/$relative" "$EXPECTED_DIR/$relative" || fail "cannot snapshot $relative"
done

cd "$TEMP_DIR" || fail "cannot enter working directory $TEMP_DIR"

# --- Digest leg -----------------------------------------------------------
set +e
timeout 30 "$BIN" --demo-worldgen >"$DIGEST_OUT" 2>&1
DIGEST_RC=$?
set -e

if [[ $DIGEST_RC -ne 0 ]]; then
    echo "WORLDGEN SMOKE FAIL: digest leg exit code $DIGEST_RC (expected 0 from the digest's own latch; 124 means an external timeout killed it)"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

if grep -q "panicked" "$DIGEST_OUT" || grep -q "RUST_BACKTRACE" "$DIGEST_OUT"; then
    echo "WORLDGEN SMOKE FAIL: panic detected in the digest leg output"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

mapfile -t OBSERVED_LINES <"$DIGEST_OUT"
if [[ ${#OBSERVED_LINES[@]} -ne ${#EXPECTED_LINES[@]} ]]; then
    echo "WORLDGEN SMOKE FAIL: digest printed ${#OBSERVED_LINES[@]} line(s) (expected exactly ${#EXPECTED_LINES[@]})"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

for i in "${!EXPECTED_LINES[@]}"; do
    if [[ -z "${OBSERVED_LINES[$i]}" ]]; then
        fail "digest line $i is empty (expected '${EXPECTED_LINES[$i]}')"
    fi
    # Exact whole-line equality; never a substring match.
    if [[ "${OBSERVED_LINES[$i]}" != "${EXPECTED_LINES[$i]}" ]]; then
        echo "WORLDGEN SMOKE FAIL: digest line $i drifted from the pinned contract"
        echo "WORLDGEN SMOKE EXPECTED: ${EXPECTED_LINES[$i]}"
        echo "WORLDGEN SMOKE OBSERVED: ${OBSERVED_LINES[$i]}"
        exit 1
    fi
done

# --- Authored-files read-only check after the digest ----------------------
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "settings.toml was rewritten during the digest leg"
cmp -s "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "pack.toml was rewritten during the digest leg"
for relative in "${PACK_TEXTURES[@]}"; do
    cmp -s "$PACK_DIR/$relative" "$EXPECTED_DIR/$relative" || fail "$relative was rewritten during the digest leg"
done

# --- Bounded launch leg A: reference pack present -------------------------
set +e
timeout 10 "$BIN" --save fresh.mcvx >"$LAUNCH_A_OUT" 2>&1
LAUNCH_A_RC=$?
set -e

if [[ $LAUNCH_A_RC -ne 124 ]]; then
    echo "WORLDGEN SMOKE FAIL: reference-pack launch exit code $LAUNCH_A_RC (expected 124 from the timeout kill; a clean exit or panic means the event loop did not survive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_A_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_A_OUT"; then
    echo "WORLDGEN SMOKE FAIL: panic detected in the reference-pack launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi

cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "settings.toml was rewritten during the authored launch leg"
cmp -s "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "pack.toml was rewritten during the authored launch leg"
for relative in "${PACK_TEXTURES[@]}"; do
    cmp -s "$PACK_DIR/$relative" "$EXPECTED_DIR/$relative" || fail "$relative was rewritten during the authored launch leg"
done

# --- Bounded launch leg B: pack removed, stack cleared --------------------
mkdir -p "$TEMP_DIR/packs-off" || fail "cannot create packs-off/"
mv "$PACK_DIR" "$TEMP_DIR/packs-off/mymod-ruby" || fail "cannot move the pack folder out of packs/"
printf '[packs]\nstack = []\n' >"$TEMP_DIR/settings.toml"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "cannot snapshot the cleared settings.toml"

set +e
timeout 10 "$BIN" --save fresh.mcvx >"$LAUNCH_B_OUT" 2>&1
LAUNCH_B_RC=$?
set -e

if [[ $LAUNCH_B_RC -ne 124 ]]; then
    echo "WORLDGEN SMOKE FAIL: pack-removed launch exit code $LAUNCH_B_RC (expected 124 from the timeout kill; the empty-rule default must keep the event loop alive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_B_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_B_OUT"; then
    echo "WORLDGEN SMOKE FAIL: panic detected in the pack-removed launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "the cleared settings.toml was rewritten during the pack-removed leg"

echo "WORLDGEN SMOKE PASS [profile=$PROFILE]: --demo-worldgen resolved the reference pack's 2 authored rules (1 ore, 1 patch), generated mymod:ruby_ore in 2476 underground cells strictly inside y 4..40, placed 334 patch cells only in plains/forest, kept coal=4248 and iron=3875 identical to an empty-rule regeneration, and reproduced pre-M010 chunks byte-for-byte with no rules; the real binary survived 10 s launches with the reference pack and with the pack removed (exit 124, no panic), and packs/ plus settings.toml stayed byte-identical while present"
for line in "${EXPECTED_LINES[@]}"; do
    echo "WORLDGEN SMOKE EVIDENCE: $line"
done
echo "WORLDGEN SMOKE EVIDENCE: authored launch exit=124 missing-pack launch exit=124 panic=none pack+settings byte-identical while present"
exit 0
