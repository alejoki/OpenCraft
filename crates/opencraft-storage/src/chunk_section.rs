//! Canonical palette codec for one edited chunk section.
//!
//! Neutral by construction: the codec sees raw `u16` content indices and never
//! learns what a content index means. Canonical by construction: the encoder
//! sorts edits by local index and derives a sorted palette from them, so the
//! same logical edits always produce the same bytes regardless of caller order
//! and a caller cannot emit a non-canonical section.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! palette_len u16                    (1..=32768)
//! palette     [u16; palette_len]     (content indices, strictly ascending)
//! edit_len    u16                    (1..=32768)
//! edits       [{local: u16, palette_index: u16}; edit_len]
//!                                    (local strictly ascending, < 32768)
//! ```

use opencraft_core::chunk::CHUNK_VOLUME;

use crate::StorageError;

/// Palette/edit cap: at most one entry per chunk cell.
const MAX_ENTRIES: usize = CHUNK_VOLUME;

/// One decoded chunk section: a content palette plus sparse edits into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkSection {
    palette: Box<[u16]>,
    edits: Box<[(u16, u16)]>,
}

impl ChunkSection {
    /// Decode exactly one section from `bytes`.
    ///
    /// Every byte must belong to the section: short input is
    /// [`StorageError::Truncated`] and extra input is
    /// [`StorageError::TrailingBytes`]. Nothing is allocated before the
    /// remaining byte length has been checked against the declared counts.
    pub fn decode(bytes: &[u8]) -> Result<Self, StorageError> {
        let mut cursor = Cursor::new(bytes);

        let palette_len = cursor.read_u16()?;
        if palette_len == 0 || usize::from(palette_len) > MAX_ENTRIES {
            return Err(StorageError::InvalidPaletteLength(palette_len));
        }
        let palette_len_usize = usize::from(palette_len);
        cursor.require(
            palette_len_usize
                .checked_mul(2)
                .ok_or(StorageError::Truncated)?,
        )?;
        let mut palette = Vec::with_capacity(palette_len_usize);
        for _ in 0..palette_len_usize {
            palette.push(cursor.read_u16()?);
        }
        for pair in palette.windows(2) {
            if pair[0] >= pair[1] {
                return Err(StorageError::PaletteNotAscending {
                    previous: pair[0],
                    next: pair[1],
                });
            }
        }

        let edit_len = cursor.read_u16()?;
        if edit_len == 0 || usize::from(edit_len) > MAX_ENTRIES {
            return Err(StorageError::InvalidEditCount(edit_len));
        }
        let edit_len_usize = usize::from(edit_len);
        cursor.require(
            edit_len_usize
                .checked_mul(4)
                .ok_or(StorageError::Truncated)?,
        )?;
        let mut edits = Vec::with_capacity(edit_len_usize);
        let mut previous_local: Option<u16> = None;
        for _ in 0..edit_len_usize {
            let local = cursor.read_u16()?;
            let palette_index = cursor.read_u16()?;
            if usize::from(local) >= CHUNK_VOLUME {
                return Err(StorageError::LocalIndexOutOfRange(local));
            }
            if let Some(previous) = previous_local {
                if local <= previous {
                    return Err(StorageError::EditNotAscending {
                        previous,
                        next: local,
                    });
                }
            }
            if palette_index >= palette_len {
                return Err(StorageError::PaletteIndexOutOfRange {
                    index: palette_index,
                    palette_len,
                });
            }
            previous_local = Some(local);
            edits.push((local, palette_index));
        }

        if cursor.remaining() > 0 {
            return Err(StorageError::TrailingBytes(cursor.remaining()));
        }

        Ok(Self {
            palette: palette.into_boxed_slice(),
            edits: edits.into_boxed_slice(),
        })
    }

    /// Palette content indices in ascending order.
    pub fn palette(&self) -> &[u16] {
        &self.palette
    }

    /// Sparse edits as `(local_index, palette_index)` in ascending local order.
    pub fn edits(&self) -> &[(u16, u16)] {
        &self.edits
    }

    /// Number of edits.
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// A decoded section always carries at least one edit.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }
}

