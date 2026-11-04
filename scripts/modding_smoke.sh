#!/usr/bin/env bash
# Real-binary modding smoke for the checked-in test fixture packs (M010/S08-T06).
#
# Proves the modding surface end to end through the production binary, with no
# test harness and no screenshot automation:
#
#   Scenario 0 (doc reader-test): the fenced `toml` block under
#   `## Format v2` in docs/TEXTURE-PACKS.md must still author every Format v2
#   array (blocks, items, tiers, tools, recipes, block_patches,
#   recipe_patches), and the whole document must still name worldgen, both
#   reference packs, `--validate-pack` and the content/save section. This is
#   the executable half of the pack-author documentation.
#
#   Scenario 1 (clean): `--validate-pack mymod-ruby` exits 0 with `ok:` lines
#   for all 12 authored entries and `12 entries, 0 errors, 0 warnings`;
#   `--validate-pack ruby-overlay` exits 0 with `0 entries, 0 errors,
#   0 warnings`. Both run with an empty DISPLAY, so a window cannot mask a
#   validation failure.
#
#   Scenario 2 (broken): a pack with one block whose texture is absent and one
#   valid item sibling exits 1, reports `error: block broken:missing_art`,
#   keeps the valid sibling `ok: item broken:ok_item`, and reports a non-zero
#   error count.
#
#   Scenario 3 (missing): `--validate-pack ghost` exits 1 with
#   `error: pack ghost:`.
#
#   Scenario 4 (bounded launches): with both good packs selected,
#   `timeout 10 "$BIN"` exits 124 with no panic; after adding the broken pack
#   to the stack the game still launches the same way. Throughout both legs the
#   copied pack tree and settings.toml stay byte-identical.
#
# Everything runs in a mktemp working directory. The crate root's packs/,
# settings.toml and saves/ are never touched. Judge every scenario by this
# script's own exit code; a child 124 is expected only for the bounded
# launches (MEM377). This script is self-contained on purpose: it must not
# delegate to validate_pack_smoke.sh, and it must never run `cargo test`.
#
# Dependencies: bash, timeout, cmp, mktemp, cp, grep, awk, find, sort.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "MODDING SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
# create its window for the bounded launch legs. Prefer the Wayland socket for
# the same reason as the other smokes.
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
CRATE_OVERLAY="$CRATE_ROOT/tests/fixtures/packs/ruby-overlay"
DOC="$CRATE_ROOT/docs/TEXTURE-PACKS.md"
BUILD_LOG="$(mktemp /tmp/opencraft-modding-build.XXXXXX.log)" || {
    echo "MODDING SMOKE FAIL: could not create a build log" >&2
    exit 1
}

if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "MODDING SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    rm -f "$BUILD_LOG"
    exit 1
fi
rm -f "$BUILD_LOG"

if [[ ! -x "$BIN" ]]; then
    echo "MODDING SMOKE FAIL: profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
    exit 1
fi

for path in "$CRATE_PACK/pack.toml" "$CRATE_OVERLAY/pack.toml" "$DOC"; do
    if [[ ! -f "$path" ]]; then
        echo "MODDING SMOKE FAIL: checked-in input not found at $path"
        exit 1
    fi
done

for tool in timeout cmp mktemp cp grep awk find sort; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "MODDING SMOKE FAIL: required tool '$tool' not found on PATH"
        exit 1
    }
done

TEMP_DIR="$(mktemp -d /tmp/opencraft-modding-smoke.XXXXXX)" || {
    echo "MODDING SMOKE FAIL: could not create a temp dir" >&2
    exit 1
}
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

