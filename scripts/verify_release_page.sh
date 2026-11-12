#!/usr/bin/env bash
# Verify the published GitHub release page after publishing (M016/S10-T02).
#
# Usage: bash scripts/verify_release_page.sh <tag> <version>
#
# Calls `gh release view <tag> --json tagName,assets,body` and asserts:
#   - tagName equals the requested tag
#   - the asset name set is exactly the two D269 archives
#   - both asset sizes are greater than zero
#   - the release body contains `SmartScreen` and `version 9`
#
# Prints one machine-readable latch line on success and one readable FAIL line
# on any mismatch. Dependencies: bash, gh, python3.

set -euo pipefail

fail() {
    echo "verify_release_page: FAIL $*" >&2
    exit 1
}

if [[ $# -ne 2 ]]; then
    fail "usage: verify_release_page.sh <tag> <version>"
fi

TAG="$1"
VERSION="$2"

command -v gh >/dev/null 2>&1 || fail "gh not found on PATH"
command -v python3 >/dev/null 2>&1 || fail "python3 not found on PATH"

JSON="$(gh release view "$TAG" --json tagName,assets,body 2>&1)" ||
    fail "gh release view $TAG failed: $JSON"

printf '%s' "$JSON" | python3 -c '
import json
import sys

tag = sys.argv[1]
version = sys.argv[2]
try:
    payload = json.load(sys.stdin)
except json.JSONDecodeError as error:
    print(f"verify_release_page: FAIL malformed gh JSON: {error}", file=sys.stderr)
    sys.exit(1)

actual_tag = payload.get("tagName")
if actual_tag != tag:
    print(
        f"verify_release_page: FAIL tagName {actual_tag!r} != {tag!r}",
        file=sys.stderr,
    )
    sys.exit(1)

expected = {
    f"opencraft-{version}-x86_64-unknown-linux-gnu.tar.gz",
    f"opencraft-{version}-x86_64-pc-windows-msvc.zip",
}
assets = payload.get("assets") or []
names = {asset.get("name") for asset in assets}
if names != expected:
    print(
        "verify_release_page: FAIL asset set mismatch: expected "
        + ", ".join(sorted(expected))
        + "; found "
        + ", ".join(sorted(name for name in names if name)),
        file=sys.stderr,
    )
    sys.exit(1)
for asset in assets:
    if int(asset.get("size") or 0) <= 0:
        asset_name = asset.get("name")
        print(
            f"verify_release_page: FAIL asset {asset_name!r} is zero bytes",
            file=sys.stderr,
        )
        sys.exit(1)

body = payload.get("body") or ""
missing = [needle for needle in ("SmartScreen", "version 9") if needle not in body]
if missing:
    print(
        "verify_release_page: FAIL release body is missing: " + ", ".join(missing),
        file=sys.stderr,
    )
    sys.exit(1)
' "$TAG" "$VERSION" || exit 1

echo "verify_release_page: tag=$TAG assets=2 ok"
