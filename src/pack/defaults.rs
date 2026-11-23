//! Deterministic generator for the shipped `packs/default` pack.
//!
//! The default pack is the game's existing procedural art and synthesized
//! sounds exported to the same on-disk format a hand-authored pack uses. It is
//! generated on the CPU from [`crate::render::texture`],
//! [`crate::ui::item_icon`] and [`crate::audio`], never authored by hand, so
//! the shipped folder, the embedded restore master and the live procedural
//! fallbacks can never drift silently.
//!
//! The generator is pure and byte-deterministic: two calls always produce the
//! same 75 `(path, bytes)` entries in sorted path order, and
//! [`default_pack_digest`] pins that exact byte stream. Writing those entries
//! to disk is [`export_default_pack`]'s only job.
//!
//! This module intentionally contains no restore policy (see the later
//! `ensure_default_pack` task): exporting always overwrites, while restoring
//! must never touch bytes that are already present.

use std::collections::BTreeMap;
use std::path::Path;

use crate::audio::{default_buffers, encode_pcm16_wav, SoundSlot};
use crate::content::names::builtin_sprite_name_for;
use crate::pack::{LAYER_FILES, MOB_LAYER_FILES};
use crate::render::texture::{generate_all_texture_layers_rgba, MOB_LAYER_BASE, TEX_SIZE};
use crate::ui::item_icon::{generate_item_sprite_layers_rgba, item_sprite_table};

/// Folder name of the shipped pack, resolved under `packs/`.
pub const DEFAULT_PACK_FOLDER: &str = "default";

/// Display name declared in the shipped `pack.toml`.
pub const DEFAULT_PACK_NAME: &str = "Default";

/// One-sentence description of the shipped reference presentation.
pub const DEFAULT_PACK_DESCRIPTION: &str =
    "The shipped reference presentation exported from the built-in procedural art.";

/// Number of files the generator emits: `pack.toml`, 25 block layers, 13 mob
/// layers, 27 built-in item icons and 9 sound files.
pub const DEFAULT_PACK_FILE_COUNT: usize = 75;

/// FNV-1a offset basis for [`default_pack_digest`].
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a prime for [`default_pack_digest`].
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Encode one 16x16 RGBA layer as an 8-bit RGBA PNG.
///
/// The `png` crate is used with its defaults (no palette, no timestamps, no
/// ancillary text chunks), so the same `rgba` bytes always yield the same PNG
/// bytes. Panics when `rgba` is not exactly
/// [`TEX_SIZE`]x[`TEX_SIZE`] RGBA (`1024`) bytes; that is a generator bug, not
/// an input error, because the procedural sources always produce that shape.
pub fn encode_rgba_png(rgba: &[u8]) -> Vec<u8> {
    let expected = (TEX_SIZE * TEX_SIZE * 4) as usize;
    assert_eq!(
        rgba.len(),
        expected,
        "encode_rgba_png expects a {TEX_SIZE}x{TEX_SIZE} RGBA layer ({expected} bytes), got {}",
        rgba.len()
    );

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, TEX_SIZE, TEX_SIZE);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .expect("PNG header encoding into memory cannot fail");
        writer
            .write_image_data(rgba)
            .expect("PNG data encoding for a 16x16 RGBA layer cannot fail");
        writer
            .finish()
            .expect("PNG stream finalization into memory cannot fail");
    }
    out
}

/// The 27 built-in item sprite names mapped to their `icons/` pack paths,
/// sorted by name.
///
/// The names come from the pinned [`item_sprite_table`] through the canonical
/// inverse [`builtin_sprite_name_for`]. A table entry without a canonical name
/// would make the generated manifest reference a nonexistent sprite, so it is
/// a hard panic naming the offending identity.
fn default_builtin_icons() -> BTreeMap<&'static str, String> {
    let mut icons = BTreeMap::new();
    for (id, _layer) in item_sprite_table() {
        let name = builtin_sprite_name_for(*id).unwrap_or_else(|| {
            panic!("built-in sprite table entry {id:?} has no canonical sprite name")
        });
        icons.insert(name, format!("icons/{name}.png"));
    }
    assert_eq!(
        icons.len(),
        item_sprite_table().len(),
        "built-in sprite names must be unique"
    );
    icons
}

