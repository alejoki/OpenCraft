//! Loader-synthesized six-face overrides for built-in blocks.
//!
//! Built-in blocks keep their hardcoded [`BlockId::face_tex_layer`] defaults.
//! A pack can now point one of them at a flat per-direction PNG without
//! changing any built-in id or any legacy layer index: a file that is already
//! named in [`LAYER_FILES`] reuses its decoded base layer, while a new file is
//! decoded into [`crate::content::spec::PackContentSpec::assets`] and allocated
//! as a pack layer by the neutral catalog.
//!
//! Resolution is per face with one fixed precedence: the direction suffix,
//! then `_side` for the four sides, then the bare stem. The bare stem is the
//! legacy layer name for most blocks, so a pack that ships only `stone.png`
//! keeps every built-in face unchanged.
//!
//! [`BlockId::face_tex_layer`]: crate::world::block::BlockId::face_tex_layer

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::content::spec::{BuiltinFaceEntry, FaceSource, PackAsset};
use crate::pack::decode::decode_layer_file;
use crate::pack::manifest::PackWarning;
use crate::pack::spec::PackSpec;
use crate::pack::LAYER_FILES;

/// Built-in block short name -> flat file stem it authors.
///
/// `air`, `water` and `socket_marker` are deliberately absent: they are not
/// authored cube art, so a pack cannot point them at a face. Declaration order
/// is fixed so the plan (and its diagnostics) stay deterministic.
pub const BUILTIN_FACE_STEMS: [(&str, &str); 21] = [
    ("grass", "grass"),
    ("dirt", "dirt"),
    ("stone", "stone"),
    ("cobblestone", "cobblestone"),
    ("sand", "sand"),
    ("oak_log", "log"),
    ("oak_leaves", "leaves"),
    ("oak_planks", "planks"),
    ("brick", "brick"),
    ("snow", "snow"),
    ("bedrock", "bedrock"),
    ("coal_ore", "coal_ore"),
    ("iron_ore", "iron_ore"),
    ("crafting_table", "crafting_table"),
    ("farmland", "farmland"),
    ("wheat_stage_0", "wheat_stage0"),
    ("wheat_stage_1", "wheat_stage1"),
    ("wheat_stage_2", "wheat_stage2"),
    ("wheat_stage_3", "wheat_stage3"),
    ("chest", "chest"),
    ("loot_chest", "loot_chest"),
];

/// Number of faces in the frozen direction order.
const FACE_COUNT: usize = 6;

/// Candidate suffixes per face, most specific first.
///
/// Index order is the frozen `0=+X/east, 1=-X/west, 2=+Y/top, 3=-Y/bottom,
/// 4=+Z/south, 5=-Z/north` order shared with the catalog, the registry and
/// `prefab::Direction`. `""` is the bare stem (`stone.png`).
const FACE_CANDIDATES: [&[&str]; FACE_COUNT] = [
    &["_east", "_side", ""],
    &["_west", "_side", ""],
    &["_top", ""],
    &["_bottom", ""],
    &["_south", "_side", ""],
    &["_north", "_side", ""],
];

/// Resolve flat built-in face files in `dir` into `spec`.
///
/// Reads the pack folder once, then for every built-in stem resolves each face
/// through [`FACE_CANDIDATES`]. A present legacy file (one of [`LAYER_FILES`])
/// becomes [`FaceSource::Layer`] without being re-read; a present new file is
/// decoded with the layer rules and inserted into `spec.content.assets` before
/// becoming [`FaceSource::Authored`]. A present-but-invalid new file warns
/// once with its bare file name and falls through to the next candidate; it
/// never aborts the pack. An entry is recorded only when at least one face is
/// authored, so a legacy-only folder leaves the plan (and the digest) empty.
///
/// Unresolved faces stay `None`, which keeps the built-in
/// [`BlockId::face_tex_layer`] default.
///
/// [`BlockId::face_tex_layer`]: crate::world::block::BlockId::face_tex_layer
pub fn apply_builtin_faces(dir: &Path, spec: &mut PackSpec) {
    let names = folder_file_names(dir);
    if names.is_empty() {
        return;
    }

    let mut warnings = Vec::new();
    let mut warned: BTreeSet<String> = BTreeSet::new();

    for (block, stem) in BUILTIN_FACE_STEMS {
        let mut faces: [Option<FaceSource>; FACE_COUNT] = std::array::from_fn(|_| None);
        let mut authored = false;

        for (face, candidates) in FACE_CANDIDATES.iter().enumerate() {
            for suffix in *candidates {
                let file = format!("{stem}{suffix}.png");
                if !names.contains(&file) || warned.contains(&file) {
                    continue;
                }
                match resolve_present_file(dir, &file, &mut spec.content.assets) {
                    FaceFile::Source(source) => {
                        authored |= matches!(source, FaceSource::Authored(_));
                        faces[face] = Some(source);
                        break;
                    }
                    FaceFile::Invalid(reason) => {
                        // Present but unusable: warn once by bare name and
                        // fall through to the next candidate.
                        warned.insert(file.clone());
                        warnings.push(PackWarning {
                            field: file,
                            reason,
                        });
                    }
                    FaceFile::Missing => {}
                }
            }
        }

        if authored {
            spec.content.builtin_faces.push(BuiltinFaceEntry {
                block: block.to_string(),
                faces,
            });
        }
    }

    spec.warnings.append(&mut warnings);
}

