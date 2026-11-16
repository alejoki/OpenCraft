//! External contract for the device-free storage half of the reuse proof.
//!
//! Imports come from the consumer crate and `opencraft_storage`'s public
//! modules on purpose: this target proves the archive/codec scenarios and the
//! neutral storage APIs are usable from an outside crate that never imports
//! `opencraft`.

use engine_reuse_proof::report::ProofReport;
use engine_reuse_proof::storage;
use opencraft_storage::archive::{encode_archive, SaveImage, SectionId};
use opencraft_storage::chunk_section::{encode_chunk_section, ChunkSection};
use opencraft_storage::StorageError;

const VERSION: u32 = 10;

fn encode(version: u32, flags: u32, sections: &[(SectionId, &[u8])]) -> Vec<u8> {
    encode_archive(version, flags, sections).expect("encode archive")
}

fn chunk_section(edits: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_chunk_section(&mut out, edits).expect("encode chunk section");
    out
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn archive_round_trips_metadata_and_bodies() {
    let image = encode(
        VERSION,
        0,
        &[(SectionId(2), &[9_u8, 9]), (SectionId(7), &[1, 2, 3])],
    );
    let parsed = SaveImage::parse(image).expect("parse archive");

    assert_eq!(parsed.version(), VERSION);
    assert_eq!(parsed.flags(), 0);
    assert_eq!(parsed.section_count(), 2);
    assert_eq!(
        parsed
            .sections()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        [SectionId(2), SectionId(7)],
        "directory stays in ascending id order"
    );
    assert_eq!(parsed.section(SectionId(2)), Some(&[9_u8, 9][..]));
    assert_eq!(parsed.section(SectionId(7)), Some(&[1_u8, 2, 3][..]));
    assert!(parsed.section(SectionId(3)).is_none());
}

#[test]
fn archive_encoding_is_byte_deterministic_and_canonicalizes_section_order() {
    let ascending = encode(
        VERSION,
        0,
        &[(SectionId(2), &[9_u8, 9]), (SectionId(7), &[1, 2, 3])],
    );
    let shuffled = encode(
        VERSION,
        0,
        &[(SectionId(7), &[1_u8, 2, 3]), (SectionId(2), &[9, 9])],
    );

    assert_eq!(
        ascending, shuffled,
        "caller section order must not change the bytes"
    );
    assert_eq!(
        ascending,
        encode(
            VERSION,
            0,
            &[(SectionId(2), &[9_u8, 9]), (SectionId(7), &[1, 2, 3])]
        ),
        "repeated encodes must be byte-identical"
    );
}

#[test]
fn archive_parse_is_lazy_over_a_malformed_section_body() {
    // Two bytes of 0xFF declare a palette length of 65535, beyond the chunk
    // cap, so the body is malformed for the chunk codec but still a
    // structurally valid opaque archive section.
    let malformed_body = [0xFF_u8, 0xFF];
    let image = encode(VERSION, 0, &[(SectionId(3), &malformed_body)]);

    let parsed = SaveImage::parse(image).expect("archive structure is valid while the body is not");
    let body = parsed
        .section(SectionId(3))
        .expect("section body is readable without decoding");

    assert_eq!(body, &malformed_body[..]);
    assert!(
        matches!(
            ChunkSection::decode(body),
            Err(StorageError::InvalidPaletteLength(65_535))
        ),
        "decoding the malformed body must surface a typed codec error"
    );
}

#[test]
fn malformed_archives_fail_closed() {
    let valid = encode(VERSION, 0, &[(SectionId(7), &[1_u8, 2, 3])]);

    let mut bad_magic = valid.clone();
    bad_magic[0] = b'X';
    assert!(matches!(
        SaveImage::parse(bad_magic),
        Err(StorageError::InvalidMagic)
    ));

    let mut bad_flags = valid.clone();
    write_u32(&mut bad_flags, 8, 1);
    assert!(matches!(
        SaveImage::parse(bad_flags),
        Err(StorageError::UnsupportedFlags(1))
    ));

    let mut truncated_directory = valid.clone();
    truncated_directory.truncate(16 + 21 - 1);
    assert!(matches!(
        SaveImage::parse(truncated_directory),
        Err(StorageError::Truncated)
    ));

    let mut bad_codec = valid.clone();
    bad_codec[16 + 20] = 1;
    assert!(matches!(
        SaveImage::parse(bad_codec),
        Err(StorageError::UnknownCodec(1))
    ));

    let mut trailing = valid;
    trailing.push(0);
    assert!(matches!(
        SaveImage::parse(trailing),
        Err(StorageError::TrailingBytes(1))
    ));
}

#[test]
fn chunk_codec_round_trips_and_canonicalizes_edit_order() {
    let ascending = chunk_section(&[(0, 5), (2, 5), (4, 7)]);
    let shuffled = chunk_section(&[(4, 7), (0, 5), (2, 5)]);

    assert_eq!(
        ascending, shuffled,
        "caller edit order must not change the bytes"
    );

    let decoded = ChunkSection::decode(&ascending).expect("decode chunk section");
    assert_eq!(decoded.palette(), &[5_u16, 7][..]);
    assert_eq!(
        decoded.edits(),
        &[(0_u16, 0_u16), (2, 0), (4, 1)][..],
        "edits ascend by local index and point into the sorted palette"
    );
    assert_eq!(decoded.len(), 3);
    assert!(!decoded.is_empty());
}

#[test]
fn consumer_proof_reports_the_archive_and_codec_values() {
    let proof = storage::run();

    assert_eq!(proof.section_count, 2);
    assert_eq!(proof.chunk_edits, 3);
    // 16-byte header + two 21-byte directory entries + a 20-byte chunk
    // section + the 6-byte opaque section = 84 bytes.
    assert_eq!(proof.archive_bytes, 84, "pinned two-section archive layout");

    let repeated = storage::run();
    assert_eq!(
        proof.archive_bytes, repeated.archive_bytes,
        "archive_bytes must be stable across runs"
    );
}

#[test]
fn proof_report_renders_the_three_storage_lines() {
    let rendered = ProofReport::to_string(&engine_reuse_proof::run());

    assert_eq!(rendered.lines().count(), 18, "four proof groups of lines");
    for line in [
        "storage.archive_bytes=84",
        "storage.section_count=2",
        "storage.chunk_edits=3",
    ] {
        assert!(
            rendered.lines().any(|actual| actual == line),
            "report must render {line:?}:\n{rendered}"
        );
    }
}

#[test]
fn storage_proof_source_stays_in_memory_only() {
    let source = include_str!("../src/storage.rs");
    for forbidden in [
        "std::fs",
        "File::",
        "std::env",
        "SystemTime",
        "Instant",
        "thread::",
        "wgpu",
        "tempfile",
        "opencraft::",
    ] {
        assert!(
            !source.contains(forbidden),
            "the storage proof must stay in-memory and game-free; found {forbidden:?}"
        );
    }
}
