//! GPU-free texture pack loading core.
//!
//! Everything in this module is pure data handling: no wgpu or glyphon
//! imports may appear here so the whole surface is testable headlessly.
//!
//! The [`LoadedPack`] contract is the boundary later slices consume: it always
//! contains exactly [`LAYER_FILES`] resampled 16x16 RGBA layers plus optional
//! presentation data. Loading never fails and never panics on bad input; every
//! fallback is reported as a structured [`SettingsWarning`].

use std::path::Path;

use crate::render::texture::generate_all_texture_layers_rgba;
use crate::settings::SettingsWarning;

pub mod decode;
pub mod manifest;
pub mod resample;
pub mod sound;

/// File names for the 16 block texture layers, index-aligned with
/// `BlockId::face_tex_layer` (layers 0..16). These names are the authoring
/// contract for packs; do not reorder.
pub const LAYER_FILES: [&str; 16] = [
    "grass_top.png",   // 0
    "grass_side.png",  // 1
    "dirt.png",        // 2
    "stone.png",       // 3
    "cobblestone.png", // 4
    "sand.png",        // 5
    "log_top.png",     // 6
    "log_side.png",    // 7
    "leaves.png",      // 8
    "planks.png",      // 9
    "brick.png",       // 10
    "snow_top.png",    // 11
    "bedrock.png",     // 12
    "snow_side.png",   // 13
    "coal_ore.png",    // 14
    "iron_ore.png",    // 15
];

/// Directory-name convention for packs, resolved relative to the process
/// working directory (same convention as `saves/` and `settings.toml`).
pub const PACKS_DIR: &str = "packs";

/// Smallest accepted layer side length, in pixels.
const MIN_LAYER_SIDE: u32 = 16;
/// Largest accepted layer side length, in pixels.
const MAX_LAYER_SIDE: u32 = 256;

/// A pack logo decoded at its native size (never resampled).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackLogo {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Everything a loaded pack provides, always in a usable state.
///
/// `layers` is always exactly 16 entries of 16x16 RGBA bytes (1024 bytes each),
/// the same format [`crate::render::texture::create_block_texture_array`]
/// consumes. Missing, invalid, and out-of-range files fall back per layer to
/// the procedural texture and appear in `warnings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedPack {
    pub layers: Vec<Vec<u8>>,
    pub logo: Option<PackLogo>,
    pub overlay_tint: Option<[u8; 4]>,
    pub description: Option<String>,
    pub display_name: String,
    pub warnings: Vec<SettingsWarning>,
}

/// Load a pack directory, applying per-field and per-file fallback.
///
/// Reads `pack.toml` (missing or unreadable -> default manifest plus one
/// warning), then resolves each of [`LAYER_FILES`]. A layer file that does not
/// exist is silently replaced by its procedural layer; a file that exists but
/// fails to decode, is non-square, or is outside 16..=256 pixels falls back the
/// same way and emits one warning naming the bare file. The optional logo is
/// decoded at native size (failure -> `None` plus a warning). Unknown files and
/// unknown manifest keys are ignored. Never panics, never returns `Err`.
pub fn load_pack_from_dir(dir: &Path) -> LoadedPack {
    let mut warnings = Vec::new();

    let manifest_path = dir.join("pack.toml");
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => {
            let (manifest, mut manifest_warnings) = manifest::parse_manifest(&text);
            warnings.append(&mut manifest_warnings);
            manifest
        }
        Err(err) => {
            warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!("failed to read {}: {err}", manifest_path.display()),
            });
            manifest::PackManifest::default()
        }
    };

    let layers = load_layers(dir, &mut warnings);
    let logo = load_logo(dir, manifest.logo.as_deref(), &mut warnings);

    let display_name = manifest
        .name
        .clone()
        .or_else(|| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Unnamed Pack".to_string());

    LoadedPack {
        layers,
        logo,
        overlay_tint: manifest.overlay_tint,
        description: manifest.description,
        display_name,
        warnings,
    }
}

