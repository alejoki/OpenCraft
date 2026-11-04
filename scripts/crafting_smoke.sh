#!/usr/bin/env bash
# Real-binary crafting/mining/eating digest + smoke for the reference pack
# (M010/S05-T06).
#
# Proves the S05 surface end to end through the production binary, with no test
# harness and no screenshot automation:
#
#   Digest leg: `"$BIN" --demo-crafting --save fixture.mcvx` (with the
#   checked-in packs/mymod-ruby copied into the temp cwd and selected in
#   settings.toml)
#     (a) exit code 0 — the digest's own `crafting: ok` latch. A timeout kill
#         or panic is an explicit FAILURE.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) exactly the pinned non-empty lines, compared WHOLE-LINE (never
#         substrings), in order. The lines pin: the 4x4 table pickaxe craft
#         through `apply_inventory_click_button_with` (authored 900
#         durability), the 2x2 stew craft (authored stack 16), ruby vs iron
#         break ticks and one-point wear on the production break path, the
#         authored 7-hunger / 0.8 s hold-to-eat flow, the disabled shortbow
#         (no preview, no craft), an incomplete grid that matches nothing and
#         consumes nothing, the v8 pack-tool round-trip (850 remaining), and
#         the pack-removed placeholder (count 1, one unresolved name).
#     (d) the fixture save starts with `MCVX` and the little-endian version
#         word 8.
#
#   Bounded launch leg A: `timeout 10 "$BIN" --save fixture.mcvx` with the
#   reference pack selected must exit 124 with no panic — the real renderer
#   and event loop open the world saved with a pack tool.
#
#   Bounded launch leg B: after moving the pack folder out of packs/ and
#   clearing the stack, the same fixture must still launch (exit 124, no
#   panic) — the placeholder policy keeps the world loadable.
#
#   Bounded launch leg C: a junk `broken` pack selected instead must still
#   launch (exit 124, no panic) — S02/S08 per-entry fallback keeps a broken
#   pack from blocking startup.
#
#   Throughout: every copied pack file and the settings file must stay
#   byte-identical while the pack is present (the game is read-only on packs/
#   and settings.toml here).
#
# The pinned digest lines live in two places on purpose: here and in
# `src/main.rs::crafting_demo_pins_the_checked_in_reference_pack_line_set`.
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
        echo "CRAFTING SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
CRATE_PACK="$CRATE_ROOT/packs/mymod-ruby"
BUILD_LOG="$(mktemp /tmp/opencraft-crafting-build.XXXXXX.log)" || {
    echo "CRAFTING SMOKE FAIL: could not create a build log" >&2
    exit 1
}

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "CRAFTING SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    rm -f "$BUILD_LOG"
    exit 1
fi
rm -f "$BUILD_LOG"

if [[ ! -x "$BIN" ]]; then
    echo "CRAFTING SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

if [[ ! -f "$CRATE_PACK/pack.toml" ]]; then
    echo "CRAFTING SMOKE FAIL: checked-in reference pack not found at $CRATE_PACK"
    exit 1
fi

for tool in timeout cmp mktemp od tr cp mv; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "CRAFTING SMOKE FAIL: required tool '$tool' not found on PATH"
        exit 1
    }
done

TEMP_DIR="$(mktemp -d /tmp/opencraft-crafting-smoke.XXXXXX)" || {
    echo "CRAFTING SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "CRAFTING SMOKE FAIL: $*" >&2
    exit 1
}

# The contractual digest lines, in their required order. Keep byte-for-byte
# identical to src/main.rs::crafting_demo_pins_the_checked_in_reference_pack_line_set.
EXPECTED_LINES=(
    "crafting: leg=table item=mymod:ruby_pickaxe count=1 durability=900 max=900"
    "crafting: leg=player item=mymod:ruby_stew count=1 stack=16"
    "crafting: leg=mine block=mymod:ruby_ore ruby_ticks=7 iron_ticks=10 ruby_wear=899 iron_wear=249"
    "crafting: leg=eat item=mymod:ruby_stew restored=7 hunger=12"
    "crafting: leg=shortbow preview=false craft=false"
    "crafting: leg=missing preview=false craft=false consumed=0"
    "crafting: leg=save durability=850 unresolved=0 resolved=true"
    "crafting: leg=placeholder name=mymod:ruby_pickaxe unresolved=1 count=1 placeholder=true"
    "crafting: ok"
)

mkdir -p "$TEMP_DIR/packs" || fail "cannot create packs/ in the temp dir"
cp -R "$CRATE_PACK" "$TEMP_DIR/packs/mymod-ruby" || fail "cannot copy the checked-in reference pack"
PACK_DIR="$TEMP_DIR/packs/mymod-ruby"
FIXTURE="$TEMP_DIR/fixture.mcvx"
DIGEST_OUT="$TEMP_DIR/digest.log"
LAUNCH_A_OUT="$TEMP_DIR/launch-a.log"
LAUNCH_B_OUT="$TEMP_DIR/launch-b.log"
LAUNCH_C_OUT="$TEMP_DIR/launch-c.log"

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
timeout 30 "$BIN" --demo-crafting --save fixture.mcvx >"$DIGEST_OUT" 2>&1
DIGEST_RC=$?
set -e

if [[ $DIGEST_RC -ne 0 ]]; then
    echo "CRAFTING SMOKE FAIL: digest leg exit code $DIGEST_RC (expected 0 from the digest's own latch; 124 means an external timeout killed it)"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

