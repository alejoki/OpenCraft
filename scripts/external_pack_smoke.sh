#!/usr/bin/env bash
# Standing external v3 pack smoke (M019/S05-T04).
#
# The differentiator of M019 is that a no-Rust author can reproduce the shipped
# surface from docs/TEXTURE-PACKS.md alone. This script is the standing
# executable gate for that claim: it drives the checked-in doc-authored fixture
# through the production binary with no window, no GPU and no player state.
#
# Scenarios (every scratch tree lives under mktemp; the crate root's
# settings.toml, saves/ and packs/ are never touched):
#   1. Validate: only `tests/fixtures/packs/external-v3` is copied into
#      `<tmp>/packs/external-v3`, then `--validate-pack external-v3` must exit 0
#      with exactly `validate-pack: external-v3: 11 entries, 0 errors,
#      0 warnings`, the per-entry `ok:` lines for the six-face block, the
#      cross/cutout plant, the crop and the blend block, and no `error:` or
#      `warning:` line.
#   2. Demo: `--demo-external-pack tests/fixtures/packs/external-v3` from the
#      repo root must print every proof leg (stack/sixface/model/crop/blend/
#      icon/save/reload), end `external_pack_demo: ok=true` and exit 0.
#   3. Atlas: `--demo-atlas` runs into two separate temp dirs; both must print
#      `atlas_demo: ok=true`, the pinned width/height/layers/bytes/digest/png
#      tokens, and write byte-identical `atlas.png` files (`cmp`).
#
# A failed assertion exits non-zero with a named `EXTERNAL PACK SMOKE FAIL:`
# line. A green run ends with one `EXTERNAL PACK SMOKE OK: ...` banner.
#
# Dependencies: bash, timeout, cmp, mktemp, cp, grep, wc.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"
FIXTURE="$CRATE_ROOT/tests/fixtures/packs/external-v3"

fail() {
    echo "EXTERNAL PACK SMOKE FAIL: $*" >&2
    exit 1
}

pass() {
    echo "PASS: $*"
}

# Pin the atlas artifact measured from the real writer. The composed atlas is
# built from the 48 built-in base layers only, so these tokens are independent
# of the local settings file and pack stack.
ATLAS_WIDTH=128
ATLAS_HEIGHT=96
ATLAS_LAYERS=48
ATLAS_BYTES=32465
ATLAS_RGBA_DIGEST="0xaa212bc3f53ce34c"
ATLAS_PNG_DIGEST="0x4a89e7b5ab337a8a"

[[ -d "$FIXTURE" ]] || fail "fixture directory not found: $FIXTURE"

if [[ ! -x "$BIN" ]]; then
    echo "external_pack_smoke: $BIN not built yet; running cargo build --locked" >&2
    (cd "$CRATE_ROOT" && cargo build --locked) \
        || fail "cargo build --locked failed"
fi
[[ -x "$BIN" ]] || fail "binary not found: $BIN (run cargo build --locked first)"

for tool in timeout cmp mktemp grep wc; do
    command -v "$tool" >/dev/null 2>&1 || fail "required tool '$tool' not found on PATH"
done

