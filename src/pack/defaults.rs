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
//! same 76 `(path, bytes)` entries in sorted path order, and
//! [`default_pack_digest`] pins that exact byte stream. Writing those entries
//! to disk is [`export_default_pack`]'s only job.
//!
//! [`ensure_default_pack`] is the opposite policy on purpose: exporting always
//! overwrites, while restoring only fills absent files and repairs an unusable
//! manifest, so user edits and corrupt-present bytes survive a relaunch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::audio::{default_buffers, encode_pcm16_wav, SoundSlot};
use crate::content::names::builtin_sprite_name_for;
use crate::pack::default_master::DEFAULT_PACK_FILES;
use crate::pack::manifest::{format_gate_error, parse_pack_file, read_manifest_text};
use crate::pack::{LAYER_FILES, MOB_LAYER_FILES};
use crate::render::texture::{generate_all_texture_layers_rgba, MOB_LAYER_BASE, TEX_SIZE};
use crate::settings::SettingsWarning;
use crate::ui::item_icon::{generate_item_sprite_layers_rgba, item_sprite_table};

/// Folder name of the shipped pack, resolved under `packs/`.
pub const DEFAULT_PACK_FOLDER: &str = "default";

/// Display name declared in the shipped `pack.toml`.
pub const DEFAULT_PACK_NAME: &str = "Default";

/// One-sentence description of the shipped reference presentation.
pub const DEFAULT_PACK_DESCRIPTION: &str =
    "The shipped reference presentation exported from the built-in procedural art.";

/// Number of files the generator emits: `pack.toml`, 26 block layers, 13 mob
/// layers, 27 built-in item icons and 9 sound files.
pub const DEFAULT_PACK_FILE_COUNT: usize = 76;

/// FNV-1a offset basis for [`default_pack_digest`].
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a prime for [`default_pack_digest`].
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Encode an arbitrary RGBA8 image as a PNG with the pack writer's defaults.
///
/// This is the shared core of [`encode_rgba_png`] and the `--demo-atlas`
/// artifact writer. The `png` crate is used with its defaults (RGBA8, no
/// palette, no timestamps, no ancillary text chunks), so the same `rgba`
/// bytes always yield the same PNG bytes. Panics when `rgba` is not exactly
/// `width * height * 4` bytes; that is a generator bug, not an input error,
/// because every caller supplies a composed image of a known shape.
pub fn encode_rgba_png_dims(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let expected = width as usize * height as usize * 4;
    assert_eq!(
        rgba.len(),
        expected,
        "encode_rgba_png_dims expects a {width}x{height} RGBA image ({expected} bytes), got {}",
        rgba.len()
    );

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .expect("PNG header encoding into memory cannot fail");
        writer
            .write_image_data(rgba)
            .expect("PNG data encoding for an RGBA image cannot fail");
        writer
            .finish()
            .expect("PNG stream finalization into memory cannot fail");
    }
    out
}

/// Encode one 16x16 RGBA layer as an 8-bit RGBA PNG.
///
/// Thin wrapper over [`encode_rgba_png_dims`] that keeps the fixed 16x16
/// layer contract and its existing panic message. See that function for the
/// encoder defaults that make the bytes deterministic.
pub fn encode_rgba_png(rgba: &[u8]) -> Vec<u8> {
    let expected = (TEX_SIZE * TEX_SIZE * 4) as usize;
    assert_eq!(
        rgba.len(),
        expected,
        "encode_rgba_png expects a {TEX_SIZE}x{TEX_SIZE} RGBA layer ({expected} bytes), got {}",
        rgba.len()
    );
    encode_rgba_png_dims(TEX_SIZE, TEX_SIZE, rgba)
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
    let entries = build_default_pack();
    pack_digest(
        entries
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
}

/// Fold arbitrary `(path, bytes)` entries into the pinned digest framing.
///
/// Shared with the embedded-master drift tests so the folder-derived bytes,
/// the compile-time master and the in-memory generator all fold the exact same
/// way. As with [`default_pack_digest`], entries must be supplied in sorted
/// path order.
pub(crate) fn pack_digest<'a, I>(entries: I) -> u64
where
    I: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    let mut hash = FNV_OFFSET_BASIS;
    for (path, bytes) in entries {
        fnv1a_update(&mut hash, path.as_bytes());
        fnv1a_update(&mut hash, &[0x00]);
        fnv1a_update(&mut hash, &(bytes.len() as u32).to_le_bytes());
        fnv1a_update(&mut hash, bytes);
    }
    hash
}

