#!/usr/bin/env bash
# Requirements registry checker (M003/S05-T02).
#
# Validates the checked-in REQUIREMENTS-REGISTRY.md:
#   1. the registry file exists,
#   2. every "### R### — ..." record parses into the nine contract bullets,
#   3. exactly R001-R088 appear once each, with no duplicate or malformed IDs,
#   4. required fields are non-empty and Status is exactly "active" or
#      "superseded" with a Superseded by: M###/S## line; R005's deliberate
#      M007/S01 supersession and R033's deliberate M008/S06 supersession are
#      pinned so they cannot silently revert to active,
#   5. ownership follows M001/M002 for R001-R004, M003 for R005-R015,
#      M004 for R016-R020, M005 for R021-R026, M007 for R027-R034,
#      M008 for R035-R044, M009 for R045-R054, M013 for R055-R061,
#      M010 for R062-R070, M014 for R071-R081, and M015 for R082-R088,
#   6. every M003, M004, M005, M007, M008, M009, M013, M010, M014, and M015
#      slice is covered by at least one mapped record,
#   7. the M003 (criteria 1-5), M004 (criteria 1-7), M005 (criteria 1-8),
#      M007 (criteria 1-7), M008 (criteria 1-8), M009 (criteria 1-9),
#      M013 (criteria 1-8), M010 (criteria 1-11), M014 (criteria 1-12), and
#      M015 (criteria 1-10) criterion trace tables are parsed section-scoped
#      and match their expected requirement/slice mappings with non-empty
#      evidence,
#   8. the shipped identity surface (manifest, lockfile, src, scripts) contains
#      no legacy project-name reference, in any case or separator form,
#   9. all ten validated criterion trace tables are printed.
#
# M014 is append-only: R001-R070 and the eight prior trace tables are never
# edited (D220). Five prior records (R055, R063, R064, R065, R069) appear in
# M014 trace rows as explicitly supported prior surfaces, so the M014
# ownership assertion accepts that fixed support set in addition to
# M014-owned records.
#
# M015 follows the same append-only rule: its trace cites R063, R064, R065,
# and R069 as a fixed support set in addition to M015-owned records.
#
# REQUIREMENTS_FILE overrides the registry path (used by the negative-path
# selftest). The default is <repo-root>/REQUIREMENTS-REGISTRY.md.
#
# Dependencies: bash, awk, grep, seq, mktemp. No network, no cargo, no .gsd.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REGISTRY="${REQUIREMENTS_FILE:-$REPO_ROOT/REQUIREMENTS-REGISTRY.md}"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

pass() {
    echo "PASS: $*"
}

# Never treat framework-state or other ignored trees as the capability contract.
case "$REGISTRY" in
    *".gsd/"*)
        fail "refusing to read framework state path: $REGISTRY"
        ;;
esac

# ---------------------------------------------------------------------------
# 1. Registry file exists.
# ---------------------------------------------------------------------------
[[ -f "$REGISTRY" ]] || fail "registry file not found: $REGISTRY"
pass "registry file exists: $REGISTRY"

record_tmp="$(mktemp)"
criteria_m3_tmp="$(mktemp)"
criteria_m4_tmp="$(mktemp)"
criteria_m5_tmp="$(mktemp)"
criteria_m7_tmp="$(mktemp)"
criteria_m8_tmp="$(mktemp)"
criteria_m9_tmp="$(mktemp)"
criteria_m13_tmp="$(mktemp)"
criteria_m10_tmp="$(mktemp)"
criteria_m14_tmp="$(mktemp)"
criteria_m15_tmp="$(mktemp)"
trap 'rm -f "$record_tmp" "$criteria_m3_tmp" "$criteria_m4_tmp" "$criteria_m5_tmp" "$criteria_m7_tmp" "$criteria_m8_tmp" "$criteria_m9_tmp" "$criteria_m13_tmp" "$criteria_m10_tmp" "$criteria_m14_tmp" "$criteria_m15_tmp"' EXIT

# ---------------------------------------------------------------------------
# 2. Parse each record heading and its contract bullets.
#
# Emits one line per record:
#   id|class|status|description|why|source|owner|supporting|validation|superseded_by
# A record ends at the next heading; bullets with no record are ignored.
# ---------------------------------------------------------------------------
awk '
function flush() {
    if (id == "") return
    print id "|" class "|" status "|" desc "|" why "|" src "|" own "|" supp "|" val "|" supe
}
/^### R[0-9][0-9][0-9] / {
    flush()
    id = $2
    class = status = desc = why = src = own = supp = val = supe = ""
    next
}
/^- Class: /                { sub(/^- Class: /, "");                class = $0; next }
/^- Status: /               { sub(/^- Status: /, "");               status = $0; next }
/^- Description: /          { sub(/^- Description: /, "");          desc = $0; next }
/^- Why it matters: /       { sub(/^- Why it matters: /, "");       why = $0; next }
/^- Source: /               { sub(/^- Source: /, "");               src = $0; next }
/^- Primary owning slice: / { sub(/^- Primary owning slice: /, ""); own = $0; next }
/^- Supporting slices: /    { sub(/^- Supporting slices: /, "");    supp = $0; next }
/^- Validation: /           { sub(/^- Validation: /, "");           val = $0; next }
/^- Superseded by: /        { sub(/^- Superseded by: /, "");        supe = $0; next }
END { flush() }
' "$REGISTRY" > "$record_tmp"

# ---------------------------------------------------------------------------
# 3. Exactly 88 unique R001-R088 records.
# ---------------------------------------------------------------------------
declare -A seen=()
declare -A class_of=()
declare -A status_of=()
declare -A desc_of=()
declare -A why_of=()
declare -A src_of=()
declare -A owner_of=()
declare -A supp_of=()
declare -A val_of=()
declare -A supe_of=()

