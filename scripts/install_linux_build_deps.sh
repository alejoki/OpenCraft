#!/usr/bin/env bash
# Install the Linux build dependencies (ALSA headers + pkg-config) needed by
# alsa-sys/rodio for CI and release builds.
#
# GitHub-hosted runners point apt at mirror+file:/etc/apt/apt-mirrors.txt, whose
# primary entry (azure.archive.ubuntu.com) resolves through a Traffic Manager
# CNAME chain. That chain intermittently fails DNS on the runner
# ("Temporary failure resolving 'azure.archive.ubuntu.com'"), which used to
# spend ~2.5 minutes per attempt and then fail the whole release. This script:
#   * exits early when ALSA is already discoverable via pkg-config;
#   * retries with short, bounded apt timeouts;
#   * rewrites apt-mirrors.txt to the direct archive.ubuntu.com endpoint
#     (plain A records, no CNAME chain) after the first failure;
#   * verifies pkg-config can see alsa before reporting success.
set -uo pipefail

ATTEMPTS=${ATTEMPTS:-6}
SLEEP_SECS=${SLEEP_SECS:-15}
MIRRORS_FILE=${MIRRORS_FILE:-/etc/apt/apt-mirrors.txt}
FALLBACK_MIRROR=${FALLBACK_MIRROR:-http://archive.ubuntu.com/ubuntu/}

as_root() {
  if [ "$(id -u)" = "0" ]; then
    "$@"
  else
    sudo "$@"
  fi
}

apt_run() {
  as_root apt-get \
    -o Acquire::Retries=3 \
    -o Acquire::http::Timeout=20 \
    -o Acquire::https::Timeout=20 \
    "$@"
}

if pkg-config --exists alsa 2>/dev/null; then
  echo "build_deps: alsa already discoverable via pkg-config, nothing to install"
  exit 0
fi

rewrite_mirror() {
  if [ -f "$MIRRORS_FILE" ]; then
    echo "build_deps: rewriting $MIRRORS_FILE to $FALLBACK_MIRROR" >&2
    printf '%s\n' "$FALLBACK_MIRROR" | as_root tee "$MIRRORS_FILE" >/dev/null
  else
    echo "build_deps: $MIRRORS_FILE not present, keeping default apt sources" >&2
  fi
}

attempt=1
while [ "$attempt" -le "$ATTEMPTS" ]; do
  echo "build_deps: apt attempt $attempt/$ATTEMPTS (mirror: $(head -n 1 "$MIRRORS_FILE" 2>/dev/null || echo default))"
  if apt_run update && apt_run install -y libasound2-dev pkg-config; then
    if pkg-config --exists alsa; then
      echo "build_deps: linux build dependencies installed on attempt $attempt"
      exit 0
    fi
    echo "build_deps: apt reported success but pkg-config cannot find alsa" >&2
  fi

  # After the first failure switch away from the CNAME-chained Azure mirror.
  if [ "$attempt" -eq 2 ]; then
    rewrite_mirror
  fi

  if [ "$attempt" -lt "$ATTEMPTS" ]; then
    echo "build_deps: apt attempt $attempt/$ATTEMPTS failed, retrying in ${SLEEP_SECS}s" >&2
    sleep "$SLEEP_SECS"
  fi
  attempt=$((attempt + 1))
done

echo "build_deps: could not install libasound2-dev after $ATTEMPTS attempts (apt mirrors unreachable?)" >&2
exit 1