/// Build the shipped `pack.toml` text.
///
/// The manifest is presentation-only: it declares the v3 format, the display
/// name/description/version, one `[builtin_icons]` entry per pinned item
/// sprite, and a documented no-op `[menu]` tint. It deliberately declares no
/// `[[blocks]]`, `[[items]]`, `[[tiers]]`, `[[tools]]` or `[[recipes]]`, so the
/// pack allocates nothing in the content registry and never enters the save
/// identity.
pub fn default_pack_manifest() -> String {
    let mut text = String::new();
    text.push_str("format_version = 3\n");
    text.push_str(&format!("name = \"{DEFAULT_PACK_NAME}\"\n"));
    text.push_str(&format!("description = \"{DEFAULT_PACK_DESCRIPTION}\"\n"));
    text.push_str("version = \"1.0.0\"\n\n");

    text.push_str("[builtin_icons]\n");
    for (name, path) in default_builtin_icons() {
        text.push_str(&format!("{name} = \"{path}\"\n"));
    }

    text.push_str("\n[menu]\n");
    text.push_str("# Fully transparent RGBA: the shipped presentation applies no menu tint.\n");
    text.push_str("overlay_tint = [0, 0, 0, 0]\n");
    text
}

/// Generate every shipped file as `(relative path, bytes)`, sorted by path.
///
/// The layer file names are the pack authoring contract ([`LAYER_FILES`] and
/// [`MOB_LAYER_FILES`]); never invent alternatives. Layers are taken from the
/// same 48-layer procedural set the renderer uploads at startup: block layers
/// `0..`[`crate::pack::AUTHORABLE_TEX_LAYERS`] and mob layers
/// [`MOB_LAYER_BASE`]`..`[`MOB_LAYER_BASE`] + 13. Icon PNGs carry the exact
/// procedural sprite pixels at each pinned table layer, and each WAV is
/// [`crate::audio::encode_pcm16_wav`] over the synthesized default buffer.
pub fn build_default_pack() -> Vec<(String, Vec<u8>)> {
    let layers = generate_all_texture_layers_rgba();
    let sprites = generate_item_sprite_layers_rgba();
    let buffers = default_buffers();

    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(DEFAULT_PACK_FILE_COUNT);
    entries.push((
        "pack.toml".to_string(),
        default_pack_manifest().into_bytes(),
    ));

    for (index, file) in LAYER_FILES.iter().enumerate() {
        entries.push(((*file).to_string(), encode_rgba_png(&layers[index])));
    }

    for (offset, file) in MOB_LAYER_FILES.iter().enumerate() {
        let layer = MOB_LAYER_BASE as usize + offset;
        entries.push(((*file).to_string(), encode_rgba_png(&layers[layer])));
    }

    for (id, layer) in item_sprite_table() {
        let name = builtin_sprite_name_for(*id).unwrap_or_else(|| {
            panic!("built-in sprite table entry {id:?} has no canonical sprite name")
        });
        entries.push((
            format!("icons/{name}.png"),
            encode_rgba_png(&sprites[*layer as usize]),
        ));
    }

    for slot in SoundSlot::ALL {
        entries.push((
            format!("{}.wav", slot.name()),
            encode_pcm16_wav(&buffers[slot.index()]),
        ));
    }

    entries.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        entries.len(),
        DEFAULT_PACK_FILE_COUNT,
        "the shipped default pack must contain exactly {DEFAULT_PACK_FILE_COUNT} files"
    );
    entries
}

/// Fold bytes into an FNV-1a hash.
fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// Byte digest of the whole generated pack, for drift pinning.
///
/// FNV-1a (64-bit, offset basis `0xcbf29ce484222325`, prime
/// `0x100000001b3`) is folded over each entry in sorted path order using this
/// exact framing:
///
/// ```text
/// path.as_bytes() || 0x00 || (bytes.len() as u32).to_le_bytes() || bytes
/// ```
///
/// The separator and the explicit little-endian length make the framing
/// unambiguous, so the pinned constant is reproducible on every platform.
pub fn default_pack_digest() -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for (path, bytes) in build_default_pack() {
        fnv1a_update(&mut hash, path.as_bytes());
        fnv1a_update(&mut hash, &[0x00]);
        fnv1a_update(&mut hash, &(bytes.len() as u32).to_le_bytes());
        fnv1a_update(&mut hash, &bytes);
    }
    hash
}

/// Write every generated file under `dir`, creating parents as needed.
///
/// This is the exporter, so overwriting is correct here; the restore policy
/// that must never overwrite present bytes lives in a later task. Returns the
/// first I/O error, which the example binary turns into a non-zero exit.
pub fn export_default_pack(dir: &Path) -> std::io::Result<()> {
    for (relative, bytes) in build_default_pack() {
        let path = dir.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &bytes)?;
    }
    Ok(())
}

