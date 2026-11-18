//! Neutral sectioned save archive container.
//!
//! The archive is a strict, fail-closed framing layer. It knows the header,
//! the fixed 21-byte directory entries and the raw codec; it never learns
//! v10 section ids, content semantics or game types. Section bodies stay
//! opaque and are only sliced on request, so parsing validates structure
//! without paying for body decoding.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! header (16 bytes)
//!   magic  [u8; 4]  = "MCVX"
//!   version u32     (offset 4; the 8-byte catalog probe keeps working)
//!   flags   u32     (offset 8; must be 0)
//!   count   u32     (offset 12)
//! directory (count * 21 bytes)
//!   id       u32
//!   offset   u64
//!   stored   u32
//!   raw      u32    (= stored for codec 0)
//!   codec    u8     (= 0)
//! bodies (contiguous, in ascending id order)
//! ```

use crate::StorageError;

/// Save archive magic, kept at offset 0 for the existing catalog probe.
pub const MAGIC: [u8; 4] = *b"MCVX";
/// Whole-image byte cap; the game keeps its own 256 MiB save cap.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// Section cap: the v9 chunk cap (1,000,000) plus room for fixed sections.
pub const MAX_SECTIONS: usize = 1_048_576;
/// The only codec in M018: section bodies are stored verbatim.
pub const CODEC_RAW: u8 = 0;

const HEADER_LEN: usize = 16;
const ENTRY_LEN: usize = 21;

/// Opaque section identifier. Semantics belong to the save-format layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionId(pub u32);

/// One validated directory entry.
#[derive(Clone, Copy, Debug)]
pub struct SectionEntry {
    id: u32,
    offset: u64,
    stored_len: u32,
    raw_len: u32,
    codec: u8,
}

impl SectionEntry {
    /// Section id this entry describes.
    pub fn id(&self) -> SectionId {
        SectionId(self.id)
    }

    /// Byte offset of the stored body within the archive.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Stored body length in bytes.
    pub fn stored_len(&self) -> u32 {
        self.stored_len
    }

    /// Uncompressed body length in bytes (equal to `stored_len` for codec 0).
    pub fn raw_len(&self) -> u32 {
        self.raw_len
    }

    /// Body codec; always [`CODEC_RAW`] for archives this reader accepts.
    pub fn codec(&self) -> u8 {
        self.codec
    }
}

/// A parsed archive that owns its bytes and validated directory.
pub struct SaveImage {
    bytes: Vec<u8>,
    entries: Box<[SectionEntry]>,
    version: u32,
    flags: u32,
}

impl SaveImage {
    /// Parse and validate an archive image.
    ///
    /// Only the header and directory are inspected. Section bodies are never
    /// decoded here, which is what keeps higher-level section laziness
    /// structural rather than advisory.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, StorageError> {
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if byte_len > MAX_ARCHIVE_BYTES {
            return Err(StorageError::TooLarge {
                len: byte_len,
                limit: MAX_ARCHIVE_BYTES,
            });
        }
        if bytes.get(..MAGIC.len()) != Some(MAGIC.as_slice()) {
            return Err(StorageError::InvalidMagic);
        }
        let version = read_u32_at(&bytes, 4).ok_or(StorageError::Truncated)?;
        let flags = read_u32_at(&bytes, 8).ok_or(StorageError::Truncated)?;
        if flags != 0 {
            return Err(StorageError::UnsupportedFlags(flags));
        }
        let count = read_u32_at(&bytes, 12).ok_or(StorageError::Truncated)?;
        let count_usize =
            usize::try_from(count).map_err(|_| StorageError::InvalidSectionCount(count))?;
        if count_usize > MAX_SECTIONS {
            return Err(StorageError::InvalidSectionCount(count));
        }
        let directory_end = HEADER_LEN
            .checked_add(
                count_usize
                    .checked_mul(ENTRY_LEN)
                    .ok_or(StorageError::InvalidSectionCount(count))?,
            )
            .ok_or(StorageError::InvalidSectionCount(count))?;
        // Bounds are checked before allocating the directory.
        if directory_end > bytes.len() {
            return Err(StorageError::Truncated);
        }

