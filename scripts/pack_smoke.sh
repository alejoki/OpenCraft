#!/usr/bin/env bash
# Bounded texture-pack smoke for bare launches (M004/S05-T04).
#
# Roadmap criterion 6 requires a fourth smoke harness proving that bare
# launches survive a selected pack, a corrupt pack, and a missing pack, with
# the selection persisting across relaunch. Roadmap criterion 7 requires a
# reader-test of the authoring doc; scenario 0 below is its executable form —
# the selected-pack fixture is built solely from the Quickstart TOML block, so
# a guide that cannot produce a working pack fails this harness.
#
# Scenarios (all inside mktemp working directories — the crate root's saves/,
# settings.toml, and packs/ are never touched):
#   0. Doc reader-test gate, no launch: docs/TEXTURE-PACKS.md exists, its
#      `## Quickstart` ```toml block extracts non-empty with `name =` and
#      `overlay_tint`, and all 16 layer file names appear in the guide.
#   1. Selected pack + persistence: a `[packs]`-only settings.toml selects
#      "demo", whose pack.toml is the extracted Quickstart block verbatim.
#      The pack also ships a valid hand-crafted break.wav and a corrupt
#      ui_click.wav, so sound overrides are exercised on every launch. Two
#      bounded launches must survive; settings.toml, pack.toml, and both
#      sound files must be byte-identical after each run (settings persist
#      without rewrite, and the game is read-only on packs/).
#   2. Corrupt pack: junk pack.toml plus junk stone.png and junk place.wav
#      under a selected "broken" pack degrade per file — launch survives and
#      every file stays byte-identical.
#   3. Missing pack: settings point at "ghost" with no packs/ directory —
#      launch survives on the Default fallback and creates no packs/.
#
# Dependencies: bash, timeout, cmp, mktemp, grep, awk. No PNG generation is
# needed: a manifest-only pack is valid, and missing layer files are a legal
# silent fallback in the loader contract. The valid WAV fixture is written
# with printf (canonical 44-byte RIFF/PCM16 header plus 16 payload bytes), so
# no audio encoder is required either.

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

# Resolve the binary and doc absolutely before changing directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/debug/opencraft"
DOC="$CRATE_ROOT/docs/TEXTURE-PACKS.md"

fail() {
    echo "PACK SMOKE FAIL: $*" >&2
    exit 1
}

if [[ ! -x "$BIN" ]]; then
    echo "PACK SMOKE FAIL: $BIN not found — run cargo build first" >&2
    exit 1
fi

for tool in timeout cmp mktemp grep awk; do
    command -v "$tool" >/dev/null 2>&1 || fail "required tool '$tool' not found on PATH"
done

TEMP_DIR_A="$(mktemp -d /tmp/opencraft-pack-smoke-a.XXXXXX)" || fail "mktemp failed for scenario 1"
TEMP_DIR_B="$(mktemp -d /tmp/opencraft-pack-smoke-b.XXXXXX)" || fail "mktemp failed for scenario 2"
TEMP_DIR_C="$(mktemp -d /tmp/opencraft-pack-smoke-c.XXXXXX)" || fail "mktemp failed for scenario 3"
cleanup() { rm -rf "$TEMP_DIR_A" "$TEMP_DIR_B" "$TEMP_DIR_C"; }
trap cleanup EXIT

# Run one bare launch from $1 for up to 10s, writing combined output to $2.
# The process must be killed by `timeout` (exit 124) and must not panic.
run_bounded_launch() {
    local cwd="$1"
    local out="$2"
    local label="$3"
    local rc

    cd "$cwd" || {
        echo "PACK SMOKE FAIL: $label: cannot enter working directory $cwd"
        return 1
    }

    timeout 10 "$BIN" >"$out" 2>&1
    rc=$?

    if [[ $rc -ne 124 ]]; then
        echo "PACK SMOKE FAIL: $label exit code $rc (expected 124 from timeout kill)"
        echo "--- captured output ---"
        cat "$out"
        return 1
    fi

    if grep -q "panicked" "$out" || grep -q "RUST_BACKTRACE" "$out"; then
        echo "PACK SMOKE FAIL: $label panic detected in output"
        echo "--- captured output ---"
        cat "$out"
        return 1
    fi

    return 0
}

# Write a minimal canonical PCM16 mono WAV to $1: 44-byte RIFF header, 8000 Hz,
# 8 samples (16 data bytes). Hand-crafted in bash so the harness needs no
# encoder while still exercising rodio's real decoder.
write_valid_wav() {
    local path="$1"
    printf 'RIFF\x34\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x80\x3e\x00\x00\x02\x00\x10\x00data\x10\x00\x00\x00' > "$path" || return 1
    printf '\x00\x00\x00\x10\x00\x20\x00\x30\x00\x00\x00\xf0\x00\xe0\x00\xd0' >> "$path" || return 1
}

