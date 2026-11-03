#!/usr/bin/env bash
# Real-binary Creative palette digest + smoke for the reference pack
# (M010/S07-T05).
#
# Proves the S07 searchable Creative palette end to end through the production
# binary, with no test harness and no screenshot automation:
#
#   Digest leg: `"$BIN" --demo-palette` (with the checked-in packs/mymod-ruby
#   copied into the temp cwd and selected in settings.toml)
#     (a) exit code 0 — the digest's own `palette: ok` latch. A timeout kill
#         or panic is an explicit FAILURE.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) exactly the pinned non-empty lines, compared WHOLE-LINE (never
#         substrings), in order. The lines pin: the 46-entry stock split
#         across built-in/pack blocks, items and tools; the 1280x720 layout
#         (panel, first and 54th slot, quad and label counts); the `rub`,
#         `mymod` and `builtin` filter results in sorted-display-name order;
#         five authored full-stack grants through the production hotbar grant
#         path (64 ore, 64 ruby, 16 stew, 900-durability pickaxe and sword);
#         the authored 7-hunger stew use; the two appended save placeholders
#         that never enter the stock; and the Creative-only key.
#
#   Bounded launch leg A: `timeout 10 "$BIN"` with the reference pack selected
#   must exit 124 with no panic — the real renderer and event loop start with
#   the pack loaded.
#
#   Bounded launch leg B: after moving the pack folder out of packs/ and
#   clearing the stack, the bare launch must still exit 124 with no panic —
#   the palette stock degrades without blocking startup.
#
#   Throughout: every copied pack file and the settings file must stay
#   byte-identical while the pack is present (the game is read-only on packs/
#   and settings.toml here), and the cleared settings must stay byte-identical
#   during leg B.
#
# The pinned digest lines live in two places on purpose: here and in
# `src/main.rs::palette_demo_pins_the_checked_in_reference_pack_line_set`.
# Update both halves together when the reference pack or palette model
# changes. If the binary drifts from the pinned test, fix the source/test
# first; never relax this script to match a broken binary. This smoke becomes
# part of the S08 14-smoke matrix; do not edit any other script from here.
#
# Dependencies: bash, timeout, cmp, mktemp, cp, mv.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "PALETTE SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
BUILD_LOG="$(mktemp /tmp/opencraft-palette-build.XXXXXX.log)" || {
    echo "PALETTE SMOKE FAIL: could not create a build log" >&2
    exit 1
}

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "PALETTE SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    rm -f "$BUILD_LOG"
    exit 1
fi
rm -f "$BUILD_LOG"

if [[ ! -x "$BIN" ]]; then
    echo "PALETTE SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

if [[ ! -f "$CRATE_PACK/pack.toml" ]]; then
    echo "PALETTE SMOKE FAIL: checked-in reference pack not found at $CRATE_PACK"
    exit 1
fi

for tool in timeout cmp mktemp cp mv; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "PALETTE SMOKE FAIL: required tool '$tool' not found on PATH"
        exit 1
    }
done

TEMP_DIR="$(mktemp -d /tmp/opencraft-palette-smoke.XXXXXX)" || {
    echo "PALETTE SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "PALETTE SMOKE FAIL: $*" >&2
    exit 1
}

# The contractual digest lines, in their required order. Keep byte-for-byte
# identical to src/main.rs::palette_demo_pins_the_checked_in_reference_pack_line_set.
EXPECTED_LINES=(
    "palette: leg=entries builtin_blocks=14 builtin_items=11 builtin_tools=16 pack_blocks=1 pack_items=2 pack_tools=2 total=46"
    "palette: leg=layout surface=1280x720 panel=428,158,424,404 slot0=444,242,40,40 slot53=796,462,40,40 quads=57 labels=4"
    "palette: leg=filter query=rub matched=5 names=mymod:ruby,mymod:ruby_ore,mymod:ruby_pickaxe,mymod:ruby_stew,mymod:ruby_sword"
    "palette: leg=filter query=mymod matched=5"
    "palette: leg=filter query=builtin matched=41"
    "palette: leg=grant slot=0 item=mymod:ruby_ore count=64 tool=false placeable=true"
    "palette: leg=grant slot=1 item=mymod:ruby count=64 tool=false placeable=false"
    "palette: leg=grant slot=2 item=mymod:ruby_stew count=16 tool=false placeable=false"
    "palette: leg=grant slot=3 item=mymod:ruby_pickaxe count=1 tool=true durability=900"
    "palette: leg=grant slot=4 item=mymod:ruby_sword count=1 tool=true durability=900"
    "palette: leg=use item=mymod:ruby_stew restored=7 hunger=19"
    "palette: leg=placeholder excluded=2 total=46"
    "palette: leg=key key=C creative=true survival=false"
    "palette: ok"
)

