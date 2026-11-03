#!/usr/bin/env bash
# Real-binary presentation digest + smoke for modded content (M010/S04-T05).
#
# Proves the M010/S04 presentation surface end to end through the production
# binary, with no test harness and no screenshot automation:
#
#   Digest leg: `"$BIN" --demo-presentation --save fixture.mcvx` (with a v2
#   pack whose stack is selected in settings.toml)
#     (a) exit code 0 — the digest's own `ok` latch. A timeout kill or panic is
#         an explicit FAILURE.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) exactly the pinned non-empty lines, compared WHOLE-LINE (never
#         substrings), in order. The lines pin the frozen registry counts, the
#         256-layer block array and item atlas (built-in and pack layers
#         authored-match), the authored block/item definitions, a production
#         mesh of the two authored blocks, a production mining break of ruby
#         ore (61 ticks, authored drop, count, slot, limit), the v8 fixture
#         reload with zero unresolved names, and the reserved placeholder
#         policy (face 25, icon 27, unbreakable, mesh layer 25).
#     (d) the fixture save starts with `MCVX` and the little-endian version
#         word 8.
#
#   Bounded launch leg A: `timeout 10 "$BIN" --save fixture.mcvx` (pack still
#   selected) must exit 124 with no panic — the real renderer and event loop
#   open the authored world (authored faces / item icon / stack limit).
#
#   Bounded launch leg B: after moving the pack folder out of packs/ and
#   clearing the stack, the same fixture must still launch (exit 124, no
#   panic) — the renderer opens the placeholder world with the reserved
#   checker faces and the built-in defaults.
#
#   Throughout: every pack file and the settings file must stay byte-identical
#   (the game is read-only on packs/ and settings.toml here).
#
# The pinned digest lines live in two places on purpose: here and in
# `src/main.rs::presentation_demo_pins_the_authored_line_set`. Update both
# halves together when the fixture or a count changes. If the binary drifts
# from the pinned test, fix the source/test first; never relax this script to
# match a broken binary.
#
# The script builds the requested profile itself and runs its binary.
# `SMOKE_PROFILE=release` is the opt-in real-binary release run. Everything
# runs inside a mktemp working directory, so the crate root's real saves/,
# settings.toml, and packs/ are never touched.
#
# Dependencies: bash, timeout, cmp, mktemp, od, tr, base64, cargo.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "PRESENTATION SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/$PROFILE/opencraft"
BUILD_LOG="$(mktemp /tmp/opencraft-presentation-build.XXXXXX.log)" || {
    echo "PRESENTATION SMOKE FAIL: could not create a build log" >&2
    exit 1
}

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "PRESENTATION SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    rm -f "$BUILD_LOG"
    exit 1
fi
rm -f "$BUILD_LOG"

if [[ ! -x "$BIN" ]]; then
    echo "PRESENTATION SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

for tool in timeout cmp mktemp od tr base64; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "PRESENTATION SMOKE FAIL: required tool '$tool' not found on PATH"
        exit 1
    }
done

TEMP_DIR="$(mktemp -d /tmp/opencraft-presentation-smoke.XXXXXX)" || {
    echo "PRESENTATION SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "PRESENTATION SMOKE FAIL: $*" >&2
    exit 1
}

# The contractual digest lines, in their required order. Keep byte-for-byte
# identical to src/main.rs::presentation_demo_pins_the_authored_line_set.
EXPECTED_LINES=(
    "presentation_demo: leg=registry blocks=25 items=1 pack_layers=2 pack_icons=1"
    "presentation_demo: leg=block_array layers=256 builtin_match=true pack_match=true"
    "presentation_demo: leg=item_atlas layers=256 builtin_match=true pack_match=true"
    "presentation_demo: leg=block name=mymod:ruby_ore id=24 faces=49,49,49,49,49,49 hardness=3.000 drop=mymod:ruby stack=64"
    "presentation_demo: leg=block name=mymod:ruby_block id=23 faces=48,48,48,48,48,48 hardness=4.000 drop=mymod:ruby_block stack=64"
    "presentation_demo: leg=item name=mymod:ruby id=0 icon=32 stack=64"
    "presentation_demo: leg=mesh blocks=2 quads=12 layers=48,49"
    "presentation_demo: leg=mine target=mymod:ruby_ore ticks=61 broken=true applied=true drop=mymod:ruby count=1 slot=0 limit=64"
    "presentation_demo: leg=save unresolved=0 ruby_ore=true ruby_block=true ruby_item=true"
    "presentation_demo: leg=placeholder unresolved=3 face=25 icon=27 break=none mesh=25"
    "presentation_demo: ok=true"
)

