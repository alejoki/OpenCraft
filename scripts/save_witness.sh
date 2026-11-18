#!/usr/bin/env bash
# Sourceable v10 save-archive witness helpers (M018/S02-T05).
#
# The live `.mcvx` save is the neutral `opencraft-storage` sectioned archive:
#
#   header (16 bytes)
#     magic   [u8; 4] = "MCVX"
#     version u32 LE  (offset 4; this helper only accepts 10)
#     flags   u32 LE  (offset 8; must be 0)
#     count   u32 LE  (offset 12; v10 writes >= 9 fixed sections)
#   directory (count * 21 bytes, ascending section id)
#     id       u32 LE
#     offset   u64 LE
#     stored   u32 LE
#     raw      u32 LE  (= stored for codec 0)
#     codec    u8      (= 0, stored verbatim)
#   bodies (contiguous, gapless, in directory order, ending exactly at EOF)
#
# The smoke scripts assert structure (magic/version/flags/count plus the
# section bodies they actually witnessed) instead of pinning the old v9 flat
# byte offsets. Section ids are the stable v10 contract:
#
#   1  sorted kind-tagged content table: u32 count, then
#      {u8 kind, u16 name_len, name} per entry, sorted by (name, kind).
#      Kind tags: 1 block, 2 material, 3 tool, 4 generic item, 5 mod tool.
#   2  world/player: seed u32; position x/y/z f32; yaw f32; pitch f32;
#      fly u8; selected_slot u8; render_distance u16; time_of_day u32;
#      day_index u32; season_length_days u32; mode u8 (0 Survival,
#      1 Creative); health u16; hunger u16; air u16. 47 bytes.
#   3  inventory extension (36 slots, cursor, both grids).
#   4  chests: u32 count plus sorted chest records.
#   5  mobs: u32 count plus sorted 19-byte mob records
#      {u8 kind, f32 x/y/z, f32 yaw, u16 health}.
#   6  content meta: u32 pack count, pack identities, u64 registry digest.
#   7  config: u16 schema plus u64 effective-value digest. 10 bytes.
#   8  crops: u32 count plus sorted (x, y, z) i32 triples.
#   9  chunk index: u32 count plus sorted
#      {i32 x, i32 y, i32 z, u32 edit_count} records.
#   16+i  per-chunk palette section for chunk index entry i.
#
# Every helper fails closed with a `SAVE WITNESS FAIL:` message naming the
# file/field that failed. The helper is bash-only (od/dd/head/wc) and is
# sourced by the real-binary smokes; it never edits a save.
#
# Usage:
#   source "$SCRIPT_DIR/save_witness.sh"
#   assert_mcvx_v10 "$SAVE"
#   DAY=$(mcvx_section_u32 "$SAVE" 2 32)
#   mcvx_section_to_file "$SAVE" 2 "$TEMP_DIR/world.bin"

# Fail closed. These helpers are only used by smoke scripts and every failure
# is fatal, so the message exits directly instead of returning a status a
# caller could forget to check.
save_witness_fail() {
    echo "SAVE WITNESS FAIL: $*" >&2
    exit 1
}

# Print one unsigned byte at a file offset.
read_u8() {
    local file="$1" offset="$2" value
    value=$(od -An -tu1 -j "$offset" -N 1 "$file" 2>/dev/null | tr -d ' \n') ||
        save_witness_fail "cannot read u8 at offset $offset of $file"
    [[ "$value" =~ ^[0-9]+$ ]] ||
        save_witness_fail "u8 at offset $offset of $file is not readable (EOF or unreadable)"
    printf '%s\n' "$value"
}

# Print one little-endian u16 at a file offset.
read_u16_le() {
    local file="$1" offset="$2" value
    value=$(od -An -tu2 --endian=little -j "$offset" -N 2 "$file" 2>/dev/null | tr -d ' \n') ||
        save_witness_fail "cannot read u16 at offset $offset of $file"
    [[ "$value" =~ ^[0-9]+$ ]] ||
        save_witness_fail "u16 at offset $offset of $file is not readable (EOF or unreadable)"
    printf '%s\n' "$value"
}