/// Assert the shipped manifest parses as a clean v3 manifest with 27 icons.
///
/// Kept next to the generator so a format change that breaks validation fails
/// the pack's own test suite first.
#[cfg(test)]
fn assert_manifest_gate(text: &str) {
    use crate::pack::manifest::{format_gate_error, parse_pack_file, PackFormat};

    let (manifest, root, warnings) = parse_pack_file(text);
    assert!(warnings.is_empty(), "manifest warnings: {warnings:?}");
    assert_eq!(manifest.format_version, PackFormat::V3);
    assert_eq!(format_gate_error(manifest.format_version), None);
    assert_eq!(manifest.name.as_deref(), Some(DEFAULT_PACK_NAME));
    assert_eq!(
        manifest.description.as_deref(),
        Some(DEFAULT_PACK_DESCRIPTION)
    );
    assert_eq!(manifest.version.as_deref(), Some("1.0.0"));
    assert_eq!(manifest.overlay_tint, Some([0, 0, 0, 0]));

    let root = root.expect("the shipped manifest is valid TOML");
    let (content, warnings, diagnostics) =
        crate::pack::v3::parse_content(&root, manifest.format_version);
    assert!(warnings.is_empty(), "content warnings: {warnings:?}");
    assert!(
        diagnostics.is_empty(),
        "content diagnostics: {diagnostics:?}"
    );
    assert_eq!(content.builtin_icons.len(), 27);
    assert!(content.blocks.is_empty());
    assert!(content.items.is_empty());
    assert!(content.tiers.is_empty());
    assert!(content.tools.is_empty());
    assert!(content.recipes.is_empty());
    assert!(content.worldgen.is_empty());
    assert!(content.block_patches.is_empty());
    assert!(content.recipe_patches.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::manifest::{parse_pack_file, PackFormat};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp root")
    }

    /// Export the generated pack into `<root>/default` and return that folder.
    fn export_to(root: &Path) -> PathBuf {
        let dir = root.join(DEFAULT_PACK_FOLDER);
        export_default_pack(&dir).expect("export the default pack");
        dir
    }

    #[test]
    fn default_pack_file_set_is_exactly_75_unique_sorted_paths() {
        let entries = build_default_pack();
        assert_eq!(entries.len(), DEFAULT_PACK_FILE_COUNT);

        let paths: Vec<&str> = entries.iter().map(|(path, _)| path.as_str()).collect();
        let unique: BTreeSet<&str> = paths.iter().copied().collect();
        assert_eq!(unique.len(), paths.len(), "duplicate paths in {paths:?}");
        assert!(
            paths.windows(2).all(|pair| pair[0] < pair[1]),
            "paths must be sorted"
        );

        let mut expected: BTreeSet<String> = BTreeSet::new();
        expected.insert("pack.toml".to_string());
        expected.extend(LAYER_FILES.iter().map(|file| (*file).to_string()));
        expected.extend(MOB_LAYER_FILES.iter().map(|file| (*file).to_string()));
        for (id, _layer) in item_sprite_table() {
            let name = builtin_sprite_name_for(*id).expect("canonical sprite name");
            expected.insert(format!("icons/{name}.png"));
        }
        expected.extend(
            SoundSlot::ALL
                .iter()
                .map(|slot| format!("{}.wav", slot.name())),
        );

        let expected: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
        assert_eq!(unique, expected);
    }

    #[test]
    fn default_pack_manifest_passes_the_v3_gate_with_27_builtin_icons() {
        let text = default_pack_manifest();
        assert_manifest_gate(&text);

        // The icon table is sorted by name and points at the generated files.
        let (_, root, _) = parse_pack_file(&text);
        let root = root.expect("valid TOML");
        let (content, _, _) = crate::pack::v3::parse_content(&root, PackFormat::V3);
        let names: Vec<&String> = content.builtin_icons.keys().collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        for (name, path) in &content.builtin_icons {
            assert_eq!(path, &format!("icons/{name}.png"));
        }
    }

    #[test]
    fn exported_layers_equal_the_procedural_source_bytes() {
        let root = temp_root();
        let dir = export_to(root.path());

        let spec = crate::pack::spec::load_spec_from_dir(&dir);
        assert!(spec.rejected.is_none(), "default pack must be accepted");
        assert!(spec.warnings.is_empty(), "warnings: {:?}", spec.warnings);
        assert!(
            spec.diagnostics.is_empty(),
            "diagnostics: {:?}",
            spec.diagnostics
        );

        let resolved = crate::pack::spec::resolve_presentation(&[spec]);
        assert_eq!(resolved.layers, generate_all_texture_layers_rgba());
        assert_eq!(resolved.display_name, DEFAULT_PACK_NAME);
    }

    #[test]
    fn exported_icon_pngs_decode_to_the_procedural_sprite_bytes() {
        let entries = build_default_pack();
        let sprites = generate_item_sprite_layers_rgba();
        let by_path: BTreeMap<&str, &Vec<u8>> = entries
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes))
            .collect();

        for (id, layer) in item_sprite_table() {
            let name = builtin_sprite_name_for(*id).expect("canonical sprite name");
            let path = format!("icons/{name}.png");
            let bytes = by_path
                .get(path.as_str())
                .unwrap_or_else(|| panic!("missing generated icon {path}"));
            let decoded = crate::pack::decode::decode_png_rgba_bytes(bytes, &path, None)
                .unwrap_or_else(|err| panic!("icon {path} failed to decode: {err}"));
            assert_eq!(decoded.width, TEX_SIZE, "icon {name} width");
            assert_eq!(decoded.height, TEX_SIZE, "icon {name} height");
            assert_eq!(
                decoded.rgba, sprites[*layer as usize],
                "icon {name} must equal sprite layer {layer}"
            );
        }
    }

    #[test]
    fn exported_sounds_round_trip_through_the_pack_decoder() {
        let root = temp_root();
        let dir = export_to(root.path());

        let (buffers, warnings) = crate::pack::sound::load_sound_overrides(&dir);
        assert!(warnings.is_empty(), "sound warnings: {warnings:?}");

        let expected = default_buffers();
        for slot in SoundSlot::ALL {
            let index = slot.index();
            let actual = &buffers[index];
            let original = &expected[index];
            assert_eq!(
                actual.channels(),
                original.channels(),
                "{} channels",
                slot.name()
            );
            assert_eq!(
                actual.sample_rate(),
                original.sample_rate(),
                "{} sample rate",
                slot.name()
            );
            assert_eq!(
                actual.samples().len(),
                original.samples().len(),
                "{} sample count",
                slot.name()
            );
            for (sample_index, (actual, original)) in
                actual.samples().iter().zip(original.samples()).enumerate()
            {
                assert!(
                    (actual - original).abs() <= 2.0 / 32768.0,
                    "{} sample {sample_index}: decoded {actual} differs from synthesized {original}",
                    slot.name()
                );
            }
        }
    }

    #[test]
    fn exported_pack_validates_with_27_entries_and_no_findings() {
        let root = temp_root();
        let _dir = export_to(root.path());

        let (lines, ok) =
            crate::pack::validate::validate_pack_lines(root.path(), DEFAULT_PACK_FOLDER);
        assert!(ok, "validation lines: {lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("validate-pack: default: 27 entries, 0 errors, 0 warnings"),
            "lines: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.starts_with("error:") && !line.starts_with("warning:")),
            "unexpected findings: {lines:?}"
        );
    }

    #[test]
    fn default_pack_is_byte_deterministic_across_builds() {
        let first = build_default_pack();
        let second = build_default_pack();
        assert_eq!(first, second, "the generator must be byte-deterministic");
        assert_eq!(
            default_pack_digest(),
            default_pack_digest(),
            "the digest must be stable across calls"
        );
    }

    #[test]
    fn default_pack_digest_matches_the_pinned_literal() {
        // Pinned on the generator's first run; a change means the shipped pack
        // bytes (art, sounds or manifest) drifted. Re-pin only together with a
        // deliberate regeneration of `packs/default` and its embedded master.
        const PINNED: u64 = 7_461_094_496_369_431_543;
        assert_eq!(
            default_pack_digest(),
            PINNED,
            "default-pack bytes drifted from the pinned digest"
        );
    }

    #[test]
    #[should_panic(expected = "encode_rgba_png expects")]
    fn encode_rgba_png_rejects_a_wrong_length_layer() {
        // A short layer is a generator bug, not an authored-pack input: the
        // encoder must fail loudly instead of writing a malformed PNG.
        let _ = encode_rgba_png(&[0u8; 12]);
    }
}