/// Sorted names of the pack directories directly under `packs_root`.
///
/// Only subdirectories count as packs; regular files (and symlinks that do
/// not resolve to a directory) are skipped. A missing or unreadable root is
/// not an error: a fresh install simply has no packs, so the result is empty.
/// Names that are not valid UTF-8 cannot be addressed as a setting and are
/// skipped as well.
pub fn available_pack_names(packs_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(packs_root) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Load `packs_root/name`, treating a missing pack folder as an empty pack.
///
/// A missing (or non-directory) pack folder is the settings-deletion scenario:
/// it yields 16 procedural layers, no presentation data, and exactly one
/// warning naming the pack. Never crashes.
pub fn load_pack_by_name(packs_root: &Path, name: &str) -> LoadedPack {
    let dir = packs_root.join(name);
    if !dir.is_dir() {
        return LoadedPack {
            layers: generate_all_texture_layers_rgba(),
            logo: None,
            overlay_tint: None,
            description: None,
            display_name: name.to_string(),
            warnings: vec![SettingsWarning {
                field: name.to_string(),
                reason: format!("pack folder {} is missing", dir.display()),
            }],
        };
    }

    load_pack_from_dir(&dir)
}

/// Resolve every layer index, keeping the procedural layer on any failure.
fn load_layers(dir: &Path, warnings: &mut Vec<SettingsWarning>) -> Vec<Vec<u8>> {
    let mut layers = generate_all_texture_layers_rgba();

    for (index, file) in LAYER_FILES.iter().enumerate() {
        let path = dir.join(file);
        if !path.exists() {
            // Missing layer files are legal: use the procedural texture silently.
            continue;
        }

        let decoded = match decode::decode_png_rgba(&path) {
            Ok(decoded) => decoded,
            Err(err) => {
                warnings.push(SettingsWarning {
                    field: (*file).to_string(),
                    reason: err,
                });
                continue;
            }
        };

        if decoded.width != decoded.height {
            warnings.push(SettingsWarning {
                field: (*file).to_string(),
                reason: format!(
                    "image is {}x{}, layers must be square",
                    decoded.width, decoded.height
                ),
            });
            continue;
        }

        if decoded.width < MIN_LAYER_SIDE || decoded.width > MAX_LAYER_SIDE {
            warnings.push(SettingsWarning {
                field: (*file).to_string(),
                reason: format!(
                    "image is {}x{}, layers must be between {} and {} pixels per side",
                    decoded.width, decoded.height, MIN_LAYER_SIDE, MAX_LAYER_SIDE
                ),
            });
            continue;
        }

        layers[index] = resample::resample_to_16(&decoded.rgba, decoded.width);
    }

    layers
}

/// Decode the optional manifest logo at native size, warning on failure.
fn load_logo(
    dir: &Path,
    logo: Option<&str>,
    warnings: &mut Vec<SettingsWarning>,
) -> Option<PackLogo> {
    let logo = logo?;
    let path = dir.join(logo);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    const LAYER_BYTES: usize = 16 * 16 * 4;

    fn procedural_layers() -> Vec<Vec<u8>> {
        generate_all_texture_layers_rgba()
    }

    fn pack_dir(root: &tempfile::TempDir, name: &str) -> PathBuf {
        let dir = root.path().join(name);
        fs::create_dir_all(&dir).expect("create pack dir");
        dir
    }

    fn write_png(path: &Path, dim: u32) {
        write_png_sized(path, dim, dim);
    }

    /// Write a deterministic RGBA gradient PNG of the given size.
    fn write_png_sized(path: &Path, width: u32, height: u32) {
        write_png_data(path, width, height, &gradient(width, height));
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

    fn read_pack_toml(dir: &Path, text: &str) {
        fs::write(dir.join("pack.toml"), text).expect("write pack.toml");
    }

    fn warning_fields(pack: &LoadedPack) -> Vec<&str> {
        pack.warnings.iter().map(|w| w.field.as_str()).collect()
    }

    fn assert_procedural_layer(pack: &LoadedPack, procedural: &[Vec<u8>], index: usize) {
        assert_eq!(
            pack.layers[index], procedural[index],
            "layer {index} should fall back to the procedural texture"
        );
    }

    #[test]
    fn full_pack_resolves_layers_logo_and_presentation_data() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "mixed");

        read_pack_toml(
            &dir,
            r#"
name = "Test Pack"
description = "A pack authored in a headless test"
unknown_key = "ignored"

[menu]
logo = "logo.png"
overlay_tint = [10, 20, 30, 40]

[unknown_table]
whatever = true
"#,
        );

        // Valid layers at mixed sizes: 16x16 identity, 64x64 and 32x32 PoT,
        // 24x24 non-PoT, 256x256 upper bound.
        let valid: [(usize, u32); 5] = [(0, 16), (2, 64), (7, 24), (8, 32), (9, 256)];
        for (index, dim) in valid {
            write_png(&dir.join(LAYER_FILES[index]), dim);
        }

        // Failure cases: corrupt, undersized, oversized, non-square.
        fs::write(dir.join(LAYER_FILES[1]), b"not a png at all").unwrap();
        write_png(&dir.join(LAYER_FILES[3]), 8);
        write_png(&dir.join(LAYER_FILES[4]), 512);
        write_png_sized(&dir.join(LAYER_FILES[5]), 32, 16);

        // LAYER_FILES[6] (log_top.png) is intentionally absent.

        // Unknown file in the pack directory is silently ignored.
        write_png(&dir.join("mystery.png"), 16);

        // Logo at native size.
        let logo_data = gradient(32, 32);
        write_png_data(&dir.join("logo.png"), 32, 32, &logo_data);

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        // Contract: always 16 complete 16x16 RGBA layers.
        assert_eq!(pack.layers.len(), 16);
        for (index, layer) in pack.layers.iter().enumerate() {
            assert_eq!(layer.len(), LAYER_BYTES, "layer {index} size");
        }

        // Valid overrides match resampling of the authored pixels and differ
        // from the procedural fallback at the same index.
        for (index, dim) in valid {
            let expected = resample::resample_to_16(&gradient(dim, dim), dim);
            assert_eq!(pack.layers[index], expected, "layer {index} resample");
            assert_ne!(
                pack.layers[index], procedural[index],
                "layer {index} must override the procedural texture"
            );
        }

        // Every invalid layer equals the procedural layer and produced exactly
        // one warning whose field is the bare file name.
        for index in [1, 3, 4, 5] {
            assert_procedural_layer(&pack, &procedural, index);
        }

        // Missing layer: procedural, no warning.
        assert_procedural_layer(&pack, &procedural, 6);

        assert_eq!(
            warning_fields(&pack),
            vec![
                LAYER_FILES[1],
                LAYER_FILES[3],
                LAYER_FILES[4],
                LAYER_FILES[5],
            ]
        );
        assert!(
            pack.warnings.iter().all(|w| !w.reason.is_empty()),
            "every warning carries a reason"
        );
        for field in warning_fields(&pack) {
            assert_ne!(field, "mystery.png");
            assert_ne!(field, "unknown_key");
        }

        // Presentation data.
        let logo = pack.logo.expect("logo decoded");
        assert_eq!(logo.width, 32);
        assert_eq!(logo.height, 32);
        assert_eq!(logo.rgba, logo_data);
        assert_eq!(pack.overlay_tint, Some([10, 20, 30, 40]));
        assert_eq!(
            pack.description.as_deref(),
            Some("A pack authored in a headless test")
        );
        assert_eq!(pack.display_name, "Test Pack");
    }

    #[test]
    fn missing_manifest_warns_once_and_still_loads() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "manifestless");

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        assert_eq!(pack.warnings.len(), 1);
        assert_eq!(pack.warnings[0].field, "pack.toml");
        assert_eq!(pack.layers, procedural);
        assert_eq!(pack.layers.len(), 16);
        assert!(pack.layers.iter().all(|layer| layer.len() == LAYER_BYTES));
        assert_eq!(pack.logo, None);
        assert_eq!(pack.overlay_tint, None);
        assert_eq!(pack.description, None);
        assert_eq!(pack.display_name, "manifestless");
    }

    #[test]
    fn syntax_corrupt_manifest_warns_once_and_uses_defaults() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "broken-manifest");
        read_pack_toml(&dir, "name = [unterminated\n[[menu");

        let pack = load_pack_from_dir(&dir);

        assert_eq!(pack.warnings.len(), 1);
        assert_eq!(pack.warnings[0].field, "pack.toml");
        assert_eq!(pack.layers, procedural_layers());
        assert_eq!(pack.logo, None);
        assert_eq!(pack.overlay_tint, None);
        assert_eq!(pack.display_name, "broken-manifest");
    }

    #[test]
    fn missing_pack_name_yields_defaults_and_one_warning() {
        let root = tempfile::tempdir().unwrap();

        let pack = load_pack_by_name(root.path(), "ghost");

        assert_eq!(pack.layers, procedural_layers());
        assert_eq!(pack.layers.len(), 16);
        assert!(pack.layers.iter().all(|layer| layer.len() == LAYER_BYTES));
        assert_eq!(pack.logo, None);
        assert_eq!(pack.overlay_tint, None);
        assert_eq!(pack.description, None);
        assert_eq!(pack.display_name, "ghost");
        assert_eq!(pack.warnings.len(), 1);
        assert_eq!(pack.warnings[0].field, "ghost");
        assert!(
            pack.warnings[0].reason.contains("missing"),
            "reason should say the folder is missing: {}",
            pack.warnings[0].reason
        );
    }

    #[test]
    fn load_pack_by_name_delegates_to_from_dir_for_existing_packs() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "real");
        read_pack_toml(&dir, "name = \"Real Pack\"\n");
        write_png(&dir.join(LAYER_FILES[3]), 16);

        let by_name = load_pack_by_name(root.path(), "real");
        let from_dir = load_pack_from_dir(&dir);

        assert_eq!(by_name, from_dir);
        assert_eq!(by_name.display_name, "Real Pack");
        assert!(by_name.warnings.is_empty());
    }

    #[test]
    fn missing_logo_file_warns_and_keeps_none() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "no-logo");
        read_pack_toml(&dir, "[menu]\nlogo = \"absent.png\"\n");

        let pack = load_pack_from_dir(&dir);

        assert_eq!(pack.logo, None);
        assert_eq!(warning_fields(&pack), vec!["absent.png"]);
    }

    #[test]
    fn non_square_logo_keeps_native_size() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "wide-logo");
        read_pack_toml(&dir, "[menu]\nlogo = \"wide.png\"\n");
        write_png_sized(&dir.join("wide.png"), 32, 16);

        let pack = load_pack_from_dir(&dir);

        // Logos keep their native aspect ratio, so a non-square logo is valid.
        let logo = pack.logo.expect("native logo loads");
        assert_eq!((logo.width, logo.height), (32, 16));
        assert_eq!(logo.rgba.len(), 32 * 16 * 4);
        assert!(pack.warnings.is_empty());
    }

    #[test]
    fn available_pack_names_lists_only_directories_in_sorted_order() {
        let root = tempfile::tempdir().unwrap();
        for name in ["zeta", "alpha", "middle"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        // Regular files are not packs.
        fs::write(root.path().join("notes.txt"), b"not a dir").unwrap();
        // A symlink to a file is not a pack either.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.path().join("notes.txt"),
            root.path().join("link-to-file"),
        )
        .unwrap();
        // A symlink that resolves to a directory is addressable like a pack.
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("alpha"), root.path().join("link-to-dir"))
            .unwrap();

        let mut expected = vec![
            "alpha".to_string(),
            "middle".to_string(),
            "zeta".to_string(),
        ];
        #[cfg(unix)]
        {
            expected.push("link-to-dir".to_string());
            expected.sort();
        }

        assert_eq!(available_pack_names(root.path()), expected);
    }

    #[test]
    fn available_pack_names_is_empty_for_a_missing_or_empty_root() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(
            available_pack_names(&root.path().join("no-such-packs-dir")),
            Vec::<String>::new()
        );
        assert!(available_pack_names(root.path()).is_empty());
    }

    #[test]
    fn layer_at_256_boundary_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "boundary");
        read_pack_toml(&dir, "name = \"Boundary\"\n");
        write_png(&dir.join(LAYER_FILES[0]), 256);

        let pack = load_pack_from_dir(&dir);

        assert_eq!(
            pack.layers[0],
            resample::resample_to_16(&gradient(256, 256), 256)
        );
        assert!(pack.warnings.is_empty(), "{:?}", pack.warnings);
    }
}
