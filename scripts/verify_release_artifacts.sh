#!/usr/bin/env bash
# Re-verify downloaded release archives before/after publishing (M016/S10-T02).
#
# Usage: bash scripts/verify_release_artifacts.sh <dist_dir> <version>
#
# The directory must contain exactly the three release assets (any extra
# entry is a failure, so a stale or misnamed upload cannot pass):
#   opencraft-<version>-x86_64-unknown-linux-gnu.tar.gz
#   opencraft-<version>-x86_64-pc-windows-msvc.zip
#   opencraft-<version>-x86_64-pc-windows-msvc-setup.exe
#
# Linux: extract, require an executable `opencraft` plus `config/combat.toml`,
# `worldgen/villages/plains.toml` and `LICENSE`, then run three headless
# seams from the extracted directory with DISPLAY/WAYLAND_DISPLAY removed:
#   --demo-ui        exit 0, exactly 3 lines, last ends ok=true
#   --demo-village   exit 0, exactly 12 lines, last = village_demo: ok=true
#   --validate-pack  exit 0, exact 12-entry summary (fixture pack copied into
#                    a scratch packs/ inside the temp extraction, never into
#                    the archive)
# Windows: a python3 zipfile reader requires `opencraft.exe`, the same config
# and worldgen files, and `LICENSE` at the archive root, and rejects any
# `packs/`, `saves/`, `settings.toml` entry or wrapper directory. The exe must
# not import VCRUNTIME140.dll: D290 builds the release with a static CRT so a
# clean Windows box needs no VC++ redistributable.
# Installer: the setup exe must be a PE image whose version resource names
# OpenCraft and <version> in UTF-16LE.
#
# Dependencies: bash, python3, tar, mktemp, timeout, cp, wc, tail, grep.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

fail() {
    echo "verify_release: FAIL $*" >&2
    exit 1
}