# ---------------------------------------------------------------------------
# Scenario 0: reader-test gate for docs/TEXTURE-PACKS.md. No launch.
# Extract the fenced toml block under the exact `## Quickstart` heading so
# other headings or future toml examples cannot leak into the fixture.
# ---------------------------------------------------------------------------
[[ -f "$DOC" ]] || fail "scenario 0: $DOC is missing"

QUICKSTART_TOML="$TEMP_DIR_A/quickstart-pack.toml"
awk '
    /^## / {
        if ($0 ~ /^## Quickstart[[:space:]]*$/) { in_section = 1 } else { in_section = 0 }
        next
    }
    in_section && /^```toml[[:space:]]*$/ { in_block = 1; next }
    in_block && /^```[[:space:]]*$/ { in_block = 0; next }
    in_block { print }
' "$DOC" > "$QUICKSTART_TOML"

[[ -s "$QUICKSTART_TOML" ]] || fail "scenario 0: found no toml fenced block under the '## Quickstart' heading in $DOC"
grep -q 'name =' "$QUICKSTART_TOML" || fail "scenario 0: Quickstart TOML block has no 'name =' line"
grep -q 'overlay_tint' "$QUICKSTART_TOML" || fail "scenario 0: Quickstart TOML block has no 'overlay_tint' line"

LAYERS=(
    grass_top.png
    grass_side.png
    dirt.png
    stone.png
    cobblestone.png
    sand.png
    log_top.png
    log_side.png
    leaves.png
    planks.png
    brick.png
    snow_top.png
    bedrock.png
    snow_side.png
    coal_ore.png
    iron_ore.png
)
for layer in "${LAYERS[@]}"; do
    grep -Fq "$layer" "$DOC" || fail "scenario 0: guide does not document layer '$layer'"
done

echo "PACK SMOKE PASS: scenario 0 (doc reader-test) — Quickstart TOML extracted with name/overlay_tint and all 16 layer names documented"

# ---------------------------------------------------------------------------
# Scenario 1: selected pack + persistence across relaunch.
# A minimal [packs]-only settings file is valid: missing sections fall back
# per field. The pack manifest is the doc's Quickstart block verbatim.
# ---------------------------------------------------------------------------
mkdir -p "$TEMP_DIR_A/packs/demo" || fail "scenario 1: cannot create packs/demo"
cat > "$TEMP_DIR_A/settings.toml" <<'EOF'
[packs]
texture_pack = "demo"
EOF
cp "$QUICKSTART_TOML" "$TEMP_DIR_A/packs/demo/pack.toml" || fail "scenario 1: cannot seed pack.toml"
write_valid_wav "$TEMP_DIR_A/packs/demo/break.wav" || fail "scenario 1: cannot seed valid break.wav"
printf 'not audio at all\n' > "$TEMP_DIR_A/packs/demo/ui_click.wav" || fail "scenario 1: cannot seed corrupt ui_click.wav"

EXPECTED_DIR_A="$TEMP_DIR_A/expected"
mkdir -p "$EXPECTED_DIR_A" || fail "scenario 1: cannot create expected/ dir"
cp "$TEMP_DIR_A/settings.toml" "$EXPECTED_DIR_A/settings.toml" || fail "scenario 1: cannot snapshot settings.toml"
cp "$TEMP_DIR_A/packs/demo/pack.toml" "$EXPECTED_DIR_A/pack.toml" || fail "scenario 1: cannot snapshot pack.toml"
cp "$TEMP_DIR_A/packs/demo/break.wav" "$EXPECTED_DIR_A/break.wav" || fail "scenario 1: cannot snapshot break.wav"
cp "$TEMP_DIR_A/packs/demo/ui_click.wav" "$EXPECTED_DIR_A/ui_click.wav" || fail "scenario 1: cannot snapshot ui_click.wav"

run_bounded_launch "$TEMP_DIR_A" "$TEMP_DIR_A/output-1.log" "scenario 1 launch 1" || exit 1

[[ -f "$TEMP_DIR_A/settings.toml" ]] || fail "scenario 1: settings.toml missing after launch 1"
cmp -s "$TEMP_DIR_A/settings.toml" "$EXPECTED_DIR_A/settings.toml" || fail "scenario 1: settings.toml was rewritten during launch 1"
cmp -s "$TEMP_DIR_A/packs/demo/pack.toml" "$EXPECTED_DIR_A/pack.toml" || fail "scenario 1: packs/demo/pack.toml was rewritten during launch 1"
cmp -s "$TEMP_DIR_A/packs/demo/break.wav" "$EXPECTED_DIR_A/break.wav" || fail "scenario 1: packs/demo/break.wav was rewritten during launch 1"
cmp -s "$TEMP_DIR_A/packs/demo/ui_click.wav" "$EXPECTED_DIR_A/ui_click.wav" || fail "scenario 1: packs/demo/ui_click.wav was rewritten during launch 1"