WORK="$(mktemp -d /tmp/opencraft-external-pack.XXXXXX)" || fail "mktemp failed"
cleanup() {
    find "$WORK" -depth -mindepth 1 -delete 2>/dev/null || true
    rmdir "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

# Headless launch helper: strip the display environment so a binary that tried
# to open a window would fail instead of silently passing.
run_headless() {
    local cwd="$1" out="$2"
    shift 2
    (cd "$cwd" && env -u DISPLAY -u WAYLAND_DISPLAY timeout 120 "$BIN" "$@" >"$out" 2>&1)
}

# ---------------------------------------------------------------------------
# Scenario 1: validate the external fixture from a tree that holds only it.
# ---------------------------------------------------------------------------
VAL_DIR="$WORK/validate"
mkdir -p "$VAL_DIR/packs" || fail "cannot create $VAL_DIR/packs"
cp -a "$FIXTURE" "$VAL_DIR/packs/external-v3" || fail "cannot copy the fixture"

rc=0
run_headless "$VAL_DIR" "$WORK/validate.out" --validate-pack external-v3 || rc=$?
[[ "$rc" -eq 0 ]] || {
    cat "$WORK/validate.out" >&2
    fail "--validate-pack external-v3 exited $rc (expected 0)"
}

grep -qx 'validate-pack: external-v3: 11 entries, 0 errors, 0 warnings' "$WORK/validate.out" \
    || { cat "$WORK/validate.out" >&2; fail "missing exact validate-pack summary line"; }
grep -q '^error:' "$WORK/validate.out" \
    && { cat "$WORK/validate.out" >&2; fail "validate-pack emitted an error: line"; }
grep -q '^warning:' "$WORK/validate.out" \
    && { cat "$WORK/validate.out" >&2; fail "validate-pack emitted a warning: line"; }

for expected in \
    'ok: block external:reed model=cross alpha=cutout' \
    'ok: block external:wheat model=crop alpha=cutout' \
    'ok: block external:frosted_glass model=cube alpha=blend' \
    'ok: builtin_icon apple' \
    'ok: face stone top' \
    'ok: face stone bottom' \
    'ok: face stone east' \
    'ok: face stone west' \
    'ok: face stone north' \
    'ok: face stone south' \
    'ok: face dirt top'; do
    grep -qxF "$expected" "$WORK/validate.out" \
        || { cat "$WORK/validate.out" >&2; fail "missing validate-pack entry: $expected"; }
done
pass "external-v3 validates with 11 entries, 0 errors, 0 warnings in a fixture-only tree"

# ---------------------------------------------------------------------------
# Scenario 2: load, registry, six faces, models, save and reload headlessly.
# ---------------------------------------------------------------------------
rc=0
run_headless "$CRATE_ROOT" "$WORK/demo.out" --demo-external-pack "$FIXTURE" || rc=$?
[[ "$rc" -eq 0 ]] || {
    cat "$WORK/demo.out" >&2
    fail "--demo-external-pack exited $rc (expected 0)"
}

grep -qx 'external_pack_demo: ok=true' "$WORK/demo.out" \
    || { cat "$WORK/demo.out" >&2; fail "missing external_pack_demo: ok=true latch"; }

for pattern in \
    'leg=stack pack=external-v3 name=External Stoneworks entries=11 errors=0' \
    'leg=sixface block=stone resolved=6 distinct=6' \
    'leg=model name=external:reed model=cross alpha=cutout' \
    'leg=crop name=external:wheat model=crop alpha=cutout stages=4' \
    'leg=blend name=external:frosted_glass model=cube alpha=blend sorted=true' \
    'leg=icon name=apple builtin=true match=true' \
    'leg=save path=' \
    'leg=reload ok=true'; do
    grep -qF "$pattern" "$WORK/demo.out" \
        || { cat "$WORK/demo.out" >&2; fail "missing demo leg: $pattern"; }
done
# The save leg must also prove a real v10 write with the three edited blocks.
grep -qE 'leg=save path=.* entries=3 ok=true$' "$WORK/demo.out" \
    || { cat "$WORK/demo.out" >&2; fail "save leg did not report entries=3 ok=true"; }
pass "external pack demo proves stack/sixface/model/crop/blend/icon/save/reload headlessly"

# ---------------------------------------------------------------------------
# Scenario 3: deterministic atlas artifact into two separate temp dirs.
# ---------------------------------------------------------------------------
ATLAS_A="$WORK/atlas-a"
ATLAS_B="$WORK/atlas-b"
mkdir -p "$ATLAS_A" "$ATLAS_B" || fail "cannot create atlas output directories"

rc=0
run_headless "$CRATE_ROOT" "$WORK/atlas-a.out" --demo-atlas "$ATLAS_A" || rc=$?
[[ "$rc" -eq 0 ]] || {
    cat "$WORK/atlas-a.out" >&2
    fail "--demo-atlas (run a) exited $rc (expected 0)"
}
rc=0
run_headless "$CRATE_ROOT" "$WORK/atlas-b.out" --demo-atlas "$ATLAS_B" || rc=$?
[[ "$rc" -eq 0 ]] || {
    cat "$WORK/atlas-b.out" >&2
    fail "--demo-atlas (run b) exited $rc (expected 0)"
}

for out in "$WORK/atlas-a.out" "$WORK/atlas-b.out"; do
    grep -qx 'atlas_demo: ok=true' "$out" \
        || { cat "$out" >&2; fail "missing atlas_demo: ok=true latch in $out"; }
    grep -qE "atlas: path=.*/atlas\.png width=$ATLAS_WIDTH height=$ATLAS_HEIGHT layers=$ATLAS_LAYERS bytes=$ATLAS_BYTES digest=$ATLAS_RGBA_DIGEST png=$ATLAS_PNG_DIGEST$" "$out" \
        || { cat "$out" >&2; fail "atlas line drifted from the pinned tokens in $out"; }
done

[[ -f "$ATLAS_A/atlas.png" ]] || fail "run a did not write $ATLAS_A/atlas.png"
[[ -f "$ATLAS_B/atlas.png" ]] || fail "run b did not write $ATLAS_B/atlas.png"
cmp -s "$ATLAS_A/atlas.png" "$ATLAS_B/atlas.png" \
    || { cmp "$ATLAS_A/atlas.png" "$ATLAS_B/atlas.png" >&2 || true; fail "two atlas runs produced different bytes"; }
bytes="$(wc -c <"$ATLAS_A/atlas.png" | tr -d ' ')"
[[ "$bytes" == "$ATLAS_BYTES" ]] \
    || fail "atlas.png is $bytes bytes (expected $ATLAS_BYTES)"
pass "atlas artifact is deterministic: width=$ATLAS_WIDTH height=$ATLAS_HEIGHT layers=$ATLAS_LAYERS bytes=$ATLAS_BYTES digest=$ATLAS_RGBA_DIGEST png=$ATLAS_PNG_DIGEST"

echo "EXTERNAL PACK SMOKE OK: external-v3 validates (11 entries), demos (8 legs), saves/reloads and the atlas artifact is byte-stable"