record_count=0
while IFS='|' read -r id class status desc why src own supp val supe; do
    [[ -n "$id" ]] || continue
    [[ "$id" =~ ^R[0-9]{3}$ ]] || fail "invalid requirement ID: '$id' (expected R###)"
    [[ -z "${seen[$id]:-}" ]] || fail "duplicate requirement ID: $id"

    seen[$id]=1
    class_of[$id]="$class"
    status_of[$id]="$status"
    desc_of[$id]="$desc"
    why_of[$id]="$why"
    src_of[$id]="$src"
    owner_of[$id]="$own"
    supp_of[$id]="$supp"
    val_of[$id]="$val"
    supe_of[$id]="$supe"
    record_count=$((record_count + 1))
done < "$record_tmp"

[[ "$record_count" -eq 88 ]] || fail "expected 88 requirement records, found $record_count"

for n in $(seq 1 88); do
    id="$(printf 'R%03d' "$n")"
    [[ -n "${seen[$id]:-}" ]] || fail "missing requirement record: $id"
done

pass "88 requirement records R001-R088 present exactly once with unique IDs"

# ---------------------------------------------------------------------------
# 4. Required fields are non-empty; Status is exactly "active" or
#    "superseded" with a non-empty Superseded by: M###/S## reference.
# ---------------------------------------------------------------------------
for id in "${!seen[@]}"; do
    [[ -n "${class_of[$id]}" ]] || fail "$id has an empty Class field"
    [[ -n "${desc_of[$id]}" ]] || fail "$id has an empty Description field"
    [[ -n "${why_of[$id]}" ]] || fail "$id has an empty Why-it-matters field"
    [[ -n "${src_of[$id]}" ]] || fail "$id has an empty Source field"
    [[ -n "${owner_of[$id]}" ]] || fail "$id has an empty Primary owning slice field"
    [[ -n "${val_of[$id]}" ]] || fail "$id has an empty Validation field"
    case "${status_of[$id]}" in
        active) ;;
        superseded)
            [[ -n "${supe_of[$id]}" ]] \
                || fail "$id Status is 'superseded' but has no 'Superseded by:' slice reference"
            [[ "${supe_of[$id]}" =~ ^M[0-9]{3}/S[0-9]{2}$ ]] \
                || fail "$id Superseded by must name an M###/S## slice, got '${supe_of[$id]}'"
            ;;
        *)
            fail "$id Status must be 'active' or 'superseded' with a Superseded by: slice, got '${status_of[$id]}'"
            ;;
    esac
done
pass "all records carry non-empty Class/Description/Why/Source/Owner/Validation and Status active or justified superseded"

# R005's supersession is deliberate (M007/S01 rewrote the height envelope but
# kept the cave/ore/bedrock contract). Pin it so an accidental revert to
# "active" cannot quietly erase the recorded supersession.
[[ "${status_of[R005]}" == "superseded" ]] \
    || fail "R005 must record its deliberate M007/S01 supersession (got '${status_of[R005]}')"
[[ "${supe_of[R005]}" == "M007/S01" ]] \
    || fail "R005 must name M007/S01 as its superseding slice, got '${supe_of[R005]}'"
pass "R005 records the deliberate M007/S01 supersession"

# R033's deliberately empty loot seam was filled by M008/S06. Pin the
# supersession so a revert to active cannot erase the M008/S06 handoff.
[[ "${status_of[R033]}" == "superseded" ]] \
    || fail "R033 must record its deliberate M008/S06 supersession (got '${status_of[R033]}')"
[[ "${supe_of[R033]}" == "M008/S06" ]] \
    || fail "R033 must name M008/S06 as its superseding slice, got '${supe_of[R033]}'"
pass "R033 records the deliberate M008/S06 supersession"

# ---------------------------------------------------------------------------
# 5. Ownership: R001-R004 -> M001 or M002; R005-R015 -> M003; R016-R020 -> M004;
#    R021-R026 -> M005; R027-R034 -> M007; R035-R044 -> M008; R045-R054 -> M009;
#    R055-R061 -> M013; R062-R070 -> M010; R071-R081 -> M014;
#    R082-R088 -> M015.
# ---------------------------------------------------------------------------
for n in $(seq 1 4); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M001"* || "$text" == *"M002"* ]] \
        || fail "$id is not owned by M001 or M002 (owner/supporting: '$text')"
done

for n in $(seq 5 15); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M003"* ]] \
        || fail "$id is not owned by M003 (owner/supporting: '$text')"
done

for n in $(seq 16 20); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M004"* ]] \
        || fail "$id is not owned by M004 (owner/supporting: '$text')"
done

for n in $(seq 21 26); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M005"* ]] \
        || fail "$id is not owned by M005 (owner/supporting: '$text')"
done

for n in $(seq 27 34); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M007"* ]] \
        || fail "$id is not owned by M007 (owner/supporting: '$text')"
done

for n in $(seq 35 44); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M008"* ]] \
        || fail "$id is not owned by M008 (owner/supporting: '$text')"
done

for n in $(seq 45 54); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M009"* ]] \
        || fail "$id is not owned by M009 (owner/supporting: '$text')"
done

for n in $(seq 55 61); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M013"* ]] \
        || fail "$id is not owned by M013 (owner/supporting: '$text')"
done

for n in $(seq 62 70); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M010"* ]] \
        || fail "$id is not owned by M010 (owner/supporting: '$text')"
done

for n in $(seq 71 81); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M014"* ]] \
        || fail "$id is not owned by M014 (owner/supporting: '$text')"
done

for n in $(seq 82 88); do
    id="$(printf 'R%03d' "$n")"
    text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"M015"* ]] \
        || fail "$id is not owned by M015 (owner/supporting: '$text')"
done
pass "ownership matches M001/M002 for R001-R004, M003 for R005-R015, M004 for R016-R020, M005 for R021-R026, M007 for R027-R034, M008 for R035-R044, M009 for R045-R054, M013 for R055-R061, M010 for R062-R070, M014 for R071-R081, and M015 for R082-R088"

# ---------------------------------------------------------------------------
# 6. M003 slice coverage. A mapping counts when the slice token appears in the
#    record's owner or supporting-slices field.
# ---------------------------------------------------------------------------
has_slice() {
    local id="$1" token="$2"
    local text="${owner_of[$id]} ${supp_of[$id]}"
    [[ "$text" == *"$token"* ]]
}

