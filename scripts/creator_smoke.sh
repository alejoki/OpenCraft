#!/usr/bin/env bash
# Real-binary creator-surface smoke (M014/S09-T04).
#
# Proves the whole M014 creator loop end to end through the production binary,
# in both profiles, with no test harness and no screenshot automation. Every
# fixture lives in its own mktemp working directory, so the crate root's
# config/, packs/, settings.toml, and saves/ are never touched.
#
# Profile switch: SMOKE_PROFILE=debug (default) or SMOKE_PROFILE=release.
# An invalid value fails closed before any build, display setup, or launch.
#
# Legs, all judged by this script's own exit code:
#
#   Doc gate (no launch): docs/CONFIG.md quotes both release rejection lines;
#     docs/TEXTURE-PACKS.md documents `recipe changed` and `--demo-restart`;
#     docs/SAVE-NOTES.md documents `--demo-restart` and the one-time
#     `content definitions changed` notice; README.md names
#     `scripts/creator_smoke.sh`, `--check-config`, `--creator-trace`, and
#     `config/`. This is the executable half of the S09 documentation.
#
#   Leg A config audit: in a copied config/ tree, debug `--check-config` exits 0
#     with six `ok:` lines and the pinned summary prefix; setting
#     movement.walk_speed to 0 exits 1 with the exact per-key error line; the
#     original tree is restored. In release, `--check-config` and
#     `--creator-trace` each exit 1 with exactly one pinned dev-only line and
#     create no settings, saves, or packs.
#
#   Leg B live config (debug only): a bounded launch with `--creator-trace`
#     applies `walk_speed = 8.0` live and rejects `walk_speed = 0` with the
#     pinned field reason; the edited config tree stays byte-identical. The
#     release leg is a bounded launch with no `creator:` output and no panic.
#
#   Leg B2 all-domain live config (debug only): one bounded launch applies a
#     single edit in each curated file (player, survival, combat, world,
#     mining, render) through the live watcher. The trace must contain exactly
#     six applied lines (one per file) and zero rejections, the edited tree
#     must stay byte-identical, and the post-run `--check-config` digest must
#     equal the final applied digest `0x18d98c480e113ab1` (not the default
#     `0xeaeac8ddce568dec`). The release profile skips this leg: release has
#     no creator watcher.
#
#   Leg C live textures (debug only): a v2 texture-only pack overrides
#     block stone (layer 3), mob pig_body (layer 32), and the built-in apple
#     sprite (layer 1); rewriting all three applies each frozen layer, and a
#     corrupt stone.png is rejected once while the pack files stay
#     byte-identical. The release leg launches bounded with no `creator:`
#     output and identical pack/settings bytes.
#
#   Leg D restart recipe: `--demo-restart` saves count=1, the live watcher
#     (debug only) reports exactly one `recipe changed` restart-required line
#     after the stew recipe count moves to 2, and a fresh `--demo-restart` on
#     the same save previews/crafts count=2 with the one-time content notice.
#     The release profile runs both demo phases without the watcher leg.
#
#   Leg E v8-to-v9: `--demo-save-v9` loads the checked-in v8 fixture, re-saves
#     v9 with config schema 1 and a digest, reloads with no notice, and leaves
#     the input bytes untouched; the derived file is exactly input+10 bytes.
#     Both profiles then prove the one-shot config-mismatch notice: a
#     `walk_speed = 6.0` edit makes the derived v9 file warn exactly once
#     (`config changed since this world was saved`), re-save with digest
#     `0x14092db54f42a9d1` at 446 bytes, and a third run on that output loads
#     with no notice (the notice is one-shot, not repeated). The debug profile
#     additionally re-checks the mismatched tree's `--check-config` digest;
#     release rejects `--check-config` by design (Leg A).
#
#   Leg F release hygiene (release only): a bare bounded launch in an empty
#     temp cwd exits 124, does not panic, and creates no packs/ or config/;
#     no checked-in test pack leaked into the crate packs/ tree; a bounded
#     launch with config + a real pack produces no `creator:` output and
#     leaves config, pack, and settings bytes identical.
#
# Every bounded live launch must be killed by `timeout` (exit 124); a clean
# exit or panic is a FAILURE. Only exit 124 is accepted for those legs.
#
# The five PNG fixtures are valid 16x16 RGBA base64 blobs copied from the
# existing presentation/validate smokes. No PNG encoder or new dependency is
# involved.
#
# Dependencies: bash, timeout, cmp, mktemp, cp, grep, awk, base64, sed, diff.

set -u

# Profile switch (default debug). An invalid value fails closed here, before
# any build, display setup, or binary launch.
PROFILE="${SMOKE_PROFILE:-debug}"
case "$PROFILE" in
    debug|release) ;;
    *)
        echo "CREATOR SMOKE FAIL: invalid SMOKE_PROFILE='$PROFILE' (expected debug or release)"
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
# create its window for the bounded live legs. Prefer the Wayland socket:
# Xwayland's :0 needs an xauth cookie the launching shell may not have, and
# winit picks the X11 backend whenever DISPLAY is set (it does not fall back
# cleanly to Wayland).
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    if [[ -e "/run/user/$(id -u)/wayland-0" ]]; then
        export WAYLAND_DISPLAY="wayland-0"
        export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    elif [[ -e "/tmp/.X11-unix/X0" ]]; then
        export DISPLAY=":0"
    fi
fi

# Resolve the crate root and binary absolutely before changing directory (the
# build and every helper run from an absolute path).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$CRATE_ROOT/target/$PROFILE/opencraft"
BUILD_LOG="$(mktemp /tmp/opencraft-creator-build.XXXXXX.log)" || {
    echo "CREATOR SMOKE FAIL: could not create a build log" >&2
    exit 1
}
TEMP_DIR="$(mktemp -d /tmp/opencraft-creator-smoke.XXXXXX)" || {
    echo "CREATOR SMOKE FAIL: could not create a temp dir" >&2
    rm -f "$BUILD_LOG"
    exit 1
}

