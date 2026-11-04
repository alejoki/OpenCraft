//! Authored-vs-resolved pack split and ordered stack merging.
//!
//! [`PackSpec`] records what a pack *authors* — one `Option` per layer file and
//! presentation field. [`resolve_presentation`] then folds an ordered stack of
//! specs into the always-complete [`LoadedPack`] the renderer consumes.
//!
//! Merging resolved packs would erase an earlier pack's authored art with
//! procedural defaults whenever a later pack authors only one file (MEM509).
//! Merging specs keeps the merge per authored file/field: a later pack wins
//! only where it actually ships content, and procedural art appears only when
//! no pack in the stack authored that layer.
//!
//! Everything here is GPU- and audio-device-free pure data plus filesystem
//! reads, mirroring [`crate::pack::load_pack_from_dir`]'s never-fail contract.

use std::path::Path;

use crate::content::spec::{ContentDiagnostic, PackContentSpec};
use crate::pack::asset;
use crate::pack::decode;
use crate::pack::manifest;
use crate::pack::manifest::PackFormat;
use crate::pack::resample;
use crate::pack::sound;
pub use crate::pack::sound::SoundSpec;
use crate::pack::v2;
use crate::pack::{LoadedPack, PackLogo, AUTHORABLE_TEX_LAYERS, LAYER_FILES, MOB_LAYER_FILES};
use crate::render::texture::{
    generate_all_texture_layers_rgba, MOB_AUTHORABLE_TEX_LAYERS, MOB_LAYER_BASE,
};
use crate::settings::SettingsWarning;

/// Smallest accepted layer side length, in pixels.
pub(crate) const MIN_LAYER_SIDE: u32 = 16;
/// Largest accepted layer side length, in pixels.
pub(crate) const MAX_LAYER_SIDE: u32 = 256;

/// What one pack authors, before any fallback is applied.
///
/// `block_layers` is exactly [`AUTHORABLE_TEX_LAYERS`] entries and
/// `mob_layers` exactly [`MOB_AUTHORABLE_TEX_LAYERS`] entries, index-aligned
/// with [`LAYER_FILES`] and [`MOB_LAYER_FILES`]. Each `Some` holds resampled
/// 16x16 RGBA bytes; `None` means the pack does not override that layer.
/// `warnings` covers the manifest, layers, and logo only — sound problems live
/// in [`SoundSpec::warnings`] because sound resolution is a separate entry
/// point.
#[derive(Clone, Debug)]
pub struct PackSpec {
    /// Pack folder name, used as the fallback display name.
    pub folder: String,
    /// Declared pack format (defaults to v1).
    pub format_version: PackFormat,
    /// Optional human-readable pack version, part of the registry digest.
    pub version: Option<String>,
    /// Parsed v1 manifest fields (`None` = unauthored).
    pub manifest: manifest::PackManifest,
    /// Authored block layers, index-aligned with [`LAYER_FILES`].
    pub block_layers: Vec<Option<Vec<u8>>>,
    /// Authored mob part layers, index-aligned with [`MOB_LAYER_FILES`].
    pub mob_layers: Vec<Option<Vec<u8>>>,
    /// Logo decoded at native size, when authored and valid.
    pub logo: Option<PackLogo>,
    /// Authored sound presence.
    pub sound: SoundSpec,
    /// Accepted v2 content entries (empty for v1 packs).
    pub content: PackContentSpec,
    /// Rejected v2 entries and asset failures (empty for v1 packs).
    pub diagnostics: Vec<ContentDiagnostic>,
    /// Manifest/layer/logo fallbacks, in the v1 warning order.
    pub warnings: Vec<SettingsWarning>,
}

