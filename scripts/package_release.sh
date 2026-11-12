#!/usr/bin/env bash
# Package one release archive from the debug/release binary tree (M016/S10-T02).
#
# Usage: bash scripts/package_release.sh <version> <triple>
#   e.g. bash scripts/package_release.sh 0.2.0 x86_64-unknown-linux-gnu
#
# Stages the portable folder required by D269 -- the release binary, the
# tracked `config/` and `worldgen/` trees, and `LICENSE` -- then proves the
# staged copy headlessly from a fresh temp CWD before archiving it:
#   (a) --demo-ui         exits 0 with exactly 3 lines whose last ends ok=true
#   (b) --demo-village    exits 0 with exactly 12 lines, last = village_demo: ok=true
#   (c) --validate-pack   exits 0 with the exact 12-entry summary line
#
# Player state is never staged: no `packs/`, `saves/`, `settings.toml`,
# `tests/`, `target/`, or the staged directory itself. The archive is written
# to `dist/` (gitignored) and the staged directory is kept for inspection.
#
# Dependencies: bash, cp, mktemp, tar (Linux triple) or powershell.exe (Windows
# triple under Git Bash); `timeout` is used when present.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

fail() {
    echo "package_release: FAIL $*" >&2
    exit 1
}

if [[ $# -ne 2 ]]; then
    fail "usage: package_release.sh <version> <triple>"
fi

VERSION="$1"
TRIPLE="$2"

if [[ -z "$VERSION" || -z "$TRIPLE" ]]; then
    fail "version and triple must both be non-empty"
fi

BIN="$CRATE_ROOT/target/release/opencraft"
if [[ "$TRIPLE" == *windows* ]]; then
    BIN="$BIN.exe"
fi
[[ -f "$BIN" && -x "$BIN" ]] ||
    fail "missing release binary $BIN (run cargo build --release --locked first)"
BIN_NAME="$(basename "$BIN")"

for input in config worldgen LICENSE; do
    [[ -e "$CRATE_ROOT/$input" ]] || fail "missing tracked input $CRATE_ROOT/$input"
done
[[ -d "$CRATE_ROOT/tests/fixtures/packs/mymod-ruby" ]] ||
    fail "missing fixture pack tests/fixtures/packs/mymod-ruby"

DIST="$CRATE_ROOT/dist"
STAGE="$DIST/opencraft-$VERSION-$TRIPLE"

rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -a "$BIN" "$STAGE/$BIN_NAME"
cp -a "$CRATE_ROOT/config" "$STAGE/config"
cp -a "$CRATE_ROOT/worldgen" "$STAGE/worldgen"
cp -a "$CRATE_ROOT/LICENSE" "$STAGE/LICENSE"

SMOKE="$(mktemp -d "${TMPDIR:-/tmp}/opencraft-package.XXXXXX")" ||
    fail "mktemp failed"
cleanup() {
    rm -rf "$SMOKE"
}
trap cleanup EXIT

cp -a "$STAGE/." "$SMOKE/" || fail "cannot copy the staged tree into $SMOKE"
mkdir -p "$SMOKE/packs" || fail "cannot create $SMOKE/packs"
cp -a "$CRATE_ROOT/tests/fixtures/packs/mymod-ruby" "$SMOKE/packs/mymod-ruby" ||
    fail "cannot copy the fixture pack into the smoke tree"

# One headless run from the staged tree. Display variables are removed so a
# binary that tried to open a window would fail instead of silently passing.
# `timeout` is optional: Git Bash may not ship it, and the demos are bounded.
run_headless() {
    local label="$1"
    shift
    local out="$SMOKE/$label.out"
    local rc=0
    if command -v timeout >/dev/null 2>&1; then
        (cd "$SMOKE" && env -u DISPLAY -u WAYLAND_DISPLAY timeout 120 "./$BIN_NAME" "$@" >"$out" 2>&1) || rc=$?
    else
        (cd "$SMOKE" && env -u DISPLAY -u WAYLAND_DISPLAY "./$BIN_NAME" "$@" >"$out" 2>&1) || rc=$?
    fi
    echo "$rc"
}

check_lines() {
    local label="$1"
    local expected="$2"
    local out="$SMOKE/$label.out"
    local actual
    actual="$(wc -l <"$out" | tr -d ' ')"
    [[ "$actual" == "$expected" ]] || {
        cat "$out" >&2
        fail "$label printed $actual lines (expected $expected)"
    }
}

# (a) --demo-ui: 3 lines, last ends ok=true.
rc="$(run_headless demo-ui --demo-ui)"
[[ "$rc" == "0" ]] || {
    cat "$SMOKE/demo-ui.out" >&2
    fail "--demo-ui exited $rc (expected 0)"
}
check_lines demo-ui 3
tail -n 1 "$SMOKE/demo-ui.out" | grep -Eq 'ok=true$' || {
    cat "$SMOKE/demo-ui.out" >&2
    fail "--demo-ui last line does not end with ok=true"
}

# (b) --demo-village: 12 lines, last exactly village_demo: ok=true.
rc="$(run_headless demo-village --demo-village)"
[[ "$rc" == "0" ]] || {
    cat "$SMOKE/demo-village.out" >&2
    fail "--demo-village exited $rc (expected 0)"
}
check_lines demo-village 12
[[ "$(tail -n 1 "$SMOKE/demo-village.out")" == "village_demo: ok=true" ]] || {
    cat "$SMOKE/demo-village.out" >&2
    fail "--demo-village last line is not 'village_demo: ok=true'"
}

# (c) --validate-pack: exact 12-entry summary line.
rc="$(run_headless validate-pack --validate-pack mymod-ruby)"
[[ "$rc" == "0" ]] || {
    cat "$SMOKE/validate-pack.out" >&2
    fail "--validate-pack mymod-ruby exited $rc (expected 0)"
}
[[ "$(tail -n 1 "$SMOKE/validate-pack.out")" == \
    "validate-pack: mymod-ruby: 12 entries, 0 errors, 0 warnings" ]] || {
    cat "$SMOKE/validate-pack.out" >&2
    fail "--validate-pack mymod-ruby did not print the exact 12-entry summary"
}

mkdir -p "$DIST"

if [[ "$TRIPLE" == *windows* ]]; then
    command -v powershell.exe >/dev/null 2>&1 ||
        fail "powershell.exe not found on PATH (Windows packaging needs Git Bash on Windows)"
    ARCHIVE="$DIST/opencraft-$VERSION-$TRIPLE.zip"
    STAGE_ARG="$STAGE"
    OUT_ARG="$ARCHIVE"
    if command -v cygpath >/dev/null 2>&1; then
        STAGE_ARG="$(cygpath -w "$STAGE")"
        OUT_ARG="$(cygpath -w "$ARCHIVE")"
    fi
    rm -f "$ARCHIVE"
    powershell.exe -NoProfile -NonInteractive -Command \
        "Compress-Archive -Path '$STAGE_ARG/*' -DestinationPath '$OUT_ARG' -Force" ||
        fail "Compress-Archive failed for $ARCHIVE"
else
    ARCHIVE="$DIST/opencraft-$VERSION-$TRIPLE.tar.gz"
    rm -f "$ARCHIVE"
    tar -czf "$ARCHIVE" -C "$STAGE" "$BIN_NAME" config worldgen LICENSE ||
        fail "tar failed for $ARCHIVE"
fi

[[ -s "$ARCHIVE" ]] || fail "archive missing or empty: $ARCHIVE"
BYTES="$(wc -c <"$ARCHIVE" | tr -d ' ')"

echo "package_release: archive=$ARCHIVE bytes=$BYTES"
echo "package_release: ok"
