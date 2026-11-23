//! Device-free content-pack parse and asset-pipeline proof.
//!
//! This is the outside-crate proof for `opencraft-content`: an authored
//! `format_version = 3` manifest is parsed, its blocks and items arrays are
//! validated entry by entry, and one embedded PNG travels the same decode and
//! 16x16 resample path a real pack asset takes. The resampled sprite is then
//! inserted into the neutral spec, a `catalog::Catalog` is built from the same
//! pack stack and its FNV-1a digest is reported. Everything runs from memory:
//! no path is opened, no clock is read, no thread is spawned and no GPU object
//! is created, so the proof is byte-deterministic on a headless CI runner.

use opencraft_content::catalog::{
    build_catalog, BlockDef, Catalog, PackIdentity, EMPTY_DIGEST, PACK_ICON_BASE, PACK_LAYER_BASE,
};
use opencraft_content::decode::decode_png_rgba_bytes;
use opencraft_content::manifest::parse_pack_file;
use opencraft_content::resample::resample_to_16;
use opencraft_content::spec::{BlockAlpha, BlockModel, PackAsset};
use opencraft_content::v3::parse_content;

/// A minimal valid v3 manifest: one block and one item.
///
/// The entries are deliberately the smallest accepted shape so a rejection
/// here means the parser or the manifest changed, not that the proof drifted.
const PACK_TOML: &str = r#"format_version = 3

[[blocks]]
id = "proof:stone"
textures = { all = "proof.png" }
hardness = 1.5

[[items]]
id = "proof:gem"
icon = "proof.png"
stack = 64
"#;

/// One valid 16x16 RGBA PNG, 82 bytes.
///
/// This is the `PNG_RUBY` fixture from `scripts/validate_pack_smoke.sh`,
/// decoded once while authoring and embedded here so the consumer needs no
/// pack directory, fixture path or writable disk.
#[rustfmt::skip]
const PNG: [u8; 82] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0xf3, 0xff,
    0x61, 0x00, 0x00, 0x00, 0x19, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x50, 0x50, 0x38, 0xf1,
    0x9f, 0x12, 0xcc, 0x30, 0x6a, 0xc0, 0xa8, 0x01, 0xa3, 0x06, 0x0c, 0x17, 0x03, 0x00, 0xd7, 0x34,
    0x07, 0x1f, 0x12, 0xf3, 0x11, 0xa7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
    0x60, 0x82,
];

/// The observable result of one device-free content proof run.
#[derive(Clone, Copy, Debug)]
pub struct ContentProof {
    /// Accepted authored entries across the proof pack's blocks and items.
    pub pack_entries: usize,
    /// Accepted block entries.
    pub blocks: usize,
    /// Accepted item entries.
    pub items: usize,
    /// Bytes in the resampled 16x16 sprite.
    pub sprite_bytes: usize,
    /// FNV-1a digest of the neutral catalog built from the proof pack.
    pub catalog_digest: u64,
}

/// Run the pack-parse and asset-pipeline proofs deterministically.
///
/// The manifest is parsed in memory, so the proof needs no pack directory and
/// no writable disk. A rejected entry or a non-empty warning list would mean
/// the pinned format changed, so both are asserted here.
pub fn run() -> ContentProof {
    let (manifest, root, warnings) = parse_pack_file(PACK_TOML);
    assert!(
        warnings.is_empty(),
        "the proof manifest must parse without warnings: {warnings:?}"
    );
    let root = root.expect("the proof manifest is valid TOML");

    let (mut spec, warnings, diagnostics) = parse_content(&root, manifest.format_version);
    assert!(
        warnings.is_empty(),
        "the proof content must parse without warnings: {warnings:?}"
    );
    assert!(
        diagnostics.is_empty(),
        "the proof content must parse with zero diagnostics: {diagnostics:?}"
    );
    assert_eq!(spec.blocks.len(), 1, "the proof pack authors one block");
    assert_eq!(spec.items.len(), 1, "the proof pack authors one item");
    let pack_entries = spec.blocks.len() + spec.items.len();
    let blocks = spec.blocks.len();
    let items = spec.items.len();

    let decoded = decode_png_rgba_bytes(&PNG, "proof.png", Some(256))
        .expect("the embedded proof PNG is a valid 16x16 RGBA image");
    assert_eq!(
        (decoded.width, decoded.height),
        (16, 16),
        "the embedded proof PNG is 16x16"
    );

    let sprite = resample_to_16(&decoded.rgba, decoded.width);
    assert_eq!(sprite.len(), 16 * 16 * 4, "the sprite is 1024 RGBA bytes");

    // Feed the decoded sprite back through the neutral spec exactly like a real
    // pack loader would, then build the catalog over one base block.
    spec.assets.insert(
        "proof.png".to_string(),
        PackAsset {
            rgba: sprite.clone(),
            source_side: 16,
        },
    );
    let base = Catalog::from_base(
        vec![BlockDef {
            id: 0,
            name: "stone".to_string(),
            display_name: "Stone".to_string(),
            faces: [3; 6],
            hardness: Some(1.5),
            drop: None,
            tool_class: None,
            stack_limit: 64,
            model: BlockModel::Cube,
            alpha: BlockAlpha::Opaque,
            placeholder: false,
        }],
        Vec::new(),
        Vec::new(),
    );
    let packs = [(
        PackIdentity {
            folder: "proof".to_string(),
            version: Some("1.0.0".to_string()),
        },
        spec,
    )];
    let (catalog, report) = build_catalog(&base, &packs);

    assert_eq!(
        report.entries, 2,
        "the catalog build considers one block and one item"
    );
    assert_eq!(
        report.errors(),
        0,
        "the proof pack must build without errors: {:?}",
        report.diagnostics
    );
    assert_eq!(
        report.warnings(),
        0,
        "the proof pack must build without warnings: {:?}",
        report.diagnostics
    );

    assert_eq!(
        catalog.blocks().len(),
        2,
        "one base block plus one pack block"
    );
    let block = &catalog.blocks()[1];
    assert_eq!(block.id, 1, "the pack block allocates right after the base");
    assert_eq!(
        block.faces, [PACK_LAYER_BASE; 6],
        "the pack block's six faces share the first pack layer"
    );
    assert_eq!(catalog.items().len(), 1, "one pack item");
    assert_eq!(catalog.items()[0].index, 0, "the neutral item index is 0");
    assert_eq!(
        catalog.items()[0].icon_layer,
        PACK_ICON_BASE,
        "the pack item icon takes the first pack icon layer"
    );
    assert_eq!(catalog.pack_layers().len(), 1, "one pack block layer");
    assert_eq!(catalog.pack_icons().len(), 1, "one pack icon layer");
    assert_eq!(catalog.packs().len(), 1, "one pack identity in the stack");
    assert_ne!(
        catalog.digest(),
        EMPTY_DIGEST,
        "a non-empty pack stack must move the content digest"
    );

    ContentProof {
        pack_entries,
        blocks,
        items,
        sprite_bytes: sprite.len(),
        catalog_digest: catalog.digest(),
    }
}