/// Fold one named byte stream into the [`pack_digest`] framing.
///
/// `label` plays the role of the entry path, so an artifact that is not a
/// pack file still uses exactly one hashing convention:
/// `label.as_bytes() || 0x00 || (len as u32).to_le_bytes() || bytes`.
pub fn byte_stream_digest(label: &str, bytes: &[u8]) -> u64 {
    pack_digest(std::iter::once((label, bytes)))
}

/// Digest of the composed block-face atlas RGBA sheet.
///
/// This is the pinned "composed layers are stable" value; it covers the
/// pixel bytes exactly as composed, before PNG encoding.
pub fn atlas_digest(bytes: &[u8]) -> u64 {
    byte_stream_digest("atlas.rgba", bytes)
}

/// Digest of the encoded `atlas.png` file bytes.
///
/// Pinned separately from [`atlas_digest`] so PNG-encoder drift fails by name
/// instead of hiding behind a stable pixel digest.
pub fn atlas_png_digest(bytes: &[u8]) -> u64 {
    byte_stream_digest("atlas.png", bytes)
}

/// Write every generated file under `dir`, creating parents as needed.
///
/// This is the exporter, so overwriting is correct here; the restore policy
/// in [`ensure_default_pack`] must never overwrite present bytes. Returns the
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

/// Warning field used for every restore-policy notice.
pub const DEFAULT_PACK_RESTORE_FIELD: &str = "packs/default";

/// Result of one [`ensure_default_pack`] call.
///
/// `restored_files` counts the files actually written by this call (the
/// manifest included), `manifest_rewritten` reports whether `pack.toml` was
/// rebuilt from the embedded master, `ready` is the post-call manifest gate,
/// and `warnings` carries the single success notice or one entry per failing
/// path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultPackOutcome {
    /// Files written by this call; absent files only, plus a repaired manifest.
    pub restored_files: usize,
    /// Whether `pack.toml` was rewritten from the embedded master.
    pub manifest_rewritten: bool,
    /// Whether the folder now declares an accepted v3 `pack.toml`.
    pub ready: bool,
    /// Restore notices for the first Settings open; never logged directly.
    pub warnings: Vec<SettingsWarning>,
}

impl DefaultPackOutcome {
    /// Build one warning on the pinned restore field.
    fn warning(reason: String) -> SettingsWarning {
        SettingsWarning {
            field: DEFAULT_PACK_RESTORE_FIELD.to_string(),
            reason,
        }
    }
}

/// Whether `packs_root/default/pack.toml` is a readable, accepted v3 manifest.
///
/// This is deliberately manifest-only: it never decodes a PNG or a WAV, so a
/// missing or corrupt art file cannot make an otherwise usable pack look
/// unusable. A syntactically invalid manifest (no TOML root), a non-regular or
/// oversized file (rejected by [`read_manifest_text`]) and any format other
/// than v3 all return `false`.
pub fn default_pack_ready(packs_root: &Path) -> bool {
    let manifest_path = packs_root.join(DEFAULT_PACK_FOLDER).join("pack.toml");
    let Ok(text) = read_manifest_text(&manifest_path) else {
        return false;
    };
    let (manifest, root, _warnings) = parse_pack_file(&text);
    root.is_some() && format_gate_error(manifest.format_version).is_none()
}