has_slice R005 M003/S01 || fail "no requirement maps to M003/S01 (expected R005)"

mapped=0
for id in R006 R007; do
    if has_slice "$id" M003/S02; then
        mapped=1
    fi
done
[[ "$mapped" -eq 1 ]] || fail "no requirement maps to M003/S02 (expected R006 or R007)"

mapped=0
for id in R008 R009 R010; do
    if has_slice "$id" M003/S03; then
        mapped=1
    fi
done
[[ "$mapped" -eq 1 ]] || fail "no requirement maps to M003/S03 (expected one of R008, R009, R010)"

mapped=0
for id in R011 R012 R013 R014 R015; do
    if has_slice "$id" M003/S04; then
        mapped=1
    fi
done
[[ "$mapped" -eq 1 ]] || fail "no requirement maps to M003/S04 (expected one of R011-R015)"

has_slice R015 M003/S05 || fail "no requirement maps to M003/S05 (expected R015)"
pass "M003 slices S01-S05 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6b. M004 slice coverage.
# ---------------------------------------------------------------------------
has_slice R016 M004/S01 || fail "no requirement maps to M004/S01 (expected R016)"
has_slice R017 M004/S02 || fail "no requirement maps to M004/S02 (expected R017)"
has_slice R018 M004/S03 || fail "no requirement maps to M004/S03 (expected R018)"
has_slice R019 M004/S04 || fail "no requirement maps to M004/S04 (expected R019)"
has_slice R020 M004/S05 || fail "no requirement maps to M004/S05 (expected R020)"
pass "M004 slices S01-S05 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6c. M005 slice coverage.
# ---------------------------------------------------------------------------
has_slice R021 M005/S01 || fail "no requirement maps to M005/S01 (expected R021)"
has_slice R022 M005/S02 || fail "no requirement maps to M005/S02 (expected R022)"
has_slice R023 M005/S02 || fail "no requirement maps to M005/S02 (expected R023)"
has_slice R024 M005/S04 || fail "no requirement maps to M005/S04 (expected R024)"
has_slice R025 M005/S05 || fail "no requirement maps to M005/S05 (expected R025)"
has_slice R026 M005/S06 || fail "no requirement maps to M005/S06 (expected R026)"
pass "M005 slices S01-S06 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6d. M007 slice coverage.
# ---------------------------------------------------------------------------
has_slice R027 M007/S01 || fail "no requirement maps to M007/S01 (expected R027)"
has_slice R028 M007/S02 || fail "no requirement maps to M007/S02 (expected R028)"
has_slice R029 M007/S03 || fail "no requirement maps to M007/S03 (expected R029)"
has_slice R030 M007/S04 || fail "no requirement maps to M007/S04 (expected R030)"
has_slice R031 M007/S05 || fail "no requirement maps to M007/S05 (expected R031)"
has_slice R032 M007/S06 || fail "no requirement maps to M007/S06 (expected R032)"
has_slice R033 M007/S07 || fail "no requirement maps to M007/S07 (expected R033)"
has_slice R034 M007/S08 || fail "no requirement maps to M007/S08 (expected R034)"
pass "M007 slices S01-S08 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6e. M008 slice coverage.
# ---------------------------------------------------------------------------
has_slice R035 M008/S01 || fail "no requirement maps to M008/S01 (expected R035)"
has_slice R042 M008/S02 || fail "no requirement maps to M008/S02 (expected R042)"
has_slice R039 M008/S03 || fail "no requirement maps to M008/S03 (expected R039)"
has_slice R036 M008/S04 || fail "no requirement maps to M008/S04 (expected R036)"
has_slice R038 M008/S05 || fail "no requirement maps to M008/S05 (expected R038)"
has_slice R040 M008/S06 || fail "no requirement maps to M008/S06 (expected R040)"
has_slice R041 M008/S07 || fail "no requirement maps to M008/S07 (expected R041)"
has_slice R044 M008/S08 || fail "no requirement maps to M008/S08 (expected R044 supporting)"
has_slice R044 M008/S09 || fail "no requirement maps to M008/S09 (expected R044 owner)"
pass "M008 slices S01-S09 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6f. M009 slice coverage.
# ---------------------------------------------------------------------------
has_slice R045 M009/S01 || fail "no requirement maps to M009/S01 (expected R045)"
has_slice R046 M009/S02 || fail "no requirement maps to M009/S02 (expected R046)"
has_slice R047 M009/S03 || fail "no requirement maps to M009/S03 (expected R047)"
has_slice R048 M009/S04 || fail "no requirement maps to M009/S04 (expected R048)"
has_slice R049 M009/S05 || fail "no requirement maps to M009/S05 (expected R049)"
has_slice R050 M009/S06 || fail "no requirement maps to M009/S06 (expected R050)"
has_slice R051 M009/S07 || fail "no requirement maps to M009/S07 (expected R051)"
has_slice R052 M009/S08 || fail "no requirement maps to M009/S08 (expected R052)"
has_slice R053 M009/S09 || fail "no requirement maps to M009/S09 (expected R053)"
has_slice R054 M009/S10 || fail "no requirement maps to M009/S10 (expected R054)"
pass "M009 slices S01-S10 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6g. M013 slice coverage. Assert a specific record per slice so R061's broad
#     supporting list cannot mask a broken row.
# ---------------------------------------------------------------------------
has_slice R055 M013/S01 || fail "no requirement maps to M013/S01 (expected R055)"
has_slice R055 M013/S02 || fail "no requirement maps to M013/S02 (expected R055)"
has_slice R056 M013/S03 || fail "no requirement maps to M013/S03 (expected R056)"
has_slice R057 M013/S04 || fail "no requirement maps to M013/S04 (expected R057)"
has_slice R058 M013/S05 || fail "no requirement maps to M013/S05 (expected R058)"
has_slice R059 M013/S05 || fail "no requirement maps to M013/S05 (expected R059)"
has_slice R060 M013/S06 || fail "no requirement maps to M013/S06 (expected R060)"
has_slice R061 M013/S07 || fail "no requirement maps to M013/S07 (expected R061)"
pass "M013 slices S01-S07 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6h. M010 slice coverage. Assert a specific record per slice so R070's broad
#     supporting list cannot mask a broken row.
# ---------------------------------------------------------------------------
has_slice R062 M010/S01 || fail "no requirement maps to M010/S01 (expected R062)"
has_slice R063 M010/S02 || fail "no requirement maps to M010/S02 (expected R063)"
has_slice R064 M010/S03 || fail "no requirement maps to M010/S03 (expected R064)"
has_slice R065 M010/S04 || fail "no requirement maps to M010/S04 (expected R065)"
has_slice R066 M010/S05 || fail "no requirement maps to M010/S05 (expected R066)"
has_slice R067 M010/S06 || fail "no requirement maps to M010/S06 (expected R067)"
has_slice R068 M010/S07 || fail "no requirement maps to M010/S07 (expected R068)"
has_slice R069 M010/S08 || fail "no requirement maps to M010/S08 (expected R069)"
has_slice R070 M010/S08 || fail "no requirement maps to M010/S08 (expected R070)"
pass "M010 slices S01-S08 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6i. M014 slice coverage. Assert a specific record per slice so R081's broad
#     supporting list cannot mask a broken row.
# ---------------------------------------------------------------------------
has_slice R071 M014/S01 || fail "no requirement maps to M014/S01 (expected R071)"
has_slice R073 M014/S02 || fail "no requirement maps to M014/S02 (expected R073)"
has_slice R076 M014/S03 || fail "no requirement maps to M014/S03 (expected R076)"
has_slice R079 M014/S04 || fail "no requirement maps to M014/S04 (expected R079)"
has_slice R071 M014/S05 || fail "no requirement maps to M014/S05 (expected R071)"
has_slice R071 M014/S06 || fail "no requirement maps to M014/S06 (expected R071)"
has_slice R077 M014/S07 || fail "no requirement maps to M014/S07 (expected R077)"
has_slice R078 M014/S08 || fail "no requirement maps to M014/S08 (expected R078)"
has_slice R080 M014/S09 || fail "no requirement maps to M014/S09 (expected R080)"
has_slice R081 M014/S10 || fail "no requirement maps to M014/S10 (expected R081)"
pass "M014 slices S01-S10 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 6j. M015 slice coverage. Assert a specific record per slice so R088's broad
#     supporting list cannot mask a broken row.
# ---------------------------------------------------------------------------
has_slice R082 M015/S01 || fail "no requirement maps to M015/S01 (expected R082)"
has_slice R083 M015/S02 || fail "no requirement maps to M015/S02 (expected R083)"
has_slice R084 M015/S03 || fail "no requirement maps to M015/S03 (expected R084)"
has_slice R085 M015/S04 || fail "no requirement maps to M015/S04 (expected R085)"
has_slice R086 M015/S05 || fail "no requirement maps to M015/S05 (expected R086)"
has_slice R087 M015/S06 || fail "no requirement maps to M015/S06 (expected R087)"
has_slice R088 M015/S07 || fail "no requirement maps to M015/S07 (expected R088)"
pass "M015 slices S01-S07 are each mapped to at least one requirement"