/// Encode one edited chunk section canonically, appending to `out`.
///
/// `edits` are `(local_index, content_index)` pairs in any order. Duplicate
/// local indices, local indices at or above [`CHUNK_VOLUME`] and empty input
/// are rejected, and `out` is left untouched when they are.
pub fn encode_chunk_section(
    out: &mut Vec<u8>,
    edits: &[(u16, u16)],
) -> Result<(), StorageError> {
    if edits.is_empty() {
        return Err(StorageError::InvalidEditCount(0));
    }

    let mut sorted = edits.to_vec();
    sorted.sort_unstable_by_key(|&(local, _)| local);

    let mut previous: Option<u16> = None;
    for &(local, _) in &sorted {
        if usize::from(local) >= CHUNK_VOLUME {
            return Err(StorageError::LocalIndexOutOfRange(local));
        }
        if previous == Some(local) {
            return Err(StorageError::DuplicateLocalIndex(local));
        }
        previous = Some(local);
    }

    // Unique locals below CHUNK_VOLUME cap the count at MAX_ENTRIES.
    let edit_len =
        u16::try_from(sorted.len()).map_err(|_| StorageError::InvalidEditCount(u16::MAX))?;

    let mut palette: Vec<u16> = sorted.iter().map(|&(_, content)| content).collect();
    palette.sort_unstable();
    palette.dedup();
    let palette_len =
        u16::try_from(palette.len()).map_err(|_| StorageError::InvalidPaletteLength(u16::MAX))?;
    debug_assert!(palette_len > 0, "a nonempty edit list has a palette");

    out.extend_from_slice(&palette_len.to_le_bytes());
    for content in &palette {
        out.extend_from_slice(&content.to_le_bytes());
    }
    out.extend_from_slice(&edit_len.to_le_bytes());
    for &(local, content) in &sorted {
        let slot = palette
            .binary_search(&content)
            .expect("palette is built from these edits");
        let slot = u16::try_from(slot).expect("palette indices stay below CHUNK_VOLUME");
        out.extend_from_slice(&local.to_le_bytes());
        out.extend_from_slice(&slot.to_le_bytes());
    }
    Ok(())
}

