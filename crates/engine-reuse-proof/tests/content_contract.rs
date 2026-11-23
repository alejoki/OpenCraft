//! External contract for the device-free content half of the reuse proof.
//!
//! Imports come from the consumer crate and `opencraft_content`'s public
//! modules on purpose: this target proves the pack manifest parser, the v3
//! entry parser and the PNG/resample pipeline are usable from an outside crate
//! that never imports `opencraft` and never opens a pack directory.

use engine_reuse_proof::content;
use engine_reuse_proof::report::ProofReport;
use opencraft_content::decode::decode_png_rgba_bytes;
use opencraft_content::manifest::parse_pack_file;
use opencraft_content::resample::resample_to_16;
use opencraft_content::v3::parse_content;

/// A valid pack whose second block is rejected, so one diagnostic must appear
/// while the item sibling survives.
const PARTLY_BROKEN_PACK: &str = r#"format_version = 3

[[blocks]]
id = "proof:stone"
textures = { all = "proof.png" }
hardness = 1.5

[[blocks]]
id = "proof:broken"
textures = { all = "proof.png" }

[[items]]
id = "proof:gem"
icon = "proof.png"
stack = 64
"#;

#[test]
fn content_proof_reports_the_parse_and_sprite_values() {
    let proof = content::run();

    assert_eq!(proof.blocks, 1, "one accepted block entry");
    assert_eq!(proof.items, 1, "one accepted item entry");
    assert_eq!(proof.pack_entries, 2, "one block plus one item");
    assert_eq!(proof.sprite_bytes, 1024, "16x16 RGBA is 1024 bytes");
    assert_ne!(
        proof.catalog_digest,
        opencraft_content::catalog::EMPTY_DIGEST,
        "the proof pack must move the neutral catalog digest"
    );

    let repeated = content::run();
    assert_eq!(
        (
            proof.blocks,
            proof.items,
            proof.pack_entries,
            proof.sprite_bytes,
            proof.catalog_digest
        ),
        (
            repeated.blocks,
            repeated.items,
            repeated.pack_entries,
            repeated.sprite_bytes,
            repeated.catalog_digest
        ),
        "the content proof must be stable across runs"
    );
}

#[test]
fn proof_report_renders_the_content_line() {
    let rendered = ProofReport::to_string(&engine_reuse_proof::run());

    assert_eq!(rendered.lines().count(), 22, "six proof groups of lines");
    for expected in [
        "content.pack_entries=2",
        "content.catalog_digest=0xdd3648c397a414c0",
    ] {
        assert!(
            rendered.lines().any(|actual| actual == expected),
            "report must render {expected:?}:\n{rendered}"
        );
    }
}

#[test]
fn content_proof_source_stays_in_memory_only() {
    let source = include_str!("../src/content.rs");
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
            "the content proof must stay in-memory and game-free; found {forbidden:?}"
        );
    }
}

#[test]
fn malformed_png_bytes_fail_closed() {
    // The proof's decode path must reject non-PNG input instead of producing a
    // zeroed sprite: a corrupt asset is a diagnostic, never a silent fallback.
    assert!(decode_png_rgba_bytes(b"not a png", "proof.png", Some(256)).is_err());

    // Truncated within a valid signature still fails closed.
    let truncated = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    assert!(decode_png_rgba_bytes(&truncated, "proof.png", Some(256)).is_err());
}

#[test]
fn rejected_entries_keep_the_valid_siblings_and_name_the_entry() {
    let (manifest, root, warnings) = parse_pack_file(PARTLY_BROKEN_PACK);
    assert!(
        warnings.is_empty(),
        "the manifest fields themselves are valid"
    );
    let root = root.expect("the partly broken pack is valid TOML");

    let (spec, warnings, diagnostics) = parse_content(&root, manifest.format_version);
    assert!(warnings.is_empty(), "v3 arrays produce no manifest warning");
    assert_eq!(spec.blocks.len(), 1, "the invalid block is rejected");
    assert_eq!(spec.items.len(), 1, "the valid item sibling is kept");
    assert_eq!(
        diagnostics.len(),
        1,
        "exactly one diagnostic for the missing hardness: {diagnostics:?}"
    );
    assert_eq!(diagnostics[0].entry, "block proof:broken");
    assert!(
        diagnostics[0].reason.contains("hardness"),
        "the diagnostic must name the missing field: {}",
        diagnostics[0].reason
    );
}

#[test]
fn a_valid_16x16_png_resamples_to_1024_bytes() {
    // Same shape as the proof's embedded PNG: a 16x16 image is already the
    // target size, so the resample is an exact copy.
    const PNG: [u8; 82] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0xf3, 0xff, 0x61, 0x00, 0x00, 0x00, 0x19, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x50,
        0x50, 0x38, 0xf1, 0x9f, 0x12, 0xcc, 0x30, 0x6a, 0xc0, 0xa8, 0x01, 0xa3, 0x06, 0x0c, 0x17,
        0x03, 0x00, 0xd7, 0x34, 0x07, 0x1f, 0x12, 0xf3, 0x11, 0xa7, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    let decoded = decode_png_rgba_bytes(&PNG, "proof.png", Some(256)).expect("valid 16x16 PNG");
    assert_eq!((decoded.width, decoded.height), (16, 16));
    assert_eq!(decoded.rgba.len(), 1024);

    let sprite = resample_to_16(&decoded.rgba, decoded.width);
    assert_eq!(sprite, decoded.rgba, "16x16 input resamples to itself");
    assert_eq!(sprite.len(), 1024);
}
