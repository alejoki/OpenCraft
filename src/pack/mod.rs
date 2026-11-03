//! GPU-free texture pack loading core.
//!
//! Everything in this module is pure data handling: no wgpu or glyphon
//! imports may appear here so the whole surface is testable headlessly.
//!
//! The [`LoadedPack`] contract is the boundary later slices consume: it always
//! contains exactly [`crate::render::texture::NUM_TEX_LAYERS`] resampled 16x16
//! RGBA layers plus optional presentation data. The first
//! [`AUTHORABLE_TEX_LAYERS`] layers are block art from [`LAYER_FILES`], layers
//! 25..=31 are reserved block placeholders, layers 32..=44 are mob part art
//! from [`MOB_LAYER_FILES`], and layers 45..=47 are reserved spares. Loading
//! never fails and never panics on bad input; every fallback is reported as a
//! structured [`SettingsWarning`].

use std::path::Path;

use crate::render::texture::{generate_all_texture_layers_rgba, MOB_AUTHORABLE_TEX_LAYERS};
use crate::settings::SettingsWarning;

pub mod asset;
pub mod decode;
pub mod manifest;
pub mod resample;
pub mod sound;
pub mod spec;
pub mod v2;

/// Load an ordered stack of pack folders under `packs_root`.
pub use spec::load_stack;

/// Number of block texture layers a pack may author.
///
/// Index-aligned with the first [`AUTHORABLE_TEX_LAYERS`] entries of
/// `BlockId::face_tex_layer`/the procedural layer set. Layers
/// [`AUTHORABLE_TEX_LAYERS`]..[`crate::render::texture::MOB_LAYER_BASE`] are
/// reserved block placeholders that cannot be overridden.
pub const AUTHORABLE_TEX_LAYERS: usize = 25;

/// File names for the 25 authorable block texture layers, index-aligned with
/// `BlockId::face_tex_layer` (layers 0..25). These names are the authoring
/// contract for packs; do not reorder.
pub const LAYER_FILES: [&str; AUTHORABLE_TEX_LAYERS] = [
    "grass_top.png",           // 0
    "grass_side.png",          // 1
    "dirt.png",                // 2
    "stone.png",               // 3
    "cobblestone.png",         // 4
    "sand.png",                // 5
    "log_top.png",             // 6
    "log_side.png",            // 7
    "leaves.png",              // 8
    "planks.png",              // 9
    "brick.png",               // 10
    "snow_top.png",            // 11
    "bedrock.png",             // 12
    "snow_side.png",           // 13
    "coal_ore.png",            // 14
    "iron_ore.png",            // 15
    "farmland.png",            // 16
    "wheat_stage0.png",        // 17
    "wheat_stage1.png",        // 18
    "wheat_stage2.png",        // 19
    "wheat_stage3.png",        // 20
    "chest.png",               // 21
    "loot_chest.png",          // 22
    "crafting_table_top.png",  // 23
    "crafting_table_side.png", // 24
];

/// File names for the 13 authorable mob part layers, index-aligned with
/// array layers [`crate::render::texture::MOB_LAYER_BASE`]..=44. These names
/// are the authoring contract for packs; do not reorder.
pub const MOB_LAYER_FILES: [&str; MOB_AUTHORABLE_TEX_LAYERS] = [
    "pig_body.png",      // 32
    "pig_head.png",      // 33
    "pig_leg.png",       // 34
    "cow_body.png",      // 35
    "cow_head.png",      // 36
    "cow_leg.png",       // 37
    "zombie_body.png",   // 38
    "zombie_head.png",   // 39
    "zombie_leg.png",    // 40
    "skeleton_body.png", // 41
    "skeleton_head.png", // 42
    "skeleton_leg.png",  // 43
    "arrow.png",         // 44
];

/// Directory-name convention for packs, resolved relative to the process
/// working directory (same convention as `saves/` and `settings.toml`).
pub const PACKS_DIR: &str = "packs";