# Print one little-endian u32 at a file offset.
read_u32_le() {
    local file="$1" offset="$2" value
    value=$(od -An -tu4 --endian=little -j "$offset" -N 4 "$file" 2>/dev/null | tr -d ' \n') ||
        save_witness_fail "cannot read u32 at offset $offset of $file"
    [[ "$value" =~ ^[0-9]+$ ]] ||
        save_witness_fail "u32 at offset $offset of $file is not readable (EOF or unreadable)"
    printf '%s\n' "$value"
}

# Print one little-endian u64 at a file offset.
read_u64_le() {
    local file="$1" offset="$2" value
    value=$(od -An -tu8 --endian=little -j "$offset" -N 8 "$file" 2>/dev/null | tr -d ' \n') ||
        save_witness_fail "cannot read u64 at offset $offset of $file"
    [[ "$value" =~ ^[0-9]+$ ]] ||
        save_witness_fail "u64 at offset $offset of $file is not readable (EOF or unreadable)"
    printf '%s\n' "$value"
}

# Parsed archive state. Valid after mcvx_parse_archive returns.
MCVX_ARCHIVE_BYTES=0
MCVX_SECTION_COUNT=0
MCVX_SECTION_OFFSET=()
MCVX_SECTION_LEN=()

# Parse and validate the v10 header and directory of `$1`.
#
# Rejects a missing/truncated file, wrong magic/version/flags, fewer than the
# nine fixed sections, unsorted or duplicate ids, a non-zero codec, a stored
# and raw length mismatch, and any body gap, overlap, or trailing byte.
mcvx_parse_archive() {
    local file="$1"
    [[ -f "$file" ]] || save_witness_fail "archive not found: $file"

    local size
    size=$(wc -c <"$file" | tr -d ' ')
    [[ "$size" =~ ^[0-9]+$ ]] || save_witness_fail "cannot size archive: $file"
    (( size >= 16 )) ||
        save_witness_fail "$file is $size bytes, shorter than the 16-byte MCVX header"

    local magic
    magic=$(head -c 4 "$file") || save_witness_fail "cannot read magic of $file"
    [[ "$magic" == "MCVX" ]] ||
        save_witness_fail "$file magic is '$magic' (expected MCVX)"

    local version flags count
    version=$(read_u32_le "$file" 4)
    flags=$(read_u32_le "$file" 8)
    count=$(read_u32_le "$file" 12)
    [[ "$version" == 10 ]] ||
        save_witness_fail "$file version is $version (expected 10)"
    [[ "$flags" == 0 ]] ||
        save_witness_fail "$file flags are $flags (expected 0)"
    (( count >= 9 )) ||
        save_witness_fail "$file section count is $count (expected >= 9 fixed sections)"

    local directory_end=$((16 + count * 21))
    (( directory_end <= size )) ||
        save_witness_fail "$file directory needs $directory_end bytes but the file is $size bytes"

    # Read the whole directory once as decimal bytes and decode entries in
    # bash, so a many-section archive does not spawn 5 processes per entry.
    local -a raw_bytes=()
    local raw
    raw=$(od -An -v -tu1 -j 16 -N $((count * 21)) "$file" 2>/dev/null | tr '\n' ' ') ||
        save_witness_fail "cannot read the section directory of $file"
    read -r -a raw_bytes <<<"$raw"
    (( ${#raw_bytes[@]} == count * 21 )) ||
        save_witness_fail "$file directory decodes to ${#raw_bytes[@]} bytes (expected $((count * 21)))"

    MCVX_ARCHIVE_BYTES=$size
    MCVX_SECTION_COUNT=$count
    MCVX_SECTION_OFFSET=()
    MCVX_SECTION_LEN=()

    local index base id offset stored raw_len codec
    local expected_offset=$directory_end
    local previous_id=-1
    for ((index = 0; index < count; index++)); do
        base=$((index * 21))
        id=$((raw_bytes[base] +
            raw_bytes[base + 1] * 256 +
            raw_bytes[base + 2] * 65536 +
            raw_bytes[base + 3] * 16777216))
        offset=$((raw_bytes[base + 4] +
            raw_bytes[base + 5] * 256 +
            raw_bytes[base + 6] * 65536 +
            raw_bytes[base + 7] * 16777216 +
            raw_bytes[base + 8] * 4294967296 +
            raw_bytes[base + 9] * 1099511627776 +
            raw_bytes[base + 10] * 281474976710656 +
            raw_bytes[base + 11] * 72057594037927936))
        stored=$((raw_bytes[base + 12] +
            raw_bytes[base + 13] * 256 +
            raw_bytes[base + 14] * 65536 +
            raw_bytes[base + 15] * 16777216))
        raw_len=$((raw_bytes[base + 16] +
            raw_bytes[base + 17] * 256 +
            raw_bytes[base + 18] * 65536 +
            raw_bytes[base + 19] * 16777216))
        codec=${raw_bytes[base + 20]}

        (( id > previous_id )) ||
            save_witness_fail "$file section directory is not strictly ascending at entry $index (id $id after $previous_id)"
        previous_id=$id
        [[ "$codec" == 0 ]] ||
            save_witness_fail "$file section $id uses codec $codec (expected 0)"
        [[ "$raw_len" == "$stored" ]] ||
            save_witness_fail "$file section $id stored length $stored differs from raw length $raw_len"
        (( offset == expected_offset )) ||
            save_witness_fail "$file section $id starts at $offset (expected $expected_offset: bodies must be contiguous)"
        expected_offset=$((offset + stored))
        (( expected_offset <= size )) ||
            save_witness_fail "$file section $id ends at $expected_offset beyond the $size-byte file"

        MCVX_SECTION_OFFSET[$id]=$offset
        MCVX_SECTION_LEN[$id]=$stored
    done

    (( expected_offset == size )) ||
        save_witness_fail "$file ends at $expected_offset but has $((size - expected_offset)) trailing byte(s)"
}

# Parse `$1` and print the structural evidence line.
assert_mcvx_v10() {
    local file="$1"
    mcvx_parse_archive "$file"
    printf 'mcvx_v10: bytes=%s sections=%s magic=MCVX version=10 flags=0 header=16\n' \
        "$MCVX_ARCHIVE_BYTES" "$MCVX_SECTION_COUNT"
}

# Print the section count of a validated v10 archive.
mcvx_section_count() {
    mcvx_parse_archive "$1"
    printf '%s\n' "$MCVX_SECTION_COUNT"
}

# Resolve `$2` in `$1`, setting MCVX_BODY_OFFSET/MCVX_BODY_LEN or failing.
mcvx_require_section() {
    local file="$1" id="$2"
    mcvx_parse_archive "$file"
    [[ -n "${MCVX_SECTION_OFFSET[$id]+present}" ]] ||
        save_witness_fail "$file has no section $id"
    MCVX_BODY_OFFSET=${MCVX_SECTION_OFFSET[$id]}
    MCVX_BODY_LEN=${MCVX_SECTION_LEN[$id]}
}

# Print the stored byte length of section `$2` in `$1`.
mcvx_section_len() {
    mcvx_require_section "$1" "$2"
    printf '%s\n' "$MCVX_BODY_LEN"
}

# Print the file offset of section `$2` in `$1`.
mcvx_section_offset() {
    mcvx_require_section "$1" "$2"
    printf '%s\n' "$MCVX_BODY_OFFSET"
}

# Copy section `$2`'s body from `$1` into `$3`.
mcvx_section_to_file() {
    local file="$1" id="$2" out="$3"
    mcvx_require_section "$file" "$id"
    dd if="$file" of="$out" bs=1 skip="$MCVX_BODY_OFFSET" count="$MCVX_BODY_LEN" status=none ||
        save_witness_fail "cannot extract section $id from $file into $out"
}

# Print one little-endian u8 at relative offset `$3` inside section `$2`.
mcvx_section_u8() {
    local file="$1" id="$2" relative="$3"
    mcvx_require_section "$file" "$id"
    (( relative >= 0 && relative + 1 <= MCVX_BODY_LEN )) ||
        save_witness_fail "$file section $id is $MCVX_BODY_LEN bytes; u8 at relative $relative is out of range"
    read_u8 "$file" $((MCVX_BODY_OFFSET + relative))
}

# Print one little-endian u16 at relative offset `$3` inside section `$2`.
mcvx_section_u16() {
    local file="$1" id="$2" relative="$3"
    mcvx_require_section "$file" "$id"
    (( relative >= 0 && relative + 2 <= MCVX_BODY_LEN )) ||
        save_witness_fail "$file section $id is $MCVX_BODY_LEN bytes; u16 at relative $relative is out of range"
    read_u16_le "$file" $((MCVX_BODY_OFFSET + relative))
}

# Print one little-endian u32 at relative offset `$3` inside section `$2`.
mcvx_section_u32() {
    local file="$1" id="$2" relative="$3"
    mcvx_require_section "$file" "$id"
    (( relative >= 0 && relative + 4 <= MCVX_BODY_LEN )) ||
        save_witness_fail "$file section $id is $MCVX_BODY_LEN bytes; u32 at relative $relative is out of range"
    read_u32_le "$file" $((MCVX_BODY_OFFSET + relative))
}

# Print one little-endian u64 at relative offset `$3` inside section `$2`.
mcvx_section_u64() {
    local file="$1" id="$2" relative="$3"
    mcvx_require_section "$file" "$id"
    (( relative >= 0 && relative + 8 <= MCVX_BODY_LEN )) ||
        save_witness_fail "$file section $id is $MCVX_BODY_LEN bytes; u64 at relative $relative is out of range"
    read_u64_le "$file" $((MCVX_BODY_OFFSET + relative))
}

# Print section 1's entries as `kind:name`, one per line, in wire order.
#
# The body is `u32 count` then `{u8 kind, u16 name_len, name}`; the helper
# rejects an entry overrun or trailing bytes so a malformed table fails closed
# instead of silently reporting a short list.
mcvx_content_table_names() {
    local file="$1" offset length count cursor index kind name_len name
    mcvx_require_section "$file" 1
    offset=$MCVX_BODY_OFFSET
    length=$MCVX_BODY_LEN
    (( length >= 4 )) ||
        save_witness_fail "$file section 1 is $length bytes, shorter than the u32 content count"
    count=$(read_u32_le "$file" "$offset")
    cursor=4
    for ((index = 0; index < count; index++)); do
        (( cursor + 3 <= length )) ||
            save_witness_fail "$file content table entry $index overruns the $length-byte section 1 body"
        kind=$(read_u8 "$file" $((offset + cursor)))
        name_len=$(read_u16_le "$file" $((offset + cursor + 1)))
        (( cursor + 3 + name_len <= length )) ||
            save_witness_fail "$file content table entry $index name overruns the $length-byte section 1 body"
        name=$(dd if="$file" bs=1 skip=$((offset + cursor + 3)) count="$name_len" status=none) ||
            save_witness_fail "cannot read content table entry $index from $file"
        printf '%s:%s\n' "$kind" "$name"
        cursor=$((cursor + 3 + name_len))
    done
    (( cursor == length )) ||
        save_witness_fail "$file content table body has $((length - cursor)) trailing byte(s)"
}

# Print the number of entries in section 1's content table.
mcvx_content_table_count() {
    local file="$1" offset length
    mcvx_require_section "$file" 1
    offset=$MCVX_BODY_OFFSET
    length=$MCVX_BODY_LEN
    (( length >= 4 )) ||
        save_witness_fail "$file section 1 is $length bytes, shorter than the u32 content count"
    read_u32_le "$file" "$offset"
}

# Attach to a live display for the bounded real-binary smoke launches.
#
# Agent/CI shells can lack DISPLAY/WAYLAND_DISPLAY even when a desktop session
# is live. Prefer the Wayland socket (Xwayland's :0 needs an xauth cookie the
# launching shell may not have, and winit selects the X11 backend whenever
# DISPLAY is set). Probe wayland-0 first, then any other wayland-* socket: some
# hosts expose only wayland-1, and the old wayland-0-only probe fell through to
# X0 and died with XOpenDisplayFailed before any assertion.
smoke_detect_display() {
    if [[ -n "${DISPLAY:-}" || -n "${WAYLAND_DISPLAY:-}" ]]; then
        return 0
    fi
    local runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    local socket
    if [[ -S "$runtime/wayland-0" ]]; then
        export WAYLAND_DISPLAY="wayland-0"
        export XDG_RUNTIME_DIR="$runtime"
        return 0
    fi
    for socket in "$runtime"/wayland-*; do
        [[ -S "$socket" ]] || continue
        export WAYLAND_DISPLAY="${socket##*/}"
        export XDG_RUNTIME_DIR="$runtime"
        return 0
    done
    if [[ -e "/tmp/.X11-unix/X0" ]]; then
        export DISPLAY=":0"
    fi
}
