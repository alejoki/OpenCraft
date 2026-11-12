#!/usr/bin/env bash
# Install the Linux build dependencies (ALSA headers + pkg-config) needed by
# alsa-sys/rodio for CI and release builds.
#
# GitHub-hosted runners point apt at mirror+file:/etc/apt/apt-mirrors.txt. The
# default entry (azure.archive.ubuntu.com) resolves through a Traffic Manager
# CNAME chain that intermittently fails DNS on runners, which broke the Linux
# lane repeatedly ("Temporary failure resolving 'azure.archive.ubuntu.com'").
#
# Strategy:
#   1. Exit early when ALSA is already discoverable via pkg-config.
#   2. Probe candidate mirrors with a short curl request and point apt at the
#      first one that actually serves the noble Release index.
#   3. Run bounded apt update/install attempts with short timeouts, then verify
#      pkg-config can see alsa before reporting success.
set -uo pipefail

ATTEMPTS=${ATTEMPTS:-3}
SLEEP_SECS=${SLEEP_SECS:-10}
MIRRORS_FILE=${MIRRORS_FILE:-/etc/apt/apt-mirrors.txt}
SOURCES_FILE=${SOURCES_FILE:-/etc/apt/sources.list.d/ubuntu.sources}
PROBE_PATH=${PROBE_PATH:-dists/noble/Release}
CANDIDATE_MIRRORS=${CANDIDATE_MIRRORS:-"http://archive.ubuntu.com/ubuntu/ http://azure.archive.ubuntu.com/ubuntu/ https://mirrors.edge.kernel.org/ubuntu/ https://ftp.osuosl.org/pub/ubuntu/"}

as_root() {
  if [ "$(id -u)" = "0" ]; then
    "$@"
  else
    sudo "$@"
  fi
}

apt_run() {
  as_root apt-get \
    -o Acquire::Retries=1 \
    -o Acquire::http::Timeout=20 \
    -o Acquire::https::Timeout=20 \
    -o Acquire::ForceIPv4=true \
    "$@"
}

if pkg-config --exists alsa 2>/dev/null; then
  echo "build_deps: alsa already discoverable via pkg-config, nothing to install"
  exit 0
fi

chosen=""
for mirror in $CANDIDATE_MIRRORS; do
  if command -v curl >/dev/null 2>&1 && curl -fsS --max-time 10 -o /dev/null "${mirror}${PROBE_PATH}" 2>/dev/null; then
    chosen="$mirror"
    echo "build_deps: apt mirror reachable: $mirror"
    break
  fi
  echo "build_deps: apt mirror unreachable: $mirror" >&2
done

if [ -n "$chosen" ]; then
  if [ -f "$MIRRORS_FILE" ]; then
    printf '%s\n' "$chosen" | as_root tee "$MIRRORS_FILE" >/dev/null
  fi
  if [ -f "$SOURCES_FILE" ]; then
    as_root sed -i -E "s#^(URIs:).*#\1 $chosen#" "$SOURCES_FILE"
  fi
  echo "build_deps: apt sources point at $chosen"
else
  echo "build_deps: no candidate mirror reachable, leaving apt sources untouched" >&2
fi

attempt=1
while [ "$attempt" -le "$ATTEMPTS" ]; do
  echo "build_deps: apt attempt $attempt/$ATTEMPTS"
  # A missing index (for example noble-security on archive.ubuntu.com) must not
  # abort the install; the package we need ships in noble-updates.
  apt_run update || echo "build_deps: apt update reported problems, using existing indexes" >&2
  if apt_run install -y libasound2-dev pkg-config; then
    if pkg-config --exists alsa; then
      echo "build_deps: linux build dependencies installed on attempt $attempt"
      exit 0
    fi
    echo "build_deps: apt reported success but pkg-config cannot find alsa" >&2
  fi
  if [ "$attempt" -lt "$ATTEMPTS" ]; then
    echo "build_deps: apt attempt $attempt/$ATTEMPTS failed, retrying in ${SLEEP_SECS}s" >&2
    sleep "$SLEEP_SECS"
  fi
  attempt=$((attempt + 1))
done

echo "build_deps: could not install libasound2-dev after $ATTEMPTS attempts (apt mirrors unreachable?)" >&2
exit 1