GAME_PID=""
cleanup() {
    if [[ -n "$GAME_PID" ]]; then
        kill "$GAME_PID" 2>/dev/null || true
    fi
    rm -rf "$TEMP_DIR"
    rm -f "$BUILD_LOG"
}
trap cleanup EXIT

fail() {
    echo "CREATOR SMOKE FAIL: $*" >&2
    exit 1
}

for tool in timeout cmp mktemp cp grep awk base64 sed diff; do
    command -v "$tool" >/dev/null 2>&1 || fail "required tool '$tool' not found on PATH"
done

# ---------------------------------------------------------------------------
# Checked-in inputs. Fail with a precise path rather than a mystery later.
# ---------------------------------------------------------------------------
for path in \
    "$CRATE_ROOT/config/player.toml" \
    "$CRATE_ROOT/tests/fixtures/packs/mymod-ruby/pack.toml" \
    "$CRATE_ROOT/tests/fixtures/saves/v8-baseline.mcvx" \
    "$CRATE_ROOT/docs/CONFIG.md" \
    "$CRATE_ROOT/docs/TEXTURE-PACKS.md" \
    "$CRATE_ROOT/docs/SAVE-NOTES.md" \
    "$CRATE_ROOT/docs/UAT-M014.md" \
    "$CRATE_ROOT/README.md"; do
    [[ -e "$path" ]] || fail "checked-in input not found at $path"
done

# ---------------------------------------------------------------------------
# Doc reader-test gate. No launch.
# ---------------------------------------------------------------------------
check_doc() {
    local doc="$1"
    local needle="$2"
    grep -qF -- "$needle" "$doc" ||
        fail "doc gate: $doc must contain exactly [$needle]"
}

check_doc "$CRATE_ROOT/docs/CONFIG.md" 'check-config: dev build only'
check_doc "$CRATE_ROOT/docs/CONFIG.md" 'creator-trace: dev build only'
check_doc "$CRATE_ROOT/docs/TEXTURE-PACKS.md" 'recipe changed'
check_doc "$CRATE_ROOT/docs/TEXTURE-PACKS.md" '--demo-restart'
check_doc "$CRATE_ROOT/docs/SAVE-NOTES.md" '--demo-restart'
check_doc "$CRATE_ROOT/docs/SAVE-NOTES.md" 'content definitions changed'
check_doc "$CRATE_ROOT/docs/UAT-M014.md" 'stone_speed_multiplier'
check_doc "$CRATE_ROOT/docs/UAT-M014.md" 'iron_speed_multiplier'
check_doc "$CRATE_ROOT/README.md" 'creator_smoke.sh'
check_doc "$CRATE_ROOT/README.md" '--check-config'
check_doc "$CRATE_ROOT/README.md" '--creator-trace'
check_doc "$CRATE_ROOT/README.md" 'config/'

# ---------------------------------------------------------------------------
# Build the requested profile from the crate root.
# ---------------------------------------------------------------------------
if ! (cd "$CRATE_ROOT" && cargo "${BUILD_ARGS[@]}") >"$BUILD_LOG" 2>&1; then
    echo "CREATOR SMOKE FAIL: cargo ${BUILD_ARGS[*]} failed (profile $PROFILE)"
    echo "--- captured build output ---"
    cat "$BUILD_LOG"
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    fail "profile '$PROFILE' binary not found at $BIN after cargo ${BUILD_ARGS[*]}"
fi

# ---------------------------------------------------------------------------
# Fixtures and assertions.
# ---------------------------------------------------------------------------

install_config() {
    local dest="$1"
    mkdir -p "$dest/config" || fail "cannot create $dest/config"
    cp -R "$CRATE_ROOT/config/." "$dest/config/" ||
        fail "cannot copy the checked-in config/ tree into $dest"
}

install_ruby_pack() {
    local dest="$1"
    mkdir -p "$dest/packs" || fail "cannot create $dest/packs"
    cp -R "$CRATE_ROOT/tests/fixtures/packs/mymod-ruby" "$dest/packs/mymod-ruby" ||
        fail "cannot copy the checked-in mymod-ruby fixture into $dest"
    printf '[packs]\nstack = ["mymod-ruby"]\n' >"$dest/settings.toml" ||
        fail "cannot write $dest/settings.toml"
}

# The five distinct valid 16x16 RGBA PNGs (82 bytes each) reused by the other
# real-binary smokes. The two pack_smoke blobs are deliberately NOT used: their
# IDAT data does not decode in this project's PNG reader.
PNG_A="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGM4wSXynxLMMGrAqAGjBgwXAwAOR+UQCzENtgAAAABJRU5ErkJggg=="
PNG_B="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGMQOSH3nxLMMGrAqAGjBgwXAwD8kPkQN9iRQgAAAABJRU5ErkJggg=="
PNG_C="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGOQ07jznxLMMGrAqAGjBgwXAwChTyEfFJbR5wAAAABJRU5ErkJggg=="
PNG_D="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGM4oaDwnxLMMGrAqAGjBgwXAwAnUgcfZ/f+jQAAAABJRU5ErkJggg=="
PNG_E="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR4nGNQUDjxnxLMMGrAqAGjBgwXAwDXNAcfEvMRpwAAAABJRU5ErkJggg=="

write_valid_png() {
    local path="$1"
    local payload="$2"
    printf '%s' "$payload" | base64 -d >"$path" ||
        fail "cannot decode the embedded PNG fixture into $path"
}

install_texture_pack() {
    local dest="$1"
    install_config "$dest"
    mkdir -p "$dest/packs/textures" || fail "cannot create $dest/packs/textures"
    cat >"$dest/packs/textures/pack.toml" <<'EOF'
format_version = 2
name = "Texture Watch"

[builtin_icons]
apple = "apple.png"
EOF
    write_valid_png "$dest/packs/textures/stone.png" "$PNG_A"
    write_valid_png "$dest/packs/textures/pig_body.png" "$PNG_B"
    write_valid_png "$dest/packs/textures/apple.png" "$PNG_C"
    printf '[packs]\nstack = ["textures"]\n' >"$dest/settings.toml" ||
        fail "cannot write $dest/settings.toml"
}