/// Read one pack directory into authored presence only.
///
/// Missing files stay `None` silently. A file that exists but fails to decode,
/// is non-square, or is outside 16..=256 pixels stays `None` and emits exactly
/// one warning whose `field` is the bare file name — byte-for-byte the v1
/// [`crate::pack::load_pack_from_dir`] semantics. A missing or unreadable
/// `pack.toml` yields the default manifest plus one warning naming the path.
pub fn load_spec_from_dir(dir: &Path) -> PackSpec {
    let mut warnings = Vec::new();

    let manifest_path = dir.join("pack.toml");
    let (manifest, root, mut manifest_warnings) = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => manifest::parse_pack_file(&text),
        Err(err) => {
            warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!("failed to read {}: {err}", manifest_path.display()),
            });
            (manifest::PackManifest::default(), None, Vec::new())
        }
    };
    warnings.append(&mut manifest_warnings);

    // The v2 gate owns the v1 "content arrays require format_version = 2"
    // warning and the future-version warning. A syntactically invalid manifest
    // has no root and produced its own warning above.
    let (mut content, mut content_warnings, mut diagnostics) = match &root {
        Some(root) => v2::parse_content(root, manifest.format_version),
        None => (PackContentSpec::default(), Vec::new(), Vec::new()),
    };
    warnings.append(&mut content_warnings);
    if manifest.format_version == PackFormat::V2 {
        v2::load_content_assets(dir, &mut content, &mut diagnostics);
    }

    // Warning order mirrors v1: manifest, then block layers, then mob layers,
    // then the logo. Missing files stay silent.
    let block_layers = LAYER_FILES
        .iter()
        .map(|file| decode_layer(dir, file, &mut warnings))
        .collect();
    let mob_layers = MOB_LAYER_FILES
        .iter()
        .map(|file| decode_layer(dir, file, &mut warnings))
        .collect();
    let logo = load_logo(dir, manifest.logo.as_deref(), &mut warnings);

    let folder = dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "Unnamed Pack".to_string());

    PackSpec {
        folder,
        format_version: manifest.format_version,
        version: manifest.version.clone(),
        manifest,
        block_layers,
        mob_layers,
        logo,
        sound: sound::load_sound_spec(dir),
        content,
        diagnostics,
        warnings,
    }
}

/// Decode one optional layer file into resampled 16x16 RGBA bytes.
///
/// A missing file is silent `None`; a present-but-invalid file is `None` plus
/// one warning named after the bare file, matching v1 `apply_layer_files`.
fn decode_layer(dir: &Path, file: &str, warnings: &mut Vec<SettingsWarning>) -> Option<Vec<u8>> {
    let path = dir.join(file);
    if !path.exists() {
        // Missing layer files are legal: the stack keeps whatever is below.
        return None;
    }

    let decoded = match decode::decode_png_rgba(&path) {
        Ok(decoded) => decoded,
        Err(err) => {
            warnings.push(SettingsWarning {
                field: file.to_string(),
                reason: err,
            });
            return None;
        }
    };

    if decoded.width != decoded.height {
        warnings.push(SettingsWarning {
            field: file.to_string(),
            reason: format!(
                "image is {}x{}, layers must be square",
                decoded.width, decoded.height
            ),
        });
        return None;
    }

    if decoded.width < MIN_LAYER_SIDE || decoded.width > MAX_LAYER_SIDE {
        warnings.push(SettingsWarning {
            field: file.to_string(),
            reason: format!(
                "image is {}x{}, layers must be between {} and {} pixels per side",
                decoded.width, decoded.height, MIN_LAYER_SIDE, MAX_LAYER_SIDE
            ),
        });
        return None;
    }

    Some(resample::resample_to_16(&decoded.rgba, decoded.width))
}

/// Decode the optional manifest logo at native size, warning on failure.
fn load_logo(
    dir: &Path,
    logo: Option<&str>,
    warnings: &mut Vec<SettingsWarning>,
) -> Option<PackLogo> {
    let logo = logo?;
    let path = match asset::resolve_asset_path(dir, logo) {
        Ok(path) => path,
        Err(reason) => {
            warnings.push(SettingsWarning {
                field: logo.to_string(),
                reason,
            });
            return None;
        }
    };
    match decode::decode_png_native(&path) {
        Ok(decoded) => Some(PackLogo {
            rgba: decoded.rgba,
            width: decoded.width,
            height: decoded.height,
        }),
        Err(err) => {
            warnings.push(SettingsWarning {
                field: logo.to_string(),
                reason: err,
            });
            None
        }
    }
}

