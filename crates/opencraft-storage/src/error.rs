//! One flat typed error for every storage-layer failure.
//!
//! The enum is deliberately flat (no `thiserror`) so the game can map variants
//! one-by-one into `PersistenceError` without inheriting a dependency.

use std::fmt;
use std::io;

/// Every failure the neutral storage layer can report.
#[derive(Debug)]
pub enum StorageError {
    /// An underlying filesystem operation failed.
    Io(io::Error),
    /// The archive does not start with the `MCVX` magic.
    InvalidMagic,
    /// The archive header declares flags this reader does not understand.
    UnsupportedFlags(u32),
    /// The archive or a section exceeds the configured byte limit.
    TooLarge { len: u64, limit: u64 },
    /// The path exists but is not a regular file.
    NotRegularFile,
    /// The archive section count is beyond the supported cap.
    InvalidSectionCount(u32),
    /// Two directory entries carry the same section id.
    DuplicateSectionId(u32),
    /// Directory ids are not strictly ascending.
    UnorderedSectionIds { previous: u32, next: u32 },
    /// The directory is structurally invalid (non-canonical offset, length
    /// mismatch for the codec, or arithmetic overflow).
    InvalidSectionDirectory,
    /// A section uses a codec this reader does not implement.
    UnknownCodec(u8),
    /// A section body extends past the end of the archive.
    SectionOutOfBounds { id: u32 },
    /// The archive ends before the header or directory is complete.
    Truncated,
    /// A chunk section's palette length is zero or beyond the chunk cap.
    InvalidPaletteLength(u16),
    /// A chunk section's edit count is zero or beyond the chunk cap.
    InvalidEditCount(u16),
    /// Chunk palette entries are not strictly ascending.
    PaletteNotAscending { previous: u16, next: u16 },
    /// Chunk edit local indices are not strictly ascending.
    EditNotAscending { previous: u16, next: u16 },
    /// A chunk edit's local index is outside the chunk volume.
    LocalIndexOutOfRange(u16),
    /// A chunk edit's palette index is outside the palette.
    PaletteIndexOutOfRange { index: u16, palette_len: u16 },
    /// A chunk edit reuses a local index.
    DuplicateLocalIndex(u16),
    /// Bytes remain after the last declared section body.
    TrailingBytes(usize),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "storage I/O error: {error}"),
            Self::InvalidMagic => f.write_str("invalid save archive magic"),
            Self::UnsupportedFlags(flags) => {
                write!(f, "unsupported save archive flags: {flags:#010x}")
            }
            Self::TooLarge { len, limit } => {
                write!(f, "archive is {len} bytes, over the {limit}-byte limit")
            }
            Self::NotRegularFile => f.write_str("save file is not a regular file"),
            Self::InvalidSectionCount(count) => {
                write!(f, "invalid save archive section count: {count}")
            }
            Self::DuplicateSectionId(id) => {
                write!(f, "duplicate save archive section id: {id}")
            }
            Self::UnorderedSectionIds { previous, next } => write!(
                f,
                "save archive section ids are not ascending: {previous} then {next}"
            ),
            Self::InvalidSectionDirectory => f.write_str("invalid save archive section directory"),
            Self::UnknownCodec(codec) => {
                write!(f, "unknown save archive section codec: {codec}")
            }
            Self::SectionOutOfBounds { id } => {
                write!(f, "save archive section {id} extends past the archive end")
            }
            Self::Truncated => f.write_str("save archive is truncated"),
            Self::InvalidPaletteLength(len) => {
                write!(f, "invalid chunk palette length: {len}")
            }
            Self::InvalidEditCount(count) => {
                write!(f, "invalid chunk edit count: {count}")
            }
            Self::PaletteNotAscending { previous, next } => {
                write!(f, "chunk palette is not ascending: {previous} then {next}")
            }
            Self::EditNotAscending { previous, next } => {
                write!(f, "chunk edits are not ascending: {previous} then {next}")
            }
            Self::LocalIndexOutOfRange(index) => {
                write!(f, "chunk local index out of range: {index}")
            }
            Self::PaletteIndexOutOfRange { index, palette_len } => write!(
                f,
                "chunk palette index {index} out of range for palette length {palette_len}"
            ),
            Self::DuplicateLocalIndex(index) => {
                write!(f, "duplicate chunk local index: {index}")
            }
            Self::TrailingBytes(count) => {
                write!(f, "save archive has {count} trailing bytes")
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_convert_and_keep_their_source() {
        let error = StorageError::from(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        assert!(matches!(error, StorageError::Io(_)));
        assert!(error.to_string().contains("denied"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn every_non_io_variant_has_a_specific_message() {
        let errors = [
            StorageError::InvalidMagic,
            StorageError::UnsupportedFlags(2),
            StorageError::TooLarge { len: 10, limit: 5 },
            StorageError::NotRegularFile,
            StorageError::InvalidSectionCount(0),
            StorageError::DuplicateSectionId(7),
            StorageError::UnorderedSectionIds {
                previous: 7,
                next: 3,
            },
            StorageError::InvalidSectionDirectory,
            StorageError::UnknownCodec(1),
            StorageError::SectionOutOfBounds { id: 7 },
            StorageError::Truncated,
            StorageError::InvalidPaletteLength(0),
            StorageError::InvalidEditCount(0),
            StorageError::PaletteNotAscending {
                previous: 7,
                next: 3,
            },
            StorageError::EditNotAscending {
                previous: 7,
                next: 3,
            },
            StorageError::LocalIndexOutOfRange(40_000),
            StorageError::PaletteIndexOutOfRange {
                index: 9,
                palette_len: 2,
            },
            StorageError::DuplicateLocalIndex(4),
            StorageError::TrailingBytes(1),
        ];

        for error in errors {
            let message = error.to_string();
            assert!(!message.is_empty(), "empty message for {error:?}");
            assert!(
                std::error::Error::source(&error).is_none(),
                "unexpected source for {error:?}"
            );
        }
    }
}