# Three distinct valid 16x16 RGBA PNGs (solid colours) decoded from embedded
# base64 blobs, so no PNG encoder is required.
PNG_RUBY_ORE="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGM4wSXynxLMMGrAqAGjBgwXAwAOR+UQCzENtgAAAABJRU5ErkJggg=="
PNG_RUBY_BLOCK="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGMQOSH3nxLMMGrAqAGjBgwXAwD8kPkQN9iRQgAAAABJRU5ErkJggg=="
PNG_RUBY="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGOQ07jznxLMMGrAqAGjBgwXAwChTyEfFJbR5wAAAABJRU5ErkJggg=="

write_valid_png() {
    local path="$1"
    local payload="$2"
    printf '%s' "$payload" | base64 -d >"$path" || return 1
}

mkdir -p "$TEMP_DIR/packs/mymod-ruby" || fail "cannot create packs/mymod-ruby"
PACK_DIR="$TEMP_DIR/packs/mymod-ruby"
FIXTURE="$TEMP_DIR/fixture.mcvx"
DIGEST_OUT="$TEMP_DIR/digest.log"
LAUNCH_A_OUT="$TEMP_DIR/launch-a.log"
LAUNCH_B_OUT="$TEMP_DIR/launch-b.log"

cat >"$PACK_DIR/pack.toml" <<'EOF'
format_version = 2
name = "Ruby Mod"
version = "1.0.0"

[[blocks]]
id = "mymod:ruby_ore"
name = "Ruby Ore"
textures = { all = "ruby_ore.png" }
hardness = 3.0
drop = "mymod:ruby"
tool = "pickaxe"
stack = 64

[[blocks]]
id = "mymod:ruby_block"
name = "Ruby Block"
textures = { all = "ruby_block.png" }
hardness = 4.0
stack = 64

[[items]]
id = "mymod:ruby"
icon = "ruby.png"
stack = 64
EOF

write_valid_png "$PACK_DIR/ruby_ore.png" "$PNG_RUBY_ORE" || fail "cannot seed ruby_ore.png"
write_valid_png "$PACK_DIR/ruby_block.png" "$PNG_RUBY_BLOCK" || fail "cannot seed ruby_block.png"
write_valid_png "$PACK_DIR/ruby.png" "$PNG_RUBY" || fail "cannot seed ruby.png"
cmp -s "$PACK_DIR/ruby_ore.png" "$PACK_DIR/ruby_block.png" && fail "fixture PNGs must be distinct"
cmp -s "$PACK_DIR/ruby_block.png" "$PACK_DIR/ruby.png" && fail "fixture PNGs must be distinct"

cat >"$TEMP_DIR/settings.toml" <<'EOF'
[packs]
stack = ["mymod-ruby"]
EOF

EXPECTED_DIR="$TEMP_DIR/expected"
mkdir -p "$EXPECTED_DIR" || fail "cannot create expected/ dir"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "cannot snapshot settings.toml"
cp "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "cannot snapshot pack.toml"
cp "$PACK_DIR/ruby_ore.png" "$EXPECTED_DIR/ruby_ore.png" || fail "cannot snapshot ruby_ore.png"
cp "$PACK_DIR/ruby_block.png" "$EXPECTED_DIR/ruby_block.png" || fail "cannot snapshot ruby_block.png"
cp "$PACK_DIR/ruby.png" "$EXPECTED_DIR/ruby.png" || fail "cannot snapshot ruby.png"

cd "$TEMP_DIR" || fail "cannot enter working directory $TEMP_DIR"

# --- Digest leg -----------------------------------------------------------
set +e
timeout 30 "$BIN" --demo-presentation --save fixture.mcvx >"$DIGEST_OUT" 2>&1
DIGEST_RC=$?
set -e

if [[ $DIGEST_RC -ne 0 ]]; then
    echo "PRESENTATION SMOKE FAIL: digest leg exit code $DIGEST_RC (expected 0 from the digest's own latch; 124 means an external timeout killed it)"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

if grep -q "panicked" "$DIGEST_OUT" || grep -q "RUST_BACKTRACE" "$DIGEST_OUT"; then
    echo "PRESENTATION SMOKE FAIL: panic detected in the digest leg output"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

