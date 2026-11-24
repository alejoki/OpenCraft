#!/usr/bin/env bash
# Headless `--validate-pack` smoke (M010/S02-T04).
#
# Proves the strict validation surface without a window, GPU, save or settings
# file:
#   1. clean v3 pack -> exit 0, per-entry `ok:` lines, summary, no errors
#   2. broken pack (block with a missing texture + valid item sibling) ->
#      exit 1, `error: block broken:missing_art`, valid sibling still `ok:`
#   3. missing pack -> exit 1 with `error: pack ghost:`
#   4. `--validate-pack` with no value -> exit 1 (fails closed, never launches)
#   5. legacy v2 pack -> exit 1 with one pack-level error naming the format
#      version and the required `format_version = 3`, no `ok:` entries
#
# Everything runs in a mktemp working directory. The crate root's packs/,
# saves/ and settings.toml are never touched; a seeded settings-free tree with
# saves/keep.bin proves validation is read-only and never writes either.
#
# Dependencies: bash, timeout, cmp, diff, mktemp, grep, base64. The PNGs are
# decoded from embedded base64 blobs of genuinely valid 16x16 RGBA images, so
# no image encoder is required.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"

fail() {
    echo "VALIDATE PACK SMOKE FAIL: $*" >&2
    exit 1
}

if [[ ! -x "$BIN" ]]; then
    echo "VALIDATE PACK SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

for tool in timeout cmp diff mktemp grep base64; do
    command -v "$tool" >/dev/null 2>&1 || fail "required tool '$tool' not found on PATH"
done

WORK="$(mktemp -d /tmp/opencraft-validate-pack.XXXXXX)" || fail "mktemp failed"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

cd "$WORK" || fail "cannot enter working directory $WORK"

write_valid_png() {
    local path="$1"
    local payload="$2"
    printf '%s' "$payload" | base64 -d > "$path" || return 1
}

PNG_RUBY_ORE="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGM4oaDwnxLMMGrAqAGjBgwXAwAnUgcfZ/f+jQAAAABJRU5ErkJggg=="
PNG_RUBY="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGNQUDjxnxLMMGrAqAGjBgwXAwDXNAcfEvMRpwAAAABJRU5ErkJggg=="

# ---------------------------------------------------------------------------
# Fixture: one clean v3 pack plus the sentinel state.
# ---------------------------------------------------------------------------
mkdir -p packs/mymod-ruby saves || fail "cannot create fixture directories"
printf 'sentinel payload: must stay byte-identical\n' > saves/keep.bin ||
    fail "cannot seed saves/keep.bin"
cp saves/keep.bin "$WORK/keep.expected" || fail "cannot snapshot the sentinel save"

cat > packs/mymod-ruby/pack.toml <<'EOF'
format_version = 3
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

[[items]]
id = "mymod:ruby"
icon = "ruby.png"
stack = 64
EOF

write_valid_png "packs/mymod-ruby/ruby_ore.png" "$PNG_RUBY_ORE" ||
    fail "cannot seed ruby_ore.png"
write_valid_png "packs/mymod-ruby/ruby.png" "$PNG_RUBY" ||
    fail "cannot seed ruby.png"

# Broken pack: one missing asset and one valid sibling.
mkdir -p packs/broken || fail "cannot create broken pack"
cat > packs/broken/pack.toml <<'EOF'
format_version = 3

[[blocks]]
id = "broken:missing_art"
textures = { all = "missing.png" }
hardness = 1.0

[[items]]
id = "broken:ok_item"
icon = "ok.png"
stack = 16
EOF
write_valid_png "packs/broken/ok.png" "$PNG_RUBY" || fail "cannot seed broken/ok.png"

# Legacy pack: a v2 manifest with content arrays must be rejected by name by
# the v3-only gate before any entry is parsed.
mkdir -p packs/legacy || fail "cannot create legacy pack"
cat > packs/legacy/pack.toml <<'EOF'
format_version = 2
name = "Old Two"

[[blocks]]
id = "legacy:old_block"
textures = { all = "old_block.png" }
hardness = 1.0
EOF
write_valid_png "packs/legacy/old_block.png" "$PNG_RUBY" || fail "cannot seed legacy/old_block.png"

# External v3 fixture: authored from docs/TEXTURE-PACKS.md alone. It proves
# the documented flat six-face overrides, the three block models, all three
# alpha classes and a [builtin_icons] override validate without repo help.
cp -a "$CRATE_ROOT/tests/fixtures/packs/external-v3" packs/external-v3 ||
    fail "cannot seed external-v3 fixture"

cp -a packs "$WORK/packs.expected" || fail "cannot snapshot the pack tree"

run_validate() {
    local out="$1"
    shift
    # Clear display variables: if the binary ever tried to open a window the
    # launch would fail instead of silently keeping the smoke green.
    DISPLAY= WAYLAND_DISPLAY= timeout 30 "$BIN" "$@" >"$out" 2>&1
    echo $?
}

# ---------------------------------------------------------------------------
# Scenario 1: clean pack.
# ---------------------------------------------------------------------------
OUT="$WORK/clean.out"
rc="$(run_validate "$OUT" --validate-pack mymod-ruby)"
[[ "$rc" -eq 0 ]] || {
    cat "$OUT" >&2
    fail "scenario 1: clean pack exit $rc (expected 0)"
}
grep -q '^ok: block mymod:ruby_ore$' "$OUT" ||
    fail "scenario 1: missing 'ok: block mymod:ruby_ore'"
grep -q '^ok: item mymod:ruby$' "$OUT" ||
    fail "scenario 1: missing 'ok: item mymod:ruby'"