# ---------------------------------------------------------------------------
# 7. Criterion trace tables, parsed section-scoped.
#
# A whole-file /^\| [1-5] \|/ match would collide because the M003, M004,
# M005, M007, M008, M009, M013, M010, M014 and M015 tables all restart their
# criterion numbers at 1. Track the current table by its exact heading, reset
# on any other "## " heading ("### R###" record headings do not match), and
# enforce each table's own criterion range.
# ---------------------------------------------------------------------------
awk -F'|' -v m3="$criteria_m3_tmp" -v m4="$criteria_m4_tmp" -v m5="$criteria_m5_tmp" -v m7="$criteria_m7_tmp" -v m8="$criteria_m8_tmp" -v m9="$criteria_m9_tmp" -v m13="$criteria_m13_tmp" -v m10="$criteria_m10_tmp" -v m14="$criteria_m14_tmp" -v m15="$criteria_m15_tmp" '
function emit(c, r, s, e, dest) {
    gsub(/[[:space:]]/, "", c)
    gsub(/[[:space:]]/, "", r)
    gsub(/[[:space:]]/, "", s)
    sub(/^[[:space:]]+/, "", e)
    sub(/[[:space:]]+$/, "", e)
    print c "|" r "|" s "|" e >> dest
}
/^## M003 Criterion Trace/ { section = "m3"; next }
/^## M004 Criterion Trace/ { section = "m4"; next }
/^## M005 Criterion Trace/ { section = "m5"; next }
/^## M007 Criterion Trace/ { section = "m7"; next }
/^## M008 Criterion Trace/ { section = "m8"; next }
/^## M009 Criterion Trace/ { section = "m9"; next }
/^## M013 Criterion Trace/ { section = "m13"; next }
/^## M010 Criterion Trace/ { section = "m10"; next }
/^## M014 Criterion Trace/ { section = "m14"; next }
/^## M015 Criterion Trace/ { section = "m15"; next }
/^## /                    { section = ""; next }
/^\| [0-9]+ \|/ {
    if (section == "") next
    c = $2
    gsub(/[[:space:]]/, "", c)
    if (section == "m3") {
        if (c + 0 < 1 || c + 0 > 5) {
            print "ERROR: criterion row out of range in M003 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m3)
    } else if (section == "m4") {
        if (c + 0 < 1 || c + 0 > 7) {
            print "ERROR: criterion row out of range in M004 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m4)
    } else if (section == "m5") {
        if (c + 0 < 1 || c + 0 > 8) {
            print "ERROR: criterion row out of range in M005 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m5)
    } else if (section == "m7") {
        if (c + 0 < 1 || c + 0 > 7) {
            print "ERROR: criterion row out of range in M007 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m7)
    } else if (section == "m8") {
        if (c + 0 < 1 || c + 0 > 8) {
            print "ERROR: criterion row out of range in M008 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m8)
    } else if (section == "m9") {
        if (c + 0 < 1 || c + 0 > 9) {
            print "ERROR: criterion row out of range in M009 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m9)
    } else if (section == "m13") {
        if (c + 0 < 1 || c + 0 > 8) {
            print "ERROR: criterion row out of range in M013 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m13)
    } else if (section == "m10") {
        if (c + 0 < 1 || c + 0 > 11) {
            print "ERROR: criterion row out of range in M010 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m10)
    } else if (section == "m14") {
        if (c + 0 < 1 || c + 0 > 12) {
            print "ERROR: criterion row out of range in M014 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m14)
    } else if (section == "m15") {
        if (c + 0 < 1 || c + 0 > 10) {
            print "ERROR: criterion row out of range in M015 table: " c > "/dev/stderr"
            bad = 1
            next
        }
        emit($2, $3, $4, $5, m15)
    }
}
END { exit bad ? 1 : 0 }
' "$REGISTRY" || fail "criterion trace row out of range (M003 allows 1-5, M004 allows 1-7, M005 allows 1-8, M007 allows 1-7, M008 allows 1-8, M009 allows 1-9, M013 allows 1-8, M010 allows 1-11, M014 allows 1-12, M015 allows 1-10)"

declare -A crit_req=()
declare -A crit_sl=()
declare -A crit_ev=()
declare -A crit_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit_count[$c]=$(( ${crit_count[$c]:-0} + 1 ))
    crit_req[$c]="$r"
    crit_sl[$c]="$s"
    crit_ev[$c]="$e"
done < "$criteria_m3_tmp"

declare -A expected_req=(
    [1]="R005"
    [2]="R006,R007"
    [3]="R008,R009,R010"
    [4]="R011,R012,R013,R014"
    [5]="R015"
)
declare -A expected_sl=(
    [1]="S01"
    [2]="S02"
    [3]="S03"
    [4]="S04"
    [5]="S04,S05"
)

for c in 1 2 3 4 5; do
    [[ "${crit_count[$c]:-0}" -eq 1 ]] \
        || fail "criterion $c must have exactly one trace row, found ${crit_count[$c]:-0}"
    [[ "${crit_req[$c]}" == "${expected_req[$c]}" ]] \
        || fail "criterion $c requirements must be '${expected_req[$c]}', got '${crit_req[$c]}'"
    [[ "${crit_sl[$c]}" == "${expected_sl[$c]}" ]] \
        || fail "criterion $c slices must be '${expected_sl[$c]}', got '${crit_sl[$c]}'"
    [[ -n "${crit_ev[$c]}" ]] \
        || fail "criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M003"* ]] \
            || fail "criterion $c references requirement $rid which is not M003-owned"
    done
done
pass "M003 criterion trace rows 1-5 match expected requirement/slice mappings with evidence"

declare -A crit4_req=()
declare -A crit4_sl=()
declare -A crit4_ev=()
declare -A crit4_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit4_count[$c]=$(( ${crit4_count[$c]:-0} + 1 ))
    crit4_req[$c]="$r"
    crit4_sl[$c]="$s"
    crit4_ev[$c]="$e"
done < "$criteria_m4_tmp"

declare -A expected_req_m4=(
    [1]="R016,R018"
    [2]="R016"
    [3]="R018"
    [4]="R017"
    [5]="R016,R017,R018,R019"
    [6]="R014,R020"
    [7]="R020"
)
declare -A expected_sl_m4=(
    [1]="S01,S03"
    [2]="S01"
    [3]="S03"
    [4]="S02"
    [5]="S01,S02,S03,S04"
    [6]="S05"
    [7]="S05"
)

for c in 1 2 3 4 5 6 7; do
    [[ "${crit4_count[$c]:-0}" -eq 1 ]] \
        || fail "M004 criterion $c must have exactly one trace row, found ${crit4_count[$c]:-0}"
    [[ "${crit4_req[$c]}" == "${expected_req_m4[$c]}" ]] \
        || fail "M004 criterion $c requirements must be '${expected_req_m4[$c]}', got '${crit4_req[$c]}'"
    [[ "${crit4_sl[$c]}" == "${expected_sl_m4[$c]}" ]] \
        || fail "M004 criterion $c slices must be '${expected_sl_m4[$c]}', got '${crit4_sl[$c]}'"
    [[ -n "${crit4_ev[$c]}" ]] \
        || fail "M004 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit4_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M004 criterion $c references unknown requirement: $rid"
        if [[ "$rid" == "R014" ]]; then
            # R014 is the M003-owned smoke-harness row that M004 extended with
            # the fourth harness, so prove the extension rather than M004 ownership.
            [[ "${val_of[$rid]}" == *"pack_smoke.sh"* ]] \
                || fail "M004 criterion $c references R014 but its Validation does not cite pack_smoke.sh"
        else
            text="${owner_of[$rid]} ${supp_of[$rid]}"
            [[ "$text" == *"M004"* ]] \
                || fail "M004 criterion $c references requirement $rid which is not M004-owned"
        fi
    done
done
pass "M004 criterion trace rows 1-7 match expected requirement/slice mappings with evidence"

declare -A crit5_req=()
declare -A crit5_sl=()
declare -A crit5_ev=()
declare -A crit5_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit5_count[$c]=$(( ${crit5_count[$c]:-0} + 1 ))
    crit5_req[$c]="$r"
    crit5_sl[$c]="$s"
    crit5_ev[$c]="$e"
done < "$criteria_m5_tmp"

declare -A expected_req_m5=(
    [1]="R022"
    [2]="R021"
    [3]="R023"
    [4]="R024"
    [5]="R025"
    [6]="R024"
    [7]="R022"
    [8]="R026"
)
declare -A expected_sl_m5=(
    [1]="S02,S03"
    [2]="S01"
    [3]="S02"
    [4]="S04"
    [5]="S05"
    [6]="S04"
    [7]="S02,S03"
    [8]="S06"
)

for c in 1 2 3 4 5 6 7 8; do
    [[ "${crit5_count[$c]:-0}" -eq 1 ]] \
        || fail "M005 criterion $c must have exactly one trace row, found ${crit5_count[$c]:-0}"
    [[ "${crit5_req[$c]}" == "${expected_req_m5[$c]}" ]] \
        || fail "M005 criterion $c requirements must be '${expected_req_m5[$c]}', got '${crit5_req[$c]}'"
    [[ "${crit5_sl[$c]}" == "${expected_sl_m5[$c]}" ]] \
        || fail "M005 criterion $c slices must be '${expected_sl_m5[$c]}', got '${crit5_sl[$c]}'"
    [[ -n "${crit5_ev[$c]}" ]] \
        || fail "M005 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit5_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M005 criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M005"* ]] \
            || fail "M005 criterion $c references requirement $rid which is not M005-owned"
    done
done
pass "M005 criterion trace rows 1-8 match expected requirement/slice mappings with evidence"

declare -A crit7_req=()
declare -A crit7_sl=()
declare -A crit7_ev=()
declare -A crit7_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit7_count[$c]=$(( ${crit7_count[$c]:-0} + 1 ))
    crit7_req[$c]="$r"
    crit7_sl[$c]="$s"
    crit7_ev[$c]="$e"
done < "$criteria_m7_tmp"

declare -A expected_req_m7=(
    [1]="R028,R031,R032"
    [2]="R027"
    [3]="R029"
    [4]="R034"
    [5]="R028"
    [6]="R033"
    [7]="R034"
)
declare -A expected_sl_m7=(
    [1]="S02,S05,S06"
    [2]="S01"
    [3]="S03"
    [4]="S08"
    [5]="S02"
    [6]="S07"
    [7]="S08"
)

for c in 1 2 3 4 5 6 7; do
    [[ "${crit7_count[$c]:-0}" -eq 1 ]] \
        || fail "M007 criterion $c must have exactly one trace row, found ${crit7_count[$c]:-0}"
    [[ "${crit7_req[$c]}" == "${expected_req_m7[$c]}" ]] \
        || fail "M007 criterion $c requirements must be '${expected_req_m7[$c]}', got '${crit7_req[$c]}'"
    [[ "${crit7_sl[$c]}" == "${expected_sl_m7[$c]}" ]] \
        || fail "M007 criterion $c slices must be '${expected_sl_m7[$c]}', got '${crit7_sl[$c]}'"
    [[ -n "${crit7_ev[$c]}" ]] \
        || fail "M007 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit7_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M007 criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M007"* ]] \
            || fail "M007 criterion $c references requirement $rid which is not M007-owned"
    done
done
pass "M007 criterion trace rows 1-7 match expected requirement/slice mappings with evidence"

declare -A crit8_req=()
declare -A crit8_sl=()
declare -A crit8_ev=()
declare -A crit8_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit8_count[$c]=$(( ${crit8_count[$c]:-0} + 1 ))
    crit8_req[$c]="$r"
    crit8_sl[$c]="$s"
    crit8_ev[$c]="$e"
done < "$criteria_m8_tmp"

declare -A expected_req_m8=(
    [1]="R036,R037,R038"
    [2]="R041"
    [3]="R039,R040"
    [4]="R035,R043"
    [5]="R042"
    [6]="R043"
    [7]="R044"
    [8]="R044"
)
declare -A expected_sl_m8=(
    [1]="S01,S04,S05"
    [2]="S02,S05,S06,S07"
    [3]="S03,S06"
    [4]="S01,S03"
    [5]="S02"
    [6]="S01,S03"
    [7]="S08,S09"
    [8]="S04,S05,S06,S07,S09"
)

for c in 1 2 3 4 5 6 7 8; do
    [[ "${crit8_count[$c]:-0}" -eq 1 ]] \
        || fail "M008 criterion $c must have exactly one trace row, found ${crit8_count[$c]:-0}"
    [[ "${crit8_req[$c]}" == "${expected_req_m8[$c]}" ]] \
        || fail "M008 criterion $c requirements must be '${expected_req_m8[$c]}', got '${crit8_req[$c]}'"
    [[ "${crit8_sl[$c]}" == "${expected_sl_m8[$c]}" ]] \
        || fail "M008 criterion $c slices must be '${expected_sl_m8[$c]}', got '${crit8_sl[$c]}'"
    [[ -n "${crit8_ev[$c]}" ]] \
        || fail "M008 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit8_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M008 criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M008"* ]] \
            || fail "M008 criterion $c references requirement $rid which is not M008-owned"
    done
done
pass "M008 criterion trace rows 1-8 match expected requirement/slice mappings with evidence"

declare -A crit9_req=()
declare -A crit9_sl=()
declare -A crit9_ev=()
declare -A crit9_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit9_count[$c]=$(( ${crit9_count[$c]:-0} + 1 ))
    crit9_req[$c]="$r"
    crit9_sl[$c]="$s"
    crit9_ev[$c]="$e"
done < "$criteria_m9_tmp"

declare -A expected_req_m9=(
    [1]="R045,R048,R049"
    [2]="R049,R050"
    [3]="R047,R051"
    [4]="R048,R051,R052"
    [5]="R046"
    [6]="R053"
    [7]="R050"
    [8]="R045,R053"
    [9]="R054"
)
declare -A expected_sl_m9=(
    [1]="S01,S04,S05"
    [2]="S05,S06"
    [3]="S03,S07"
    [4]="S04,S07,S08"
    [5]="S02"
    [6]="S09"
    [7]="S06"
    [8]="S01,S09"
    [9]="S01,S02,S03,S04,S05,S06,S07,S08,S09,S10"
)

for c in $(seq 1 9); do
    [[ "${crit9_count[$c]:-0}" -eq 1 ]] \
        || fail "M009 criterion $c must have exactly one trace row, found ${crit9_count[$c]:-0}"
    [[ "${crit9_req[$c]}" == "${expected_req_m9[$c]}" ]] \
        || fail "M009 criterion $c requirements must be '${expected_req_m9[$c]}', got '${crit9_req[$c]}'"
    [[ "${crit9_sl[$c]}" == "${expected_sl_m9[$c]}" ]] \
        || fail "M009 criterion $c slices must be '${expected_sl_m9[$c]}', got '${crit9_sl[$c]}'"
    [[ -n "${crit9_ev[$c]}" ]] \
        || fail "M009 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit9_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M009 criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M009"* ]] \
            || fail "M009 criterion $c references requirement $rid which is not M009-owned"
    done