# Run one headless leg (demo/rejection): both display variables are emptied so
# a window cannot mask the pre-startup contract.
run_headless() {
    local cwd="$1"
    local out="$2"
    local err="$3"
    shift 3
    cd "$cwd" || fail "cannot enter $cwd"
    env DISPLAY= WAYLAND_DISPLAY= "$BIN" "$@" >"$out" 2>"$err"
}

# Start one bounded live leg in the background with the detected display.
start_live() {
    local cwd="$1"
    local seconds="$2"
    local log="$3"
    shift 3
    cd "$cwd" || fail "cannot enter $cwd"
    timeout "$seconds" "$BIN" "$@" >"$log" 2>&1 &
    GAME_PID=$!
}

assert_no_panic() {
    local label="$1"
    local log="$2"
    if grep -qE 'panicked|RUST_BACKTRACE' "$log"; then
        echo "--- $label output ---"
        cat "$log"
        fail "$label: panic text detected"
    fi
}

# Wait for a bounded live leg and require the external timeout kill.
finish_live() {
    local label="$1"
    local log="$2"
    local rc
    wait "$GAME_PID"
    rc=$?
    GAME_PID=""
    if [[ $rc -ne 124 ]]; then
        echo "--- $label output ---"
        cat "$log"
        fail "$label exit code $rc (expected 124 from the external timeout; a clean exit or panic is a failure)"
    fi
    assert_no_panic "$label" "$log"
}

count_regex() {
    local pattern="$1"
    local file="$2"
    local n
    n=$(grep -cE -- "$pattern" "$file" 2>/dev/null || true)
    printf '%s' "${n:-0}"
}

expect_regex_count() {
    local label="$1"
    local expected="$2"
    local file="$3"
    local pattern="$4"
    local n
    n=$(count_regex "$pattern" "$file")
    if [[ "$n" -ne "$expected" ]]; then
        echo "--- $label output ($file) ---"
        cat "$file"
        fail "$label: expected $expected line(s) matching /$pattern/, found $n"
    fi
}

expect_exact_line() {
    local label="$1"
    local expected="$2"
    local file="$3"
    local n
    n=$(grep -cxF -- "$expected" "$file" 2>/dev/null || true)
    if [[ "${n:-0}" -ne 1 ]]; then
        echo "--- $label output ($file) ---"
        cat "$file"
        fail "$label: expected exactly one line [$expected], found ${n:-0}"
    fi
}