# Relaunch in the SAME cwd: the selected pack must remain selected and the
# settings file must still not be rewritten.
run_bounded_launch "$TEMP_DIR_A" "$TEMP_DIR_A/output-2.log" "scenario 1 relaunch" || exit 1

[[ -f "$TEMP_DIR_A/settings.toml" ]] || fail "scenario 1: settings.toml missing after relaunch"
cmp -s "$TEMP_DIR_A/settings.toml" "$EXPECTED_DIR_A/settings.toml" || fail "scenario 1: settings.toml changed across relaunch (selection did not persist unchanged)"
cmp -s "$TEMP_DIR_A/packs/demo/pack.toml" "$EXPECTED_DIR_A/pack.toml" || fail "scenario 1: packs/demo/pack.toml was rewritten during relaunch"
cmp -s "$TEMP_DIR_A/packs/demo/break.wav" "$EXPECTED_DIR_A/break.wav" || fail "scenario 1: packs/demo/break.wav was rewritten during relaunch"
cmp -s "$TEMP_DIR_A/packs/demo/ui_click.wav" "$EXPECTED_DIR_A/ui_click.wav" || fail "scenario 1: packs/demo/ui_click.wav was rewritten during relaunch"

echo "PACK SMOKE PASS: scenario 1 (selected pack + persistence) — two 10s launches with texture_pack = \"demo\" plus valid break.wav and corrupt ui_click.wav, exit 124, no panic, settings.toml, pack.toml, and both sound files byte-identical after each run"

# ---------------------------------------------------------------------------
# Scenario 2: corrupt pack. Junk manifest and junk PNG layer must degrade
# per file (built-in defaults), never block the launch or rewrite the files.
# ---------------------------------------------------------------------------
mkdir -p "$TEMP_DIR_B/packs/broken" || fail "scenario 2: cannot create packs/broken"
cat > "$TEMP_DIR_B/settings.toml" <<'EOF'
[packs]
texture_pack = "broken"
EOF
printf 'this is not a valid toml manifest [[\n' > "$TEMP_DIR_B/packs/broken/pack.toml" || fail "scenario 2: cannot seed corrupt pack.toml"
printf 'not a png at all\n' > "$TEMP_DIR_B/packs/broken/stone.png" || fail "scenario 2: cannot seed corrupt stone.png"
printf 'junk sound bytes, not a wav\n' > "$TEMP_DIR_B/packs/broken/place.wav" || fail "scenario 2: cannot seed corrupt place.wav"

EXPECTED_DIR_B="$TEMP_DIR_B/expected"
mkdir -p "$EXPECTED_DIR_B" || fail "scenario 2: cannot create expected/ dir"
cp "$TEMP_DIR_B/packs/broken/pack.toml" "$EXPECTED_DIR_B/pack.toml" || fail "scenario 2: cannot snapshot corrupt pack.toml"
cp "$TEMP_DIR_B/packs/broken/stone.png" "$EXPECTED_DIR_B/stone.png" || fail "scenario 2: cannot snapshot corrupt stone.png"
cp "$TEMP_DIR_B/packs/broken/place.wav" "$EXPECTED_DIR_B/place.wav" || fail "scenario 2: cannot snapshot corrupt place.wav"

run_bounded_launch "$TEMP_DIR_B" "$TEMP_DIR_B/output.log" "scenario 2" || exit 1

cmp -s "$TEMP_DIR_B/packs/broken/pack.toml" "$EXPECTED_DIR_B/pack.toml" || fail "scenario 2: corrupt pack.toml was rewritten during the run"
cmp -s "$TEMP_DIR_B/packs/broken/stone.png" "$EXPECTED_DIR_B/stone.png" || fail "scenario 2: corrupt stone.png was rewritten during the run"
cmp -s "$TEMP_DIR_B/packs/broken/place.wav" "$EXPECTED_DIR_B/place.wav" || fail "scenario 2: corrupt place.wav was rewritten during the run"

echo "PACK SMOKE PASS: scenario 2 (corrupt pack) — junk pack.toml, junk stone.png, and junk place.wav fell back per file, exit 124, no panic, all files byte-identical"

# ---------------------------------------------------------------------------
# Scenario 3: missing pack. The selection names a folder that does not exist
# and there is no packs/ root at all; launch must fall back to Default and
# must not create a packs/ directory.
# ---------------------------------------------------------------------------
cat > "$TEMP_DIR_C/settings.toml" <<'EOF'
[packs]
texture_pack = "ghost"
EOF

run_bounded_launch "$TEMP_DIR_C" "$TEMP_DIR_C/output.log" "scenario 3" || exit 1

[[ ! -e "$TEMP_DIR_C/packs" ]] || fail "scenario 3: packs/ directory was created during a run with no packs root"

echo "PACK SMOKE PASS: scenario 3 (missing pack) — texture_pack = \"ghost\" with no packs/ root fell back to Default, exit 124, no panic, no packs/ created"

echo "PACK SMOKE: all scenarios passed"
exit 0
