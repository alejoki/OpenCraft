#!/usr/bin/env bash
# Smoke-test the per-user Windows installer (D290).
#
# Usage: bash scripts/smoke_windows_installer.sh <version>
#
# Runs under Git Bash on Windows after scripts/build_windows_installer.sh has
# produced dist/opencraft-<version>-x86_64-pc-windows-msvc-setup.exe. It
# performs one silent per-user install into dist/install-smoke, proves the
# installed tree carries the binary, config/, worldgen/, LICENSE and the
# uninstaller, then runs the headless --demo-ui seam from the installed
# directory (the same three-line ok=true contract package_release.sh uses):
#
#   --demo-ui   exit 0, exactly 3 lines, last ends ok=true
#
# The installed directory is left in place for inspection; it lives under
# gitignored dist/.
#
# Dependencies: bash, cygpath, grep, tail, wc.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

fail() {
    echo "smoke_windows_installer: FAIL $*" >&2
    exit 1
}

if [[ $# -ne 1 ]]; then
    fail "usage: smoke_windows_installer.sh <version>"
fi

VERSION="$1"
[[ -n "$VERSION" ]] || fail "version must not be empty"

command -v cygpath >/dev/null 2>&1 ||
    fail "cygpath not found; this script must run under Git Bash on Windows"

SETUP="$CRATE_ROOT/dist/opencraft-$VERSION-x86_64-pc-windows-msvc-setup.exe"
[[ -f "$SETUP" ]] || fail "missing $SETUP (run build_windows_installer.sh first)"

INSTALL_DIR="$CRATE_ROOT/dist/install-smoke"
LOG="$CRATE_ROOT/dist/install-smoke.log"
OUT="$CRATE_ROOT/dist/install-smoke-demo-ui.out"

rm -rf "$INSTALL_DIR"
rm -f "$LOG" "$OUT"

INSTALL_WIN="$(cygpath -w "$INSTALL_DIR")"
LOG_WIN="$(cygpath -w "$LOG")"

rc=0
MSYS2_ARG_CONV_EXCL='*' "$SETUP" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART \
    "/DIR=$INSTALL_WIN" "/LOG=$LOG_WIN" || rc=$?
if [[ "$rc" -ne 0 ]]; then
    [[ -f "$LOG" ]] && cat "$LOG" >&2
    fail "silent install exited $rc (expected 0)"
fi

for required in opencraft.exe config/combat.toml worldgen/villages/plains.toml LICENSE unins000.exe; do
    [[ -f "$INSTALL_DIR/$required" ]] ||
        fail "installed tree is missing $required"
done

rc=0
(cd "$INSTALL_DIR" && ./opencraft.exe --demo-ui >"$OUT" 2>&1) || rc=$?
if [[ "$rc" -ne 0 ]]; then
    cat "$OUT" >&2
    fail "installed --demo-ui exited $rc (expected 0)"
fi

lines="$(wc -l <"$OUT" | tr -d ' ')"
[[ "$lines" == "3" ]] || {
    cat "$OUT" >&2
    fail "installed --demo-ui printed $lines lines (expected 3)"
}
tail -n 1 "$OUT" | grep -Eq 'ok=true$' || {
    cat "$OUT" >&2
    fail "installed --demo-ui last line does not end with ok=true"
}

echo "smoke_windows_installer: ok"