expect_stdout_lines() {
    local label="$1"
    local file="$2"
    shift 2
    local -a observed=()
    mapfile -t observed <"$file"
    if [[ ${#observed[@]} -ne $# ]]; then
        echo "--- $label stdout ($file) ---"
        cat "$file"
        fail "$label: expected $# stdout line(s), found ${#observed[@]}"
    fi
    local i=0
    local expected
    for expected in "$@"; do
        if [[ "${observed[$i]}" != "$expected" ]]; then
            echo "CREATOR SMOKE EXPECTED: $expected"
            echo "CREATOR SMOKE OBSERVED: ${observed[$i]}"
            fail "$label: stdout line $((i + 1)) drifted from the pinned contract"
        fi
        i=$((i + 1))
    done
}

assert_tree_unchanged() {
    local label="$1"
    local live="$2"
    local snapshot="$3"
    if ! diff -r "$live" "$snapshot" >/dev/null 2>&1; then
        echo "--- $label diff (live vs snapshot) ---"
        diff -r "$live" "$snapshot" || true
        fail "$label: tree changed: $live"
    fi
}

assert_file_unchanged() {
    local label="$1"
    local live="$2"
    local snapshot="$3"
    cmp -s "$live" "$snapshot" || fail "$label: file changed: $live"
}

assert_absent() {
    local label="$1"
    local path="$2"
    [[ ! -e "$path" ]] || fail "$label: unexpected path created at $path"
}

# Edit ONLY the `[[recipes]]` mymod:ruby_stew block's count 1 -> 2. The id also
# appears in `[[items]]`, so a naive sed over the first id would edit the wrong
# block; this state machine is keyed to the `[[recipes]]` header and the block
# id, and post-conditions pin the stew and pickaxe counts separately.
edit_stew_recipe_count() {
    local toml="$1"
    local tmp="$toml.tmp"

    awk '
        /^\[\[/ { in_recipes = ($0 == "[[recipes]]"); in_stew = 0 }
        in_recipes && /^id = "mymod:ruby_stew"$/ { in_stew = 1 }
        in_recipes && in_stew && /^count = 1$/ { sub(/^count = 1$/, "count = 2") }
        { print }
    ' "$toml" >"$tmp" || fail "cannot rewrite $toml with the recipe edit"
    mv "$tmp" "$toml" || fail "cannot install the edited $toml"

    local stew
    stew=$(awk '
        /^\[\[/ {
            if (in_stew && count != "") { print count; in_stew = 0; exit }
            in_recipes = ($0 == "[[recipes]]"); in_stew = 0; count = ""
        }
        in_recipes && /^id = "mymod:ruby_stew"$/ { in_stew = 1 }
        in_recipes && in_stew && /^count = / { count = $3 }
        END { if (in_stew && count != "") print count }
    ' "$toml")
    [[ "$stew" == "2" ]] || fail "recipe edit: mymod:ruby_stew count is '$stew', expected 2"

    local pickaxe
    pickaxe=$(awk '
        /^\[\[/ {
            if (in_stew && count != "") { print count; in_stew = 0; exit }
            in_recipes = ($0 == "[[recipes]]"); in_stew = 0; count = ""
        }
        in_recipes && /^id = "mymod:ruby_pickaxe"$/ { in_stew = 1 }
        in_recipes && in_stew && /^count = / { count = $3 }
        END { if (in_stew && count != "") print count }
    ' "$toml")
    [[ "$pickaxe" == "1" ]] || fail "recipe edit: mymod:ruby_pickaxe count is '$pickaxe', expected 1"
}

# ---------------------------------------------------------------------------
# Leg A: config audit (debug) or release rejection.
# ---------------------------------------------------------------------------
LEG_A="$(mktemp -d "$TEMP_DIR/leg-a.XXXXXX")" || fail "mktemp failed for leg A"
install_config "$LEG_A"

if [[ "$PROFILE" == "debug" ]]; then
    run_headless "$LEG_A" "$LEG_A/check-ok.out" "$LEG_A/check-ok.err" --check-config
    CHECK_RC=$?
    if [[ $CHECK_RC -ne 0 ]]; then
        cat "$LEG_A/check-ok.out" "$LEG_A/check-ok.err" || true
        fail "leg A: debug --check-config exit code $CHECK_RC (expected 0)"
    fi
    expect_regex_count "leg A valid check-config" 6 "$LEG_A/check-ok.out" '^ok: config/'
    LEG_A_SUMMARY="$(tail -n 1 "$LEG_A/check-ok.out")"
    [[ "$LEG_A_SUMMARY" == "config: files=6 missing=0 warnings=0 errors=0 keys=86 schema=1 digest=0x"* ]] ||
        fail "leg A: summary line drifted: [$LEG_A_SUMMARY]"
    LEG_A_LINES="$(wc -l <"$LEG_A/check-ok.out")"
    [[ "$LEG_A_LINES" -eq 7 ]] ||
        fail "leg A: valid check-config printed $LEG_A_LINES lines (expected 6 ok lines + 1 summary)"

    sed -i 's/^walk_speed = 4$/walk_speed = 0/' "$LEG_A/config/player.toml" ||
        fail "leg A: cannot inject the invalid movement.walk_speed value"
    run_headless "$LEG_A" "$LEG_A/check-invalid.out" "$LEG_A/check-invalid.err" --check-config
    CHECK_RC=$?
    if [[ $CHECK_RC -ne 1 ]]; then
        cat "$LEG_A/check-invalid.out" "$LEG_A/check-invalid.err" || true
        fail "leg A: invalid --check-config exit code $CHECK_RC (expected 1)"
    fi
    expect_exact_line "leg A invalid check-config" \
        'error: config/player.toml: movement.walk_speed: value 0 is below minimum 0.5' \
        "$LEG_A/check-invalid.out"

    cp "$CRATE_ROOT/config/player.toml" "$LEG_A/config/player.toml" ||
        fail "leg A: cannot restore config/player.toml"
else
    run_headless "$LEG_A" "$LEG_A/reject-check-config.out" "$LEG_A/reject-check-config.err" --check-config
    CHECK_RC=$?
    if [[ $CHECK_RC -ne 1 ]]; then
        cat "$LEG_A/reject-check-config.out" "$LEG_A/reject-check-config.err" || true
        fail "leg A release: --check-config exit code $CHECK_RC (expected 1)"
    fi
    expect_stdout_lines "leg A release --check-config" \
        "$LEG_A/reject-check-config.out" \
        'check-config: dev build only'
    [[ ! -s "$LEG_A/reject-check-config.err" ]] ||
        fail "leg A release: --check-config wrote to stderr"
    for entry in settings.toml saves packs; do
        assert_absent "leg A release --check-config" "$LEG_A/$entry"
    done

    run_headless "$LEG_A" "$LEG_A/reject-creator-trace.out" "$LEG_A/reject-creator-trace.err" --creator-trace
    CHECK_RC=$?
    if [[ $CHECK_RC -ne 1 ]]; then
        cat "$LEG_A/reject-creator-trace.out" "$LEG_A/reject-creator-trace.err" || true
        fail "leg A release: --creator-trace exit code $CHECK_RC (expected 1)"
    fi
    expect_stdout_lines "leg A release --creator-trace" \
        "$LEG_A/reject-creator-trace.out" \
        'creator-trace: dev build only'
    [[ ! -s "$LEG_A/reject-creator-trace.err" ]] ||
        fail "leg A release: --creator-trace wrote to stderr"
    for entry in settings.toml saves packs; do
        assert_absent "leg A release --creator-trace" "$LEG_A/$entry"
    done
fi

# ---------------------------------------------------------------------------
# Leg B: live config apply/reject (debug) or bounded release launch.
# ---------------------------------------------------------------------------
if [[ "$PROFILE" == "debug" ]]; then
    LEG_B="$(mktemp -d "$TEMP_DIR/leg-b.XXXXXX")" || fail "mktemp failed for leg B"
    install_config "$LEG_B"

    start_live "$LEG_B" 12 "$LEG_B/live.log" --new --save live.mcvx --creator-trace
    sleep 5
    sed -i 's/^walk_speed = 4$/walk_speed = 8.0/' "$LEG_B/config/player.toml" ||
        fail "leg B: cannot apply the valid walk_speed edit"
    sleep 1.5
    sed -i 's/^walk_speed = 8\.0$/walk_speed = 0/' "$LEG_B/config/player.toml" ||
        fail "leg B: cannot inject the invalid walk_speed edit"
    cp -R "$LEG_B/config" "$LEG_B/config.snapshot" ||
        fail "leg B: cannot snapshot the edited config tree"
    sleep 1.5
    finish_live "leg B live config" "$LEG_B/live.log"

    assert_tree_unchanged "leg B live config" "$LEG_B/config" "$LEG_B/config.snapshot"
    expect_regex_count "leg B live config" 1 "$LEG_B/live.log" \
        '^creator: applied file=config/player\.toml changed=1 digest=0x[0-9a-f]+$'
    expect_exact_line "leg B live config" \
        'creator: rejected file=config/player.toml field=movement.walk_speed reason=value 0 is below minimum 0.5' \
        "$LEG_B/live.log"
    expect_regex_count "leg B live config" 2 "$LEG_B/live.log" '^creator: '
else
    LEG_B="$(mktemp -d "$TEMP_DIR/leg-b-release.XXXXXX")" || fail "mktemp failed for leg B release"
    install_config "$LEG_B"

    start_live "$LEG_B" 5 "$LEG_B/live.log" --new --save live.mcvx
    finish_live "leg B release bounded launch" "$LEG_B/live.log"
    expect_regex_count "leg B release bounded launch" 0 "$LEG_B/live.log" '^creator: '
fi

# ---------------------------------------------------------------------------
# Leg B2: all-domain live config (debug only).
# ---------------------------------------------------------------------------
if [[ "$PROFILE" == "debug" ]]; then
    LEG_B2="$(mktemp -d "$TEMP_DIR/leg-b2.XXXXXX")" || fail "mktemp failed for leg B2"
    install_config "$LEG_B2"

    start_live "$LEG_B2" 14 "$LEG_B2/live.log" --new --save domains.mcvx --creator-trace
    sleep 5
    sed -i 's/^walk_speed = 4$/walk_speed = 8.0/' "$LEG_B2/config/player.toml" ||
        fail "leg B2: cannot edit config/player.toml"
    sed -i 's/^hunger_drain_secs = 30\.0$/hunger_drain_secs = 2.0/' "$LEG_B2/config/survival.toml" ||
        fail "leg B2: cannot edit config/survival.toml"
    sed -i 's/^reach = 3$/reach = 8.0/' "$LEG_B2/config/combat.toml" ||
        fail "leg B2: cannot edit config/combat.toml"
    sed -i 's/^day_seconds = 1200\.0$/day_seconds = 120.0/' "$LEG_B2/config/world.toml" ||
        fail "leg B2: cannot edit config/world.toml"
    sed -i 's/^table_reach = 8\.0$/table_reach = 16.0/' "$LEG_B2/config/mining.toml" ||
        fail "leg B2: cannot edit config/mining.toml"
    sed -i 's/^selected_name_secs = 2\.0$/selected_name_secs = 10.0/' "$LEG_B2/config/render.toml" ||
        fail "leg B2: cannot edit config/render.toml"
    sleep 3
    cp -R "$LEG_B2/config" "$LEG_B2/config.snapshot" ||
        fail "leg B2: cannot snapshot the edited config tree"
    finish_live "leg B2 all-domain live config" "$LEG_B2/live.log"

    assert_tree_unchanged "leg B2 all-domain live config" "$LEG_B2/config" "$LEG_B2/config.snapshot"
    for file in player survival combat world mining render; do
        expect_regex_count "leg B2 all-domain live config" 1 "$LEG_B2/live.log" \
            "^creator: applied file=config/${file}\\.toml changed=1 digest=0x[0-9a-f]+\$"
    done
    expect_regex_count "leg B2 all-domain live config" 0 "$LEG_B2/live.log" '^creator: rejected '
    expect_regex_count "leg B2 all-domain live config" 6 "$LEG_B2/live.log" '^creator: '

    run_headless "$LEG_B2" "$LEG_B2/check.out" "$LEG_B2/check.err" --check-config
    CHECK_RC=$?
    if [[ $CHECK_RC -ne 0 ]]; then
        cat "$LEG_B2/check.out" "$LEG_B2/check.err" || true
        fail "leg B2: post-live --check-config exit code $CHECK_RC (expected 0)"
    fi
    expect_regex_count "leg B2 post-live check-config" 6 "$LEG_B2/check.out" '^ok: config/'
    B2_LAST_LINE="$(grep -E '^creator: applied ' "$LEG_B2/live.log" | tail -n 1 || true)"
    [[ -n "$B2_LAST_LINE" ]] || fail "leg B2: no applied trace line found"
    B2_LAST_DIGEST="${B2_LAST_LINE##*digest=}"
    [[ "$B2_LAST_DIGEST" == "0x18d98c480e113ab1" ]] ||
        fail "leg B2: last applied digest is [$B2_LAST_DIGEST], expected 0x18d98c480e113ab1"
    [[ "$B2_LAST_DIGEST" != "0xeaeac8ddce568dec" ]] ||
        fail "leg B2: last applied digest is the default 0xeaeac8ddce568dec"
    B2_SUMMARY="$(tail -n 1 "$LEG_B2/check.out")"
    [[ "$B2_SUMMARY" == "config: files=6 missing=0 warnings=0 errors=0 keys=86 schema=1 digest=$B2_LAST_DIGEST" ]] ||
        fail "leg B2: post-live summary drifted: [$B2_SUMMARY]"
fi

# ---------------------------------------------------------------------------
# Leg C: live block/mob/builtin-item textures (debug) or bounded release.
# ---------------------------------------------------------------------------
if [[ "$PROFILE" == "debug" ]]; then
    LEG_C="$(mktemp -d "$TEMP_DIR/leg-c.XXXXXX")" || fail "mktemp failed for leg C"
    install_texture_pack "$LEG_C"

    start_live "$LEG_C" 12 "$LEG_C/live.log" --new --save tex.mcvx --creator-trace
    sleep 5
    write_valid_png "$LEG_C/packs/textures/stone.png" "$PNG_D"
    write_valid_png "$LEG_C/packs/textures/pig_body.png" "$PNG_E"
    write_valid_png "$LEG_C/packs/textures/apple.png" "$PNG_A"
    sleep 1.5
    printf 'not a png' >"$LEG_C/packs/textures/stone.png" ||
        fail "leg C: cannot corrupt packs/textures/stone.png"
    cp -R "$LEG_C/packs" "$LEG_C/packs.snapshot" ||
        fail "leg C: cannot snapshot the edited pack tree"
    sleep 1.5
    finish_live "leg C live textures" "$LEG_C/live.log"

    assert_tree_unchanged "leg C live textures" "$LEG_C/packs" "$LEG_C/packs.snapshot"
    expect_regex_count "leg C live textures" 1 "$LEG_C/live.log" \
        '^creator: texture applied path=packs/textures/stone\.png layer=3 digest=0x[0-9a-f]+$'
    expect_regex_count "leg C live textures" 1 "$LEG_C/live.log" \
        '^creator: texture applied path=packs/textures/pig_body\.png layer=32 digest=0x[0-9a-f]+$'
    expect_regex_count "leg C live textures" 1 "$LEG_C/live.log" \
        '^creator: texture applied path=packs/textures/apple\.png layer=1 digest=0x[0-9a-f]+$'
    expect_regex_count "leg C live textures" 1 "$LEG_C/live.log" \
        '^creator: texture rejected path=packs/textures/stone\.png '
    expect_regex_count "leg C live textures" 4 "$LEG_C/live.log" '^creator: '
else
    LEG_C="$(mktemp -d "$TEMP_DIR/leg-c-release.XXXXXX")" || fail "mktemp failed for leg C release"
    install_texture_pack "$LEG_C"
    cp -R "$LEG_C/packs" "$LEG_C/packs.snapshot" ||
        fail "leg C release: cannot snapshot the pack tree"
    cp "$LEG_C/settings.toml" "$LEG_C/settings.snapshot" ||
        fail "leg C release: cannot snapshot settings.toml"

    start_live "$LEG_C" 5 "$LEG_C/live.log" --new --save tex.mcvx
    finish_live "leg C release bounded launch" "$LEG_C/live.log"

    assert_tree_unchanged "leg C release bounded launch" "$LEG_C/packs" "$LEG_C/packs.snapshot"
    assert_file_unchanged "leg C release bounded launch" \
        "$LEG_C/settings.toml" "$LEG_C/settings.snapshot"
    expect_regex_count "leg C release bounded launch" 0 "$LEG_C/live.log" '^creator: '
fi

# ---------------------------------------------------------------------------
# Leg D: restart-required recipe boundary plus the two-run same-save proof.
# ---------------------------------------------------------------------------
LEG_D="$(mktemp -d "$TEMP_DIR/leg-d.XXXXXX")" || fail "mktemp failed for leg D"
install_config "$LEG_D"
install_ruby_pack "$LEG_D"

run_headless "$LEG_D" "$LEG_D/save.out" "$LEG_D/save.err" \
    --demo-restart --new --save recipe.mcvx
SAVE_RC=$?
if [[ $SAVE_RC -ne 0 ]]; then
    cat "$LEG_D/save.out" "$LEG_D/save.err" || true
    fail "leg D: --demo-restart save leg exit code $SAVE_RC (expected 0)"
fi
expect_stdout_lines "leg D save leg" "$LEG_D/save.out" \
    'restart: leg=save recipe=mymod:ruby_stew count=1 day=3 tick=6000 saved=true' \
    'restart: ok'
[[ -f "$LEG_D/recipe.mcvx" ]] || fail "leg D: save leg did not write recipe.mcvx"

if [[ "$PROFILE" == "debug" ]]; then
    start_live "$LEG_D" 10 "$LEG_D/watch.log" --save recipe.mcvx --creator-trace
    sleep 5
    edit_stew_recipe_count "$LEG_D/packs/mymod-ruby/pack.toml"
    cp -R "$LEG_D/packs" "$LEG_D/packs.snapshot" ||
        fail "leg D: cannot snapshot the edited pack tree"
    sleep 1.5
    finish_live "leg D restart watch" "$LEG_D/watch.log"

    assert_tree_unchanged "leg D restart watch" "$LEG_D/packs" "$LEG_D/packs.snapshot"
    expect_exact_line "leg D restart watch" \
        'creator: restart-required reason=mymod-ruby: recipe changed; restart to apply' \
        "$LEG_D/watch.log"
    expect_regex_count "leg D restart watch" 0 "$LEG_D/watch.log" '^creator: texture applied '
    expect_regex_count "leg D restart watch" 1 "$LEG_D/watch.log" '^creator: '
else
    # Release has no watcher leg, but the same-save restart proof still needs
    # the content edit between phase 1 and phase 2.
    edit_stew_recipe_count "$LEG_D/packs/mymod-ruby/pack.toml"
fi

run_headless "$LEG_D" "$LEG_D/load.out" "$LEG_D/load.err" \
    --demo-restart --save recipe.mcvx
LOAD_RC=$?
if [[ $LOAD_RC -ne 0 ]]; then
    cat "$LEG_D/load.out" "$LEG_D/load.err" || true
    fail "leg D: --demo-restart load leg exit code $LOAD_RC (expected 0)"
fi
expect_stdout_lines "leg D load leg" "$LEG_D/load.out" \
    'restart: leg=load recipe=mymod:ruby_stew count=2 preview=2 crafted=2 unresolved=0 day=3 tick=6000 notice=content definitions changed playable=true' \
    'restart: ok'

# ---------------------------------------------------------------------------
# Leg E: v8-to-v9 migration, input untouched, output exactly input+10 bytes.
# ---------------------------------------------------------------------------
LEG_E="$(mktemp -d "$TEMP_DIR/leg-e.XXXXXX")" || fail "mktemp failed for leg E"
install_config "$LEG_E"
cp "$CRATE_ROOT/tests/fixtures/saves/v8-baseline.mcvx" "$LEG_E/v8.mcvx" ||
    fail "leg E: cannot copy the v8 baseline fixture"
cp "$LEG_E/v8.mcvx" "$LEG_E/v8.input.copy" ||
    fail "leg E: cannot snapshot the v8 input bytes"
V8_INPUT_SIZE="$(wc -c <"$LEG_E/v8.mcvx")"

run_headless "$LEG_E" "$LEG_E/v9.out" "$LEG_E/v9.err" --demo-save-v9 --save v8.mcvx
V9_RC=$?
if [[ $V9_RC -ne 0 ]]; then
    cat "$LEG_E/v9.out" "$LEG_E/v9.err" || true
    fail "leg E: --demo-save-v9 exit code $V9_RC (expected 0)"
fi

mapfile -t V9_LINES <"$LEG_E/v9.out"
[[ ${#V9_LINES[@]} -eq 4 ]] ||
    fail "leg E: expected 4 stdout lines, found ${#V9_LINES[@]}"
[[ "${V9_LINES[0]}" == 'save_v9: leg=load version=8 packs=0 saved_config=none notice=none playable=true' ]] ||
    fail "leg E: load line drifted: [${V9_LINES[0]}]"
V9_RESAVE_RE='^save_v9: leg=resave version=9 config_schema=1 config_digest=0x[0-9a-f]+ bytes=[0-9]+$'
[[ "${V9_LINES[1]}" =~ $V9_RESAVE_RE ]] ||
    fail "leg E: resave line drifted: [${V9_LINES[1]}]"
[[ "${V9_LINES[2]}" == 'save_v9: leg=match notice=none playable=true' ]] ||
    fail "leg E: match line drifted: [${V9_LINES[2]}]"
[[ "${V9_LINES[3]}" == 'save_v9: ok' ]] ||
    fail "leg E: ok line drifted: [${V9_LINES[3]}]"

assert_file_unchanged "leg E v8 input" "$LEG_E/v8.mcvx" "$LEG_E/v8.input.copy"
[[ -f "$LEG_E/v8.v9.mcvx" ]] || fail "leg E: derived v8.v9.mcvx was not written"
V9_OUTPUT_SIZE="$(wc -c <"$LEG_E/v8.v9.mcvx")"
V9_EXPECTED_SIZE=$((V8_INPUT_SIZE + 10))
[[ "$V9_OUTPUT_SIZE" -eq "$V9_EXPECTED_SIZE" ]] ||
    fail "leg E: v9 size $V9_OUTPUT_SIZE (expected $V9_EXPECTED_SIZE = input+10)"

# --- One-shot config mismatch (both profiles) ------------------------------
# The derived v9 file is now a saved-config witness. Editing the config makes
# the next load warn exactly once, re-save the same 446-byte shape with the
# new digest, and the third run on that output is silent: the notice is
# one-shot, not repeated on every load.
cp "$LEG_E/v8.v9.mcvx" "$LEG_E/v8.v9.snapshot" ||
    fail "leg E: cannot snapshot the derived v9 output"
V9_SNAPSHOT_SIZE="$(wc -c <"$LEG_E/v8.v9.snapshot")"
[[ "$V9_SNAPSHOT_SIZE" -eq 446 ]] ||
    fail "leg E: v9 snapshot size $V9_SNAPSHOT_SIZE (expected 446)"

sed -i 's/^walk_speed = 4$/walk_speed = 6.0/' "$LEG_E/config/player.toml" ||
    fail "leg E: cannot apply the config-mismatch edit"

run_headless "$LEG_E" "$LEG_E/mismatch.out" "$LEG_E/mismatch.err" --demo-save-v9 --save v8.v9.mcvx
MISMATCH_RC=$?
if [[ $MISMATCH_RC -ne 0 ]]; then
    cat "$LEG_E/mismatch.out" "$LEG_E/mismatch.err" || true
    fail "leg E: config-mismatch --demo-save-v9 exit code $MISMATCH_RC (expected 0)"
fi
mapfile -t MISMATCH_LINES <"$LEG_E/mismatch.out"
[[ ${#MISMATCH_LINES[@]} -eq 4 ]] ||
    fail "leg E: config-mismatch run printed ${#MISMATCH_LINES[@]} stdout lines (expected 4)"
[[ "${MISMATCH_LINES[0]}" == 'save_v9: leg=load version=9 packs=0 saved_config=some notice=config changed since this world was saved playable=true' ]] ||
    fail "leg E: config-mismatch load line drifted: [${MISMATCH_LINES[0]}]"
MISMATCH_RESAVE_RE='^save_v9: leg=resave version=9 config_schema=1 config_digest=0x14092db54f42a9d1 bytes=[0-9]+$'
[[ "${MISMATCH_LINES[1]}" =~ $MISMATCH_RESAVE_RE ]] ||
    fail "leg E: config-mismatch resave line drifted: [${MISMATCH_LINES[1]}]"
MISMATCH_BYTES="${MISMATCH_LINES[1]##*bytes=}"
[[ "$MISMATCH_BYTES" -eq "$V9_SNAPSHOT_SIZE" ]] ||
    fail "leg E: resave bytes $MISMATCH_BYTES (expected snapshot size $V9_SNAPSHOT_SIZE)"
[[ "${MISMATCH_LINES[2]}" == 'save_v9: leg=match notice=none playable=true' ]] ||
    fail "leg E: config-mismatch match line drifted: [${MISMATCH_LINES[2]}]"
[[ "${MISMATCH_LINES[3]}" == 'save_v9: ok' ]] ||
    fail "leg E: config-mismatch ok line drifted: [${MISMATCH_LINES[3]}]"

assert_file_unchanged "leg E config-mismatch input" "$LEG_E/v8.v9.mcvx" "$LEG_E/v8.v9.snapshot"
[[ -f "$LEG_E/v8.v9.v9.mcvx" ]] ||
    fail "leg E: config-mismatch run did not write v8.v9.v9.mcvx"
V9_MISMATCH_OUTPUT_SIZE="$(wc -c <"$LEG_E/v8.v9.v9.mcvx")"
[[ "$V9_MISMATCH_OUTPUT_SIZE" -eq "$V9_SNAPSHOT_SIZE" ]] ||
    fail "leg E: v8.v9.v9.mcvx size $V9_MISMATCH_OUTPUT_SIZE (expected $V9_SNAPSHOT_SIZE)"
expect_regex_count "leg E config mismatch" 1 "$LEG_E/mismatch.out" \
    'notice=config changed since this world was saved'

if [[ "$PROFILE" == "debug" ]]; then
    # `--check-config` is dev-only (Leg A pins the release rejection), so only
    # debug can re-check the mismatched tree's effective digest from outside
    # the save proof. The resave line above already pins the digest above.
    run_headless "$LEG_E" "$LEG_E/check-mismatch.out" "$LEG_E/check-mismatch.err" --check-config
    CHECK_RC=$?
    if [[ $CHECK_RC -ne 0 ]]; then
        cat "$LEG_E/check-mismatch.out" "$LEG_E/check-mismatch.err" || true
        fail "leg E: config-mismatch --check-config exit code $CHECK_RC (expected 0)"
    fi
    expect_regex_count "leg E config-mismatch check-config" 6 "$LEG_E/check-mismatch.out" '^ok: config/'
    LEG_E_SUMMARY="$(tail -n 1 "$LEG_E/check-mismatch.out")"
    [[ "$LEG_E_SUMMARY" == "config: files=6 missing=0 warnings=0 errors=0 keys=86 schema=1 digest=0x14092db54f42a9d1" ]] ||
        fail "leg E: config-mismatch summary drifted: [$LEG_E_SUMMARY]"
fi

run_headless "$LEG_E" "$LEG_E/one-shot.out" "$LEG_E/one-shot.err" --demo-save-v9 --save v8.v9.v9.mcvx
ONE_SHOT_RC=$?
if [[ $ONE_SHOT_RC -ne 0 ]]; then
    cat "$LEG_E/one-shot.out" "$LEG_E/one-shot.err" || true
    fail "leg E: one-shot --demo-save-v9 exit code $ONE_SHOT_RC (expected 0)"
fi
mapfile -t ONE_SHOT_LINES <"$LEG_E/one-shot.out"
[[ ${#ONE_SHOT_LINES[@]} -eq 4 ]] ||
    fail "leg E: one-shot run printed ${#ONE_SHOT_LINES[@]} stdout lines (expected 4)"
[[ "${ONE_SHOT_LINES[0]}" == 'save_v9: leg=load version=9 packs=0 saved_config=some notice=none playable=true' ]] ||
    fail "leg E: one-shot load line drifted: [${ONE_SHOT_LINES[0]}]"
[[ "${ONE_SHOT_LINES[1]}" =~ $MISMATCH_RESAVE_RE ]] ||
    fail "leg E: one-shot resave line drifted: [${ONE_SHOT_LINES[1]}]"
[[ "${ONE_SHOT_LINES[2]}" == 'save_v9: leg=match notice=none playable=true' ]] ||
    fail "leg E: one-shot match line drifted: [${ONE_SHOT_LINES[2]}]"
[[ "${ONE_SHOT_LINES[3]}" == 'save_v9: ok' ]] ||
    fail "leg E: one-shot ok line drifted: [${ONE_SHOT_LINES[3]}]"

# ---------------------------------------------------------------------------
# Leg F: release hygiene (release only).
# ---------------------------------------------------------------------------
if [[ "$PROFILE" == "release" ]]; then
    LEG_F_EMPTY="$(mktemp -d "$TEMP_DIR/leg-f-empty.XXXXXX")" ||
        fail "mktemp failed for leg F empty cwd"
    start_live "$LEG_F_EMPTY" 5 "$LEG_F_EMPTY/live.log"
    finish_live "leg F empty-cwd launch" "$LEG_F_EMPTY/live.log"
    assert_absent "leg F empty-cwd launch" "$LEG_F_EMPTY/packs"
    assert_absent "leg F empty-cwd launch" "$LEG_F_EMPTY/config"

    if [[ -d "$CRATE_ROOT/packs" ]]; then
        for folder in mymod-ruby ruby-overlay uat-sound; do
            assert_absent "leg F crate pack leak" "$CRATE_ROOT/packs/$folder"
        done
    fi

    LEG_F_PACK="$(mktemp -d "$TEMP_DIR/leg-f-pack.XXXXXX")" ||
        fail "mktemp failed for leg F config+pack cwd"
    install_config "$LEG_F_PACK"
    install_ruby_pack "$LEG_F_PACK"
    cp -R "$LEG_F_PACK/config" "$LEG_F_PACK/config.snapshot" ||
        fail "leg F: cannot snapshot config"
    cp -R "$LEG_F_PACK/packs" "$LEG_F_PACK/packs.snapshot" ||
        fail "leg F: cannot snapshot packs"
    cp "$LEG_F_PACK/settings.toml" "$LEG_F_PACK/settings.snapshot" ||
        fail "leg F: cannot snapshot settings.toml"

    start_live "$LEG_F_PACK" 5 "$LEG_F_PACK/live.log"
    finish_live "leg F config+pack launch" "$LEG_F_PACK/live.log"

    assert_tree_unchanged "leg F config+pack launch" "$LEG_F_PACK/config" "$LEG_F_PACK/config.snapshot"
    assert_tree_unchanged "leg F config+pack launch" "$LEG_F_PACK/packs" "$LEG_F_PACK/packs.snapshot"
    assert_file_unchanged "leg F config+pack launch" \
        "$LEG_F_PACK/settings.toml" "$LEG_F_PACK/settings.snapshot"
    expect_regex_count "leg F config+pack launch" 0 "$LEG_F_PACK/live.log" '^creator: '
fi

echo "CREATOR SMOKE PASS [profile=$PROFILE]: doc gate, config audit/rejection, live config apply/reject, all-domain live config (six files), block/mob/builtin-item texture reload, restart-required recipe boundary, same-save two-run restart, v8-to-v9 migration with one-shot config mismatch, and profile-appropriate release hygiene all passed"