mkdir -p "$TEMP_DIR/packs" || fail "cannot create packs/ in the temp dir"
cp -R "$CRATE_PACK" "$TEMP_DIR/packs/mymod-ruby" || fail "cannot copy the checked-in reference pack"
PACK_DIR="$TEMP_DIR/packs/mymod-ruby"
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
timeout 30 "$BIN" --demo-palette >"$DIGEST_OUT" 2>&1
DIGEST_RC=$?
set -e

if [[ $DIGEST_RC -ne 0 ]]; then
    echo "PALETTE SMOKE FAIL: digest leg exit code $DIGEST_RC (expected 0 from the digest's own latch; 124 means an external timeout killed it)"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

if grep -q "panicked" "$DIGEST_OUT" || grep -q "RUST_BACKTRACE" "$DIGEST_OUT"; then
    echo "PALETTE SMOKE FAIL: panic detected in the digest leg output"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

# Compare every non-empty line in order. Empty lines are ignored by the
# pinned contract, but the non-empty count must match exactly.
mapfile -t OBSERVED_LINES < <(grep -v '^[[:space:]]*$' "$DIGEST_OUT")
if [[ ${#OBSERVED_LINES[@]} -ne ${#EXPECTED_LINES[@]} ]]; then
    echo "PALETTE SMOKE FAIL: digest printed ${#OBSERVED_LINES[@]} non-empty line(s) (expected exactly ${#EXPECTED_LINES[@]})"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

for i in "${!EXPECTED_LINES[@]}"; do
    # Exact whole-line equality; never a substring match.
    if [[ "${OBSERVED_LINES[$i]}" != "${EXPECTED_LINES[$i]}" ]]; then
        echo "PALETTE SMOKE FAIL: digest line $i drifted from the pinned contract"
        echo "PALETTE SMOKE EXPECTED: ${EXPECTED_LINES[$i]}"
        echo "PALETTE SMOKE OBSERVED: ${OBSERVED_LINES[$i]}"
        exit 1
    fi
done

# --- Bounded launch leg A: reference pack present -------------------------
set +e
timeout 10 "$BIN" >"$LAUNCH_A_OUT" 2>&1
LAUNCH_A_RC=$?
set -e

if [[ $LAUNCH_A_RC -ne 124 ]]; then
    echo "PALETTE SMOKE FAIL: reference-pack launch exit code $LAUNCH_A_RC (expected 124 from the timeout kill; a clean exit or panic means the event loop did not survive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_A_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_A_OUT"; then
    echo "PALETTE SMOKE FAIL: panic detected in the reference-pack launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi

cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "settings.toml was rewritten during the digest/authored launch legs"
cmp -s "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "pack.toml was rewritten during the digest/authored launch legs"
for relative in "${PACK_TEXTURES[@]}"; do
    cmp -s "$PACK_DIR/$relative" "$EXPECTED_DIR/$relative" || fail "$relative was rewritten during the digest/authored launch legs"
done

# --- Bounded launch leg B: pack removed, stack cleared --------------------
mkdir -p "$TEMP_DIR/packs-off" || fail "cannot create packs-off/"
mv "$PACK_DIR" "$TEMP_DIR/packs-off/mymod-ruby" || fail "cannot move the pack folder out of packs/"
printf '[packs]\nstack = []\n' >"$TEMP_DIR/settings.toml"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "cannot snapshot the cleared settings.toml"

set +e
timeout 10 "$BIN" >"$LAUNCH_B_OUT" 2>&1
LAUNCH_B_RC=$?
set -e

if [[ $LAUNCH_B_RC -ne 124 ]]; then
    echo "PALETTE SMOKE FAIL: pack-removed launch exit code $LAUNCH_B_RC (expected 124 from the timeout kill)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_B_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_B_OUT"; then
    echo "PALETTE SMOKE FAIL: panic detected in the pack-removed launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "the cleared settings.toml was rewritten during the placeholder leg"

echo "PALETTE SMOKE PASS [profile=$PROFILE]: --demo-palette listed 46 non-placeholder built-in and pack entries, pinned the 1280x720 panel/slot/quads/labels geometry, filtered rub/mymod/builtin in sorted-display-name order, granted five authored full stacks (64 ore, 64 ruby, 16 stew, 900-durability pickaxe and sword), used the authored 7-hunger stew, excluded two appended save placeholders, and kept the palette Creative-only; the real binary survived 10 s launches with the reference pack and with the pack removed (exit 124, no panic) while settings and pack bytes stayed byte-identical"
for line in "${EXPECTED_LINES[@]}"; do
    echo "PALETTE SMOKE EVIDENCE: $line"
done
echo "PALETTE SMOKE EVIDENCE: authored launch exit=124 removed-pack launch exit=124 panic=none pack+settings byte-identical while present cleared-settings byte-identical"
exit 0