done
pass "M009 criterion trace rows 1-9 match expected requirement/slice mappings with evidence"

declare -A crit13_req=()
declare -A crit13_sl=()
declare -A crit13_ev=()
declare -A crit13_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit13_count[$c]=$(( ${crit13_count[$c]:-0} + 1 ))
    crit13_req[$c]="$r"
    crit13_sl[$c]="$s"
    crit13_ev[$c]="$e"
done < "$criteria_m13_tmp"

declare -A expected_req_m13=(
    [1]="R055"
    [2]="R056"
    [3]="R057"
    [4]="R058"
    [5]="R059"
    [6]="R060"
    [7]="R061"
    [8]="R061"
)
declare -A expected_sl_m13=(
    [1]="S01,S02"
    [2]="S03"
    [3]="S04"
    [4]="S05"
    [5]="S05"
    [6]="S06"
    [7]="S01,S02,S03,S04,S05,S06,S07"
    [8]="S01,S02,S03,S04,S05,S06,S07"
)

for c in $(seq 1 8); do
    [[ "${crit13_count[$c]:-0}" -eq 1 ]] \
        || fail "M013 criterion $c must have exactly one trace row, found ${crit13_count[$c]:-0}"
    [[ "${crit13_req[$c]}" == "${expected_req_m13[$c]}" ]] \
        || fail "M013 criterion $c requirements must be '${expected_req_m13[$c]}', got '${crit13_req[$c]}'"
    [[ "${crit13_sl[$c]}" == "${expected_sl_m13[$c]}" ]] \
        || fail "M013 criterion $c slices must be '${expected_sl_m13[$c]}', got '${crit13_sl[$c]}'"
    [[ -n "${crit13_ev[$c]}" ]] \
        || fail "M013 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit13_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M013 criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M013"* ]] \
            || fail "M013 criterion $c references requirement $rid which is not M013-owned"
    done
