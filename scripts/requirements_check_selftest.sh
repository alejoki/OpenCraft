#!/usr/bin/env bash
# Negative-path selftest for scripts/requirements_check.sh (M003/S05-T03).
#
# Proves the registry checker is not a happy-path-only gate: a pristine copy
# must be accepted, and each of the following defects must be rejected:
#   1. duplicate requirement ID
#   2. missing requirement ID (renumbered out of range)
#   3. lost M003 slice mapping
#   4. wrong M003 criterion -> requirement mapping
#   5. missing per-record Validation field
#   6. stale legacy project identity on the shipped surface
#
# Every mutation is applied to a throwaway copy under mktemp -d; the canonical
# REQUIREMENTS-REGISTRY.md is never written. The selftest asserts the on-disk
# registry is byte-identical before and after the run.
#
# Dependencies: bash, awk, sed, cp, cmp, mktemp.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CHECKER="$REPO_ROOT/scripts/requirements_check.sh"
REGISTRY="$REPO_ROOT/REQUIREMENTS-REGISTRY.md"

SCRATCH="$(mktemp -d)"
CHECK_LOG="$SCRATCH/checker.log"
PRISTINE="$SCRATCH/pristine.md"

cleanup() {
    rm -rf "$SCRATCH"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Capture the canonical registry byte-for-byte so the run can prove it never
# mutated a tracked file.
[[ -f "$REGISTRY" ]] || fail "canonical registry not found: $REGISTRY"
[[ -f "$CHECKER" ]] || fail "checker not found: $CHECKER"
cp "$REGISTRY" "$PRISTINE"
[[ -d "$SCRATCH" && -w "$SCRATCH" ]] || fail "scratch directory is not writable: $SCRATCH"

# Run the checker against an override file. Normal output is suppressed; the
# log is replayed only when a case produces an unexpected verdict.
run_checker() {
    local file="$1" rc=0
    REQUIREMENTS_FILE="$file" bash "$CHECKER" >"$CHECK_LOG" 2>&1 || rc=$?
    return "$rc"
}

expect_pass() {
    local name="$1" file="$2" rc=0
    run_checker "$file" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        echo "FAIL: $name should have been accepted but the checker exited $rc" >&2
        sed 's/^/  checker: /' "$CHECK_LOG" >&2
        exit 1
    fi
    echo "PASS: $name accepted"
}

expect_fail() {
    local name="$1" file="$2" rc=0
    run_checker "$file" || rc=$?
    if [[ "$rc" -eq 0 ]]; then
        echo "FAIL: $name should have been rejected but the checker exited 0" >&2
        exit 1
    fi
    echo "PASS: $name rejected"
}

# A mutation that silently matches nothing would make expect_fail pass for the
# wrong reason. Fail loudly instead, without adding a PASS line.
assert_mutated() {
    local name="$1" original="$2" mutated="$3"
    if cmp -s "$original" "$mutated"; then
        echo "FAIL: $name mutation did not change the registry copy (pattern drift)" >&2
        exit 1
    fi
}

new_case() {
    local dest="$1"
    cp "$REGISTRY" "$dest"
    printf '%s' "$dest"
}

# --- Case 1: positive control -------------------------------------------
case1="$(new_case "$SCRATCH/case1.md")"
expect_pass "unmodified registry" "$case1"

# --- Case 2: duplicate ID ------------------------------------------------
case2="$(new_case "$SCRATCH/case2.md")"
sed -i 's/^### R015 /### R014 /' "$case2"
assert_mutated "duplicate ID" "$case1" "$case2"
expect_fail "duplicate requirement ID" "$case2"

# --- Case 3: missing ID --------------------------------------------------
case3="$(new_case "$SCRATCH/case3.md")"
sed -i 's/^### R015 /### R016 /' "$case3"
assert_mutated "missing ID" "$case1" "$case3"
expect_fail "missing requirement ID R015" "$case3"

# --- Case 4: removed M003 slice mapping ----------------------------------
case4="$(new_case "$SCRATCH/case4.md")"
sed -i 's#M003/S02#M003/S99#g' "$case4"
assert_mutated "slice mapping" "$case1" "$case4"
expect_fail "removed M003/S02 slice mapping" "$case4"

# --- Case 5: wrong criterion mapping -------------------------------------
case5="$(new_case "$SCRATCH/case5.md")"
sed -i 's/^| 2 | R006, R007 | S02 |/| 2 | R006 | S02 |/' "$case5"
assert_mutated "criterion mapping" "$case1" "$case5"
expect_fail "criterion 2 requirement mapping drifted" "$case5"

# --- Case 6: missing Validation field on R005 ----------------------------
case6="$SCRATCH/case6.md"
awk '
    /^### R005 / { in_r005 = 1 }
    in_r005 && !deleted && /^- Validation: / { deleted = 1; next }
    { print }
' "$REGISTRY" > "$case6"
assert_mutated "validation field" "$case1" "$case6"
expect_fail "missing Validation field on R005" "$case6"

# --- Case 7: stale legacy identity on the shipped surface ----------------
case7="$SCRATCH/case7.md"
cp "$REGISTRY" "$case7"
identity_root="$SCRATCH/identity-root"
mkdir -p "$identity_root/src" "$identity_root/scripts"
cp "$REPO_ROOT/Cargo.toml" "$REPO_ROOT/Cargo.lock" "$identity_root/"
printf 'fn main() { let _ = "Minecraft%sClone"; }\n' ' ' > "$identity_root/src/stale.rs"
rc=0
IDENTITY_SCAN_ROOT="$identity_root" REQUIREMENTS_FILE="$case7" \
    bash "$CHECKER" >"$CHECK_LOG" 2>&1 || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: stale legacy identity should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'legacy project identity found' "$CHECK_LOG" \
    || { echo "FAIL: stale identity was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: stale legacy identity rejected"

# --- Canonical registry must be untouched --------------------------------
cmp -s "$PRISTINE" "$REGISTRY" \
    || fail "canonical registry changed during selftest: $REGISTRY"

echo "SELFTEST PASS"