grep -q '^validate-pack: mymod-ruby:' "$OUT" ||
    fail "scenario 1: missing 'validate-pack: mymod-ruby:' summary"
if grep -q '^error:' "$OUT"; then
    cat "$OUT" >&2
    fail "scenario 1: clean pack reported an error"
fi

# ---------------------------------------------------------------------------
# Scenario 2: broken pack (missing texture) with a valid sibling.
# ---------------------------------------------------------------------------
OUT="$WORK/broken.out"
rc="$(run_validate "$OUT" --validate-pack broken)"
[[ "$rc" -eq 1 ]] || {
    cat "$OUT" >&2
    fail "scenario 2: broken pack exit $rc (expected 1)"
}
grep -q '^error: block broken:missing_art:' "$OUT" ||
    fail "scenario 2: missing 'error: block broken:missing_art'"
grep -q '^ok: item broken:ok_item$' "$OUT" ||
    fail "scenario 2: valid sibling must still print ok"
grep -Eq '^validate-pack: broken: [0-9]+ entries, [1-9][0-9]* errors, [0-9]+ warnings$' "$OUT" ||
    fail "scenario 2: summary must report a non-zero error count"

# ---------------------------------------------------------------------------
# Scenario 3: missing pack folder.
# ---------------------------------------------------------------------------
OUT="$WORK/missing.out"
rc="$(run_validate "$OUT" --validate-pack ghost)"
[[ "$rc" -eq 1 ]] || {
    cat "$OUT" >&2
    fail "scenario 3: missing pack exit $rc (expected 1)"
}
grep -q '^error: pack ghost:' "$OUT" ||
    fail "scenario 3: missing 'error: pack ghost:'"

# ---------------------------------------------------------------------------
# Scenario 4: no value -> fail closed instead of launching the game.
# ---------------------------------------------------------------------------
OUT="$WORK/no-value.out"
rc="$(run_validate "$OUT" --validate-pack)"
[[ "$rc" -eq 1 ]] || {
    cat "$OUT" >&2
    fail "scenario 4: missing value exit $rc (expected 1)"
}

# ---------------------------------------------------------------------------
# Scenario 5: a v2 pack is rejected by name before any entry is parsed.
# ---------------------------------------------------------------------------
OUT="$WORK/legacy.out"
rc="$(run_validate "$OUT" --validate-pack legacy)"
[[ "$rc" -eq 1 ]] || {
    cat "$OUT" >&2
    fail "scenario 5: legacy v2 pack exit $rc (expected 1)"
}
grep -Fq 'error: pack legacy: format_version 2 is not supported; every pack must declare format_version = 3' "$OUT" ||
    fail "scenario 5: missing the named v2 format-gate error"
if grep -q '^ok:' "$OUT"; then
    cat "$OUT" >&2
    fail "scenario 5: a rejected v2 pack must not report ok entries"
fi
grep -Fq 'validate-pack: legacy: 0 entries, 1 errors, 0 warnings' "$OUT" ||
    fail "scenario 5: summary must report exactly one pack-level error"

# ---------------------------------------------------------------------------
# Scenario 6: the external-v3 fixture authored from the docs alone.
# It must validate with 0 errors AND 0 warnings, and every documented feature
# must appear as a named `ok:` entry.
# ---------------------------------------------------------------------------
OUT="$WORK/external-v3.out"
rc="$(run_validate "$OUT" --validate-pack external-v3)"
[[ "$rc" -eq 0 ]] || {
    cat "$OUT" >&2
    fail "scenario 6: external-v3 exit $rc (expected 0)"
}
grep -Fq 'validate-pack: external-v3: 11 entries, 0 errors, 0 warnings' "$OUT" ||
    fail "scenario 6: missing the exact 0 errors / 0 warnings summary"
# Six distinct built-in stone faces plus a `_top`-only dirt override.
for face in east west top bottom south north; do
    grep -Fq "ok: face stone $face" "$OUT" ||
        fail "scenario 6: missing 'ok: face stone $face'"
done
grep -Fq 'ok: face dirt top' "$OUT" ||
    fail "scenario 6: missing the top-only dirt override"
grep -Fq 'ok: block external:reed model=cross alpha=cutout' "$OUT" ||
    fail "scenario 6: missing the cross/cutout entry"
grep -Fq 'ok: block external:wheat model=crop alpha=cutout' "$OUT" ||
    fail "scenario 6: missing the crop/cutout entry"
grep -Fq 'ok: block external:frosted_glass model=cube alpha=blend' "$OUT" ||
    fail "scenario 6: missing the cube/blend entry"
grep -Fq 'ok: builtin_icon apple' "$OUT" ||
    fail "scenario 6: missing the [builtin_icons] override entry"
if grep -q '^error:' "$OUT"; then
    cat "$OUT" >&2
    fail "scenario 6: external-v3 reported an error"
fi
if grep -q '^warning:' "$OUT"; then
    cat "$OUT" >&2
    fail "scenario 6: external-v3 reported a warning"
fi

# ---------------------------------------------------------------------------
# Read-only contract: no settings file, untouched sentinel save and packs.
# ---------------------------------------------------------------------------
[[ ! -e "$WORK/settings.toml" ]] ||
    fail "validation created settings.toml"
cmp -s "$WORK/saves/keep.bin" "$WORK/keep.expected" ||
    fail "validation modified saves/keep.bin"
if ! diff -r "$WORK/packs" "$WORK/packs.expected" >/dev/null 2>&1; then
    diff -r "$WORK/packs" "$WORK/packs.expected" >&2
    fail "validation modified pack files"
fi

echo "VALIDATE PACK SMOKE OK: clean=0 broken=1 missing=1 no-value=1 legacy-v2=1 external-v3=0; settings/save/packs untouched"
