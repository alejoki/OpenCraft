#!/usr/bin/env bash
# Negative-path selftest for scripts/requirements_check.sh (M003/S05-T03,
# extended M004/S05-T02, M005/S06-T02, M007/S01-T06, M007/S08-T04,
# M008/S09-T02, M009/S10-T02, and M013/S07-T01).
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
#  19. duplicate M007 requirement ID
#  20. missing M007 criterion trace row
#  21. wrong M007 criterion -> requirement mapping
#  22. lost M007 slice mapping
#  23. record-count mismatch with 62 records
#  24. record-count mismatch with 60 records
#  25. malformed M007 row with an empty Validation field
#  26. M007 row ownership reverted to another milestone
#  27. out-of-range criterion row in the M007 trace table
#  28. duplicate M008 requirement ID
#  29. lost M008 slice mapping
#  30. wrong M008 criterion -> requirement mapping
#  31. out-of-range criterion row in the M008 trace table
#  32. R033 deliberate M008/S06 supersession reverted to active
#  33. R033 supersession missing its Superseded by: slice reference
#  34. M008 row ownership reverted to another milestone
#  35. missing M008 criterion trace row
#  36. M008 row with an empty Validation field
#  37. duplicate M009 requirement ID
#  38. lost M009 slice mapping
#  39. wrong M009 criterion -> requirement mapping
#  40. out-of-range criterion row in the M009 trace table
#  41. M009 row ownership reverted to another milestone
#  42. missing M009 criterion trace row
#  43. M009 row with an empty Validation field
#  44. duplicate M013 requirement ID
#  45. lost M013 slice mapping
#  46. wrong M013 criterion -> requirement mapping
#  47. out-of-range criterion row in the M013 trace table
#  48. M013 row ownership reverted to another milestone
#  49. missing M013 criterion trace row
#  50. M013 row with an empty Validation field
#
# Cases 8-11 exercise the M004 checker surface added with R016-R020, cases
# 12-15 exercise the M005 surface added with R021-R026, cases 16-18 exercise
# the deliberate-supersession surface added to R005 (and, in cases 32-33, the
# R033 M008/S06 supersession), cases 19-27 exercise the M007 surface added
# with R027-R034, cases 28-36 exercise the M008 surface added with R035-R044,
# cases 37-43 exercise the M009 surface added with R045-R054, and cases 44-50
# exercise the M013 surface added with R055-R061. All seven criterion tables
# restart their row numbers at 1, so these cases plus the positive control prove
# the checker parses each table section-scoped rather than counting colliding
# criterion 1 rows across tables.
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
# Retargeted to the new last record: renumbering R061 to R062 removes R061
# from the R001-R061 range. Retarget chain: R015->R016 was pre-M004,
# R020->R021 was pre-M005, R026->R027 was pre-S08, R034->R035 was pre-M008,
# R044->R045 was pre-M009, R054->R055 was pre-M013, and now R061->R062 is the
# current last record. (Renumbering R026 to R027 today would create a duplicate
# instead of a missing record, which is why the old mutation moved.)
case3="$(new_case "$SCRATCH/case3.md")"
sed -i 's/^### R061 /### R062 /' "$case3"
assert_mutated "missing ID" "$case1" "$case3"
expect_fail "missing requirement ID R061" "$case3"
grep -q 'missing requirement record: R061' "$CHECK_LOG" \
    || { echo "FAIL: missing ID was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }

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

# --- Case 19: duplicate M007 ID ------------------------------------------
case19="$(new_case "$SCRATCH/case19.md")"
sed -i 's/^### R030 /### R029 /' "$case19"
assert_mutated "duplicate M007 ID" "$case1" "$case19"
rc=0
run_checker "$case19" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: duplicate M007 requirement ID should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'duplicate requirement ID: R029' "$CHECK_LOG" \
    || { echo "FAIL: duplicate M007 ID was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: duplicate M007 requirement ID rejected"

# --- Case 20: missing M007 criterion trace row ---------------------------
# Deleting row 7 leaves criteria 1-6 intact but drops criterion 7 entirely;
# the section-scoped parser must report the missing M007 row, not silently
# accept six rows. M004 row 7 and M005 row 7 do not match this pattern.
case20="$(new_case "$SCRATCH/case20.md")"
sed -i '/^| 7 | R034 | S08 |/d' "$case20"
assert_mutated "missing M007 trace row" "$case1" "$case20"
rc=0
run_checker "$case20" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: missing M007 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M007 criterion 7 must have exactly one trace row, found 0' "$CHECK_LOG" \
    || { echo "FAIL: missing M007 trace row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: missing M007 criterion trace row rejected"

# --- Case 21: wrong M007 criterion mapping -------------------------------
# Row 3 of the M003 table is R008/R009/R010, the M004 row 3 is R018, and the
# M005 row 3 is R023, so this pattern only matches the M007 table's row 3.
case21="$(new_case "$SCRATCH/case21.md")"
sed -i 's/^| 3 | R029 | S03 |/| 3 | R028 | S03 |/' "$case21"
assert_mutated "M007 criterion mapping" "$case1" "$case21"
rc=0
run_checker "$case21" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: wrong M007 criterion mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q "M007 criterion 3 requirements must be 'R029'" "$CHECK_LOG" \
    || { echo "FAIL: wrong M007 criterion mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: wrong M007 criterion mapping rejected"

# --- Case 22: removed M007 slice mapping ---------------------------------
# Target only R033's owner field so the M007 trace table's bare S07 slices are
# untouched (same surgical precedent as the M004/M005 slice cases).
case22="$(new_case "$SCRATCH/case22.md")"
sed -i 's#^- Primary owning slice: M007/S07$#- Primary owning slice: M007/S99#' "$case22"
assert_mutated "M007 slice mapping" "$case1" "$case22"
rc=0
run_checker "$case22" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: removed M007/S07 slice mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'no requirement maps to M007/S07' "$CHECK_LOG" \
    || { echo "FAIL: removed M007 slice mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: removed M007/S07 slice mapping rejected"

# --- Case 23: record-count mismatch with 62 records ---------------------
# A structurally valid R062 record must still be rejected because the
# contract is exactly R001-R061.
case23="$SCRATCH/case23.md"
awk '
/^## M003 Criterion Trace/ && !done {
    print "### R062 — Extra M013 closure record"
    print "- Class: admin/support"
    print "- Status: active"
    print "- Description: Extra record used only by the negative-path selftest."
    print "- Why it matters: The registry contract is exactly R001-R061."
    print "- Source: M013 / S07 selftest"
    print "- Primary owning slice: M013/S07"
    print "- Supporting slices: None"
    print "- Validation: bash scripts/requirements_check.sh"
    print ""
    done = 1
}
{ print }
' "$REGISTRY" > "$case23"
assert_mutated "62-record count" "$case1" "$case23"
rc=0
run_checker "$case23" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: 62-record registry should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'expected 61 requirement records, found 62' "$CHECK_LOG" \
    || { echo "FAIL: 62-record registry was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: 62-record registry rejected"

# --- Case 24: record-count mismatch with 60 records ---------------------
# Dropping the whole R061 block (heading, bullets and trailing blank) leaves
# 60 records, so the count check fires before the missing-ID sweep.
case24="$SCRATCH/case24.md"
awk '
/^### R061 / { skip = 1; next }
skip && /^$/ { skip = 0; next }
skip { next }
{ print }
' "$REGISTRY" > "$case24"
assert_mutated "60-record count" "$case1" "$case24"
rc=0
run_checker "$case24" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: 60-record registry should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'expected 61 requirement records, found 60' "$CHECK_LOG" \
    || { echo "FAIL: 60-record registry was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: 60-record registry rejected"

# --- Case 25: malformed M007 row with empty Validation -------------------
# R029 must carry a concrete evidence command; deleting the bullet must fail
# closed at the non-empty-field gate rather than pass as an active record.
case25="$SCRATCH/case25.md"
awk '
/^### R029 / { in_r029 = 1 }
in_r029 && !deleted && /^- Validation: / { deleted = 1; next }
{ print }
' "$REGISTRY" > "$case25"
assert_mutated "malformed M007 row" "$case1" "$case25"
rc=0
run_checker "$case25" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: malformed M007 row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R029 has an empty Validation field' "$CHECK_LOG" \
    || { echo "FAIL: malformed M007 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: malformed M007 row rejected"

# --- Case 26: M007 row ownership reverted to another milestone -----------
# R033 belongs to M007/S07; pointing it at M005 must be rejected by the
# ownership sweep so an M007 row cannot be quietly re-homed.
case26="$(new_case "$SCRATCH/case26.md")"
sed -i 's#^- Primary owning slice: M007/S07$#- Primary owning slice: M005/S07#' "$case26"
assert_mutated "reverted M007 ownership" "$case1" "$case26"
rc=0
run_checker "$case26" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: reverted M007 ownership should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R033 is not owned by M007' "$CHECK_LOG" \
    || { echo "FAIL: reverted M007 ownership was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: reverted M007 ownership rejected"

# --- Case 27: out-of-range row in the M007 trace table -------------------
# Appending criterion 8 after the M007 table's row 7 must be rejected by the
# M007 range check (1-7), proving the fourth section branch is range-scoped and
# does not accept a widened table just because rows 1-7 are intact.
case27="$(new_case "$SCRATCH/case27.md")"
sed -i '/^| 7 | R034 | S08 |/a\| 8 | R034 | S08 | cargo test --locked |' "$case27"
assert_mutated "M007 out-of-range row" "$case1" "$case27"
rc=0
run_checker "$case27" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: out-of-range M007 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M007 table' "$CHECK_LOG" \
    || { echo "FAIL: out-of-range M007 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: out-of-range M007 trace row rejected"

# --- Case 28: duplicate M008 ID ------------------------------------------
# R038 renamed to R037 collides with the existing R037, so the duplicate gate
# must fire before any M008 trace check.
case28="$(new_case "$SCRATCH/case28.md")"
sed -i 's/^### R038 /### R037 /' "$case28"
assert_mutated "duplicate M008 ID" "$case1" "$case28"
rc=0
run_checker "$case28" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: duplicate M008 requirement ID should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'duplicate requirement ID: R037' "$CHECK_LOG" \
    || { echo "FAIL: duplicate M008 ID was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: duplicate M008 requirement ID rejected"

# --- Case 29: removed M008 slice mapping ---------------------------------
# R041 is the only record that maps M008/S07; pointing its owner at an
# unknown M008 slice must be rejected by the M008 slice-coverage sweep.
case29="$(new_case "$SCRATCH/case29.md")"
sed -i 's#^- Primary owning slice: M008/S07$#- Primary owning slice: M008/S99#' "$case29"
assert_mutated "M008 slice mapping" "$case1" "$case29"
rc=0
run_checker "$case29" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: removed M008/S07 slice mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'no requirement maps to M008/S07' "$CHECK_LOG" \
    || { echo "FAIL: removed M008 slice mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: removed M008/S07 slice mapping rejected"

# --- Case 30: wrong M008 criterion mapping -------------------------------
# Row 3 of the M008 table is uniquely identified by R039, R040; dropping R040
# must fail the M008 criterion-3 requirement assertion.
case30="$(new_case "$SCRATCH/case30.md")"
sed -i 's/^| 3 | R039, R040 | S03, S06 |/| 3 | R039 | S03, S06 |/' "$case30"
assert_mutated "M008 criterion mapping" "$case1" "$case30"
rc=0
run_checker "$case30" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: wrong M008 criterion mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M008 criterion 3 requirements' "$CHECK_LOG" \
    || { echo "FAIL: wrong M008 criterion mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: wrong M008 criterion mapping rejected"

# --- Case 31: out-of-range row in the M008 trace table -------------------
# Appending criterion 9 after the M008 table's row 8 must be rejected by the
# M008 range check (1-8), proving the fifth section branch is range-scoped.
case31="$(new_case "$SCRATCH/case31.md")"
sed -i '/^| 8 | R044 | S04, S05, S06, S07, S09 |/a\| 9 | R044 | S09 | cargo test --locked |' "$case31"
assert_mutated "M008 out-of-range row" "$case1" "$case31"
rc=0
run_checker "$case31" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: out-of-range M008 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M008 table' "$CHECK_LOG" \
    || { echo "FAIL: out-of-range M008 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: out-of-range M008 trace row rejected"

# --- Case 32: R033 supersession reverted to active -----------------------
# M008/S06 deliberately fills R033's empty loot seam; flipping R033 back to
# active must be rejected by the R033 pin even though the generic status
# check still accepts "active".
case32="$SCRATCH/case32.md"
awk '
    /^### R033 / { in_r033 = 1 }
    in_r033 && /^- Status: / { sub(/- Status: .*/, "- Status: active"); in_r033 = 0 }
    { print }
' "$REGISTRY" > "$case32"
assert_mutated "R033 status reverted" "$case1" "$case32"
rc=0
run_checker "$case32" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: reverted R033 supersession should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R033 must record its deliberate M008/S06 supersession' "$CHECK_LOG" \
    || { echo "FAIL: reverted R033 status was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: reverted R033 supersession rejected"

# --- Case 33: R033 superseded without a Superseded by: line --------------
case33="$SCRATCH/case33.md"
awk '
    /^### R033 / { in_r033 = 1 }
    in_r033 && !deleted && /^- Superseded by: / { deleted = 1; next }
    { print }
' "$REGISTRY" > "$case33"
assert_mutated "R033 superseded reference" "$case1" "$case33"
rc=0
run_checker "$case33" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: R033 without Superseded by: should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q "has no 'Superseded by:' slice reference" "$CHECK_LOG" \
    || { echo "FAIL: missing R033 Superseded by: was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: R033 superseded record without Superseded by: rejected"

# --- Case 34: M008 row ownership reverted to another milestone -----------
# R042 belongs to M008/S02; pointing it at M007 must be rejected by the M008
# ownership sweep.
case34="$(new_case "$SCRATCH/case34.md")"
sed -i 's#^- Primary owning slice: M008/S02$#- Primary owning slice: M007/S02#' "$case34"
assert_mutated "reverted M008 ownership" "$case1" "$case34"
rc=0
run_checker "$case34" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: reverted M008 ownership should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R042 is not owned by M008' "$CHECK_LOG" \
    || { echo "FAIL: reverted M008 ownership was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: reverted M008 ownership rejected"

# --- Case 35: missing M008 criterion trace row ---------------------------
# Deleting row 8 leaves criteria 1-7 intact but drops criterion 8 entirely.
case35="$(new_case "$SCRATCH/case35.md")"
sed -i '/^| 8 | R044 | S04, S05, S06, S07, S09 |/d' "$case35"
assert_mutated "missing M008 trace row" "$case1" "$case35"
rc=0
run_checker "$case35" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: missing M008 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M008 criterion 8 must have exactly one trace row, found 0' "$CHECK_LOG" \
    || { echo "FAIL: missing M008 trace row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: missing M008 criterion trace row rejected"

# --- Case 36: malformed M008 row with empty Validation -------------------
# R041 must carry a concrete evidence command; deleting the bullet must fail
# closed at the non-empty-field gate.
case36="$SCRATCH/case36.md"
awk '
/^### R041 / { in_r041 = 1 }
in_r041 && !deleted && /^- Validation: / { deleted = 1; next }
{ print }
' "$REGISTRY" > "$case36"
assert_mutated "malformed M008 row" "$case1" "$case36"
rc=0
run_checker "$case36" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: malformed M008 row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R041 has an empty Validation field' "$CHECK_LOG" \
    || { echo "FAIL: malformed M008 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: malformed M008 row rejected"

# --- Case 37: duplicate M009 ID ------------------------------------------
# R050 renamed to R049 collides with the existing R049, so the duplicate gate
# must fire before any M009 trace check.
case37="$(new_case "$SCRATCH/case37.md")"
sed -i 's/^### R050 /### R049 /' "$case37"
assert_mutated "duplicate M009 ID" "$case1" "$case37"
rc=0
run_checker "$case37" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: duplicate M009 requirement ID should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'duplicate requirement ID: R049' "$CHECK_LOG" \
    || { echo "FAIL: duplicate M009 ID was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: duplicate M009 requirement ID rejected"

# --- Case 38: removed M009 slice mapping ---------------------------------
# R047 is the only record that maps M009/S03; pointing its owner at an
# unknown M009 slice must be rejected by the M009 slice-coverage sweep.
case38="$(new_case "$SCRATCH/case38.md")"
sed -i 's#^- Primary owning slice: M009/S03$#- Primary owning slice: M009/S99#' "$case38"
assert_mutated "M009 slice mapping" "$case1" "$case38"
rc=0
run_checker "$case38" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: removed M009/S03 slice mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'no requirement maps to M009/S03' "$CHECK_LOG" \
    || { echo "FAIL: removed M009 slice mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: removed M009/S03 slice mapping rejected"

# --- Case 39: wrong M009 criterion mapping -------------------------------
# Row 3 of the M009 table is uniquely identified by R047, R051; dropping R051
# must fail the M009 criterion-3 requirement assertion.
case39="$(new_case "$SCRATCH/case39.md")"
sed -i 's/^| 3 | R047, R051 | S03, S07 |/| 3 | R047 | S03, S07 |/' "$case39"
assert_mutated "M009 criterion mapping" "$case1" "$case39"
rc=0
run_checker "$case39" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: wrong M009 criterion mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M009 criterion 3 requirements' "$CHECK_LOG" \
    || { echo "FAIL: wrong M009 criterion mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: wrong M009 criterion mapping rejected"

# --- Case 40: out-of-range row in the M009 trace table -------------------
# Appending criterion 10 after the M009 table's row 9 must be rejected by the
# M009 range check (1-9), proving the sixth section branch is range-scoped.
case40="$(new_case "$SCRATCH/case40.md")"
sed -i '/^| 9 | R054 | S01, S02, S03, S04, S05, S06, S07, S08, S09, S10 |/a\| 10 | R054 | S10 | cargo test --locked |' "$case40"
assert_mutated "M009 out-of-range row" "$case1" "$case40"
rc=0
run_checker "$case40" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: out-of-range M009 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M009 table' "$CHECK_LOG" \
    || { echo "FAIL: out-of-range M009 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: out-of-range M009 trace row rejected"

# --- Case 41: M009 row ownership reverted to another milestone -----------
# R049 belongs to M009/S05; pointing it at M008 must be rejected by the M009
# ownership sweep.
case41="$(new_case "$SCRATCH/case41.md")"
sed -i 's#^- Primary owning slice: M009/S05$#- Primary owning slice: M008/S05#' "$case41"
assert_mutated "reverted M009 ownership" "$case1" "$case41"
rc=0
run_checker "$case41" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: reverted M009 ownership should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R049 is not owned by M009' "$CHECK_LOG" \
    || { echo "FAIL: reverted M009 ownership was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: reverted M009 ownership rejected"

# --- Case 42: missing M009 criterion trace row ---------------------------
# Deleting row 9 leaves criteria 1-8 intact but drops criterion 9 entirely.
case42="$(new_case "$SCRATCH/case42.md")"
sed -i '/^| 9 | R054 | S01, S02, S03, S04, S05, S06, S07, S08, S09, S10 |/d' "$case42"
assert_mutated "missing M009 trace row" "$case1" "$case42"
rc=0
run_checker "$case42" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: missing M009 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M009 criterion 9 must have exactly one trace row, found 0' "$CHECK_LOG" \
    || { echo "FAIL: missing M009 trace row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: missing M009 criterion trace row rejected"

# --- Case 43: M009 row with an empty Validation field -------------------
# R046 must carry a concrete evidence command; deleting the bullet must fail
# closed at the non-empty-field gate.
case43="$SCRATCH/case43.md"
awk '
/^### R046 / { in_r046 = 1 }
in_r046 && !deleted && /^- Validation: / { deleted = 1; next }
{ print }
' "$REGISTRY" > "$case43"
assert_mutated "malformed M009 row" "$case1" "$case43"
rc=0
run_checker "$case43" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: malformed M009 row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R046 has an empty Validation field' "$CHECK_LOG" \
    || { echo "FAIL: malformed M009 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: malformed M009 row rejected"

# --- Case 44: duplicate M013 ID ------------------------------------------
# R056 renamed to R055 collides with the existing R055, so the duplicate gate
# must fire before any M013 trace check.
case44="$(new_case "$SCRATCH/case44.md")"
sed -i 's/^### R056 /### R055 /' "$case44"
assert_mutated "duplicate M013 ID" "$case1" "$case44"
rc=0
run_checker "$case44" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: duplicate M013 requirement ID should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'duplicate requirement ID: R055' "$CHECK_LOG" \
    || { echo "FAIL: duplicate M013 ID was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: duplicate M013 requirement ID rejected"

# --- Case 45: lost M013 slice mapping ------------------------------------
# R056 is the only record that maps M013/S03; pointing its owner at an
# unknown M013 slice must be rejected by the M013 slice-coverage sweep.
case45="$(new_case "$SCRATCH/case45.md")"
sed -i 's#^- Primary owning slice: M013/S03$#- Primary owning slice: M013/S99#' "$case45"
assert_mutated "M013 slice mapping" "$case1" "$case45"
rc=0
run_checker "$case45" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: removed M013/S03 slice mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'no requirement maps to M013/S03' "$CHECK_LOG" \
    || { echo "FAIL: removed M013 slice mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: removed M013/S03 slice mapping rejected"

# --- Case 46: wrong M013 criterion mapping -------------------------------
# Row 1 of the M013 table is uniquely identified by R055; swapping in R056
# must fail the M013 criterion-1 requirement assertion.
case46="$(new_case "$SCRATCH/case46.md")"
sed -i 's/^| 1 | R055 | S01, S02 |/| 1 | R056 | S01, S02 |/' "$case46"
assert_mutated "M013 criterion mapping" "$case1" "$case46"
rc=0
run_checker "$case46" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: wrong M013 criterion mapping should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M013 criterion 1 requirements' "$CHECK_LOG" \
    || { echo "FAIL: wrong M013 criterion mapping was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: wrong M013 criterion mapping rejected"

# --- Case 47: out-of-range row in the M013 trace table -------------------
# Appending criterion 9 after the M013 table's row 8 must be rejected by the
# M013 range check (1-8), proving the seventh section branch is range-scoped.
case47="$(new_case "$SCRATCH/case47.md")"
sed -i '/^| 8 | R061 |/a\| 9 | R061 | S07 | cargo test --locked |' "$case47"
assert_mutated "M013 out-of-range row" "$case1" "$case47"
rc=0
run_checker "$case47" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: out-of-range M013 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M013 table' "$CHECK_LOG" \
    || { echo "FAIL: out-of-range M013 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: out-of-range M013 trace row rejected"

# --- Case 48: M013 row ownership reverted to another milestone -----------
# R059 belongs to M013/S05; pointing it at M012 must be rejected by the M013
# ownership sweep. The awk scope keeps R058's identical owner line intact so
# the failure is attributable to R059.
case48="$SCRATCH/case48.md"
awk '
/^### R059 / { in_r059 = 1 }
in_r059 && /^- Primary owning slice: M013\/S05$/ { sub(/M013\/S05/, "M012/S05"); in_r059 = 0 }
{ print }
' "$REGISTRY" > "$case48"
assert_mutated "reverted M013 ownership" "$case1" "$case48"
rc=0
run_checker "$case48" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: reverted M013 ownership should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R059 is not owned by M013' "$CHECK_LOG" \
    || { echo "FAIL: reverted M013 ownership was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: reverted M013 ownership rejected"

# --- Case 49: missing M013 criterion trace row ---------------------------
# Deleting row 8 leaves criteria 1-7 intact but drops criterion 8 entirely.
case49="$(new_case "$SCRATCH/case49.md")"
sed -i '/^| 8 | R061 |/d' "$case49"
assert_mutated "missing M013 trace row" "$case1" "$case49"
rc=0
run_checker "$case49" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: missing M013 trace row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'M013 criterion 8 must have exactly one trace row, found 0' "$CHECK_LOG" \
    || { echo "FAIL: missing M013 trace row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: missing M013 criterion trace row rejected"

# --- Case 50: M013 row with an empty Validation field -------------------
# R060 must carry a concrete evidence command; deleting the bullet must fail
# closed at the non-empty-field gate.
case50="$SCRATCH/case50.md"
awk '
/^### R060 / { in_r060 = 1 }
in_r060 && !deleted && /^- Validation: / { deleted = 1; next }
{ print }
' "$REGISTRY" > "$case50"
assert_mutated "malformed M013 row" "$case1" "$case50"
rc=0
run_checker "$case50" || rc=$?
if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: malformed M013 row should have been rejected but the checker exited 0" >&2
    exit 1
fi
grep -q 'R060 has an empty Validation field' "$CHECK_LOG" \
    || { echo "FAIL: malformed M013 row was rejected for the wrong reason" >&2; sed 's/^/  checker: /' "$CHECK_LOG" >&2; exit 1; }
echo "PASS: malformed M013 row rejected"

# --- Canonical registry must be untouched --------------------------------
cmp -s "$PRISTINE" "$REGISTRY" \
    || fail "canonical registry changed during selftest: $REGISTRY"

echo "SELFTEST PASS"
