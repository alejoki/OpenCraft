#!/usr/bin/env bash
# Real-binary UI polish smoke (M013/S07-T04).
#
# Proves the M013 UI surface end to end through the production binary, with no
# test harness and no screenshot automation:
#
#   Digest leg: `"$BIN" --demo-ui`
#     (a) exit code 0 — the digest's own `ok` latch. A timeout kill or panic is
#         an explicit FAILURE.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) exactly three non-empty lines, compared WHOLE-LINE (never
#         substrings) against EXPECTED_LINES, in order: 1280x720, 1920x1080,
#         3840x2160. Each line starts `ui_demo: ` and ends
#         `failed=none ok=true`, and pins icon counts, separated hotbar layout,
#         table art, tooltip flip/inside, selection fade, and vitals spacing.
#
#   Bounded launch leg: `timeout 10 "$BIN" --new --seed 4242 --save ui.mcvx`
#     (d) exit code 124 — `timeout` killed it, proving the production icon,
#         sprite, tooltip, selection, and vitals pipelines initialized and the
#         event loop survived. A non-124 exit (clean exit, panic 101) is a
#         FAILURE, not a pass.
#     (e) no panic text in the captured output.
#
# The three digest lines are pinned in two places on purpose: here and in
# `src/ui/demo.rs::digest_lines_are_pinned`. Update both halves together when
# the fixture or a count changes. If the binary drifts from the pinned test,
# fix the source/test first; never relax this script to match a broken binary.
#
# The script builds the debug binary itself with `cargo build --locked` and
# runs target/debug/opencraft. `SMOKE_PROFILE=release` is the opt-in
# real-binary release run. Everything runs inside a mktemp working directory,
# so the crate root's real saves/ and settings.toml are never touched.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch, so a typo can never silently
# exercise the wrong tree.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "UI SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
BUILD_LOG="$(mktemp /tmp/opencraft-ui-build.XXXXXX.log)" || {
    echo "UI SMOKE FAIL: could not create a build log" >&2
    exit 1
}

cleanup_build_log() { rm -f "$BUILD_LOG"; }
trap cleanup_build_log EXIT

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "UI SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "UI SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-ui-smoke.XXXXXX)" || {
    echo "UI SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() {
    rm -rf "$TEMP_DIR"
    rm -f "$BUILD_LOG"
}
trap cleanup EXIT

# The three contractual digest lines, in their required order. Keep byte-for-byte
# identical to src/ui/demo.rs::PINNED_LINES (pinned in two places on purpose).
EXPECTED_LINES=(
    "ui_demo: size=1280x720 player_block=144 player_sprite=54 table_block=234 table_sprite=54 hotbar_block=54 hotbar_sprite=36 empty=25 empty_table=12 quads_player=43 quads_table=60 sep_gap=16.000 sep_h=2.000 panel=676.000x384.000 table=23/24/9 authorable=25 table_distinct=1 tooltip_lines=2 tooltip_flip=1 tooltip_inside=1 select_alpha0=1.000 select_alpha2=0.000 select_restart=1 empty_hidden=1 vitals_icon=14.577 vitals_row=162.169 vitals_gap=1.822 vitals_overlap=0 air_line=1 count_clear=1 failed=none ok=true"
    "ui_demo: size=1920x1080 player_block=144 player_sprite=54 table_block=234 table_sprite=54 hotbar_block=54 hotbar_sprite=36 empty=25 empty_table=12 quads_player=43 quads_table=60 sep_gap=16.000 sep_h=2.000 panel=676.000x384.000 table=23/24/9 authorable=25 table_distinct=1 tooltip_lines=2 tooltip_flip=1 tooltip_inside=1 select_alpha0=1.000 select_alpha2=0.000 select_restart=1 empty_hidden=1 vitals_icon=21.865 vitals_row=243.253 vitals_gap=2.733 vitals_overlap=0 air_line=1 count_clear=1 failed=none ok=true"
    "ui_demo: size=3840x2160 player_block=144 player_sprite=54 table_block=234 table_sprite=54 hotbar_block=54 hotbar_sprite=36 empty=25 empty_table=12 quads_player=43 quads_table=60 sep_gap=16.000 sep_h=2.000 panel=676.000x384.000 table=23/24/9 authorable=25 table_distinct=1 tooltip_lines=2 tooltip_flip=1 tooltip_inside=1 select_alpha0=1.000 select_alpha2=0.000 select_restart=1 empty_hidden=1 vitals_icon=43.731 vitals_row=486.507 vitals_gap=5.466 vitals_overlap=0 air_line=1 count_clear=1 failed=none ok=true"
)

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/ui.mcvx"
DIGEST_OUT="$TEMP_DIR/digest.log"
LAUNCH_OUT="$TEMP_DIR/launch.log"