/// Restore the shipped default pack without ever overwriting present bytes.
///
/// Policy, in order:
///
/// 1. If `packs_root/default` cannot be created, return one warning naming the
///    folder and its OS error with `ready = false`.
/// 2. Write every embedded file except `pack.toml` only when its target path
///    does not already exist. Parent folders (`icons/`) are created as needed.
///    An edited or corrupt-but-present file keeps its bytes untouched.
/// 3. When the manifest is missing or unusable (see [`default_pack_ready`]),
///    rewrite `pack.toml` last from the embedded master. A ready manifest is
///    never touched, so a user-edited display name survives.
/// 4. A failing write records one warning naming the path and continues; a
///    blocked `icons/` therefore still lets the manifest repair run. This
///    function never panics and never logs.
/// 5. When any file was written, append exactly one success notice
///    `restored the shipped Default pack (N files)`, where `N` counts every
///    file written by this call (the manifest included).
pub fn ensure_default_pack(packs_root: &Path) -> DefaultPackOutcome {
    let initial_ready = default_pack_ready(packs_root);
    let mut outcome = DefaultPackOutcome {
        restored_files: 0,
        manifest_rewritten: false,
        ready: initial_ready,
        warnings: Vec::new(),
    };

    let pack_dir = packs_root.join(DEFAULT_PACK_FOLDER);
    if let Err(err) = std::fs::create_dir_all(&pack_dir) {
        outcome.ready = false;
        outcome.warnings.push(DefaultPackOutcome::warning(format!(
            "failed to create {}: {err}",
            pack_dir.display()
        )));
        return outcome;
    }

    // Only report each blocked parent folder once: a blocked `icons/` would
    // otherwise emit 27 identical warnings for the same cause.
    let mut reported_parents: BTreeSet<PathBuf> = BTreeSet::new();
    for (relative, bytes) in DEFAULT_PACK_FILES {
        if *relative == "pack.toml" {
            continue;
        }
        let target = pack_dir.join(relative);
        // `symlink_metadata` treats a present broken symlink as present, so a
        // restore never replaces something the user can see in the folder.
        if target.symlink_metadata().is_ok() {
            continue;
        }
        if let Some(parent) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                if reported_parents.insert(parent.to_path_buf()) {
                    outcome.warnings.push(DefaultPackOutcome::warning(format!(
                        "failed to create {}: {err}",
                        parent.display()
                    )));
                }
                continue;
            }
        }
        match std::fs::write(&target, bytes) {
            Ok(()) => outcome.restored_files += 1,
            Err(err) => outcome.warnings.push(DefaultPackOutcome::warning(format!(
                "failed to write {}: {err}",
                target.display()
            ))),
        }
    }

    if !initial_ready {
        let manifest_path = pack_dir.join("pack.toml");
        let manifest_bytes = DEFAULT_PACK_FILES
            .iter()
            .find_map(|(path, bytes)| (*path == "pack.toml").then_some(*bytes))
            .expect("the embedded master always contains pack.toml");
        match std::fs::write(&manifest_path, manifest_bytes) {
            Ok(()) => {
                outcome.restored_files += 1;
                outcome.manifest_rewritten = true;
            }
            Err(err) => outcome.warnings.push(DefaultPackOutcome::warning(format!(
                "failed to write {}: {err}",
                manifest_path.display()
            ))),
        }
    }

    outcome.ready = default_pack_ready(packs_root);
    if outcome.restored_files > 0 {
        outcome.warnings.push(DefaultPackOutcome::warning(format!(
            "restored the shipped Default pack ({} files)",
            outcome.restored_files
        )));
    }
    outcome
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

    /// The embedded master bytes for one relative path.
    fn master_bytes(relative: &str) -> &'static [u8] {
        DEFAULT_PACK_FILES
            .iter()
            .find_map(|(path, bytes)| (*path == relative).then_some(*bytes))
            .unwrap_or_else(|| panic!("master is missing {relative}"))
    }

    fn restore_success_reason(count: usize) -> String {
        format!("restored the shipped Default pack ({count} files)")
    }

    #[test]
    fn default_pack_file_set_is_exactly_76_unique_sorted_paths() {
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
        const PINNED: u64 = 4_563_584_254_880_741_902;
        assert_eq!(
            default_pack_digest(),
            PINNED,
            "default-pack bytes drifted from the pinned digest"
        );
    }

    #[test]
    fn restore_creates_all_76_files_in_a_missing_root() {
        let root = temp_root();
        let outcome = ensure_default_pack(root.path());

        assert_eq!(
            outcome.restored_files, DEFAULT_PACK_FILE_COUNT,
            "a fresh root restores every shipped file"
        );
        assert!(outcome.manifest_rewritten);
        assert!(outcome.ready);
        assert_eq!(
            outcome.warnings.len(),
            1,
            "warnings: {:?}",
            outcome.warnings
        );
        assert_eq!(outcome.warnings[0].field, DEFAULT_PACK_RESTORE_FIELD);
        assert_eq!(
            outcome.warnings[0].reason,
            restore_success_reason(DEFAULT_PACK_FILE_COUNT)
        );

        let dir = root.path().join(DEFAULT_PACK_FOLDER);
        for (relative, bytes) in DEFAULT_PACK_FILES {
            let written = std::fs::read(dir.join(relative))
                .unwrap_or_else(|err| panic!("missing restored {relative}: {err}"));
            assert_eq!(&written, bytes, "restored {relative} must equal the master");
        }
    }

    #[test]
    fn restore_is_a_silent_noop_when_the_folder_is_complete() {
        let root = temp_root();
        let first = ensure_default_pack(root.path());
        assert!(first.ready, "warnings: {:?}", first.warnings);

        let second = ensure_default_pack(root.path());
        assert_eq!(second.restored_files, 0);
        assert!(!second.manifest_rewritten);
        assert!(second.ready);
        assert!(
            second.warnings.is_empty(),
            "a complete folder must stay silent, got {:?}",
            second.warnings
        );
    }

    #[test]
    fn restore_fills_absent_files_and_keeps_edited_bytes() {
        let root = temp_root();
        let _ = ensure_default_pack(root.path());
        let dir = root.path().join(DEFAULT_PACK_FOLDER);

        let edited = b"user-edited stone bytes, not a PNG";
        std::fs::write(dir.join("stone.png"), edited).expect("edit stone.png");
        std::fs::remove_file(dir.join("grass_top.png")).expect("delete grass_top.png");

        let outcome = ensure_default_pack(root.path());
        assert_eq!(outcome.restored_files, 1, "only the absent file is written");
        assert!(!outcome.manifest_rewritten);
        assert!(outcome.ready);
        assert_eq!(
            outcome.warnings[0].reason,
            restore_success_reason(1),
            "warnings: {:?}",
            outcome.warnings
        );

        assert_eq!(
            std::fs::read(dir.join("stone.png")).unwrap(),
            edited,
            "edited bytes must survive the restore"
        );
        assert_eq!(
            std::fs::read(dir.join("grass_top.png")).unwrap(),
            master_bytes("grass_top.png"),
            "a deleted file is restored from the master"
        );
    }

    #[test]
    fn restore_rewrites_a_missing_or_non_v3_manifest_and_keeps_art_edits() {
        let root = temp_root();
        let _ = ensure_default_pack(root.path());
        let dir = root.path().join(DEFAULT_PACK_FOLDER);
        let manifest_path = dir.join("pack.toml");

        let edited = b"user-edited stone bytes, not a PNG";
        std::fs::write(dir.join("stone.png"), edited).expect("edit stone.png");

        std::fs::remove_file(&manifest_path).expect("delete pack.toml");
        let missing = ensure_default_pack(root.path());
        assert_eq!(missing.restored_files, 1);
        assert!(missing.manifest_rewritten);
        assert!(missing.ready);
        assert_eq!(
            std::fs::read(&manifest_path).unwrap(),
            master_bytes("pack.toml")
        );
        assert_eq!(std::fs::read(dir.join("stone.png")).unwrap(), edited);

        let v2 = default_pack_manifest().replace("format_version = 3", "format_version = 2");
        std::fs::write(&manifest_path, v2).expect("write a v2 manifest");
        assert!(!default_pack_ready(root.path()));

        let non_v3 = ensure_default_pack(root.path());
        assert_eq!(non_v3.restored_files, 1, "only the manifest is rewritten");
        assert!(non_v3.manifest_rewritten);
        assert!(non_v3.ready, "warnings: {:?}", non_v3.warnings);
        assert_eq!(
            std::fs::read(&manifest_path).unwrap(),
            master_bytes("pack.toml")
        );
        assert_eq!(
            std::fs::read(dir.join("stone.png")).unwrap(),
            edited,
            "manifest repair must not touch art bytes"
        );
    }

    #[test]
    fn restore_leaves_corrupt_present_art_alone() {
        let root = temp_root();
        let _ = ensure_default_pack(root.path());
        let dir = root.path().join(DEFAULT_PACK_FOLDER);

        let corrupt = b"this is present but not a PNG";
        std::fs::write(dir.join("stone.png"), corrupt).expect("write corrupt stone.png");
        assert!(
            default_pack_ready(root.path()),
            "readiness is manifest-only and must ignore corrupt art"
        );

        let outcome = ensure_default_pack(root.path());
        assert_eq!(outcome.restored_files, 0);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert_eq!(std::fs::read(dir.join("stone.png")).unwrap(), corrupt);

        let spec = crate::pack::spec::load_spec_from_dir(&dir);
        assert!(
            spec.warnings
                .iter()
                .any(|warning| warning.field == "stone.png"),
            "the loader still reports the corrupt file: {:?}",
            spec.warnings
        );
    }

    #[test]
    fn restore_reports_a_write_failure_without_panicking() {
        // (a) The packs root itself is a regular file: folder creation fails
        // with exactly one warning naming the path, and nothing panics.
        let root = temp_root();
        let blocked_root = root.path().join("packs");
        std::fs::write(&blocked_root, b"not a directory").expect("block the packs root");

        let outcome = ensure_default_pack(&blocked_root);
        assert_eq!(outcome.restored_files, 0);
        assert!(!outcome.manifest_rewritten);
        assert!(!outcome.ready);
        assert_eq!(
            outcome.warnings.len(),
            1,
            "warnings: {:?}",
            outcome.warnings
        );
        assert_eq!(outcome.warnings[0].field, DEFAULT_PACK_RESTORE_FIELD);
        assert!(
            outcome.warnings[0].reason.contains("packs"),
            "warning must name the path: {}",
            outcome.warnings[0].reason
        );

        // (b) `icons/` is blocked by a regular file: one warning names that
        // folder, the absent non-icon files and the manifest still restore.
        let root = temp_root();
        let dir = root.path().join(DEFAULT_PACK_FOLDER);
        std::fs::create_dir_all(&dir).expect("create the pack folder");
        std::fs::write(dir.join("icons"), b"blocked").expect("block icons/");

        let outcome = ensure_default_pack(root.path());
        assert!(outcome.manifest_rewritten, "the manifest must still repair");
        assert!(outcome.ready);
        assert_eq!(
            std::fs::read(dir.join("pack.toml")).unwrap(),
            master_bytes("pack.toml")
        );
        assert!(
            dir.join("arrow.png").exists(),
            "non-icon files are restored"
        );
        assert_eq!(
            outcome.restored_files, 49,
            "75 non-manifest files minus 27 icons plus the manifest"
        );

        let icon_failures: Vec<&SettingsWarning> = outcome
            .warnings
            .iter()
            .filter(|warning| warning.reason.contains("icons"))
            .collect();
        assert_eq!(
            icon_failures.len(),
            1,
            "a blocked icons/ reports one warning: {:?}",
            outcome.warnings
        );
        assert!(icon_failures[0].reason.contains("failed to create"));
        assert!(outcome
            .warnings
            .iter()
            .any(|warning| warning.reason == restore_success_reason(49)));
    }

    #[test]
    fn restore_default_pack_ready_rejects_v1_v2_and_future_manifests() {
        let root = temp_root();
        let dir = root.path().join(DEFAULT_PACK_FOLDER);
        std::fs::create_dir_all(&dir).expect("create the pack folder");
        let manifest_path = dir.join("pack.toml");

        assert!(
            !default_pack_ready(root.path()),
            "absent manifest is not ready"
        );

        for text in [
            "format_version = 1\nname = \"Old\"\n",
            "format_version = 2\nname = \"Old\"\n",
            "format_version = 4\nname = \"Future\"\n",
            "[unterminated",
        ] {
            std::fs::write(&manifest_path, text).expect("write candidate manifest");
            assert!(
                !default_pack_ready(root.path()),
                "candidate manifest must be rejected: {text:?}"
            );
        }

        std::fs::write(&manifest_path, default_pack_manifest()).expect("write master manifest");
        assert!(
            default_pack_ready(root.path()),
            "the master v3 manifest is ready"
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