done
pass "M013 criterion trace rows 1-8 match expected requirement/slice mappings with evidence"

declare -A crit10_req=()
declare -A crit10_sl=()
declare -A crit10_ev=()
declare -A crit10_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit10_count[$c]=$(( ${crit10_count[$c]:-0} + 1 ))
    crit10_req[$c]="$r"
    crit10_sl[$c]="$s"
    crit10_ev[$c]="$e"
done < "$criteria_m10_tmp"

declare -A expected_req_m10=(
    [1]="R062,R063"
    [2]="R065"
    [3]="R066"
    [4]="R067"
    [5]="R066"
    [6]="R068"
    [7]="R064"
    [8]="R064,R068"
    [9]="R064"
    [10]="R063,R069"
    [11]="R062,R063,R064,R065,R066,R067,R068,R069"
)
declare -A expected_sl_m10=(
    [1]="S01,S02"
    [2]="S04"
    [3]="S05"
    [4]="S06"
    [5]="S05"
    [6]="S07"
    [7]="S03"
    [8]="S03,S07"
    [9]="S03"
    [10]="S02,S08"
    [11]="S01,S02,S03,S04,S05,S06,S07,S08"
)

for c in $(seq 1 11); do
    [[ "${crit10_count[$c]:-0}" -eq 1 ]] \
        || fail "M010 criterion $c must have exactly one trace row, found ${crit10_count[$c]:-0}"
    [[ "${crit10_req[$c]}" == "${expected_req_m10[$c]}" ]] \
        || fail "M010 criterion $c requirements must be '${expected_req_m10[$c]}', got '${crit10_req[$c]}'"
    [[ "${crit10_sl[$c]}" == "${expected_sl_m10[$c]}" ]] \
        || fail "M010 criterion $c slices must be '${expected_sl_m10[$c]}', got '${crit10_sl[$c]}'"
    [[ -n "${crit10_ev[$c]}" ]] \
        || fail "M010 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit10_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M010 criterion $c references unknown requirement: $rid"
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M010"* ]] \
            || fail "M010 criterion $c references requirement $rid which is not M010-owned"
    done