# --- Digest leg -----------------------------------------------------------
set +e
timeout 30 "$BIN" --demo-ui >"$DIGEST_OUT" 2>&1
DIGEST_RC=$?
set -e

if [[ $DIGEST_RC -ne 0 ]]; then
    echo "UI SMOKE FAIL: digest leg exit code $DIGEST_RC (expected 0 from the digest's own latch; 124 means an external timeout killed it)"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

if grep -q "panicked" "$DIGEST_OUT" || grep -q "RUST_BACKTRACE" "$DIGEST_OUT"; then
    echo "UI SMOKE FAIL: panic detected in the digest leg output"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

mapfile -t OBSERVED_LINES <"$DIGEST_OUT"
if [[ ${#OBSERVED_LINES[@]} -ne ${#EXPECTED_LINES[@]} ]]; then
    echo "UI SMOKE FAIL: digest printed ${#OBSERVED_LINES[@]} line(s) (expected exactly ${#EXPECTED_LINES[@]})"
    echo "--- captured digest output ---"
    cat "$DIGEST_OUT"
    exit 1
fi

for i in "${!EXPECTED_LINES[@]}"; do
    if [[ -z "${OBSERVED_LINES[$i]}" ]]; then
        echo "UI SMOKE FAIL: digest line $i is empty (expected '${EXPECTED_LINES[$i]}')"
        exit 1
    fi
    # Exact whole-line equality; never a substring match.
    if [[ "${OBSERVED_LINES[$i]}" != "${EXPECTED_LINES[$i]}" ]]; then
        echo "UI SMOKE FAIL: digest line $i drifted from the pinned contract"
        echo "UI SMOKE EXPECTED: ${EXPECTED_LINES[$i]}"
        echo "UI SMOKE OBSERVED: ${OBSERVED_LINES[$i]}"
        exit 1
    fi
done

# --- Bounded launch leg ---------------------------------------------------
set +e
timeout 10 "$BIN" --new --seed 4242 --save "$SAVE" >"$LAUNCH_OUT" 2>&1
LAUNCH_RC=$?
set -e

if [[ $LAUNCH_RC -ne 124 ]]; then
    echo "UI SMOKE FAIL: bounded launch exit code $LAUNCH_RC (expected 124 from the timeout kill; a clean exit or panic means the event loop did not survive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_OUT"
    exit 1
fi

if grep -q "panicked" "$LAUNCH_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_OUT"; then
    echo "UI SMOKE FAIL: panic detected in the bounded launch output"
    echo "--- captured launch output ---"
    cat "$LAUNCH_OUT"
    exit 1
fi

echo "UI SMOKE PASS [profile=$PROFILE]: --demo-ui printed all three pinned UI digests (720p, 1080p, 4K) byte-for-byte and the production event loop survived a 10s world launch, exit 124, no panic"
for line in "${EXPECTED_LINES[@]}"; do
    echo "UI SMOKE EVIDENCE: $line"
done
echo "UI SMOKE EVIDENCE: launch exit=124 (timeout kill) panic=none"