if grep -q "panicked" "$DIGEST_OUT" || grep -q "RUST_BACKTRACE" "$DIGEST_OUT"; then
    echo "CRAFTING SMOKE FAIL: panic detected in the digest leg output"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

mapfile -t OBSERVED_LINES <"$DIGEST_OUT"
if [[ ${#OBSERVED_LINES[@]} -ne ${#EXPECTED_LINES[@]} ]]; then
    echo "CRAFTING SMOKE FAIL: digest printed ${#OBSERVED_LINES[@]} line(s) (expected exactly ${#EXPECTED_LINES[@]})"
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
        echo "CRAFTING SMOKE FAIL: digest line $i drifted from the pinned contract"
        echo "CRAFTING SMOKE EXPECTED: ${EXPECTED_LINES[$i]}"
        echo "CRAFTING SMOKE OBSERVED: ${OBSERVED_LINES[$i]}"
        exit 1
    fi
done

[[ -f "$FIXTURE" ]] || fail "the digest did not write the --save fixture at $FIXTURE"
MAGIC_HEX="$(od -An -tx1 -N4 "$FIXTURE" | tr -d ' \n')"
VERSION_HEX="$(od -An -tx1 -j4 -N4 "$FIXTURE" | tr -d ' \n')"
[[ "$MAGIC_HEX" == "4d435658" ]] || fail "fixture magic is '$MAGIC_HEX' (expected 4d435658 = MCVX)"
[[ "$VERSION_HEX" == "08000000" ]] || fail "fixture version word is '$VERSION_HEX' (expected 08000000 = little-endian 8)"

# --- Bounded launch leg A: reference pack present -------------------------
set +e
timeout 10 "$BIN" --save fixture.mcvx >"$LAUNCH_A_OUT" 2>&1
LAUNCH_A_RC=$?
set -e

if [[ $LAUNCH_A_RC -ne 124 ]]; then
    echo "CRAFTING SMOKE FAIL: reference-pack launch exit code $LAUNCH_A_RC (expected 124 from the timeout kill; a clean exit or panic means the event loop did not survive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_A_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_A_OUT"; then
    echo "CRAFTING SMOKE FAIL: panic detected in the reference-pack launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi

cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "settings.toml was rewritten during the digest/authored launch legs"
cmp -s "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "pack.toml was rewritten during the digest/authored launch legs"
for relative in "${PACK_TEXTURES[@]}"; do
    cmp -s "$PACK_DIR/$relative" "$EXPECTED_DIR/$relative" || fail "$relative was rewritten during the digest/authored launch legs"
done

# --- Bounded launch leg B: pack removed, stack cleared (placeholder) ------
mkdir -p "$TEMP_DIR/packs-off" || fail "cannot create packs-off/"
mv "$PACK_DIR" "$TEMP_DIR/packs-off/mymod-ruby" || fail "cannot move the pack folder out of packs/"
printf '[packs]\nstack = []\n' >"$TEMP_DIR/settings.toml"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "cannot snapshot the cleared settings.toml"

set +e
timeout 10 "$BIN" --save fixture.mcvx >"$LAUNCH_B_OUT" 2>&1
LAUNCH_B_RC=$?
set -e

if [[ $LAUNCH_B_RC -ne 124 ]]; then
    echo "CRAFTING SMOKE FAIL: pack-removed launch exit code $LAUNCH_B_RC (expected 124 from the timeout kill)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_B_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_B_OUT"; then
    echo "CRAFTING SMOKE FAIL: panic detected in the pack-removed launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "the cleared settings.toml was rewritten during the placeholder leg"

# --- Bounded launch leg C: broken pack selected ---------------------------
mkdir -p "$TEMP_DIR/packs/broken" || fail "cannot create packs/broken"
printf 'this is not a valid toml manifest [[\n' >"$TEMP_DIR/packs/broken/pack.toml"
printf '[packs]\nstack = ["broken"]\n' >"$TEMP_DIR/settings.toml"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-broken.toml" || fail "cannot snapshot the broken settings.toml"

set +e
timeout 10 "$BIN" --save fixture.mcvx >"$LAUNCH_C_OUT" 2>&1
LAUNCH_C_RC=$?
set -e

if [[ $LAUNCH_C_RC -ne 124 ]]; then
    echo "CRAFTING SMOKE FAIL: broken-pack launch exit code $LAUNCH_C_RC (expected 124 from the timeout kill; per-entry fallback must keep startup alive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_C_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_C_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_C_OUT"; then
    echo "CRAFTING SMOKE FAIL: panic detected in the broken-pack launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_C_OUT"
    exit 1
fi
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-broken.toml" || fail "the broken settings.toml was rewritten during the broken-pack leg"

echo "CRAFTING SMOKE PASS [profile=$PROFILE]: --demo-crafting crafted mymod:ruby_pickaxe at the 4x4 table and mymod:ruby_stew at the 2x2 grid, broke mymod:ruby_ore in 7 ruby ticks vs 10 iron ticks with one-point wear (899/249), ate 7 hunger over the authored 0.8 s hold, proved the disabled shortbow and an incomplete grid never match, round-tripped a v8 pack tool at 850 durability, and degraded the same save to a count-1 placeholder with the pack removed; the real binary survived 10 s launches with the reference pack, no pack and a broken pack (exit 124, no panic)"
for line in "${EXPECTED_LINES[@]}"; do
    echo "CRAFTING SMOKE EVIDENCE: $line"
done
echo "CRAFTING SMOKE EVIDENCE: authored launch exit=124 missing-pack launch exit=124 broken-pack launch exit=124 panic=none pack+settings byte-identical while present"
exit 0