done
pass "M010 criterion trace rows 1-11 match expected requirement/slice mappings with evidence"

declare -A crit14_req=()
declare -A crit14_sl=()
declare -A crit14_ev=()
declare -A crit14_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit14_count[$c]=$(( ${crit14_count[$c]:-0} + 1 ))
    crit14_req[$c]="$r"
    crit14_sl[$c]="$s"
    crit14_ev[$c]="$e"
done < "$criteria_m14_tmp"

declare -A expected_req_m14=(
    [1]="R071,R072"
    [2]="R075"
    [3]="R073"
    [4]="R074"
    [5]="R076,R065"
    [6]="R077,R055"
    [7]="R078,R063"
    [8]="R079,R064"
    [9]="R080"
    [10]="R081,R069"
    [11]="R081"
    [12]="R081"
)
declare -A expected_sl_m14=(
    [1]="S01,S02,S05,S06"
    [2]="S01,S09"
    [3]="S02"
    [4]="S02"
    [5]="S03"
    [6]="S07"
    [7]="S08"
    [8]="S04"
    [9]="S09"
    [10]="S09"
    [11]="S10"
    [12]="S09,S10"
)

# The five prior rows M014's trace cites as supporting surfaces. They are
# immutable (D220), so the M014 ownership assertion accepts them explicitly
# instead of requiring an M014 mention that cannot be added to them.
M014_SUPPORT_SET=" R055 R063 R064 R065 R069 "

for c in $(seq 1 12); do
    [[ "${crit14_count[$c]:-0}" -eq 1 ]] \
        || fail "M014 criterion $c must have exactly one trace row, found ${crit14_count[$c]:-0}"
    [[ "${crit14_req[$c]}" == "${expected_req_m14[$c]}" ]] \
        || fail "M014 criterion $c requirements must be '${expected_req_m14[$c]}', got '${crit14_req[$c]}'"
    [[ "${crit14_sl[$c]}" == "${expected_sl_m14[$c]}" ]] \
        || fail "M014 criterion $c slices must be '${expected_sl_m14[$c]}', got '${crit14_sl[$c]}'"
    [[ -n "${crit14_ev[$c]}" ]] \
        || fail "M014 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit14_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M014 criterion $c references unknown requirement: $rid"
        case "$M014_SUPPORT_SET" in
            *" $rid "*) continue ;;
        esac
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M014"* ]] \
            || fail "M014 criterion $c references requirement $rid which is not M014-owned"
    done