fail() {
    echo "MODDING SMOKE FAIL: $*" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# Fixture: both reference packs plus the broken validation fixture.
# ---------------------------------------------------------------------------
mkdir -p "$TEMP_DIR/packs" || fail "cannot create packs/ in the temp dir"
cp -R "$CRATE_PACK" "$TEMP_DIR/packs/mymod-ruby" || fail "cannot copy the checked-in reference pack"
cp -R "$CRATE_OVERLAY" "$TEMP_DIR/packs/ruby-overlay" || fail "cannot copy the texture-only overlay"

mkdir -p "$TEMP_DIR/packs/broken" || fail "cannot create packs/broken"
cat >"$TEMP_DIR/packs/broken/pack.toml" <<'EOF'
format_version = 2

[[blocks]]
id = "broken:missing_art"
textures = { all = "missing.png" }
hardness = 1.0

[[items]]
id = "broken:ok_item"
icon = "ok.png"
stack = 16
EOF
cp "$TEMP_DIR/packs/mymod-ruby/textures/ruby.png" "$TEMP_DIR/packs/broken/ok.png" ||
    fail "cannot seed broken/ok.png from the reference pack"

cat >"$TEMP_DIR/settings.toml" <<'EOF'
[packs]
stack = ["mymod-ruby", "ruby-overlay"]
EOF

# Snapshot the authored pack tree and both settings variants so the launch legs
# can prove the game is read-only on packs/ and settings.toml.
EXPECTED="$TEMP_DIR/expected"
mkdir -p "$EXPECTED" || fail "cannot create expected/ dir"
cp "$TEMP_DIR/settings.toml" "$EXPECTED/settings-good.toml" ||
    fail "cannot snapshot the good settings.toml"
cp -a "$TEMP_DIR/packs" "$EXPECTED/packs" || fail "cannot snapshot the pack tree"

cat >"$TEMP_DIR/settings-broken.toml" <<'EOF'
[packs]
stack = ["mymod-ruby", "ruby-overlay", "broken"]
EOF
cp "$TEMP_DIR/settings-broken.toml" "$EXPECTED/settings-broken.toml" ||
    fail "cannot snapshot the broken-stack settings.toml"

# Compare every authored pack file (contents, added files and removed files)
# against the snapshot taken before any binary ran.
compare_pack_tree() {
    local actual="$1"
    local expected="$2"
    local actual_list="$TEMP_DIR/.actual-pack-files"
    local expected_list="$TEMP_DIR/.expected-pack-files"

    (cd "$actual" && find . -type f | LC_ALL=C sort) >"$actual_list" ||
        fail "cannot list the actual pack tree"
    (cd "$expected" && find . -type f | LC_ALL=C sort) >"$expected_list" ||
        fail "cannot list the expected pack tree"
    cmp -s "$actual_list" "$expected_list" ||
        fail "the pack file list changed during the launch legs"
    while IFS= read -r relative; do
        cmp -s "$actual/$relative" "$expected/$relative" ||
            fail "$relative was rewritten during the launch legs"
    done <"$expected_list"
}

cd "$TEMP_DIR" || fail "cannot enter working directory $TEMP_DIR"

CLEAN_OUT="$TEMP_DIR/clean.out"
OVERLAY_OUT="$TEMP_DIR/overlay.out"
BROKEN_OUT="$TEMP_DIR/broken.out"
MISSING_OUT="$TEMP_DIR/missing.out"
LAUNCH_GOOD_OUT="$TEMP_DIR/launch-good.out"
LAUNCH_BROKEN_OUT="$TEMP_DIR/launch-broken.out"

# ---------------------------------------------------------------------------
# Scenario 0: the authoring docs are an executable reader-test.
# ---------------------------------------------------------------------------
DOC_BLOCK="$(awk '
    /^## Format v2$/ { in_section = 1; next }
    in_section && /^```toml$/ { in_block = 1; next }
    in_block && /^```$/ { exit }
    in_block { print }
' "$DOC")" || fail "cannot read the Format v2 TOML block from $DOC"
[[ -n "$DOC_BLOCK" ]] || fail "docs reader-test: no fenced toml block under '## Format v2'"

for needle in \
    'format_version = 2' \
    '[[blocks]]' \
    '[[items]]' \
    '[[tiers]]' \
    '[[tools]]' \
    '[[recipes]]' \
    '[[block_patches]]' \
    '[[recipe_patches]]'; do
    grep -Fq -- "$needle" <<<"$DOC_BLOCK" ||
        fail "docs reader-test: Format v2 TOML block is missing '$needle'"
done

for needle in \
    '[[worldgen]]' \
    '## Declarative worldgen' \
    'tests/fixtures/packs/mymod-ruby' \
    'tests/fixtures/packs/ruby-overlay' \
    '--validate-pack' \
    '## Content packs and saves'; do
    grep -Fq -- "$needle" "$DOC" ||
        fail "docs reader-test: $DOC is missing '$needle'"
done

# ---------------------------------------------------------------------------
# Scenario 1: clean reference packs. Empty DISPLAY proves headless validation.
# ---------------------------------------------------------------------------
DISPLAY= WAYLAND_DISPLAY= timeout 30 "$BIN" --validate-pack mymod-ruby >"$CLEAN_OUT" 2>&1
CLEAN_RC=$?
if [[ $CLEAN_RC -ne 0 ]]; then
    echo "MODDING SMOKE FAIL: clean mymod-ruby validation exit code $CLEAN_RC (expected 0; 124 means an external timeout killed it)"
    echo "--- captured validation output ---"
    cat "$CLEAN_OUT"
    exit 1
fi
if grep -q "panicked" "$CLEAN_OUT" || grep -q "RUST_BACKTRACE" "$CLEAN_OUT"; then
    echo "MODDING SMOKE FAIL: panic detected in the clean mymod-ruby validation output"
    cat "$CLEAN_OUT"
    exit 1
fi
for expected_line in \
    '^ok: block mymod:ruby_block$' \
    '^ok: block mymod:ruby_ore$' \
    '^ok: item mymod:ruby$' \
    '^ok: item mymod:ruby_stew$' \
    '^ok: tier mymod:ruby$' \
    '^ok: tool mymod:ruby_pickaxe$' \
    '^ok: tool mymod:ruby_sword$' \
    '^ok: recipe mymod:ruby_pickaxe$' \
    '^ok: recipe mymod:ruby_stew$' \
    '^ok: recipe_patch shortbow$' \
    '^ok: worldgen ore mymod:ruby_ore$' \
    '^ok: worldgen patch mymod:ruby_ore$'; do
    grep -Eq -- "$expected_line" "$CLEAN_OUT" ||
        fail "clean mymod-ruby validation is missing '$expected_line'"
done
grep -Fq 'validate-pack: mymod-ruby: 12 entries, 0 errors, 0 warnings' "$CLEAN_OUT" ||
    fail "clean mymod-ruby summary drifted from '12 entries, 0 errors, 0 warnings'"
if grep -q '^error:' "$CLEAN_OUT"; then
    echo "MODDING SMOKE FAIL: clean mymod-ruby validation reported an error"
    cat "$CLEAN_OUT"
    exit 1
fi

DISPLAY= WAYLAND_DISPLAY= timeout 30 "$BIN" --validate-pack ruby-overlay >"$OVERLAY_OUT" 2>&1
OVERLAY_RC=$?
if [[ $OVERLAY_RC -ne 0 ]]; then
    echo "MODDING SMOKE FAIL: clean ruby-overlay validation exit code $OVERLAY_RC (expected 0)"
    echo "--- captured validation output ---"
    cat "$OVERLAY_OUT"
    exit 1
fi
grep -Fq 'validate-pack: ruby-overlay: 0 entries, 0 errors, 0 warnings' "$OVERLAY_OUT" ||
    fail "clean ruby-overlay summary drifted from '0 entries, 0 errors, 0 warnings'"

# ---------------------------------------------------------------------------
# Scenario 2: broken pack keeps its valid sibling and fails the run.
# ---------------------------------------------------------------------------
DISPLAY= WAYLAND_DISPLAY= timeout 30 "$BIN" --validate-pack broken >"$BROKEN_OUT" 2>&1
BROKEN_RC=$?
if [[ $BROKEN_RC -ne 1 ]]; then
    echo "MODDING SMOKE FAIL: broken validation exit code $BROKEN_RC (expected 1)"
    echo "--- captured validation output ---"
    cat "$BROKEN_OUT"
    exit 1
fi
grep -Fq 'error: block broken:missing_art' "$BROKEN_OUT" ||
    fail "broken validation is missing 'error: block broken:missing_art'"
grep -Fq 'ok: item broken:ok_item' "$BROKEN_OUT" ||
    fail "broken validation must still print 'ok: item broken:ok_item' for the valid sibling"
grep -Eq '^validate-pack: broken: [0-9]+ entries, [1-9][0-9]* errors, [0-9]+ warnings$' "$BROKEN_OUT" ||
    fail "broken validation summary must report a non-zero error count"

# ---------------------------------------------------------------------------
# Scenario 3: missing pack folder fails closed.
# ---------------------------------------------------------------------------
DISPLAY= WAYLAND_DISPLAY= timeout 30 "$BIN" --validate-pack ghost >"$MISSING_OUT" 2>&1
MISSING_RC=$?
if [[ $MISSING_RC -ne 1 ]]; then
    echo "MODDING SMOKE FAIL: missing validation exit code $MISSING_RC (expected 1)"
    echo "--- captured validation output ---"
    cat "$MISSING_OUT"
    exit 1
fi
grep -Fq 'error: pack ghost:' "$MISSING_OUT" ||
    fail "missing validation is missing 'error: pack ghost:'"

# ---------------------------------------------------------------------------
# Scenario 4a: bounded launch with both good packs.
# ---------------------------------------------------------------------------
timeout 10 "$BIN" >"$LAUNCH_GOOD_OUT" 2>&1
LAUNCH_GOOD_RC=$?
if [[ $LAUNCH_GOOD_RC -ne 124 ]]; then
    echo "MODDING SMOKE FAIL: good-stack launch exit code $LAUNCH_GOOD_RC (expected 124 from the timeout kill; a clean exit or panic means the event loop did not survive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_GOOD_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_GOOD_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_GOOD_OUT"; then
    echo "MODDING SMOKE FAIL: panic detected in the good-stack launch output"
    cat "$LAUNCH_GOOD_OUT"
    exit 1
fi
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED/settings-good.toml" ||
    fail "settings.toml was rewritten during the good-stack launch leg"
compare_pack_tree "$TEMP_DIR/packs" "$EXPECTED/packs"

# ---------------------------------------------------------------------------
# Scenario 4b: bounded launch with the broken pack in the stack.
# ---------------------------------------------------------------------------
printf '[packs]\nstack = ["mymod-ruby", "ruby-overlay", "broken"]\n' >"$TEMP_DIR/settings.toml" ||
    fail "cannot select the broken pack"
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED/settings-broken.toml" ||
    fail "the broken-stack settings fixture is not byte-identical to its snapshot"

timeout 10 "$BIN" >"$LAUNCH_BROKEN_OUT" 2>&1
LAUNCH_BROKEN_RC=$?
if [[ $LAUNCH_BROKEN_RC -ne 124 ]]; then
    echo "MODDING SMOKE FAIL: broken-pack launch exit code $LAUNCH_BROKEN_RC (expected 124 from the timeout kill; per-entry fallback must keep startup alive)"
    echo "--- captured launch output ---"
    cat "$LAUNCH_BROKEN_OUT"
    exit 1
fi
if grep -q "panicked" "$LAUNCH_BROKEN_OUT" || grep -q "RUST_BACKTRACE" "$LAUNCH_BROKEN_OUT"; then
    echo "MODDING SMOKE FAIL: panic detected in the broken-pack launch output"
    cat "$LAUNCH_BROKEN_OUT"
    exit 1
fi
cmp -s "$TEMP_DIR/settings.toml" "$EXPECTED/settings-broken.toml" ||
    fail "the broken-stack settings.toml was rewritten during the launch leg"
compare_pack_tree "$TEMP_DIR/packs" "$EXPECTED/packs"

echo "MODDING SMOKE PASS [profile=$PROFILE]: doc reader-test (Format v2 TOML + worldgen + both reference packs + --validate-pack), clean mymod-ruby 12/0/0, clean ruby-overlay 0/0/0, broken exit=1 with error+valid sibling, missing exit=1, good-stack launch exit=124 and broken-pack launch exit=124 with no panic, packs/settings byte-identical"
echo "MODDING SMOKE EVIDENCE: clean mymod-ruby exit=$CLEAN_RC summary=12-entries-0-errors-0-warnings"
echo "MODDING SMOKE EVIDENCE: clean ruby-overlay exit=$OVERLAY_RC summary=0-entries-0-errors-0-warnings"
echo "MODDING SMOKE EVIDENCE: broken exit=$BROKEN_RC error=block-broken:missing_art valid-sibling=ok non-zero-errors=true"
echo "MODDING SMOKE EVIDENCE: missing exit=$MISSING_RC error=pack-ghost"
echo "MODDING SMOKE EVIDENCE: launches good-stack=$LAUNCH_GOOD_RC broken-pack=$LAUNCH_BROKEN_RC panic=none pack-and-settings-byte-identical"
exit 0