/// Fold an ordered stack of authored specs into one resolved presentation.
///
/// Starts from [`generate_all_texture_layers_rgba`] (exactly
/// [`crate::render::texture::NUM_TEX_LAYERS`] layers) and walks the specs in
/// stack order, overwriting only `Some` values. A later spec wins only for the
/// layers/fields it authored, so an earlier pack's art and presentation data
/// survive everywhere else; procedural art remains only where no pack authored
/// the layer. Warnings concatenate in stack order.
///
/// `display_name` is the last authored manifest name, else the last spec's
/// folder, else `"Unnamed Pack"`.
pub fn resolve_presentation(specs: &[PackSpec]) -> LoadedPack {
    let mut layers = generate_all_texture_layers_rgba();
    let mut logo = None;
    let mut overlay_tint = None;
    let mut description = None;
    let mut display_name = None;
    let mut warnings = Vec::new();

    for spec in specs {
        debug_assert_eq!(spec.block_layers.len(), AUTHORABLE_TEX_LAYERS);
        debug_assert_eq!(spec.mob_layers.len(), MOB_AUTHORABLE_TEX_LAYERS);

        for (index, authored) in spec.block_layers.iter().enumerate() {
            if let Some(rgba) = authored {
                layers[index] = rgba.clone();
            }
        }
        for (offset, authored) in spec.mob_layers.iter().enumerate() {
            if let Some(rgba) = authored {
                layers[MOB_LAYER_BASE as usize + offset] = rgba.clone();
            }
        }
        if let Some(authored_logo) = &spec.logo {
            logo = Some(authored_logo.clone());
        }
        if let Some(tint) = spec.manifest.overlay_tint {
            overlay_tint = Some(tint);
        }
        if let Some(text) = &spec.manifest.description {
            description = Some(text.clone());
        }
        if let Some(name) = &spec.manifest.name {
            display_name = Some(name.clone());
        }
        warnings.extend(spec.warnings.iter().cloned());
    }

    let display_name = display_name
        .or_else(|| specs.last().map(|spec| spec.folder.clone()))
        .unwrap_or_else(|| "Unnamed Pack".to_string());

    LoadedPack {
        layers,
        logo,
        overlay_tint,
        description,
        display_name,
        warnings,
    }
}

