#!/usr/bin/env bash
# Negative-path selftest for scripts/requirements_check.sh (M003/S05-T03,
# extended M004/S05-T02, M005/S06-T02, and M007/S01-T06).
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
#  12. duplicate M005 requirement ID
#  13. lost M005 slice mapping
#  14. wrong M005 criterion -> requirement mapping
#  15. out-of-range criterion row in the M005 trace table
#  16. superseded record missing its Superseded by: slice reference
#  17. superseded record naming a malformed (non-slice) reference
#  18. R005's deliberate M007/S01 supersession reverted to active
#
# Cases 8-11 exercise the M004 checker surface added with R016-R020, cases
# 12-15 exercise the M005 surface added with R021-R026, and cases 16-18
# exercise the M007/S01 deliberate-supersession surface added to R005. All
# three criterion tables restart their row numbers at 1, so these cases plus
# the positive control prove the checker parses each table section-scoped
# rather than counting colliding criterion 1 rows across tables.
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
# Retargeted to the new last record: renumbering R026 to R027 removes R026
# from the R001-R026 range. Retarget chain: R015->R016 was pre-M004,
# R020->R021 was pre-M005, and now R026->R027 is the current last record.
# (Renumbering R020 to R021 today would create a duplicate instead of a
# missing record, which is why the old mutation had to move.)
case3="$(new_case "$SCRATCH/case3.md")"
sed -i 's/^### R026 /### R027 /' "$case3"
assert_mutated "missing ID" "$case1" "$case3"
expect_fail "missing requirement ID R026" "$case3"

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

# --- Case 12: duplicate M005 ID ------------------------------------------
case12="$(new_case "$SCRATCH/case12.md")"
sed -i 's/^### R023 /### R022 /' "$case12"
assert_mutated "duplicate M005 ID" "$case1" "$case12"
expect_fail "duplicate M005 requirement ID R022" "$case12"

# --- Case 13: removed M005 slice mapping ---------------------------------
# The M005 trace table's slice cells use bare S04, so this mutation only
# affects R024's Primary owning slice field, not the table itself (same
# precedent as the M004 case 9).
case13="$(new_case "$SCRATCH/case13.md")"
sed -i 's#M005/S04#M005/S99#g' "$case13"
assert_mutated "M005 slice mapping" "$case1" "$case13"
expect_fail "removed M005/S04 slice mapping" "$case13"

# --- Case 14: wrong M005 criterion mapping -------------------------------
# Row 3 of the M003 table is 'R008, R009, R010' and row 3 of the M004 table
# is 'R018', so this pattern only matches the M005 table's row 3.
case14="$(new_case "$SCRATCH/case14.md")"
sed -i 's/^| 3 | R023 | S02 |/| 3 | R024 | S02 |/' "$case14"
assert_mutated "M005 criterion mapping" "$case1" "$case14"
expect_fail "M005 criterion 3 requirement mapping drifted" "$case14"

# --- Case 15: out-of-range row in the M005 trace table -------------------
# Appending criterion 9 after the M005 table's row 8 must be rejected by the
# M005 range check (1-8), proving the third section branch is range-scoped
# and does not collide with the M003/M004 tables.
case15="$(new_case "$SCRATCH/case15.md")"
sed -i '/^| 8 | R026 | S06 |/a\| 9 | R026 | S06 | cargo test --locked |' "$case15"
assert_mutated "M005 out-of-range row" "$case1" "$case15"
rc=0
run_checker "$case15" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: out-of-range M005 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M005 table' "$CHECK_LOG" \
    || { echo "FAIL: out-of-range M005 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: out-of-range M005 trace row rejected"

# --- Case 16: superseded record without a Superseded by: line ------------
# R005 is the deliberate M007/S01 supersession. Dropping its Superseded by:
# line must fail closed: a status flip alone is not a recorded supersession.
case16="$SCRATCH/case16.md"
awk '
    /^### R005 / { in_r005 = 1 }
    in_r005 && !deleted && /^- Superseded by: / { deleted = 1; next }
    { print }
' "$REGISTRY" > "$case16"
assert_mutated "superseded reference" "$case1" "$case16"
rc=0
run_checker "$case16" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: superseded record without Superseded by: should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q "has no 'Superseded by:' slice reference" "$CHECK_LOG" \
    || { echo "FAIL: missing Superseded by: was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: superseded record without Superseded by: rejected"

# --- Case 17: superseded record with a malformed reference ---------------
# A bare milestone or free-text note is not a slice reference; the checker
# requires the exact M###/S## shape so the supersession is traceable.
case17="$(new_case "$SCRATCH/case17.md")"
sed -i 's#^- Superseded by: M007/S01$#- Superseded by: M007#' "$case17"
assert_mutated "malformed superseded reference" "$case1" "$case17"
rc=0
run_checker "$case17" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: malformed Superseded by: reference should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'must name an M###/S## slice' "$CHECK_LOG" \
    || { echo "FAIL: malformed Superseded by: was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: malformed Superseded by: reference rejected"

# --- Case 18: R005 supersession reverted to active -----------------------
# The M007/S01 rewrite deliberately supersedes R005; silently flipping it
# back to active would erase the recorded supersession from the registry.
case18="$SCRATCH/case18.md"
awk '
    /^### R005 / { in_r005 = 1 }
    in_r005 && /^- Status: / { sub(/- Status: .*/, "- Status: active"); in_r005 = 0 }
    { print }
' "$REGISTRY" > "$case18"
assert_mutated "R005 status reverted" "$case1" "$case18"
rc=0
run_checker "$case18" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: reverted R005 supersession should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R005 must record its deliberate M007/S01 supersession' "$CHECK_LOG" \
    || { echo "FAIL: reverted R005 status was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: reverted R005 supersession rejected"

# --- Canonical registry must be untouched --------------------------------
cmp -s "$PRISTINE" "$REGISTRY" \
    || fail "canonical registry changed during selftest: $REGISTRY"

echo "SELFTEST PASS"
