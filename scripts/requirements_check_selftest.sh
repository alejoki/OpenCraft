#!/usr/bin/env bash
# Negative-path selftest for scripts/requirements_check.sh (M003/S05-T03,
# extended M004/S05-T02).
#
# Proves the registry checker is not a happy-path-only gate: a pristine copy
# must be accepted, and each of the following defects must be rejected:
#   1. positive control (unmodified registry accepted)
#   2. duplicate requirement ID
#   3. missing requirement ID (last record renumbered out of range)
#   4. lost M003 slice mapping
#   5. wrong M003 criterion -> requirement mapping
#   6. missing per-record Validation field
#   7. stale legacy project identity on the shipped surface
#   8. duplicate M004 requirement ID
#   9. lost M004 slice mapping
#  10. wrong M004 criterion -> requirement mapping
#  11. out-of-range criterion row in the M004 trace table
#
# Cases 8-11 exercise the M004 checker surface added with R016-R020: the two
# criterion tables restart their row numbers at 1, so these cases plus the
# positive control prove the checker parses them section-scoped rather than
# counting colliding criterion 1 rows from both tables.
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
# Retargeted to the new last record: renumbering R020 to R021 removes R020
# from the R001-R020 range. Before R016-R020 landed this case renumbered R015
# to R016, which would now create a duplicate instead of a missing record.
case3="$(new_case "$SCRATCH/case3.md")"
sed -i 's/^### R020 /### R021 /' "$case3"
assert_mutated "missing ID" "$case1" "$case3"
expect_fail "missing requirement ID R020" "$case3"

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

# --- Case 8: duplicate M004 ID -------------------------------------------
case8="$(new_case "$SCRATCH/case8.md")"
sed -i 's/^### R017 /### R016 /' "$case8"
assert_mutated "duplicate M004 ID" "$case1" "$case8"
expect_fail "duplicate M004 requirement ID R016" "$case8"

# --- Case 9: removed M004 slice mapping ----------------------------------
# The M004 trace table's slice cells use bare S03, so this mutation only
# affects R018's Primary owning slice field, not the table itself.
case9="$(new_case "$SCRATCH/case9.md")"
sed -i 's#M004/S03#M004/S99#g' "$case9"
assert_mutated "M004 slice mapping" "$case1" "$case9"
expect_fail "removed M004/S03 slice mapping" "$case9"

# --- Case 10: wrong M004 criterion mapping -------------------------------
# Row 2 of the M003 table is 'R006, R007', so this pattern only matches the
# M004 table's row 2.
case10="$(new_case "$SCRATCH/case10.md")"
sed -i 's/^| 2 | R016 | S01 |/| 2 | R016, R017 | S01 |/' "$case10"
assert_mutated "M004 criterion mapping" "$case1" "$case10"
expect_fail "M004 criterion 2 requirement mapping drifted" "$case10"

# --- Case 11: out-of-range row in the M004 trace table -------------------
# Appending criterion 8 after the M004 table's row 7 must be rejected by the
# M004 range check (1-7). This is the positive control that the widened row
# pattern plus per-section range check distinguishes the two tables.
case11="$(new_case "$SCRATCH/case11.md")"
sed -i '/^| 7 | R020 | S05 |/a\| 8 | R020 | S05 | cargo test --locked |' "$case11"
assert_mutated "M004 out-of-range row" "$case1" "$case11"
rc=0
run_checker "$case11" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: out-of-range M004 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M004 table' "$CHECK_LOG" \
    || { echo "FAIL: out-of-range M004 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: out-of-range M004 trace row rejected"

# --- Canonical registry must be untouched --------------------------------
cmp -s "$PRISTINE" "$REGISTRY" \
    || fail "canonical registry changed during selftest: $REGISTRY"

echo "SELFTEST PASS"