/// Bounds-checked read cursor; every short read is `Truncated`.
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn require(&self, count: usize) -> Result<(), StorageError> {
        if self.remaining() < count {
            return Err(StorageError::Truncated);
        }
        Ok(())
    }

    fn read_u16(&mut self) -> Result<u16, StorageError> {
        let end = self.offset.checked_add(2).ok_or(StorageError::Truncated)?;
        let raw: [u8; 2] = self
            .bytes
            .get(self.offset..end)
            .ok_or(StorageError::Truncated)?
            .try_into()
            .map_err(|_| StorageError::Truncated)?;
        self.offset = end;
        Ok(u16::from_le_bytes(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(edits: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        encode_chunk_section(&mut out, edits).expect("encode chunk section");
        out
    }

    fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn encode_writes_the_pinned_witness_bytes() {
        let bytes = encode(&[(5, 7), (2, 9)]);
        assert_eq!(
            bytes,
            [
                0x02, 0x00, // palette_len 2
                0x07, 0x00, 0x09, 0x00, // palette [7, 9]
                0x02, 0x00, // edit_len 2
                0x02, 0x00, 0x01, 0x00, // (2, 1)
                0x05, 0x00, 0x00, 0x00, // (5, 0)
            ]
        );
        assert_eq!(bytes.len(), 16);
    }

    #[test]
    fn decode_exposes_the_palette_and_edits() {
        let section = ChunkSection::decode(&encode(&[(5, 7), (2, 9)])).expect("decode witness");
        assert_eq!(section.palette(), &[7_u16, 9][..]);
        assert_eq!(section.edits(), &[(2_u16, 1_u16), (5, 0)][..]);
        assert_eq!(section.len(), 2);
        assert!(!section.is_empty());
    }

    #[test]
    fn encode_appends_to_out() {
        let mut out = vec![0xAA];
        encode_chunk_section(&mut out, &[(2, 9)]).expect("encode");
        assert_eq!(out[0], 0xAA);
        assert_eq!(
            &out[1..],
            &[0x01, 0x00, 0x09, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00][..]
        );
    }

    #[test]
    fn round_trip_keeps_the_500_edit_single_content_shape() {
        let edits: Vec<(u16, u16)> = (0..500_u16).map(|local| (local, 42)).collect();
        let bytes = encode(&edits);
        assert_eq!(bytes.len(), 2006);
        let section = ChunkSection::decode(&bytes).expect("decode");
        assert_eq!(section.palette(), &[42_u16][..]);
        assert_eq!(section.len(), 500);
        let expected: Vec<(u16, u16)> = (0..500_u16).map(|local| (local, 0)).collect();
        assert_eq!(section.edits(), expected.as_slice());
    }

    #[test]
    fn encode_canonicalizes_shuffled_edits() {
        let sorted = encode(&[(2, 9), (4, 1), (5, 7)]);
        let reversed = encode(&[(5, 7), (4, 1), (2, 9)]);
        let interleaved = encode(&[(4, 1), (5, 7), (2, 9)]);
        assert_eq!(sorted, reversed);
        assert_eq!(sorted, interleaved);
        let section = ChunkSection::decode(&reversed).expect("decode");
        assert_eq!(section.palette(), &[1_u16, 7, 9][..]);
        assert_eq!(
            section.edits(),
            &[(2_u16, 2_u16), (4, 0), (5, 1)][..]
        );
    }

    #[test]
    fn encode_collapses_repeated_content_indices_into_one_palette_slot() {
        let bytes = encode(&[(3, 7), (1, 7)]);
        assert_eq!(
            bytes,
            [
                0x01, 0x00, // palette_len 1
                0x07, 0x00, // palette [7]
                0x02, 0x00, // edit_len 2
                0x01, 0x00, 0x00, 0x00, // (1, 0)
                0x03, 0x00, 0x00, 0x00, // (3, 0)
            ]
        );
    }

    #[test]
    fn encode_rejects_an_empty_edit_list() {
        let mut out = vec![0xAA];
        let error = encode_chunk_section(&mut out, &[]).expect_err("empty edits must fail");
        assert!(matches!(error, StorageError::InvalidEditCount(0)));
        assert_eq!(out, vec![0xAA], "rejected input must not touch out");
    }

    #[test]
    fn encode_rejects_duplicate_local_indices() {
        let mut out = Vec::new();
        let error =
            encode_chunk_section(&mut out, &[(4, 1), (4, 2)]).expect_err("duplicate locals");
        assert!(matches!(error, StorageError::DuplicateLocalIndex(4)));
        assert!(out.is_empty());
    }

    #[test]
    fn encode_rejects_local_indices_at_or_above_the_chunk_volume() {
        let volume = u16::try_from(CHUNK_VOLUME).expect("chunk volume fits u16");
        for local in [volume, u16::MAX] {
            let error = encode_chunk_section(&mut Vec::new(), &[(0, 1), (local, 2)])
                .expect_err("local out of range");
            assert!(
                matches!(error, StorageError::LocalIndexOutOfRange(index) if index == local),
                "local {local} must be rejected, got {error:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_invalid_palette_lengths() {
        assert!(matches!(
            ChunkSection::decode(&[0x00, 0x00]),
            Err(StorageError::InvalidPaletteLength(0))
        ));
        // 32769 = 0x8001, one past the chunk-volume cap.
        assert!(matches!(
            ChunkSection::decode(&[0x01, 0x80]),
            Err(StorageError::InvalidPaletteLength(32769))
        ));
    }

    #[test]
    fn decode_rejects_invalid_edit_counts() {
        let zero = [0x01, 0x00, 0x07, 0x00, 0x00, 0x00];
        assert!(matches!(
            ChunkSection::decode(&zero),
            Err(StorageError::InvalidEditCount(0))
        ));
        let too_many = [0x01, 0x00, 0x07, 0x00, 0x01, 0x80];
        assert!(matches!(
            ChunkSection::decode(&too_many),
            Err(StorageError::InvalidEditCount(32769))
        ));
    }

    #[test]
    fn decode_rejects_non_ascending_palettes() {
        let descending = [0x02, 0x00, 0x09, 0x00, 0x07, 0x00, 0x00, 0x00];
        assert!(matches!(
            ChunkSection::decode(&descending),
            Err(StorageError::PaletteNotAscending {
                previous: 9,
                next: 7
            })
        ));
        let duplicate = [0x02, 0x00, 0x07, 0x00, 0x07, 0x00, 0x00, 0x00];
        assert!(matches!(
            ChunkSection::decode(&duplicate),
            Err(StorageError::PaletteNotAscending {
                previous: 7,
                next: 7
            })
        ));
    }

    #[test]
    fn decode_rejects_non_ascending_edits() {
        let mut bytes = encode(&[(2, 7), (5, 9)]);
        write_u16(&mut bytes, 8, 5);
        write_u16(&mut bytes, 12, 2);
        assert!(matches!(
            ChunkSection::decode(&bytes),
            Err(StorageError::EditNotAscending {
                previous: 5,
                next: 2
            })
        ));
    }

    #[test]
    fn decode_rejects_out_of_range_local_and_palette_indices() {
        let mut local = encode(&[(2, 7), (5, 9)]);
        write_u16(
            &mut local,
            12,
            u16::try_from(CHUNK_VOLUME).expect("chunk volume fits u16"),
        );
        assert!(matches!(
            ChunkSection::decode(&local),
            Err(StorageError::LocalIndexOutOfRange(32768))
        ));

        let mut palette_index = encode(&[(2, 7), (5, 9)]);
        write_u16(&mut palette_index, 10, 2);
        assert!(matches!(
            ChunkSection::decode(&palette_index),
            Err(StorageError::PaletteIndexOutOfRange {
                index: 2,
                palette_len: 2
            })
        ));
    }

    #[test]
    fn decode_rejects_every_truncated_prefix() {
        let multi_content: Vec<(u16, u16)> = (0..500_u16)
            .map(|local| (local, [4_u16, 9, 12][local as usize % 3]))
            .collect();
        for bytes in [encode(&[(5, 7), (2, 9)]), encode(&multi_content)] {
            for len in 0..bytes.len() {
                assert!(
                    matches!(
                        ChunkSection::decode(&bytes[..len]),
                        Err(StorageError::Truncated)
                    ),
                    "prefix of {len} bytes of a {}-byte section must be Truncated",
                    bytes.len()
                );
            }
            ChunkSection::decode(&bytes).expect("the full section decodes");
        }
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let mut bytes = encode(&[(5, 7), (2, 9)]);
        bytes.push(0xEE);
        assert!(matches!(
            ChunkSection::decode(&bytes),
            Err(StorageError::TrailingBytes(1))
        ));
        bytes.push(0xEE);
        assert!(matches!(
            ChunkSection::decode(&bytes),
            Err(StorageError::TrailingBytes(2))
        ));
    }

    #[test]
    fn decode_accepts_the_maximum_palette_and_edit_count() {
        let edits: Vec<(u16, u16)> = (0..CHUNK_VOLUME)
            .map(|index| {
                let index = u16::try_from(index).expect("chunk volume fits u16");
                (index, index)
            })
            .collect();
        let bytes = encode(&edits);
        assert_eq!(bytes.len(), 2 + CHUNK_VOLUME * 2 + 2 + CHUNK_VOLUME * 4);
        let section = ChunkSection::decode(&bytes).expect("decode maximum section");
        assert_eq!(section.palette().len(), CHUNK_VOLUME);
        assert_eq!(section.len(), CHUNK_VOLUME);
        assert_eq!(section.palette()[CHUNK_VOLUME - 1], 32767);
        assert_eq!(section.edits()[0], (0, 0));
        assert_eq!(section.edits()[CHUNK_VOLUME - 1], (32767, 32767));
    }

    #[test]
    fn decode_never_panics_on_mutated_bytes() {
        let base = encode(&[(5, 7), (2, 9), (9, 3)]);
        for index in 0..base.len() {
            for value in [0x00_u8, 0x01, 0x7F, 0xFF] {
                let mut bytes = base.clone();
                bytes[index] = value;
                let _ = ChunkSection::decode(&bytes);
            }
        }
    }
}