        let mut entries = Vec::with_capacity(count_usize);
        let mut previous_id: Option<u32> = None;
        for index in 0..count_usize {
            let base = HEADER_LEN + index * ENTRY_LEN;
            let id = read_u32_at(&bytes, base).ok_or(StorageError::Truncated)?;
            let offset = read_u64_at(&bytes, base + 4).ok_or(StorageError::Truncated)?;
            let stored_len = read_u32_at(&bytes, base + 12).ok_or(StorageError::Truncated)?;
            let raw_len = read_u32_at(&bytes, base + 16).ok_or(StorageError::Truncated)?;
            let codec = bytes
                .get(base + 20)
                .copied()
                .ok_or(StorageError::Truncated)?;
            if codec != CODEC_RAW {
                return Err(StorageError::UnknownCodec(codec));
            }
            if raw_len != stored_len {
                return Err(StorageError::InvalidSectionDirectory);
            }
            if let Some(previous) = previous_id {
                if id == previous {
                    return Err(StorageError::DuplicateSectionId(id));
                }
                if id < previous {
                    return Err(StorageError::UnorderedSectionIds { previous, next: id });
                }
            }
            previous_id = Some(id);
            entries.push(SectionEntry {
                id,
                offset,
                stored_len,
                raw_len,
                codec,
            });
        }

        // Bodies must be contiguous, gapless, alias-free and end exactly at
        // the archive end. Only directory metadata is used here.
        let mut expected_offset = u64::try_from(directory_end).unwrap_or(u64::MAX);
        for entry in &entries {
            if entry.offset != expected_offset {
                return Err(StorageError::InvalidSectionDirectory);
            }
            let end = expected_offset
                .checked_add(u64::from(entry.stored_len))
                .ok_or(StorageError::InvalidSectionDirectory)?;
            if end > byte_len {
                return Err(StorageError::SectionOutOfBounds { id: entry.id });
            }
            expected_offset = end;
        }
        if expected_offset != byte_len {
            return Err(StorageError::TrailingBytes(
                usize::try_from(byte_len - expected_offset).unwrap_or(usize::MAX),
            ));
        }

        Ok(Self {
            bytes,
            entries: entries.into_boxed_slice(),
            version,
            flags,
        })
    }

    /// Archive format version (the `u32` at offset 4).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Archive flags; this reader only accepts `0`.
    pub fn flags(&self) -> u32 {
        self.flags
    }

    /// Number of sections in the directory.
    pub fn section_count(&self) -> usize {
        self.entries.len()
    }

    /// Directory entries in ascending id order.
    pub fn sections(&self) -> impl Iterator<Item = &SectionEntry> {
        self.entries.iter()
    }

    /// Look up a directory entry by id.
    pub fn section_entry(&self, id: SectionId) -> Option<&SectionEntry> {
        self.entries
            .binary_search_by_key(&id.0, |entry| entry.id)
            .ok()
            .map(|index| &self.entries[index])
    }

    /// Borrow a section body without decoding it.
    pub fn section(&self, id: SectionId) -> Option<&[u8]> {
        let entry = self.section_entry(id)?;
        let start = usize::try_from(entry.offset).ok()?;
        let len = usize::try_from(entry.stored_len).ok()?;
        let end = start.checked_add(len)?;
        self.bytes.get(start..end)
    }
}