/// A pack logo decoded at its native size (never resampled).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackLogo {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Everything a loaded pack provides, always in a usable state.
///
/// `layers` is always exactly [`crate::render::texture::NUM_TEX_LAYERS`]
/// entries of 16x16 RGBA bytes (1024 bytes each), the same format
/// [`crate::render::texture::create_block_texture_array`] consumes. Layers
/// 0..24 come from [`LAYER_FILES`], 25..31 are reserved block placeholders,
/// 32..44 come from [`MOB_LAYER_FILES`], and 45..47 are reserved spares.
/// Missing, invalid, and out-of-range files fall back per layer to the
/// procedural texture and appear in `warnings`.
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
/// warning), then resolves each of [`LAYER_FILES`] and [`MOB_LAYER_FILES`]. A
/// layer file that does not exist is silently replaced by its procedural
/// layer; a file that exists but fails to decode, is non-square, or is outside
/// 16..=256 pixels falls back the same way and emits one warning naming the
/// bare file. Reserved layers 25..=31 and 45..=47 have no file name and are
/// never overridable, so a legacy pack that ships only the original 16 block
/// files simply keeps the built-in art for layers 16..24 and all mob art. The
/// optional logo is decoded at native size (failure -> `None` plus a warning).
/// Unknown files and unknown manifest keys are ignored. Never panics, never
/// returns `Err`.
pub fn load_pack_from_dir(dir: &Path) -> LoadedPack {
    spec::resolve_presentation(&[spec::load_spec_from_dir(dir)])
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
/// it yields all [`crate::render::texture::NUM_TEX_LAYERS`] procedural layers,
/// no presentation data, and exactly one warning naming the pack. Never
/// crashes.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::texture::MOB_LAYER_BASE;
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

        // Contract: always 48 complete 16x16 RGBA layers.
        assert_eq!(
            pack.layers.len(),
            crate::render::texture::NUM_TEX_LAYERS as usize
        );
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
        assert_eq!(
            pack.layers.len(),
            crate::render::texture::NUM_TEX_LAYERS as usize
        );
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
        assert_eq!(
            pack.layers.len(),
            crate::render::texture::NUM_TEX_LAYERS as usize
        );
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
    fn layer_file_names_cover_all_block_layers() {
        assert_eq!(LAYER_FILES.len(), 25);
        assert_eq!(LAYER_FILES.len(), AUTHORABLE_TEX_LAYERS);
        assert_eq!(AUTHORABLE_TEX_LAYERS, 25);
        assert_eq!(
            &LAYER_FILES[16..],
            &[
                "farmland.png",
                "wheat_stage0.png",
                "wheat_stage1.png",
                "wheat_stage2.png",
                "wheat_stage3.png",
                "chest.png",
                "loot_chest.png",
                "crafting_table_top.png",
                "crafting_table_side.png",
            ]
        );
        // The original 16 names keep their indices.
        assert_eq!(LAYER_FILES[0], "grass_top.png");
        assert_eq!(LAYER_FILES[15], "iron_ore.png");
        // The two table layers are the only appended names.
        assert_eq!(LAYER_FILES[22], "loot_chest.png");
        assert_eq!(LAYER_FILES[23], "crafting_table_top.png");
        assert_eq!(LAYER_FILES[24], "crafting_table_side.png");
    }

    #[test]
    fn legacy_sixteen_file_pack_keeps_new_and_reserved_layers_built_in() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "legacy16");
        read_pack_toml(&dir, "name = \"Legacy 16\"\n");
        for file in LAYER_FILES.iter().take(16) {
            write_png(&dir.join(file), 16);
        }

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();
        let expected = resample::resample_to_16(&gradient(16, 16), 16);

        assert_eq!(
            pack.layers.len(),
            crate::render::texture::NUM_TEX_LAYERS as usize
        );

        // The 16 legacy names still override exactly layers 0..=15.
        for (index, layer) in pack.layers.iter().enumerate().take(16) {
            assert_eq!(*layer, expected, "layer {index} override");
            assert_ne!(*layer, procedural[index]);
        }

        // Layers 16..25 (new authored names), the reserved ranges, and all
        // mob art keep the built-in pixels: a legacy pack needs no changes.
        for index in 16..AUTHORABLE_TEX_LAYERS {
            assert_procedural_layer(&pack, &procedural, index);
        }
        for index in (AUTHORABLE_TEX_LAYERS..MOB_LAYER_BASE as usize).chain(
            crate::render::texture::MOB_SPARE_LAYER_BASE as usize
                ..crate::render::texture::NUM_TEX_LAYERS as usize,
        ) {
            assert_procedural_layer(&pack, &procedural, index);
        }
        for offset in 0..MOB_LAYER_FILES.len() {
            assert_procedural_layer(&pack, &procedural, MOB_LAYER_BASE as usize + offset);
        }

        assert!(
            pack.warnings.is_empty(),
            "legacy pack must load warning-free: {:?}",
            pack.warnings
        );
    }

    #[test]
    fn authoring_all_twenty_five_names_overrides_every_authorable_layer() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "full25");
        read_pack_toml(&dir, "name = \"Full 25\"\n");
        for file in LAYER_FILES.iter() {
            write_png(&dir.join(file), 32);
        }

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();
        let expected = resample::resample_to_16(&gradient(32, 32), 32);

        assert_eq!(
            pack.layers.len(),
            crate::render::texture::NUM_TEX_LAYERS as usize
        );
        for (index, _) in LAYER_FILES.iter().enumerate() {
            assert_eq!(pack.layers[index], expected, "layer {index} resample");
            assert_ne!(
                pack.layers[index], procedural[index],
                "layer {index} must override the procedural texture"
            );
        }
        // Reserved block/spare layers are never authorable, even in a full
        // block pack; mob layers are untouched because no mob file exists.
        for index in (AUTHORABLE_TEX_LAYERS..MOB_LAYER_BASE as usize).chain(
            crate::render::texture::MOB_SPARE_LAYER_BASE as usize
                ..crate::render::texture::NUM_TEX_LAYERS as usize,
        ) {
            assert_procedural_layer(&pack, &procedural, index);
        }
        assert!(
            pack.warnings.is_empty(),
            "a fully authored pack has no warnings: {:?}",
            pack.warnings
        );
    }

    #[test]
    fn reserved_layer_file_names_are_ignored_silently() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "reserved-names");
        read_pack_toml(&dir, "name = \"Reserved\"\n");
        // Neither the first nor the last reserved layer can be overridden,
        // and the spare block range stays non-authorable too.
        for file in ["reserved_25.png", "reserved_31.png", "reserved_45.png"] {
            write_png(&dir.join(file), 16);
        }

        let pack = load_pack_from_dir(&dir);

        assert_eq!(pack.layers, procedural_layers());
        assert!(
            pack.warnings.is_empty(),
            "unknown/reserved file names never warn: {:?}",
            pack.warnings
        );
    }

    #[test]
    fn invalid_new_layer_warns_with_bare_file_name_and_falls_back() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "bad-new-layer");
        read_pack_toml(&dir, "name = \"Bad New Layer\"\n");
        fs::write(dir.join(LAYER_FILES[16]), b"not a png").unwrap();

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        assert_eq!(
            pack.layers.len(),
            crate::render::texture::NUM_TEX_LAYERS as usize
        );
        assert_procedural_layer(&pack, &procedural, 16);
        assert_eq!(warning_fields(&pack), vec![LAYER_FILES[16]]);
        // Only the failing new layer warns; every other new name is absent and
        // falls back silently.
        assert_eq!(pack.layers[17], procedural[17]);
        assert_eq!(pack.layers[24], procedural[24]);
    }

    #[test]
    fn crafting_table_files_override_only_their_layers() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "crafting-table");
        read_pack_toml(&dir, "name = \"Crafting Table\"\n");
        write_png(&dir.join("crafting_table_top.png"), 32);
        write_png(&dir.join("crafting_table_side.png"), 64);

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        assert_eq!(
            pack.layers[23],
            resample::resample_to_16(&gradient(32, 32), 32)
        );
        assert_eq!(
            pack.layers[24],
            resample::resample_to_16(&gradient(64, 64), 64)
        );
        assert_ne!(pack.layers[23], procedural[23]);
        assert_ne!(pack.layers[24], procedural[24]);
        assert_ne!(pack.layers[23], pack.layers[24]);

        // Only the two new names change: 0..=22 and 25..=47 stay built-in.
        for index in (0..(AUTHORABLE_TEX_LAYERS - 2))
            .chain(AUTHORABLE_TEX_LAYERS..crate::render::texture::NUM_TEX_LAYERS as usize)
        {
            assert_procedural_layer(&pack, &procedural, index);
        }
        assert!(pack.warnings.is_empty(), "{:?}", pack.warnings);
    }

    #[test]
    fn missing_crafting_table_files_keep_procedural_art_silently() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "no-table-art");
        read_pack_toml(&dir, "name = \"No Table Art\"\n");

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        assert_procedural_layer(&pack, &procedural, 23);
        assert_procedural_layer(&pack, &procedural, 24);
        assert!(
            pack.warnings.is_empty(),
            "missing table files are silent: {:?}",
            pack.warnings
        );
    }

    #[test]
    fn corrupt_crafting_table_files_warn_once_with_bare_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "bad-table-art");
        read_pack_toml(&dir, "name = \"Bad Table Art\"\n");
        fs::write(dir.join("crafting_table_top.png"), b"not a png").unwrap();
        fs::write(dir.join("crafting_table_side.png"), b"still not a png").unwrap();

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        assert_procedural_layer(&pack, &procedural, 23);
        assert_procedural_layer(&pack, &procedural, 24);
        // Exactly one warning per corrupt file, naming the bare file name.
        assert_eq!(
            warning_fields(&pack),
            vec!["crafting_table_top.png", "crafting_table_side.png"]
        );
    }

    #[test]
    fn mob_layer_files_are_index_aligned() {
        use crate::render::texture::{
            MOB_ARROW_TEX_LAYER, MOB_LAYER_BASE, MOB_SPARE_LAYER_BASE, NUM_TEX_LAYERS,
        };

        assert_eq!(MOB_LAYER_FILES.len(), MOB_AUTHORABLE_TEX_LAYERS);
        assert_eq!(MOB_AUTHORABLE_TEX_LAYERS, 13);
        assert_eq!(MOB_LAYER_BASE, 32);
        assert_eq!(MOB_ARROW_TEX_LAYER, 44);
        assert_eq!(MOB_SPARE_LAYER_BASE, 45);
        assert_eq!(
            MOB_LAYER_BASE as usize + MOB_LAYER_FILES.len(),
            MOB_ARROW_TEX_LAYER as usize + 1
        );

        // The first 23 block names stay byte-for-byte unchanged; the two
        // table layers extend the contract at 23 and 24.
        assert_eq!(AUTHORABLE_TEX_LAYERS, 25);
        assert_eq!(LAYER_FILES[0], "grass_top.png");
        assert_eq!(LAYER_FILES[22], "loot_chest.png");
        assert_eq!(LAYER_FILES[23], "crafting_table_top.png");
        assert_eq!(LAYER_FILES[24], "crafting_table_side.png");

        assert_eq!(MOB_LAYER_FILES[0], "pig_body.png");
        assert_eq!(MOB_LAYER_FILES[12], "arrow.png");

        // 25 block + 7 reserved + 13 mob + 3 spare = 48.
        assert_eq!(
            NUM_TEX_LAYERS as usize,
            AUTHORABLE_TEX_LAYERS + 7 + MOB_AUTHORABLE_TEX_LAYERS + 3
        );
    }

    #[test]
    fn mob_pack_overrides_each_part_per_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "mob-parts");
        read_pack_toml(&dir, "name = \"Mob Parts\"\n");

        // pig_body -> 32, cow_head -> 36, skeleton_leg -> 43.
        let overrides: [(usize, u32); 3] = [(0, 32), (4, 64), (11, 32)];
        for (offset, dim) in overrides {
            write_png(&dir.join(MOB_LAYER_FILES[offset]), dim);
        }

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();
        let base = MOB_LAYER_BASE as usize;

        for (offset, dim) in overrides {
            let layer = base + offset;
            let expected = resample::resample_to_16(&gradient(dim, dim), dim);
            assert_eq!(pack.layers[layer], expected, "mob layer {layer} resample");
            assert_ne!(
                pack.layers[layer], procedural[layer],
                "mob layer {layer} must override the procedural texture"
            );
        }

        // Index pins from the authoring contract.
        assert_eq!(
            pack.layers[32],
            resample::resample_to_16(&gradient(32, 32), 32)
        );
        assert_eq!(
            pack.layers[36],
            resample::resample_to_16(&gradient(64, 64), 64)
        );
        assert_eq!(
            pack.layers[43],
            resample::resample_to_16(&gradient(32, 32), 32)
        );

        // Untouched siblings and all reserved ranges keep procedural art.
        for offset in 0..MOB_LAYER_FILES.len() {
            if ![0usize, 4, 11].contains(&offset) {
                assert_procedural_layer(&pack, &procedural, base + offset);
            }
        }
        for index in (AUTHORABLE_TEX_LAYERS..base).chain(
            crate::render::texture::MOB_SPARE_LAYER_BASE as usize
                ..crate::render::texture::NUM_TEX_LAYERS as usize,
        ) {
            assert_procedural_layer(&pack, &procedural, index);
        }
        assert!(pack.warnings.is_empty(), "{:?}", pack.warnings);
    }

    #[test]
    fn missing_mob_file_falls_back_silently() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "no-mob-art");
        read_pack_toml(&dir, "name = \"No Mob Art\"\n");
        // One block file proves the loader ran; every mob file is absent.
        write_png(&dir.join(LAYER_FILES[0]), 16);

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();

        for offset in 0..MOB_LAYER_FILES.len() {
            assert_procedural_layer(&pack, &procedural, MOB_LAYER_BASE as usize + offset);
        }
        assert!(
            pack.warnings.is_empty(),
            "missing mob files are silent: {:?}",
            pack.warnings
        );
    }

    #[test]
    fn corrupt_mob_file_warns_with_bare_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "bad-mob");
        read_pack_toml(&dir, "name = \"Bad Mob\"\n");
        fs::write(dir.join(MOB_LAYER_FILES[4]), b"not a png").unwrap();

        let pack = load_pack_from_dir(&dir);
        let procedural = procedural_layers();
        let base = MOB_LAYER_BASE as usize;

        assert_procedural_layer(&pack, &procedural, base + 4);
        assert_eq!(warning_fields(&pack), vec![MOB_LAYER_FILES[4]]);
        assert!(
            pack.warnings[0].reason.contains("png") || pack.warnings[0].reason.contains("decode"),
            "reason should describe the decode failure: {}",
            pack.warnings[0].reason
        );

        // Sibling mob parts stay procedural with no extra warnings.
        assert_procedural_layer(&pack, &procedural, base + 3);
        assert_procedural_layer(&pack, &procedural, base + 5);
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
