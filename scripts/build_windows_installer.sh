#!/usr/bin/env bash
# Build the per-user Windows installer (D290).
#
# Usage: bash scripts/build_windows_installer.sh <version>
#
# Runs under Git Bash on Windows after scripts/package_release.sh has staged
# dist/opencraft-<version>-x86_64-pc-windows-msvc/. Finds Inno Setup's
# ISCC.exe (installing Inno Setup through Chocolatey when the runner does not
# already provide it), compiles installer/opencraft.iss, and proves the setup
# exe was written:
#
#   dist/opencraft-<version>-x86_64-pc-windows-msvc-setup.exe
#
# The ISCC /D arguments are Windows paths and must survive Git Bash's POSIX
# path conversion, which would turn "/DStageDir=..." into "D:\..."; the
# MSYS2_ARG_CONV_EXCL='*' prefix on the invocation disables that translation.
#
# Dependencies: bash, cygpath, Inno Setup 6.3+ (ISCC.exe), choco if ISCC is
# missing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

fail() {
    echo "build_windows_installer: FAIL $*" >&2
    exit 1
}

if [[ $# -ne 1 ]]; then
    fail "usage: build_windows_installer.sh <version>"
fi

VERSION="$1"
[[ -n "$VERSION" ]] || fail "version must not be empty"

command -v cygpath >/dev/null 2>&1 ||
    fail "cygpath not found; this script must run under Git Bash on Windows"

STAGE="$CRATE_ROOT/dist/opencraft-$VERSION-x86_64-pc-windows-msvc"
[[ -d "$STAGE" ]] || fail "missing staged tree $STAGE (run package_release.sh first)"
[[ -f "$STAGE/opencraft.exe" ]] ||
    fail "missing $STAGE/opencraft.exe (run package_release.sh first)"
[[ -f "$STAGE/LICENSE" ]] || fail "missing $STAGE/LICENSE (run package_release.sh first)"
[[ -d "$STAGE/config" ]] || fail "missing $STAGE/config (run package_release.sh first)"
[[ -d "$STAGE/worldgen" ]] || fail "missing $STAGE/worldgen (run package_release.sh first)"

find_iscc() {
    if command -v iscc >/dev/null 2>&1; then
        command -v iscc
        return 0
    fi
    local base candidate
    for base in "/c/Program Files (x86)" "/c/Program Files"; do
        for candidate in "$base"/Inno\ Setup\ */ISCC.exe; do
            if [[ -f "$candidate" ]]; then
                echo "$candidate"
                return 0
            fi
        done
    done
    return 1
}

ISCC="$(find_iscc || true)"
if [[ -z "$ISCC" ]]; then
    command -v choco >/dev/null 2>&1 ||
        fail "ISCC.exe not found and choco is unavailable; install Inno Setup 6.3+"
    echo "build_windows_installer: ISCC.exe missing; installing Inno Setup via choco"
    choco install innosetup --yes --no-progress ||
        fail "choco install innosetup failed"
    ISCC="$(find_iscc || true)"
fi
[[ -n "$ISCC" ]] || fail "ISCC.exe still not found after installing Inno Setup"

SCRIPT_WIN="$(cygpath -w "$CRATE_ROOT/installer/opencraft.iss")"
STAGE_WIN="$(cygpath -w "$STAGE")"
OUT_WIN="$(cygpath -w "$CRATE_ROOT/dist")"
SETUP="$CRATE_ROOT/dist/opencraft-$VERSION-x86_64-pc-windows-msvc-setup.exe"

rm -f "$SETUP"
MSYS2_ARG_CONV_EXCL='*' "$ISCC" \
    "/DMyAppVersion=$VERSION" \
    "/DStageDir=$STAGE_WIN" \
    "/DOutDir=$OUT_WIN" \
    "$SCRIPT_WIN" ||
    fail "ISCC failed for $SCRIPT_WIN"

[[ -s "$SETUP" ]] || fail "ISCC reported success but $SETUP is missing or empty"

echo "build_windows_installer: $SETUP"