if [[ $# -ne 2 ]]; then
    fail "usage: verify_release_artifacts.sh <dist_dir> <version>"
fi

DIST="$1"
VERSION="$2"

[[ -d "$DIST" ]] || fail "dist directory not found: $DIST"

LINUX_NAME="opencraft-$VERSION-x86_64-unknown-linux-gnu.tar.gz"
WINDOWS_NAME="opencraft-$VERSION-x86_64-pc-windows-msvc.zip"
INSTALLER_NAME="opencraft-$VERSION-x86_64-pc-windows-msvc-setup.exe"
LINUX="$DIST/$LINUX_NAME"
WINDOWS="$DIST/$WINDOWS_NAME"
INSTALLER="$DIST/$INSTALLER_NAME"

found_linux=0
found_windows=0
found_installer=0
unexpected=()
shopt -s nullglob
for entry in "$DIST"/*; do
    name="$(basename "$entry")"
    case "$name" in
        "$LINUX_NAME") found_linux=1 ;;
        "$WINDOWS_NAME") found_windows=1 ;;
        "$INSTALLER_NAME") found_installer=1 ;;
        *) unexpected+=("$name") ;;
    esac
done
[[ "$found_linux" -eq 1 ]] || fail "missing $LINUX_NAME in $DIST"
[[ "$found_windows" -eq 1 ]] || fail "missing $WINDOWS_NAME in $DIST"
[[ "$found_installer" -eq 1 ]] || fail "missing $INSTALLER_NAME in $DIST"
if [[ ${#unexpected[@]} -gt 0 ]]; then
    fail "unexpected entries in $DIST: ${unexpected[*]}"
fi
[[ -s "$LINUX" ]] || fail "empty archive: $LINUX"
[[ -s "$WINDOWS" ]] || fail "empty archive: $WINDOWS"
[[ -s "$INSTALLER" ]] || fail "empty installer: $INSTALLER"

LINUX_DIR="$(mktemp -d "${TMPDIR:-/tmp}/opencraft-verify.XXXXXX")" ||
    fail "mktemp failed"
cleanup() {
    rm -rf "$LINUX_DIR"
}
trap cleanup EXIT

tar -xzf "$LINUX" -C "$LINUX_DIR" || fail "cannot extract $LINUX"
[[ -f "$LINUX_DIR/opencraft" ]] || fail "linux archive is missing opencraft"
[[ -x "$LINUX_DIR/opencraft" ]] || fail "linux archive's opencraft is not executable"
for required in config/combat.toml worldgen/villages/plains.toml LICENSE; do
    [[ -f "$LINUX_DIR/$required" ]] ||
        fail "linux archive is missing $required at the archive root"
done

mkdir -p "$LINUX_DIR/packs" || fail "cannot create scratch packs/ for the fixture"
cp -a "$CRATE_ROOT/tests/fixtures/packs/mymod-ruby" "$LINUX_DIR/packs/mymod-ruby" ||
    fail "cannot copy the fixture pack"

run_headless() {
    local label="$1"
    shift
    local out="$LINUX_DIR/$label.out"
    local rc=0
    (cd "$LINUX_DIR" && env -u DISPLAY -u WAYLAND_DISPLAY timeout 120 "./opencraft" "$@" >"$out" 2>&1) || rc=$?
    echo "$rc"
}

check_lines() {
    local label="$1"
    local expected="$2"
    local out="$LINUX_DIR/$label.out"
    local actual
    actual="$(wc -l <"$out" | tr -d ' ')"
    [[ "$actual" == "$expected" ]] || {
        cat "$out" >&2
        fail "$label printed $actual lines (expected $expected)"
    }
}

rc="$(run_headless demo-ui --demo-ui)"
[[ "$rc" == "0" ]] || {
    cat "$LINUX_DIR/demo-ui.out" >&2
    fail "linux --demo-ui exited $rc (expected 0)"
}
check_lines demo-ui 3
tail -n 1 "$LINUX_DIR/demo-ui.out" | grep -Eq 'ok=true$' || {
    cat "$LINUX_DIR/demo-ui.out" >&2
    fail "linux --demo-ui last line does not end with ok=true"
}

rc="$(run_headless demo-village --demo-village)"
[[ "$rc" == "0" ]] || {
    cat "$LINUX_DIR/demo-village.out" >&2
    fail "linux --demo-village exited $rc (expected 0)"
}
check_lines demo-village 12
[[ "$(tail -n 1 "$LINUX_DIR/demo-village.out")" == "village_demo: ok=true" ]] || {
    cat "$LINUX_DIR/demo-village.out" >&2
    fail "linux --demo-village last line is not 'village_demo: ok=true'"
}

rc="$(run_headless validate-pack --validate-pack mymod-ruby)"
[[ "$rc" == "0" ]] || {
    cat "$LINUX_DIR/validate-pack.out" >&2
    fail "linux --validate-pack mymod-ruby exited $rc (expected 0)"
}
[[ "$(tail -n 1 "$LINUX_DIR/validate-pack.out")" == \
    "validate-pack: mymod-ruby: 12 entries, 0 errors, 0 warnings" ]] || {
    cat "$LINUX_DIR/validate-pack.out" >&2
    fail "linux --validate-pack mymod-ruby did not print the exact 12-entry summary"
}

echo "verify_release: linux=ok"

python3 - "$WINDOWS" <<'PY' || exit 1
import sys
import zipfile

path = sys.argv[1]
required = {
    "opencraft.exe",
    "config/combat.toml",
    "worldgen/villages/plains.toml",
    "LICENSE",
}
allowed_top = {"opencraft.exe", "config", "worldgen", "LICENSE"}

try:
    with zipfile.ZipFile(path) as archive:
        # Compress-Archive on Windows can store either separator; normalize so
        # the root checks and the wrapper/packs rejection see one form.
        names = [
            name.replace("\\", "/")
            for name in archive.namelist()
            if not name.endswith(("/", "\\"))
        ]
        if not names:
            print(f"verify_release: FAIL windows archive is empty: {path}", file=sys.stderr)
            sys.exit(1)
        missing = sorted(required - set(names))
        if missing:
            print(
                "verify_release: FAIL windows archive is missing at the root: "
                + ", ".join(missing),
                file=sys.stderr,
            )
            sys.exit(1)
        for name in names:
            if name.startswith("packs/") or name.startswith("saves/") or name == "settings.toml":
                print(
                    f"verify_release: FAIL windows archive contains player state: {name}",
                    file=sys.stderr,
                )
                sys.exit(1)
            top = name.split("/", 1)[0]
            if top not in allowed_top:
                print(
                    "verify_release: FAIL windows archive has an unexpected entry "
                    f"(wrapper directory or extra file): {name}",
                    file=sys.stderr,
                )
                sys.exit(1)
        exe = archive.read("opencraft.exe")
        if not exe.startswith(b"MZ"):
            print(
                "verify_release: FAIL windows opencraft.exe is not a PE image",
                file=sys.stderr,
            )
            sys.exit(1)
        if b"vcruntime140" in exe.lower():
            print(
                "verify_release: FAIL windows opencraft.exe still imports "
                "VCRUNTIME140.dll; the release build must use a static CRT (D290)",
                file=sys.stderr,
            )
            sys.exit(1)
except zipfile.BadZipFile as error:
    print(f"verify_release: FAIL windows archive is not a zip: {error}", file=sys.stderr)
    sys.exit(1)
PY

echo "verify_release: windows=ok"

python3 - "$INSTALLER" "$VERSION" <<'PY' || exit 1
import struct
import sys

path = sys.argv[1]
version = sys.argv[2]
try:
    with open(path, "rb") as setup:
        data = setup.read()
except OSError as error:
    print(f"verify_release: FAIL cannot read installer: {error}", file=sys.stderr)
    sys.exit(1)

if len(data) < 1024 * 1024:
    print(
        f"verify_release: FAIL installer is only {len(data)} bytes; "
        "expected a compiled setup exe",
        file=sys.stderr,
    )
    sys.exit(1)
if data[:2] != b"MZ" or len(data) < 0x40:
    print("verify_release: FAIL installer is not a PE image", file=sys.stderr)
    sys.exit(1)
pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
if data[pe_offset : pe_offset + 4] != b"PE\0\0":
    print("verify_release: FAIL installer has no PE header", file=sys.stderr)
    sys.exit(1)


def utf16(text):
    return text.encode("utf-16-le")


if utf16("OpenCraft") not in data:
    print(
        "verify_release: FAIL installer version resource does not name OpenCraft",
        file=sys.stderr,
    )
    sys.exit(1)
if utf16(version) not in data and utf16(version + ".0") not in data:
    print(
        f"verify_release: FAIL installer version resource does not carry {version}",
        file=sys.stderr,
    )
    sys.exit(1)
PY

echo "verify_release: installer=ok"
echo "verify_release: ok"
