#!/usr/bin/env bash
# Requirements registry checker (M003/S05-T02).
#
# Validates the checked-in REQUIREMENTS-REGISTRY.md:
#   1. the registry file exists,
#   2. every "### R### — ..." record parses into the eight contract bullets,
#   3. exactly R001-R015 appear once each, with no duplicate or malformed IDs,
#   4. required fields are non-empty and Status is exactly "active",
#   5. ownership follows M001/M002 for R001-R004 and M003 for R005-R015,
#   6. every M003 slice (S01-S05) is covered by at least one mapped record,
#   7. the M003 criterion trace table matches the expected requirement/slice
#      mapping for criteria 1-5 and carries non-empty evidence,
#   8. the validated criterion trace table is printed.
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
criteria_tmp="$(mktemp)"
trap 'rm -f "$record_tmp" "$criteria_tmp"' EXIT

# ---------------------------------------------------------------------------
# 2. Parse each record heading and its contract bullets.
#
# Emits one line per record:
#   id|class|status|description|why|source|owner|supporting|validation
# A record ends at the next heading; bullets with no record are ignored.
# ---------------------------------------------------------------------------
awk '
function flush() {
    if (id == "") return
    print id "|" class "|" status "|" desc "|" why "|" src "|" own "|" supp "|" val
}
/^### R[0-9][0-9][0-9] / {
    flush()
    id = $2
    class = status = desc = why = src = own = supp = val = ""
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
END { flush() }
' "$REGISTRY" > "$record_tmp"

# ---------------------------------------------------------------------------
# 3. Exactly 15 unique R001-R015 records.
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

record_count=0
while IFS='|' read -r id class status desc why src own supp val; do
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
    record_count=$((record_count + 1))
done < "$record_tmp"

[[ "$record_count" -eq 15 ]] || fail "expected 15 requirement records, found $record_count"

for n in $(seq 1 15); do
    id="$(printf 'R%03d' "$n")"
    [[ -n "${seen[$id]:-}" ]] || fail "missing requirement record: $id"
done

pass "15 requirement records R001-R015 present exactly once with unique IDs"

# ---------------------------------------------------------------------------
# 4. Required fields are non-empty; Status is exactly "active".
# ---------------------------------------------------------------------------
for id in "${!seen[@]}"; do
    [[ -n "${class_of[$id]}" ]] || fail "$id has an empty Class field"
    [[ -n "${desc_of[$id]}" ]] || fail "$id has an empty Description field"
    [[ -n "${why_of[$id]}" ]] || fail "$id has an empty Why-it-matters field"
    [[ -n "${src_of[$id]}" ]] || fail "$id has an empty Source field"
    [[ -n "${owner_of[$id]}" ]] || fail "$id has an empty Primary owning slice field"
    [[ -n "${val_of[$id]}" ]] || fail "$id has an empty Validation field"
    [[ "${status_of[$id]}" == "active" ]] \
        || fail "$id Status must be 'active', got '${status_of[$id]}'"
done
pass "all records carry non-empty Class/Description/Why/Source/Owner/Validation and Status active"

# ---------------------------------------------------------------------------
# 5. Ownership: R001-R004 -> M001 or M002; R005-R015 -> M003.
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
pass "ownership matches M001/M002 for R001-R004 and M003 for R005-R015"

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
# 7. M003 criterion trace table.
# ---------------------------------------------------------------------------
awk -F'|' '
/^\| [1-5] \|/ {
    c = $2; r = $3; s = $4; e = $5
    gsub(/[[:space:]]/, "", c)
    gsub(/[[:space:]]/, "", r)
    gsub(/[[:space:]]/, "", s)
    sub(/^[[:space:]]+/, "", e)
    sub(/[[:space:]]+$/, "", e)
    print c "|" r "|" s "|" e
}
' "$REGISTRY" > "$criteria_tmp"

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
done < "$criteria_tmp"

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

# ---------------------------------------------------------------------------
# 8. Print the validated criterion trace table.
# ---------------------------------------------------------------------------
echo
echo "M003 Criterion Trace"
printf '%-10s %-27s %-9s %s\n' "Criterion" "Requirements" "Slices" "Evidence"
for c in 1 2 3 4 5; do
    printf '%-10s %-27s %-9s %s\n' "$c" "${crit_req[$c]}" "${crit_sl[$c]}" "${crit_ev[$c]}"
done
echo

echo "ALL CHECKS PASSED"