/// Encode a canonical archive image.
///
/// Sections are sorted by id; duplicate ids, nonzero flags and images over
/// [`MAX_ARCHIVE_BYTES`] are rejected. Empty input yields a 16-byte
/// header-only image.
pub fn encode_archive(
    version: u32,
    flags: u32,
    sections: &[(SectionId, &[u8])],
) -> Result<Vec<u8>, StorageError> {
    if flags != 0 {
        return Err(StorageError::UnsupportedFlags(flags));
    }
    let count =
        u32::try_from(sections.len()).map_err(|_| StorageError::InvalidSectionCount(u32::MAX))?;
    if usize::try_from(count).unwrap_or(usize::MAX) > MAX_SECTIONS {
        return Err(StorageError::InvalidSectionCount(count));
    }

    let mut sorted = sections.to_vec();
    sorted.sort_unstable_by_key(|(id, _)| *id);
    for window in sorted.windows(2) {
        let (previous, _) = window[0];
        let (next, _) = window[1];
        if previous == next {
            return Err(StorageError::DuplicateSectionId(previous.0));
        }
    }

    let directory_end = HEADER_LEN
        .checked_add(
            sorted
                .len()
                .checked_mul(ENTRY_LEN)
                .ok_or(StorageError::InvalidSectionCount(count))?,
        )
        .ok_or(StorageError::InvalidSectionCount(count))?;
    let mut total = u64::try_from(directory_end).map_err(|_| StorageError::TooLarge {
        len: u64::MAX,
        limit: MAX_ARCHIVE_BYTES,
    })?;
    for (_, body) in &sorted {
        let body_len = u64::try_from(body.len()).map_err(|_| StorageError::TooLarge {
            len: u64::MAX,
            limit: MAX_ARCHIVE_BYTES,
        })?;
        total = total.checked_add(body_len).ok_or(StorageError::TooLarge {
            len: u64::MAX,
            limit: MAX_ARCHIVE_BYTES,
        })?;
    }
    if total > MAX_ARCHIVE_BYTES {
        return Err(StorageError::TooLarge {
            len: total,
            limit: MAX_ARCHIVE_BYTES,
        });
    }
    let total_usize = usize::try_from(total).map_err(|_| StorageError::TooLarge {
        len: total,
        limit: MAX_ARCHIVE_BYTES,
    })?;

    let mut out = Vec::with_capacity(total_usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());

    let mut body_offset = u64::try_from(directory_end).map_err(|_| StorageError::TooLarge {
        len: total,
        limit: MAX_ARCHIVE_BYTES,
    })?;
    for (id, body) in &sorted {
        let stored_len = u32::try_from(body.len()).map_err(|_| StorageError::TooLarge {
            len: total,
            limit: MAX_ARCHIVE_BYTES,
        })?;
        out.extend_from_slice(&id.0.to_le_bytes());
        out.extend_from_slice(&body_offset.to_le_bytes());
        out.extend_from_slice(&stored_len.to_le_bytes());
        out.extend_from_slice(&stored_len.to_le_bytes());
        out.push(CODEC_RAW);
        body_offset = body_offset
            .checked_add(u64::from(stored_len))
            .ok_or(StorageError::InvalidSectionDirectory)?;
    }
    for (_, body) in &sorted {
        out.extend_from_slice(body);
    }
    debug_assert_eq!(out.len(), total_usize);
    Ok(out)
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let raw: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

fn read_u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let raw: [u8; 8] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u64::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_SECTION: [u8; 40] = [
        b'M', b'C', b'V', b'X', // magic
        0x0A, 0x00, 0x00, 0x00, // version 10
        0x00, 0x00, 0x00, 0x00, // flags 0
        0x01, 0x00, 0x00, 0x00, // section count 1
        0x07, 0x00, 0x00, 0x00, // id 7
        0x25, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // offset 0x25
        0x03, 0x00, 0x00, 0x00, // stored len 3
        0x03, 0x00, 0x00, 0x00,      // raw len 3
        CODEC_RAW, // codec 0
        0x01, 0x02, 0x03, // body
    ];

    fn single_section() -> Vec<u8> {
        encode_archive(10, 0, &[(SectionId(7), &[1_u8, 2, 3])]).expect("encode one section")
    }

    fn two_sections() -> Vec<u8> {
        encode_archive(
            10,
            0,
            &[(SectionId(2), &[9_u8, 9]), (SectionId(7), &[1, 2, 3])],
        )
        .expect("encode two sections")
    }

    fn entry_base(index: usize) -> usize {
        HEADER_LEN + index * ENTRY_LEN
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn encode_writes_the_pinned_header_directory_and_body() {
        let bytes = single_section();
        assert_eq!(bytes, ONE_SECTION);
        assert_eq!(bytes.len(), 40);
        assert_eq!(&bytes[..4], &b"MCVX"[..]);
        assert_eq!(read_u32(&bytes, 4), 10);
        assert_eq!(read_u32(&bytes, 8), 0);
        assert_eq!(read_u32(&bytes, 12), 1);
        assert_eq!(read_u32(&bytes, 16), 7);
        assert_eq!(read_u64(&bytes, 20), 0x25);
        assert_eq!(read_u32(&bytes, 28), 3);
        assert_eq!(read_u32(&bytes, 32), 3);
        assert_eq!(bytes[36], CODEC_RAW);
        assert_eq!(&bytes[37..], &[1, 2, 3]);
    }

    #[test]
    fn parse_exposes_version_flags_sections_and_bodies() {
        let image = SaveImage::parse(single_section()).expect("parse");
        assert_eq!(image.version(), 10);
        assert_eq!(image.flags(), 0);
        assert_eq!(image.section_count(), 1);
        let entry = image.section_entry(SectionId(7)).expect("entry");
        assert_eq!(entry.id(), SectionId(7));
        assert_eq!(entry.offset(), 0x25);
        assert_eq!(entry.stored_len(), 3);
        assert_eq!(entry.raw_len(), 3);
        assert_eq!(entry.codec(), CODEC_RAW);
        assert_eq!(image.section(SectionId(7)), Some(&[1_u8, 2, 3][..]));
        assert!(image.section_entry(SectionId(8)).is_none());
        assert_eq!(image.section(SectionId(8)), None);
        assert_eq!(
            image.sections().map(SectionEntry::id).collect::<Vec<_>>(),
            vec![SectionId(7)]
        );
    }

    #[test]
    fn encode_sorts_sections_canonically() {
        let in_order = two_sections();
        let out_of_order = encode_archive(
            10,
            0,
            &[(SectionId(7), &[1_u8, 2, 3]), (SectionId(2), &[9, 9])],
        )
        .expect("encode");
        assert_eq!(in_order, out_of_order);
        let image = SaveImage::parse(out_of_order).expect("parse");
        assert_eq!(
            image.sections().map(SectionEntry::id).collect::<Vec<_>>(),
            vec![SectionId(2), SectionId(7)]
        );
        assert_eq!(image.section(SectionId(2)), Some(&[9_u8, 9][..]));
        assert_eq!(image.section(SectionId(7)), Some(&[1_u8, 2, 3][..]));
    }

    #[test]
    fn encode_rejects_duplicate_section_ids() {
        let error = encode_archive(10, 0, &[(SectionId(7), &[1_u8]), (SectionId(7), &[2_u8])])
            .expect_err("duplicate ids must be rejected");
        assert!(matches!(error, StorageError::DuplicateSectionId(7)));
    }

    #[test]
    fn encode_rejects_nonzero_flags() {
        let error = encode_archive(10, 1, &[]).expect_err("flags must be zero");
        assert!(matches!(error, StorageError::UnsupportedFlags(1)));
    }

    #[test]
    fn parse_rejects_invalid_magic() {
        let mut bytes = single_section();
        bytes[0] = b'X';
        assert!(matches!(
            SaveImage::parse(bytes),
            Err(StorageError::InvalidMagic)
        ));
    }

    #[test]
    fn parse_rejects_truncated_header_and_directory() {
        let mut header = single_section();
        header.truncate(8);
        assert!(matches!(
            SaveImage::parse(header),
            Err(StorageError::Truncated)
        ));

        let mut directory = single_section();
        directory.truncate(30);
        assert!(matches!(
            SaveImage::parse(directory),
            Err(StorageError::Truncated)
        ));
    }

    #[test]
    fn parse_rejects_nonzero_flags() {
        let mut bytes = single_section();
        write_u32(&mut bytes, 8, 3);
        assert!(matches!(
            SaveImage::parse(bytes),
            Err(StorageError::UnsupportedFlags(3))
        ));
    }

    #[test]
    fn parse_rejects_forged_section_counts_before_allocating() {
        let mut forged_max = single_section();
        write_u32(&mut forged_max, 12, u32::MAX);
        assert!(matches!(
            SaveImage::parse(forged_max),
            Err(StorageError::InvalidSectionCount(u32::MAX))
        ));

        let mut forged_large = single_section();
        write_u32(&mut forged_large, 12, 1_000);
        assert!(matches!(
            SaveImage::parse(forged_large),
            Err(StorageError::Truncated)
        ));
    }

    #[test]
    fn parse_rejects_unknown_codec_and_raw_length_mismatch() {
        let mut codec = single_section();
        codec[entry_base(0) + 20] = 1;
        assert!(matches!(
            SaveImage::parse(codec),
            Err(StorageError::UnknownCodec(1))
        ));

        let mut raw_len = single_section();
        write_u32(&mut raw_len, entry_base(0) + 16, 4);
        assert!(matches!(
            SaveImage::parse(raw_len),
            Err(StorageError::InvalidSectionDirectory)
        ));
    }

    #[test]
    fn parse_rejects_duplicate_and_unordered_section_ids() {
        let mut duplicate = two_sections();
        write_u32(&mut duplicate, entry_base(1), 2);
        assert!(matches!(
            SaveImage::parse(duplicate),
            Err(StorageError::DuplicateSectionId(2))
        ));

        let mut unordered = two_sections();
        write_u32(&mut unordered, entry_base(1), 1);
        assert!(matches!(
            SaveImage::parse(unordered),
            Err(StorageError::UnorderedSectionIds {
                previous: 2,
                next: 1
            })
        ));
    }

    #[test]
    fn parse_rejects_directory_gaps_and_overlaps() {
        let mut gap = two_sections();
        let second_offset = read_u64(&gap, entry_base(1) + 4);
        write_u64(&mut gap, entry_base(1) + 4, second_offset + 1);
        assert!(matches!(
            SaveImage::parse(gap),
            Err(StorageError::InvalidSectionDirectory)
        ));

        let mut overlap = two_sections();
        let second_offset = read_u64(&overlap, entry_base(1) + 4);
        write_u64(&mut overlap, entry_base(1) + 4, second_offset - 1);
        assert!(matches!(
            SaveImage::parse(overlap),
            Err(StorageError::InvalidSectionDirectory)
        ));

        let mut first_offset = single_section();
        write_u64(&mut first_offset, entry_base(0) + 4, 0x26);
        assert!(matches!(
            SaveImage::parse(first_offset),
            Err(StorageError::InvalidSectionDirectory)
        ));
    }

    #[test]
    fn parse_rejects_out_of_bounds_bodies_and_offsets() {
        let mut beyond_end = single_section();
        write_u32(&mut beyond_end, entry_base(0) + 12, 4);
        write_u32(&mut beyond_end, entry_base(0) + 16, 4);
        assert!(matches!(
            SaveImage::parse(beyond_end),
            Err(StorageError::SectionOutOfBounds { id: 7 })
        ));

        let mut huge_offset = single_section();
        write_u64(&mut huge_offset, entry_base(0) + 4, u64::MAX);
        assert!(matches!(
            SaveImage::parse(huge_offset),
            Err(StorageError::InvalidSectionDirectory)
        ));
    }

    #[test]
    fn parse_rejects_trailing_bytes() {
        let mut bytes = single_section();
        bytes.push(0xEE);
        assert!(matches!(
            SaveImage::parse(bytes),
            Err(StorageError::TrailingBytes(1))
        ));

        let mut header_only = encode_archive(10, 0, &[]).expect("encode empty");
        header_only.push(0x00);
        assert!(matches!(
            SaveImage::parse(header_only),
            Err(StorageError::TrailingBytes(1))
        ));
    }

    #[test]
    fn parse_validates_structure_without_decoding_bodies() {
        let body = [0xFF_u8, 0x00, 0x13];
        let bytes = encode_archive(10, 0, &[(SectionId(99), &body)]).expect("encode");
        let image = SaveImage::parse(bytes)
            .expect("parse only validates the container, not higher-level bodies");
        assert_eq!(image.section(SectionId(99)), Some(&body[..]));
    }

    #[test]
    fn parse_accepts_header_only_and_empty_section_bodies() {
        let header_only = encode_archive(10, 0, &[]).expect("encode");
        assert_eq!(header_only.len(), HEADER_LEN);
        let image = SaveImage::parse(header_only).expect("parse");
        assert_eq!(image.section_count(), 0);
        assert_eq!(image.sections().count(), 0);
        assert_eq!(image.section(SectionId(1)), None);

        let empty_body = encode_archive(10, 0, &[(SectionId(1), &[])]).expect("encode empty body");
        let image = SaveImage::parse(empty_body).expect("parse");
        assert_eq!(image.section_count(), 1);
        assert_eq!(image.section(SectionId(1)).map(<[u8]>::len), Some(0));
    }

    #[test]
    fn parse_never_panics_on_mutated_or_truncated_bytes() {
        let base = two_sections();
        for index in 0..base.len() {
            for value in [0x00_u8, 0x01, 0x7F, 0xFF] {
                let mut bytes = base.clone();
                bytes[index] = value;
                let _ = SaveImage::parse(bytes);
            }
        }
        for len in 0..base.len() {
            let _ = SaveImage::parse(base[..len].to_vec());
        }
    }
}