/// Names of regular files directly inside `dir`, ignoring non-UTF-8 entries.
///
/// The folder is read exactly once per load; every face candidate is a flat
/// name in the pack root, so no other walk is needed.
fn folder_file_names(dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

/// One present file's resolution outcome.
enum FaceFile {
    /// The file resolved to a face source.
    Source(FaceSource),
    /// The new file is present but invalid; the caller warns and falls
    /// through to the next candidate.
    Invalid(String),
    /// The file vanished between `read_dir` and decoding: treat it as absent.
    Missing,
}

/// Resolve one present file into a face source.
///
/// Legacy layer names never fail here: their decode warning was already
/// emitted by the fixed-layer pass, and the layer index is still the correct
/// built-in default. A new file is decoded once and cached in `assets`; later
/// faces that name it reuse the bytes.
fn resolve_present_file(
    dir: &Path,
    file: &str,
    assets: &mut std::collections::BTreeMap<String, PackAsset>,
) -> FaceFile {
    if let Some(index) = LAYER_FILES.iter().position(|known| *known == file) {
        return FaceFile::Source(FaceSource::Layer(index as u8));
    }
    if assets.contains_key(file) {
        // Already decoded by the content asset pass; never re-read the file.
        return FaceFile::Source(FaceSource::Authored(file.to_string()));
    }
    match decode_layer_file(dir, file) {
        Ok(Some(rgba)) => {
            assets.insert(
                file.to_string(),
                PackAsset {
                    rgba,
                    source_side: 16,
                },
            );
            FaceFile::Source(FaceSource::Authored(file.to_string()))
        }
        Ok(None) => FaceFile::Missing,
        Err(reason) => FaceFile::Invalid(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::content::faces::BlockFaces;
    use crate::content::spec::PackContentSpec;
    use crate::pack::resample;
    use crate::player::inventory::ItemId;
    use crate::render::texture::{compose_block_array_layers, generate_all_texture_layers_rgba};
    use crate::startup::build_session_registry;
    use crate::ui::item_icon::{icon_source_with, IconSource};
    use crate::world::block::BlockId;

    const LAYER_BYTES: usize = 16 * 16 * 4;

    fn pack_dir(root: &tempfile::TempDir, name: &str) -> PathBuf {
        let dir = root.path().join(name);
        fs::create_dir_all(&dir).expect("create pack dir");
        dir
    }

    fn gradient(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    seed.wrapping_add((x.wrapping_mul(7).wrapping_add(3)) as u8),
                    seed.wrapping_add((y.wrapping_mul(11).wrapping_add(5)) as u8),
                    seed.wrapping_add(((x + y).wrapping_mul(3)) as u8),
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

    /// Write a square PNG at `dim` and return the 16x16 bytes the loader emits.
    fn authored(dir: &Path, file: &str, dim: u32, seed: u8) -> Vec<u8> {
        write_png_data(&dir.join(file), dim, dim, &gradient(dim, dim, seed));
        resample::resample_to_16(&gradient(dim, dim, seed), dim)
    }

    #[test]
    fn stone_base_and_authored_top_yield_a_non_uniform_face_table() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "faces");
        fs::write(
            dir.join("pack.toml"),
            "format_version = 3\nname = \"Faces\"\n",
        )
        .unwrap();

        let base = authored(&dir, "stone.png", 16, 0xC0);
        let top = authored(&dir, "stone_top.png", 16, 0x50);

        let spec = crate::pack::spec::load_spec_from_dir(&dir);
        assert!(spec.rejected.is_none());
        assert!(spec.warnings.is_empty(), "{:?}", spec.warnings);
        assert_eq!(
            spec.content.builtin_faces.len(),
            1,
            "one loader-synthesized entry"
        );
        let entry = &spec.content.builtin_faces[0];
        assert_eq!(entry.block, "stone");
        assert_eq!(entry.faces[0], Some(FaceSource::Layer(3)));
        assert_eq!(
            entry.faces[2],
            Some(FaceSource::Authored("stone_top.png".into()))
        );

        let (registry, report) = build_session_registry(&[spec]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        let stone = registry.block(BlockId::Stone).expect("builtin stone");
        assert_eq!(stone.faces, [3, 3, 48, 3, 3, 3]);

        // The new authored face is the first pack layer, and its bytes are the
        // authored top texture (the legacy stone layer is untouched).
        assert_eq!(registry.pack_layers().len(), 1);
        assert_eq!(registry.pack_layers()[0], top);
        assert_ne!(registry.pack_layers()[0], base);

        let faces = BlockFaces::from_registry(&registry);
        assert_eq!(faces.layer(BlockId::Stone, 2), 48);
        assert_eq!(faces.layer(BlockId::Stone, 0), 3);

        let composed =
            compose_block_array_layers(&generate_all_texture_layers_rgba(), registry.pack_layers());
        assert_eq!(composed[48], top);
        assert_eq!(composed[48].len(), LAYER_BYTES);

        match icon_source_with(&registry, ItemId::Block(BlockId::Stone)) {
            Some(IconSource::Block(layers)) => {
                assert_eq!(layers.top, 48, "registry-aware icon reads the authored top");
                assert_eq!(layers.left, 3);
                assert_eq!(layers.right, 3);
            }
            other => panic!("stone icon source was {other:?}"),
        }
    }

    #[test]
    fn unprefixed_pack_is_rejected_by_name_and_never_applied() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "old");
        fs::write(dir.join("pack.toml"), "name = \"old\"\n").unwrap();
        // Even a perfectly valid legacy texture must not apply.
        authored(&dir, "stone.png", 16, 0x11);

        let spec = crate::pack::spec::load_spec_from_dir(&dir);
        let reason = spec.rejected.as_ref().expect("v1 is rejected");
        assert!(reason.contains("format_version 1 is not supported"));
        assert!(reason.contains("format_version = 3"));
        assert_eq!(spec.content, PackContentSpec::default());
        assert!(spec.block_layers.is_empty());
        assert!(spec.mob_layers.is_empty());
        assert_eq!(spec.diagnostics.len(), 1);
        assert_eq!(spec.diagnostics[0].entry, "pack");
        assert_eq!(
            spec.diagnostics[0].severity,
            crate::content::spec::Severity::Error
        );

        let (pack, specs, warnings) =
            crate::pack::spec::load_stack(root.path(), &["old".to_string()]);
        assert!(specs.is_empty(), "a rejected pack is never applied");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "old");
        assert!(warnings[0].reason.contains("format_version 1"));
        assert_eq!(pack.display_name, "old");
        assert_eq!(pack.layers, generate_all_texture_layers_rgba());

        let (lines, ok) = crate::pack::validate::validate_pack_lines(root.path(), "old");
        assert!(!ok, "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("error: pack old:")),
            "{lines:?}"
        );
        assert!(
            lines
                .last()
                .is_some_and(|line| line == "validate-pack: old: 0 entries, 1 errors, 0 warnings"),
            "{lines:?}"
        );
    }

    #[test]
    fn authored_all_and_direction_override_follow_the_face_precedence() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "grass");
        fs::write(dir.join("pack.toml"), "format_version = 3\n").unwrap();

        // Legacy top stays layer 0; authored all and authored north allocate.
        authored(&dir, "grass_top.png", 16, 0x01);
        let all = authored(&dir, "grass.png", 16, 0x20);
        let north = authored(&dir, "grass_north.png", 16, 0x30);

        let spec = crate::pack::spec::load_spec_from_dir(&dir);
        assert!(spec.rejected.is_none());
        assert!(spec.warnings.is_empty(), "{:?}", spec.warnings);

        let (registry, report) = build_session_registry(&[spec]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        let grass = registry.block(BlockId::Grass).expect("builtin grass");
        // east, west, top, bottom, south, north
        assert_eq!(grass.faces, [48, 48, 0, 48, 48, 49]);
        assert_eq!(registry.pack_layers().len(), 2);
        assert_eq!(registry.pack_layers()[0], all);
        assert_eq!(registry.pack_layers()[1], north);
    }

    #[test]
    fn invalid_new_face_warns_once_and_falls_through_to_the_legacy_layer() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "bad-top");
        fs::write(dir.join("pack.toml"), "format_version = 3\n").unwrap();
        authored(&dir, "stone.png", 16, 0x40);
        fs::write(dir.join("stone_top.png"), b"not a png").unwrap();

        let spec = crate::pack::spec::load_spec_from_dir(&dir);
        assert!(spec.rejected.is_none());
        assert_eq!(spec.warnings.len(), 1, "{:?}", spec.warnings);
        assert_eq!(spec.warnings[0].field, "stone_top.png");
        assert!(
            spec.content.builtin_faces.is_empty(),
            "the invalid new file falls back to the legacy layer, so nothing is authored"
        );
    }
}