done
pass "M014 criterion trace rows 1-12 match expected requirement/slice mappings with evidence"

declare -A crit15_req=()
declare -A crit15_sl=()
declare -A crit15_ev=()
declare -A crit15_count=()

while IFS='|' read -r c r s e; do
    [[ -n "$c" ]] || continue
    crit15_count[$c]=$(( ${crit15_count[$c]:-0} + 1 ))
    crit15_req[$c]="$r"
    crit15_sl[$c]="$s"
    crit15_ev[$c]="$e"
done < "$criteria_m15_tmp"

declare -A expected_req_m15=(
    [1]="R086,R087"
    [2]="R082,R063"
    [3]="R083"
    [4]="R083"
    [5]="R083,R084"
    [6]="R084,R064"
    [7]="R084"
    [8]="R085,R087,R065"
    [9]="R088,R069"
    [10]="R082,R083"
)
declare -A expected_sl_m15=(
    [1]="S05,S06"
    [2]="S01"
    [3]="S02"
    [4]="S02"
    [5]="S02,S03"
    [6]="S03"
    [7]="S03"
    [8]="S04,S06"
    [9]="S01,S02,S07"
    [10]="S01,S02,S03,S05"
)

# The four prior rows M015's trace cites as supporting surfaces. They are
# immutable (D220), so the M015 ownership assertion accepts them explicitly.
M015_SUPPORT_SET=" R063 R064 R065 R069 "

for c in $(seq 1 10); do
    [[ "${crit15_count[$c]:-0}" -eq 1 ]] \
        || fail "M015 criterion $c must have exactly one trace row, found ${crit15_count[$c]:-0}"
    [[ "${crit15_req[$c]}" == "${expected_req_m15[$c]}" ]] \
        || fail "M015 criterion $c requirements must be '${expected_req_m15[$c]}', got '${crit15_req[$c]}'"
    [[ "${crit15_sl[$c]}" == "${expected_sl_m15[$c]}" ]] \
        || fail "M015 criterion $c slices must be '${expected_sl_m15[$c]}', got '${crit15_sl[$c]}'"
    [[ -n "${crit15_ev[$c]}" ]] \
        || fail "M015 criterion $c has an empty Evidence cell"

    IFS=',' read -r -a refs <<< "${crit15_req[$c]}"
    for rid in "${refs[@]}"; do
        [[ -n "${seen[$rid]:-}" ]] \
            || fail "M015 criterion $c references unknown requirement: $rid"
        case "$M015_SUPPORT_SET" in
            *" $rid "*) continue ;;
        esac
        text="${owner_of[$rid]} ${supp_of[$rid]}"
        [[ "$text" == *"M015"* ]] \
            || fail "M015 criterion $c references requirement $rid which is not M015-owned"
    done
done
pass "M015 criterion trace rows 1-10 match expected requirement/slice mappings with evidence"

# ---------------------------------------------------------------------------
# 8. Shipped identity. The legacy project name must not survive anywhere in
#    the manifest, lockfile, source, or scripts. The pattern accepts hyphen,
#    underscore, or space separators and is case-insensitive so the shipped
#    window title cannot slip past the gate. IDENTITY_SCAN_ROOT lets the
#    negative-path selftest point this check at a throwaway tree.
# ---------------------------------------------------------------------------
IDENTITY_ROOT="${IDENTITY_SCAN_ROOT:-$REPO_ROOT}"
identity_matches=""
for surface in \
    "$IDENTITY_ROOT/Cargo.toml" \
    "$IDENTITY_ROOT/Cargo.lock" \
    "$IDENTITY_ROOT/src" \
    "$IDENTITY_ROOT/scripts"; do
    [[ -e "$surface" ]] || continue
    matches="$(grep -REin 'minecraft[ _-]?clone' "$surface" 2>/dev/null || true)"
    if [[ -n "$matches" ]]; then
        identity_matches+="$matches"$'\n'
    fi
done
[[ -z "$identity_matches" ]] \
    || fail "legacy project identity found on the shipped surface:"$'\n'"$identity_matches"
pass "shipped identity is opencraft with no legacy project-name references"

# ---------------------------------------------------------------------------
# 9. Print all ten validated criterion trace tables.
# ---------------------------------------------------------------------------
echo
echo "M003 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit_req[$c]}" "${crit_sl[$c]}" "${crit_ev[$c]}"
done
echo
echo "M004 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5 6 7; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit4_req[$c]}" "${crit4_sl[$c]}" "${crit4_ev[$c]}"
done
echo
echo "M005 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5 6 7 8; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit5_req[$c]}" "${crit5_sl[$c]}" "${crit5_ev[$c]}"
done
echo
echo "M007 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5 6 7; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit7_req[$c]}" "${crit7_sl[$c]}" "${crit7_ev[$c]}"
done
echo
echo "M008 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5 6 7 8; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit8_req[$c]}" "${crit8_sl[$c]}" "${crit8_ev[$c]}"
done
echo
echo "M009 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5 6 7 8 9; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit9_req[$c]}" "${crit9_sl[$c]}" "${crit9_ev[$c]}"
done
echo
echo "M013 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5 6 7 8; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit13_req[$c]}" "${crit13_sl[$c]}" "${crit13_ev[$c]}"
done
echo
echo "M010 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in $(seq 1 11); do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit10_req[$c]}" "${crit10_sl[$c]}" "${crit10_ev[$c]}"
done
echo
echo "M014 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in $(seq 1 12); do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit14_req[$c]}" "${crit14_sl[$c]}" "${crit14_ev[$c]}"
done
echo
echo "M015 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in $(seq 1 10); do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit15_req[$c]}" "${crit15_sl[$c]}" "${crit15_ev[$c]}"
done
echo

echo "ALL CHECKS PASSED"