mapfile -t OBSERVED_LINES <"$DIGEST_OUT"
if [[ ${#OBSERVED_LINES[@]} -ne ${#EXPECTED_LINES[@]} ]]; then
    echo "PRESENTATION SMOKE FAIL: digest printed ${#OBSERVED_LINES[@]} line(s) (expected exactly ${#EXPECTED_LINES[@]})"
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
        echo "PRESENTATION SMOKE FAIL: digest line $i drifted from the pinned contract"
        echo "PRESENTATION SMOKE EXPECTED: ${EXPECTED_LINES[$i]}"
        echo "PRESENTATION SMOKE OBSERVED: ${OBSERVED_LINES[$i]}"
        exit 1
    fi
done

[[ -f "$FIXTURE" ]] || fail "the digest did not write the --save fixture at $FIXTURE"
MAGIC_HEX="$(od -An -tx1 -N4 "$FIXTURE" | tr -d ' \n')"
VERSION_HEX="$(od -An -tx1 -j4 -N4 "$FIXTURE" | tr -d ' \n')"
[[ "$MAGIC_HEX" == "4d435658" ]] || fail "fixture magic is '$MAGIC_HEX' (expected 4d435658 = MCVX)"
[[ "$VERSION_HEX" == "08000000" ]] || fail "fixture version word is '$VERSION_HEX' (expected 08000000 = little-endian 8)"

# --- Bounded launch leg A: authored world, pack still selected ------------
set +e
timeout 10 "$BIN" --save fixture.mcvx >"$LAUNCH_A_OUT" 2>&1
LAUNCH_A_RC=$?
set -e

if [[ $LAUNCH_A_RC -ne 124 ]]; then
    echo "PRESENTATION SMOKE FAIL: authored launch exit code $LAUNCH_A_RC (expected 124 from the timeout kill; a clean exit or panic means the event loop did not survive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_A_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_A_OUT"; then
    echo "PRESENTATION SMOKE FAIL: panic detected in the authored launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_A_OUT"
    exit 1
fi

cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings.toml" || fail "settings.toml was rewritten during the authored digest/launch legs"
cmp -s "$PACK_DIR/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "pack.toml was rewritten during the authored legs"
cmp -s "$PACK_DIR/ruby_ore.png" "$EXPECTED_DIR/ruby_ore.png" || fail "ruby_ore.png was rewritten during the authored legs"
cmp -s "$PACK_DIR/ruby_block.png" "$EXPECTED_DIR/ruby_block.png" || fail "ruby_block.png was rewritten during the authored legs"
cmp -s "$PACK_DIR/ruby.png" "$EXPECTED_DIR/ruby.png" || fail "ruby.png was rewritten during the authored legs"

# --- Bounded launch leg B: placeholder world, pack removed + stack cleared -
mv "$PACK_DIR" "$TEMP_DIR/packs-off" || fail "cannot move the pack folder out of packs/"
printf '[packs]\nstack = []\n' >"$TEMP_DIR/settings.toml"
cp "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "cannot snapshot the cleared settings.toml"

set +e
timeout 10 "$BIN" --save fixture.mcvx >"$LAUNCH_B_OUT" 2>&1
LAUNCH_B_RC=$?
set -e

if [[ $LAUNCH_B_RC -ne 124 ]]; then
    echo "PRESENTATION SMOKE FAIL: placeholder launch exit code $LAUNCH_B_RC (expected 124 from the timeout kill; the same fixture must still open against built-in content)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_B_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_B_OUT"; then
    echo "PRESENTATION SMOKE FAIL: panic detected in the placeholder launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_B_OUT"
    exit 1
fi

cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED_DIR/settings-cleared.toml" || fail "the cleared settings.toml was rewritten during the placeholder leg"
cmp -s "$TEMP_DIR/packs-off/pack.toml" "$EXPECTED_DIR/pack.toml" || fail "pack.toml changed after being moved out of packs/"
cmp -s "$TEMP_DIR/packs-off/ruby_ore.png" "$EXPECTED_DIR/ruby_ore.png" || fail "ruby_ore.png changed after being moved out of packs/"
cmp -s "$TEMP_DIR/packs-off/ruby_block.png" "$EXPECTED_DIR/ruby_block.png" || fail "ruby_block.png changed after being moved out of packs/"
cmp -s "$TEMP_DIR/packs-off/ruby.png" "$EXPECTED_DIR/ruby.png" || fail "ruby.png changed after being moved out of packs/"

echo "PRESENTATION SMOKE PASS [profile=$PROFILE]: --demo-presentation printed all pinned lines byte-for-byte (authored faces 48/49 via a 256-layer array, 61-tick break yielding mymod:ruby), wrote an MCVX v8 fixture, and both the authored world and the placeholder world (pack removed, stack cleared) survived a 10s launch with exit 124 and no panic"
for line in "${EXPECTED_LINES[@]}"; do
    echo "PRESENTATION SMOKE EVIDENCE: $line"
done
echo "PRESENTATION SMOKE EVIDENCE: authored launch exit=124 placeholder launch exit=124 panic=none pack+settings byte-identical"
exit 0