/// Load an ordered stack of pack folder names under `packs_root`.
///
/// Returns the merged presentation, the collected specs (the registry's
/// input), and the presentation warnings. A stack entry whose folder is not a
/// directory is skipped with exactly one warning naming it; the entry is not
/// removed from `stack`, so restoring the folder restores the pack. An empty
/// stack resolves to the procedural presentation with no warnings. When no
/// pack loads at all, the last stack name is kept as the display name, which
/// preserves [`crate::pack::load_pack_by_name`]'s missing-pack naming.
pub fn load_stack(
    packs_root: &Path,
    stack: &[String],
) -> (LoadedPack, Vec<PackSpec>, Vec<SettingsWarning>) {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();

    for name in stack {
        let dir = packs_root.join(name);
        if !dir.is_dir() {
            warnings.push(SettingsWarning {
                field: name.clone(),
                reason: format!("pack folder {} is missing", dir.display()),
            });
            continue;
        }

        let spec = load_spec_from_dir(&dir);
        warnings.extend(spec.warnings.iter().cloned());
        specs.push(spec);
    }

    let mut pack = resolve_presentation(&specs);
    pack.warnings = warnings.clone();

    if specs.is_empty() {
        if let Some(name) = stack.last() {
            pack.display_name = name.clone();
        }
    }

    (pack, specs, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::audio::default_buffers;
    use crate::render::texture::NUM_TEX_LAYERS;

    const LAYER_BYTES: usize = 16 * 16 * 4;

    fn pack_dir(root: &tempfile::TempDir, name: &str) -> PathBuf {
        let dir = root.path().join(name);
        fs::create_dir_all(&dir).expect("create pack dir");
        dir
    }

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    (x.wrapping_mul(7).wrapping_add(3)) as u8,
                    (y.wrapping_mul(11).wrapping_add(5)) as u8,
                    ((x + y).wrapping_mul(3)) as u8,
                    255,
                ]);
            }
        }
        data
    }

    fn write_png_data(path: &Path, width: u32, height: u32, data: &[u8]) {
        let file = fs::File::create(path).expect("create fixture png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        writer.write_image_data(data).expect("write png image data");
    }

    fn write_png(path: &Path, dim: u32) {
        write_png_data(path, dim, dim, &gradient(dim, dim));
    }

    /// Write a square PNG and return the 16x16 bytes the loader should produce.
    fn authored_layer(dir: &Path, file: &str, dim: u32) -> Vec<u8> {
        write_png(&dir.join(file), dim);
        resample::resample_to_16(&gradient(dim, dim), dim)
    }

    /// Canonical 44-byte-header RIFF/WAVE PCM16 fixture writer.
    fn wav_bytes(channels: u16, sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let block_align = channels * 2;
        let byte_rate = sample_rate * u32::from(block_align);
        let mut out = Vec::with_capacity(44 + data_len as usize);

        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    fn ramp(len: usize) -> Vec<i16> {
        (0..len)
            .map(|i| ((i.wrapping_mul(517) % 20_000) as i16) - 10_000)
            .collect()
    }

    fn assert_procedural_layer(pack: &LoadedPack, procedural: &[Vec<u8>], index: usize) {
        assert_eq!(
            pack.layers[index], procedural[index],
            "layer {index} should keep the procedural texture"
        );
    }

    fn warning_fields(warnings: &[SettingsWarning]) -> Vec<&str> {
        warnings.iter().map(|w| w.field.as_str()).collect()
    }

    #[test]
    fn two_pack_stack_keeps_earlier_layers_and_fields() {
        let root = tempfile::tempdir().unwrap();
        let earlier = pack_dir(&root, "earlier");
        let later = pack_dir(&root, "later");

        fs::write(
            earlier.join("pack.toml"),
            r#"
name = "Pack A"
description = "earlier art"

[menu]
logo = "logo.png"
overlay_tint = [10, 20, 30, 40]
"#,
        )
        .unwrap();
        let stone = authored_layer(&earlier, "stone.png", 32);
        let earlier_dirt = authored_layer(&earlier, "dirt.png", 32);
        let logo_data = gradient(24, 24);
        write_png_data(&earlier.join("logo.png"), 24, 24, &logo_data);

        // The later pack authors exactly one PNG and no presentation fields.
        fs::write(later.join("pack.toml"), "").unwrap();
        let later_dirt = authored_layer(&later, "dirt.png", 64);

        let specs = vec![load_spec_from_dir(&earlier), load_spec_from_dir(&later)];
        assert!(
            specs.iter().all(|spec| spec.warnings.is_empty()),
            "fixtures should be warning-free: {:?}",
            specs.iter().map(|spec| &spec.warnings).collect::<Vec<_>>()
        );

        let pack = resolve_presentation(&specs);
        let procedural = generate_all_texture_layers_rgba();

        assert_eq!(pack.layers.len(), NUM_TEX_LAYERS as usize);
        assert!(
            pack.layers.iter().all(|layer| layer.len() == LAYER_BYTES),
            "every resolved layer stays 16x16 RGBA"
        );

        // The later pack's one file wins; the earlier pack's other file and
        // every presentation field survive.
        assert_eq!(pack.layers[2], later_dirt, "later dirt.png wins");
        assert_ne!(pack.layers[2], earlier_dirt);
        assert_eq!(pack.layers[3], stone, "earlier stone.png survives");

        let logo = pack.logo.as_ref().expect("earlier logo survives");
        assert_eq!((logo.width, logo.height), (24, 24));
        assert_eq!(logo.rgba, logo_data);
        assert_eq!(pack.overlay_tint, Some([10, 20, 30, 40]));
        assert_eq!(pack.description.as_deref(), Some("earlier art"));
        assert_eq!(pack.display_name, "Pack A");

        // Everything else falls back to procedural art because no pack
        // authored it.
        for index in 0..procedural.len() {
            if index == 2 || index == 3 {
                continue;
            }
            assert_procedural_layer(&pack, &procedural, index);
        }
        assert!(pack.warnings.is_empty(), "{:?}", pack.warnings);
    }

    #[test]
    fn later_pack_overrides_only_its_authored_fields() {
        let root = tempfile::tempdir().unwrap();
        let earlier = pack_dir(&root, "earlier");
        let later = pack_dir(&root, "later");

        fs::write(
            earlier.join("pack.toml"),
            "name = \"Pack A\"\ndescription = \"from A\"\n",
        )
        .unwrap();
        let earlier_stone = authored_layer(&earlier, "stone.png", 16);

        fs::write(
            later.join("pack.toml"),
            "name = \"Pack B\"\n[menu]\noverlay_tint = [1, 2, 3, 4]\n",
        )
        .unwrap();
        let later_stone = authored_layer(&later, "stone.png", 32);

        let pack =
            resolve_presentation(&[load_spec_from_dir(&earlier), load_spec_from_dir(&later)]);

        // The jointly authored file and the name come from the later pack.
        assert_eq!(pack.layers[3], later_stone);
        assert_ne!(pack.layers[3], earlier_stone);
        assert_eq!(pack.display_name, "Pack B");
        assert_eq!(pack.overlay_tint, Some([1, 2, 3, 4]));
        // The later pack did not author a description, so A's survives.
        assert_eq!(pack.description.as_deref(), Some("from A"));
    }

    #[test]
    fn stack_order_decides_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let a = pack_dir(&root, "a");
        let b = pack_dir(&root, "b");

        fs::write(a.join("pack.toml"), "name = \"A\"\n").unwrap();
        let a_stone = authored_layer(&a, "stone.png", 16);
        fs::write(b.join("pack.toml"), "name = \"B\"\n").unwrap();
        let b_stone = authored_layer(&b, "stone.png", 32);
        assert_ne!(a_stone, b_stone, "fixtures must be distinguishable");

        let a_spec = load_spec_from_dir(&a);
        let b_spec = load_spec_from_dir(&b);

        let forward = resolve_presentation(&[a_spec.clone(), b_spec.clone()]);
        assert_eq!(forward.layers[3], b_stone);
        assert_eq!(forward.display_name, "B");

        let reversed = resolve_presentation(&[b_spec, a_spec]);
        assert_eq!(reversed.layers[3], a_stone);
        assert_eq!(reversed.display_name, "A");
    }

    #[test]
    fn single_pack_resolution_matches_load_pack_from_dir() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "mixed");

        fs::write(
            dir.join("pack.toml"),
            r#"
name = "Mixed"
description = "mixed fixture"

[menu]
logo = "logo.png"
overlay_tint = [9, 8, 7, 6]
"#,
        )
        .unwrap();
        write_png(&dir.join(LAYER_FILES[0]), 16);
        write_png(&dir.join(LAYER_FILES[7]), 64);
        write_png(&dir.join(MOB_LAYER_FILES[4]), 32);
        write_png_data(&dir.join("logo.png"), 20, 10, &gradient(20, 10));
        // Present but invalid: one warning each, procedural fallback.
        fs::write(dir.join(LAYER_FILES[1]), b"not a png").unwrap();
        write_png(&dir.join(LAYER_FILES[3]), 8);
        write_png_sized(&dir.join(LAYER_FILES[4]), 32, 16);
        fs::write(dir.join(MOB_LAYER_FILES[0]), b"not a png").unwrap();
        // Missing files stay silent.

        let spec = load_spec_from_dir(&dir);
        let resolved = resolve_presentation(&[spec]);
        let direct = crate::pack::load_pack_from_dir(&dir);

        assert_eq!(resolved, direct);
        assert_eq!(
            warning_fields(&resolved.warnings),
            vec![
                LAYER_FILES[1],
                LAYER_FILES[3],
                LAYER_FILES[4],
                MOB_LAYER_FILES[0],
            ],
            "warning order must match v1 (manifest, block, mob, logo)"
        );
    }

    fn write_png_sized(path: &Path, width: u32, height: u32) {
        write_png_data(path, width, height, &gradient(width, height));
    }

    #[test]
    fn missing_stack_member_warns_once_and_keeps_display_name() {
        let root = tempfile::tempdir().unwrap();
        let present = pack_dir(&root, "present");
        fs::write(present.join("pack.toml"), "name = \"Present\"\n").unwrap();
        let stone = authored_layer(&present, "stone.png", 16);

        let stack = vec!["ghost".to_string(), "present".to_string()];
        let (pack, specs, warnings) = load_stack(root.path(), &stack);

        assert_eq!(specs.len(), 1, "only the present folder loads");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warning_fields(&warnings), vec!["ghost"]);
        assert!(
            warnings[0].reason.contains("missing"),
            "reason names the missing folder: {}",
            warnings[0].reason
        );
        assert_eq!(pack.warnings, warnings);
        assert_eq!(pack.layers[3], stone);
        assert_eq!(pack.display_name, "Present");
        assert_eq!(
            stack,
            vec!["ghost".to_string(), "present".to_string()],
            "load_stack never mutates the caller's stack"
        );
    }

    #[test]
    fn all_missing_stack_keeps_last_name_and_procedural_layers() {
        let root = tempfile::tempdir().unwrap();
        let stack = vec!["ghost".to_string(), "spirit".to_string()];

        let (pack, specs, warnings) = load_stack(root.path(), &stack);

        assert!(specs.is_empty());
        assert_eq!(warning_fields(&warnings), vec!["ghost", "spirit"]);
        assert_eq!(pack.layers, generate_all_texture_layers_rgba());
        assert_eq!(pack.display_name, "spirit");
        assert_eq!(pack.logo, None);
        assert_eq!(pack.overlay_tint, None);
        assert_eq!(pack.description, None);
    }

    #[test]
    fn sound_spec_merges_per_slot() {
        let root = tempfile::tempdir().unwrap();
        let earlier = pack_dir(&root, "earlier");
        let later = pack_dir(&root, "later");
        fs::write(earlier.join("pack.toml"), "").unwrap();
        fs::write(later.join("pack.toml"), "").unwrap();

        let earlier_break = ramp(64);
        fs::write(
            earlier.join("break.wav"),
            wav_bytes(1, 22_050, &earlier_break),
        )
        .unwrap();
        fs::write(earlier.join("ui_click.wav"), b"not audio").unwrap();

        let later_place = ramp(128);
        fs::write(later.join("place.wav"), wav_bytes(2, 44_100, &later_place)).unwrap();
        fs::write(later.join("arrow_impact.wav"), b"not audio").unwrap();

        let specs = vec![load_spec_from_dir(&earlier), load_spec_from_dir(&later)];
        let (buffers, warnings) = sound::resolve_sounds(&specs);
        let defaults = default_buffers();

        assert_eq!(
            warning_fields(&warnings),
            vec!["ui_click.wav", "arrow_impact.wav"],
            "warnings concatenate in stack order"
        );

        // Earlier pack's authored slot survives the later pack's silence.
        assert_eq!(buffers[0].channels(), 1);
        assert_eq!(buffers[0].sample_rate(), 22_050);
        assert_eq!(buffers[0].samples().len(), earlier_break.len());
        assert_ne!(buffers[0], defaults[0]);

        // Later pack's authored slot wins.
        assert_eq!(buffers[1].channels(), 2);
        assert_eq!(buffers[1].sample_rate(), 44_100);
        assert_eq!(buffers[1].samples().len(), later_place.len());
        assert_ne!(buffers[1], defaults[1]);

        // The remaining seven slots keep the synthesized defaults; the two
        // corrupt files fall back rather than authoring anything.
        for slot in crate::audio::SoundSlot::ALL {
            use crate::audio::SoundSlot;
            if matches!(slot, SoundSlot::Break | SoundSlot::Place) {
                continue;
            }
            assert_eq!(buffers[slot.index()], defaults[slot.index()]);
        }
    }

    #[test]
    fn empty_stack_is_procedural_with_no_warnings() {
        let root = tempfile::tempdir().unwrap();

        let (pack, specs, warnings) = load_stack(root.path(), &[]);

        assert!(specs.is_empty());
        assert!(warnings.is_empty());
        assert_eq!(pack.layers, generate_all_texture_layers_rgba());
        assert_eq!(pack.display_name, "Unnamed Pack");
        assert_eq!(pack.logo, None);
        assert_eq!(pack.overlay_tint, None);
        assert_eq!(pack.description, None);
        assert!(pack.warnings.is_empty());
    }

    #[test]
    fn v2_fixture_loads_content_assets_and_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "ruby");

        fs::write(
            dir.join("pack.toml"),
            r#"
format_version = 2
name = "Ruby Mod"
version = "1.0.0"

[[blocks]]
id = "mymod:ruby_ore"
textures = { all = "ruby_ore.png" }
hardness = 3.0
drop = "mymod:ruby"

[[blocks]]
textures = { all = "missing_id.png" }
hardness = 1.0

[[items]]
id = "mymod:ruby"
icon = "ruby_ore.png"
"#,
        )
        .unwrap();
        write_png(&dir.join("ruby_ore.png"), 32);

        let spec = load_spec_from_dir(&dir);

        assert_eq!(spec.format_version, crate::pack::manifest::PackFormat::V2);
        assert_eq!(spec.version.as_deref(), Some("1.0.0"));
        assert_eq!(spec.content.blocks.len(), 1);
        assert_eq!(spec.content.blocks[0].id, "mymod:ruby_ore");
        assert_eq!(spec.content.items.len(), 1);
        assert_eq!(spec.content.assets.len(), 1);
        assert_eq!(
            spec.content.assets["ruby_ore.png"].rgba,
            resample::resample_to_16(&gradient(32, 32), 32)
        );

        assert_eq!(spec.diagnostics.len(), 1);
        assert!(spec.diagnostics[0]
            .reason
            .contains("missing required field `id`"));
        assert!(spec.warnings.is_empty(), "{:?}", spec.warnings);
    }

    #[test]
    fn v1_fixture_keeps_content_empty_and_adds_no_warnings() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "plain");
        fs::write(dir.join("pack.toml"), "name = \"Plain\"\n").unwrap();
        write_png(&dir.join("stone.png"), 16);

        let spec = load_spec_from_dir(&dir);

        assert_eq!(spec.format_version, crate::pack::manifest::PackFormat::V1);
        assert_eq!(
            spec.content,
            crate::content::spec::PackContentSpec::default()
        );
        assert!(spec.diagnostics.is_empty());
        assert!(spec.warnings.is_empty(), "{:?}", spec.warnings);
        assert!(spec.block_layers[3].is_some());
    }

    #[test]
    fn v1_content_array_warns_once_and_keeps_textures() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "legacy");
        fs::write(
            dir.join("pack.toml"),
            r#"
name = "Legacy"

[[blocks]]
id = "mymod:ruby"
textures = { all = "ruby.png" }
hardness = 1.0
"#,
        )
        .unwrap();
        write_png(&dir.join("stone.png"), 16);

        let spec = load_spec_from_dir(&dir);

        assert_eq!(spec.format_version, crate::pack::manifest::PackFormat::V1);
        assert!(spec.content.blocks.is_empty());
        assert!(spec.diagnostics.is_empty());
        assert!(spec.block_layers[3].is_some());
        assert_eq!(spec.warnings.len(), 1);
        assert_eq!(spec.warnings[0].field, "pack.toml");
        assert!(spec.warnings[0]
            .reason
            .contains("content arrays require format_version = 2"));
    }

    #[test]
    fn logo_path_escape_warns_and_never_reads_outside() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "escape");
        fs::write(dir.join("pack.toml"), "[menu]\nlogo = \"../outside.png\"\n").unwrap();
        write_png(&root.path().join("outside.png"), 16);

        let spec = load_spec_from_dir(&dir);

        assert_eq!(spec.logo, None);
        assert_eq!(spec.warnings.len(), 1);
        assert_eq!(spec.warnings[0].field, "../outside.png");
        assert!(spec.warnings[0].reason.contains("must not contain `..`"));
    }

    #[test]
    fn future_format_warns_once_and_still_loads_textures() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "future");
        fs::write(
            dir.join("pack.toml"),
            r#"
format_version = 3
name = "From The Future"

[[blocks]]
id = "mymod:ruby"
textures = { all = "ruby.png" }
hardness = 1.0
"#,
        )
        .unwrap();
        write_png(&dir.join("stone.png"), 16);

        let spec = load_spec_from_dir(&dir);

        assert_eq!(
            spec.format_version,
            crate::pack::manifest::PackFormat::Future(3)
        );
        assert!(spec.content.blocks.is_empty());
        assert!(spec.diagnostics.is_empty());
        assert!(
            spec.block_layers[3].is_some(),
            "v1 texture loading continues"
        );
        assert_eq!(spec.warnings.len(), 1);
        assert!(spec.warnings[0].reason.contains("newer than 2"));
    }
}
