#!/usr/bin/env bash
# Real-binary scripted ocean-crossing + frame-budget smoke (M007/S08-T03).
#
# The milestone's two operational claims need one bounded release run of the
# real binary:
#   (a) the `--demo-swim` seam scripts the pinned S03 ocean crossing through
#       the production player update, lands on the far shore, prints its digest
#       and shuts itself down with status 0;
#   (b) the demo-shutdown `frame_budget:` digest reports p95 wall frame time at
#       or under one 60 Hz frame (16.6 ms).
#
#   Build: `cargo build --locked --release`
#     The frame budget is only honest in release; a debug build would measure
#     unoptimised work and fail the claim for the wrong reason.
#
#   Run: `--new --seed 12345 --demo-swim --save P` at the default time scale
#     (the swim deliberately does not accelerate: a seasonal rollover would
#     regenerate terrain mid-crossing).
#     (a) exit code 0 — the app's own swim latch. A `timeout` kill surfaces as
#         124 and is an explicit FAILURE here, never the expected outcome.
#     (b) no "panicked" / "RUST_BACKTRACE" text.
#     (c) stdout contains the `swim_demo:` digest line with `landed=true`
#         (the watcher walked entry -> float -> rise -> crossing -> landed).
#     (d) stdout contains the `frame_budget:` digest line, whose `p95=<n>ms`
#         field the script parses and asserts <= 16.6 ms.
#
# The PASS/FAIL contract never depends on a screenshot: the run is a real
# windowed process, but a display or capture failure must not change the
# verdict (water_smoke's guarded capture is deliberately not required here).
# Seed 12345 is the S03-observed route: ocean at (0, 0) h=14, first land near
# +Z 321 (Forest), so the scripted swim has a real crossing to make.
#
# Everything runs inside a mktemp working directory, so the crate root's real
# saves/ and settings.toml are never touched.

set -u

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

# Resolve the crate root and release binary absolutely before changing
# directory (the build runs from the crate root).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/release/opencraft"
BUILD_LOG="$(mktemp /tmp/opencraft-swim-build.XXXXXX.log)" || {
    echo "SWIM SMOKE FAIL: could not create a build log" >&2
    exit 1
}

cleanup_build_log() { rm -f "$BUILD_LOG"; }
trap cleanup_build_log EXIT

if ! (cd "$CRATE_ROOT" && cargo build --locked --release) >"$BUILD_LOG" 2>&1; then
    echo "SWIM SMOKE FAIL: cargo build --locked --release failed"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "SWIM SMOKE FAIL: $BIN not found after the release build"
    exit 1
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-swim-smoke.XXXXXX)" || {
    echo "SWIM SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() {
    rm -rf "$TEMP_DIR"
    rm -f "$BUILD_LOG"
}
trap cleanup EXIT

SEED=12345
# The app's own wall-clock swim deadline is 360 s; the outer timeout is only a
# backstop for a hang, and a kill here is an explicit failure.
OUTER_TIMEOUT=420
FRAME_P95_TARGET=16.6

cd "$TEMP_DIR"
SAVE="$TEMP_DIR/swim.mcvx"
OUT="$TEMP_DIR/run.log"

set +e
timeout "$OUTER_TIMEOUT" "$BIN" --new --seed "$SEED" --demo-swim \
    --save "$SAVE" >"$OUT" 2>&1
RC=$?
set -e

if [[ $RC -ne 0 ]]; then
    echo "SWIM SMOKE FAIL: exit code $RC (expected 0 from the swim demo's own landing latch; 124 means the swim never landed and an external timeout killed it)"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if grep -q "panicked" "$OUT" || grep -q "RUST_BACKTRACE" "$OUT"; then
    echo "SWIM SMOKE FAIL: panic detected in output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

SWIM_LINE=$(grep -m1 'swim_demo:' "$OUT" || true)
if [[ -z "$SWIM_LINE" ]]; then
    echo "SWIM SMOKE FAIL: no swim_demo: digest line in the output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

if [[ "$SWIM_LINE" != *"landed=true"* ]]; then
    echo "SWIM SMOKE FAIL: swim did not land — observed digest: $SWIM_LINE"
    exit 1
fi

FRAME_LINE=$(grep -m1 'frame_budget:' "$OUT" || true)
if [[ -z "$FRAME_LINE" ]]; then
    echo "SWIM SMOKE FAIL: no frame_budget: digest line in the output"
    echo "--- captured output ---"
    cat "$OUT"
    exit 1
fi

P95_TOKEN=$(grep -o 'p95=[0-9.]*ms' <<<"$FRAME_LINE" || true)
if [[ -z "$P95_TOKEN" ]]; then
    echo "SWIM SMOKE FAIL: frame_budget digest has no parseable p95 field — observed line: $FRAME_LINE"
    exit 1
fi
P95_MS=${P95_TOKEN#p95=}
P95_MS=${P95_MS%ms}

if ! awk -v p95="$P95_MS" -v target="$FRAME_P95_TARGET" \
    'BEGIN { exit !((p95 + 0) <= (target + 0)) }'; then
    echo "SWIM SMOKE FAIL: frame p95 is ${P95_MS}ms (expected <= ${FRAME_P95_TARGET}ms) — observed line: $FRAME_LINE"
    exit 1
fi

echo "SWIM SMOKE PASS: seed $SEED swam the pinned +Z crossing and landed with exit 0; release frame p95 ${P95_MS}ms <= ${FRAME_P95_TARGET}ms"
echo "SWIM SMOKE EVIDENCE: $SWIM_LINE"
echo "SWIM SMOKE EVIDENCE: $FRAME_LINE"
