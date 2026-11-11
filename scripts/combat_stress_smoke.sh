#!/usr/bin/env bash
# Repeated real-binary combat smoke stress proof (M016/S11-T04).
#
# Runs `scripts/combat_smoke.sh` strictly serially N times (default 40) and
# asserts that every run exits 0, so the Skeleton-stage spot scan never aborts
# with `stage=skeleton observed=...spot`. The per-run script already pins the
# six exact digests, the frame budget, the 317-byte v9 save layout and the
# reload line; this script adds the repetition + aggregation contract.
#
# Inputs:
#   SMOKE_RUNS     consecutive runs (default 40, positive integer)
#   SMOKE_PROFILE  debug|release (default release)
#
# A non-zero run stops the loop immediately: consecutive acceptance is broken,
# so later runs cannot rescue it. Every run log lives in a mktemp dir removed on
# exit, so nothing is written into the repo. Runs are serial on purpose —
# parallel windows contend for the compositor and perturb frame pacing.

set -u

fail() {
    echo "COMBAT STRESS FAIL: $*" >&2
    exit 1
}

RUNS="${SMOKE_RUNS:-40}"
PROFILE="${SMOKE_PROFILE:-release}"

# Fail closed before any build or launch so a typo can never silently exercise
# the wrong run count or tree.
case "$RUNS" in
    ''|*[!0-9]*) fail "SMOKE_RUNS='$RUNS' is not a positive integer" ;;
esac
if [[ "$RUNS" -eq 0 ]]; then
    fail "SMOKE_RUNS=0 (expected at least 1 consecutive run)"
fi
case "$PROFILE" in
    debug|release) ;;
    *) fail "SMOKE_PROFILE='$PROFILE' (expected debug or release)" ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Display auto-detection: agent/CI shells can lack DISPLAY/WAYLAND_DISPLAY even
# when a desktop session is live. Prefer Wayland — winit picks the X11 backend
# whenever DISPLAY is set, and Xwayland's :0 needs an xauth cookie this shell
# may not have. On this host only wayland-1 exists, so never hardcode wayland-0.
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    wayland_socket=""
    for candidate in /run/user/"$(id -u)"/wayland-*; do
        [[ -e "$candidate" ]] || continue
        [[ "$candidate" == *.lock ]] && continue
        wayland_socket="$candidate"
        break
    done
    if [[ -n "$wayland_socket" ]]; then
        WAYLAND_DISPLAY="$(basename "$wayland_socket")"
        export WAYLAND_DISPLAY
        XDG_RUNTIME_DIR="/run/user/$(id -u)"
        export XDG_RUNTIME_DIR
        unset DISPLAY
    elif [[ -e /tmp/.X11-unix/X0 ]]; then
        DISPLAY=":0"
        export DISPLAY
    else
        echo "COMBAT STRESS NOTE: no Wayland socket and no /tmp/.X11-unix/X0; runs may fail to open a window" >&2
    fi
fi

TEMP_DIR="$(mktemp -d /tmp/opencraft-combat-stress.XXXXXX)" || fail "could not create a temp dir"
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

p95_min=""
p95_max=""
started_at=$(date +%s)

for ((i = 1; i <= RUNS; i++)); do
    log="$TEMP_DIR/run-$i.log"
    (cd "$CRATE_ROOT" && SMOKE_PROFILE="$PROFILE" bash scripts/combat_smoke.sh) >"$log" 2>&1
    rc=$?
    if [[ $rc -ne 0 ]]; then
        echo "COMBAT STRESS FAIL: run $i/$RUNS exit=$rc" >&2
        if grep -q -E 'stage=skeleton observed=.*spot' "$log"; then
            echo "COMBAT STRESS FAIL: skeleton_spot=1 (log matches 'stage=skeleton observed=...spot')" >&2
        else
            echo "COMBAT STRESS FAIL: skeleton_spot=0" >&2
        fi
        echo "--- last 40 lines of run $i/$RUNS log ---" >&2
        tail -n 40 "$log" >&2
        exit 1
    fi

    # combat_smoke.sh reprints the digest as a `COMBAT SMOKE EVIDENCE:`
    # line, so match the p95 token anywhere in the log rather than anchoring
    # to the bare `frame_budget:` prefix.
    frame_line=$(grep -m1 'frame_budget:' "$log" || true)
    p95_token=$(grep -m1 -o 'p95=[0-9.]*ms' "$log" || true)
    if [[ -z "$p95_token" ]]; then
        echo "COMBAT STRESS FAIL: run $i/$RUNS exited 0 but has no parseable p95 token (frame line: '${frame_line:-<missing>}')" >&2
        echo "--- last 40 lines of run $i/$RUNS log ---" >&2
        tail -n 40 "$log" >&2
        exit 1
    fi
    p95="${p95_token#p95=}"
    p95="${p95%ms}"

    # awk carries the running min/max — no float comparison in bash.
    read -r p95_min p95_max < <(awk -v cur="$p95" -v mn="$p95_min" -v mx="$p95_max" '
        BEGIN {
            if (mn == "") mn = cur
            if (mx == "") mx = cur
            if (cur + 0 < mn + 0) mn = cur
            if (cur + 0 > mx + 0) mx = cur
            printf "%s %s\n", mn, mx
        }')
done

elapsed=$(( $(date +%s) - started_at ))
echo "COMBAT STRESS EVIDENCE: profile=$PROFILE elapsed=${elapsed}s"
echo "COMBAT STRESS PASS: profile=$PROFILE runs=$RUNS fails=0 skeleton_spot=0 p95_min=${p95_min}ms p95_max=${p95_max}ms"
