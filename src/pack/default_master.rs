//! Compile-time master copy of the shipped `packs/default` pack.
//!
//! [`DEFAULT_PACK_FILES`] embeds the exact bytes of every file in the committed
//! `packs/default` folder into the binary. A later slice restores a missing or
//! unusable folder from this table at startup, so the recovery copy can never
//! depend on files that ship next to the executable.
//!
//! The table is the read-only recovery source; the committed folder is the
//! editable source of truth. Two tests keep them honest:
//!
//! * [`tests::default_master_matches_the_committed_folder`] proves the embedded
//!   bytes equal the checked-in folder bytes in both directions.
//! * [`tests::default_master_matches_the_generator`] proves the embedded bytes
//!   equal [`crate::pack::defaults::build_default_pack`], so procedural art or
//!   synthesized sound drift fails loudly instead of silently changing the
//!   recovery master.
//!
//! Entries are one per generated file, sorted by relative path exactly like the
//! generator, and every path is relative to the pack folder with forward
//! slashes only. Regenerate this table mechanically (never by hand) with:
//!
//! ```text
//! find packs/default -type f | sed 's#^packs/default/##' | LC_ALL=C sort
//! ```

/// Every file of the shipped `packs/default` pack, sorted by relative path.
///
/// The byte slice for each entry is the committed file's exact contents, so the
/// compiler fails loudly if the shipped folder is deleted from a checkout.
/// Paths are relative to the pack folder and use forward slashes only.
pub static DEFAULT_PACK_FILES: &[(&str, &[u8])] = &[
    ("arrow.png", include_bytes!("../../packs/default/arrow.png")),
    (
        "arrow_impact.wav",
        include_bytes!("../../packs/default/arrow_impact.wav"),
    ),
    (
        "bedrock.png",
        include_bytes!("../../packs/default/bedrock.png"),
    ),
    (
        "bow_release.wav",
        include_bytes!("../../packs/default/bow_release.wav"),
    ),
    ("break.wav", include_bytes!("../../packs/default/break.wav")),
    ("brick.png", include_bytes!("../../packs/default/brick.png")),
    ("chest.png", include_bytes!("../../packs/default/chest.png")),
    (
        "coal_ore.png",
        include_bytes!("../../packs/default/coal_ore.png"),
    ),
    (
        "cobblestone.png",
        include_bytes!("../../packs/default/cobblestone.png"),
    ),
    (
        "cow_body.png",
        include_bytes!("../../packs/default/cow_body.png"),
    ),
    (
        "cow_head.png",
        include_bytes!("../../packs/default/cow_head.png"),
    ),
    (
        "cow_leg.png",
        include_bytes!("../../packs/default/cow_leg.png"),
    ),
    (
        "crafting_table_side.png",
        include_bytes!("../../packs/default/crafting_table_side.png"),
    ),
    (
        "crafting_table_top.png",
        include_bytes!("../../packs/default/crafting_table_top.png"),
    ),
    ("dirt.png", include_bytes!("../../packs/default/dirt.png")),
    (
        "farmland.png",
        include_bytes!("../../packs/default/farmland.png"),
    ),
    ("glass.png", include_bytes!("../../packs/default/glass.png")),
    (
        "grass_side.png",
        include_bytes!("../../packs/default/grass_side.png"),
    ),
    (
        "grass_top.png",
        include_bytes!("../../packs/default/grass_top.png"),
    ),
    (
        "icons/apple.png",
        include_bytes!("../../packs/default/icons/apple.png"),
    ),
    (
        "icons/arrow.png",
        include_bytes!("../../packs/default/icons/arrow.png"),
    ),
    (
        "icons/bone.png",
        include_bytes!("../../packs/default/icons/bone.png"),
    ),
    (
        "icons/bread.png",
        include_bytes!("../../packs/default/icons/bread.png"),
    ),
    (
        "icons/iron_axe.png",
        include_bytes!("../../packs/default/icons/iron_axe.png"),
    ),
    (
        "icons/iron_hoe.png",
        include_bytes!("../../packs/default/icons/iron_hoe.png"),
    ),
    (
        "icons/iron_pickaxe.png",
        include_bytes!("../../packs/default/icons/iron_pickaxe.png"),
    ),
    (
        "icons/iron_shovel.png",
        include_bytes!("../../packs/default/icons/iron_shovel.png"),
    ),
    (
        "icons/iron_sword.png",
        include_bytes!("../../packs/default/icons/iron_sword.png"),
    ),
    (
        "icons/leather.png",
        include_bytes!("../../packs/default/icons/leather.png"),
    ),
    (
        "icons/raw_beef.png",
        include_bytes!("../../packs/default/icons/raw_beef.png"),
    ),
    (
        "icons/raw_porkchop.png",
        include_bytes!("../../packs/default/icons/raw_porkchop.png"),
    ),
    (
        "icons/rotten_flesh.png",
        include_bytes!("../../packs/default/icons/rotten_flesh.png"),
    ),
    (
        "icons/stick.png",
        include_bytes!("../../packs/default/icons/stick.png"),
    ),
    (
        "icons/stone_axe.png",
        include_bytes!("../../packs/default/icons/stone_axe.png"),
    ),
    (
        "icons/stone_hoe.png",
        include_bytes!("../../packs/default/icons/stone_hoe.png"),
    ),
    (
        "icons/stone_pickaxe.png",
        include_bytes!("../../packs/default/icons/stone_pickaxe.png"),
    ),
    (
        "icons/stone_shovel.png",
        include_bytes!("../../packs/default/icons/stone_shovel.png"),
    ),
    (
        "icons/stone_sword.png",
        include_bytes!("../../packs/default/icons/stone_sword.png"),
    ),
    (
        "icons/wheat.png",
        include_bytes!("../../packs/default/icons/wheat.png"),
    ),
    (
        "icons/wheat_seeds.png",
        include_bytes!("../../packs/default/icons/wheat_seeds.png"),
    ),
    (
        "icons/wood_axe.png",
        include_bytes!("../../packs/default/icons/wood_axe.png"),
    ),
    (
        "icons/wood_bow.png",
        include_bytes!("../../packs/default/icons/wood_bow.png"),
    ),
    (
        "icons/wood_hoe.png",
        include_bytes!("../../packs/default/icons/wood_hoe.png"),
    ),
    (
        "icons/wood_pickaxe.png",
        include_bytes!("../../packs/default/icons/wood_pickaxe.png"),
    ),
    (
        "icons/wood_shovel.png",
        include_bytes!("../../packs/default/icons/wood_shovel.png"),
    ),
    (
        "icons/wood_sword.png",
        include_bytes!("../../packs/default/icons/wood_sword.png"),
    ),
    (
        "iron_ore.png",
        include_bytes!("../../packs/default/iron_ore.png"),
    ),
    (
        "leaves.png",
        include_bytes!("../../packs/default/leaves.png"),
    ),
    (
        "log_side.png",
        include_bytes!("../../packs/default/log_side.png"),
    ),
    (
        "log_top.png",
        include_bytes!("../../packs/default/log_top.png"),
    ),
    (
        "loot_chest.png",
        include_bytes!("../../packs/default/loot_chest.png"),
    ),
    (
        "mine_complete.wav",
        include_bytes!("../../packs/default/mine_complete.wav"),
    ),
    (
        "mob_death.wav",
        include_bytes!("../../packs/default/mob_death.wav"),
    ),
    (
        "mob_hurt.wav",
        include_bytes!("../../packs/default/mob_hurt.wav"),
    ),
    (
        "mob_idle.wav",
        include_bytes!("../../packs/default/mob_idle.wav"),
    ),
    ("pack.toml", include_bytes!("../../packs/default/pack.toml")),
    (
        "pig_body.png",
        include_bytes!("../../packs/default/pig_body.png"),
    ),
    (
        "pig_head.png",
        include_bytes!("../../packs/default/pig_head.png"),
    ),
    (
        "pig_leg.png",
        include_bytes!("../../packs/default/pig_leg.png"),
    ),
    ("place.wav", include_bytes!("../../packs/default/place.wav")),
    (
        "planks.png",
        include_bytes!("../../packs/default/planks.png"),
    ),
    ("sand.png", include_bytes!("../../packs/default/sand.png")),
    (
        "skeleton_body.png",
        include_bytes!("../../packs/default/skeleton_body.png"),
    ),
    (
        "skeleton_head.png",
        include_bytes!("../../packs/default/skeleton_head.png"),
    ),
    (
        "skeleton_leg.png",
        include_bytes!("../../packs/default/skeleton_leg.png"),
    ),
    (
        "snow_side.png",
        include_bytes!("../../packs/default/snow_side.png"),
    ),
    (
        "snow_top.png",
        include_bytes!("../../packs/default/snow_top.png"),
    ),
    ("stone.png", include_bytes!("../../packs/default/stone.png")),
    (
        "ui_click.wav",
        include_bytes!("../../packs/default/ui_click.wav"),
    ),
    (
        "wheat_stage0.png",
        include_bytes!("../../packs/default/wheat_stage0.png"),
    ),
    (
        "wheat_stage1.png",
        include_bytes!("../../packs/default/wheat_stage1.png"),
    ),
    (
        "wheat_stage2.png",
        include_bytes!("../../packs/default/wheat_stage2.png"),
    ),
    (
        "wheat_stage3.png",
        include_bytes!("../../packs/default/wheat_stage3.png"),
    ),
    (
        "zombie_body.png",
        include_bytes!("../../packs/default/zombie_body.png"),
    ),
    (
        "zombie_head.png",
        include_bytes!("../../packs/default/zombie_head.png"),
    ),
    (
        "zombie_leg.png",
        include_bytes!("../../packs/default/zombie_leg.png"),
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use super::DEFAULT_PACK_FILES;
    use crate::pack::defaults::{build_default_pack, default_pack_digest, pack_digest};

    /// Absolute path of the committed `packs/default` folder for this checkout.
    fn committed_pack_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("packs/default")
    }

    /// Recursively collect every regular file under `dir` as
    /// `(forward-slash relative path, bytes)`, sorted by path.
    fn read_folder(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let entries = std::fs::read_dir(dir)
                .unwrap_or_else(|err| panic!("read_dir {}: {err}", dir.display()));
            for entry in entries {
                let entry = entry
                    .unwrap_or_else(|err| panic!("read_dir entry in {}: {err}", dir.display()));
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .unwrap_or_else(|err| panic!("file_type {}: {err}", path.display()));
                if file_type.is_dir() {
                    walk(root, &path, out);
                } else if file_type.is_file() {
                    let relative = path
                        .strip_prefix(root)
                        .expect("walked file is under the pack root")
                        .components()
                        .map(|component| {
                            component.as_os_str().to_str().unwrap_or_else(|| {
                                panic!("non-UTF-8 path component in {}", path.display())
                            })
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    let bytes = std::fs::read(&path)
                        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
                    out.push((relative, bytes));
                }
            }
        }

        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort_by(|left, right| left.0.cmp(&right.0));
        out
    }

    #[test]
    fn default_master_has_76_unique_sorted_entries() {
        let paths: Vec<&str> = DEFAULT_PACK_FILES.iter().map(|(path, _)| *path).collect();
        assert_eq!(paths.len(), 76, "the shipped pack exports exactly 76 files");
        assert_eq!(
            paths.len(),
            crate::pack::defaults::DEFAULT_PACK_FILE_COUNT,
            "the master table must match the generator's declared file count"
        );

        let unique: BTreeSet<&str> = paths.iter().copied().collect();
        assert_eq!(unique.len(), paths.len(), "duplicate paths: {paths:?}");
        assert!(
            paths.windows(2).all(|pair| pair[0] < pair[1]),
            "master paths must be sorted like the generator: {paths:?}"
        );

        for path in &paths {
            assert!(!path.is_empty(), "empty path in master table");
            assert!(
                !path.starts_with('/'),
                "absolute path {path:?} must not appear in the master table"
            );
            assert!(
                !path.split('/').any(|segment| segment == ".."),
                "path {path:?} must stay inside the pack folder"
            );
            assert!(
                !path.contains('\\'),
                "path {path:?} must use forward slashes only"
            );
        }
    }

    #[test]
    fn default_master_matches_the_committed_folder() {
        let folder = read_folder(&committed_pack_dir());
        let folder_paths: BTreeSet<&str> = folder.iter().map(|(path, _)| path.as_str()).collect();
        let master_paths: BTreeSet<&str> =
            DEFAULT_PACK_FILES.iter().map(|(path, _)| *path).collect();

        let missing: Vec<&&str> = master_paths.difference(&folder_paths).collect();
        assert!(
            missing.is_empty(),
            "files embedded in the master but absent from packs/default: {missing:?}"
        );
        let extra: Vec<&&str> = folder_paths.difference(&master_paths).collect();
        assert!(
            extra.is_empty(),
            "files in packs/default but missing from the master: {extra:?}"
        );

        let folder_bytes: BTreeMap<&str, &[u8]> = folder
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        for (path, embedded) in DEFAULT_PACK_FILES {
            let on_disk = folder_bytes
                .get(path)
                .unwrap_or_else(|| panic!("missing committed file {path}"));
            assert_eq!(
                *on_disk, *embedded,
                "embedded bytes for {path} differ from packs/default"
            );
        }
    }

    #[test]
    fn default_master_matches_the_generator() {
        let generated = build_default_pack();
        assert_eq!(
            DEFAULT_PACK_FILES.len(),
            generated.len(),
            "master and generator must emit the same number of files"
        );
        for (index, ((master_path, master_bytes), (generated_path, generated_bytes))) in
            DEFAULT_PACK_FILES.iter().zip(generated.iter()).enumerate()
        {
            assert_eq!(
                *master_path,
                generated_path.as_str(),
                "entry {index} path differs between master and generator"
            );
            assert_eq!(
                *master_bytes,
                generated_bytes.as_slice(),
                "entry {index} ({master_path}) bytes differ between master and generator"
            );
        }
    }

    #[test]
    fn default_pack_digest_matches_for_the_master_the_folder_and_the_generator() {
        let master_digest = pack_digest(DEFAULT_PACK_FILES.iter().copied());

        let folder = read_folder(&committed_pack_dir());
        let folder_digest = pack_digest(
            folder
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
        );

        let generated = build_default_pack();
        let generator_digest = pack_digest(
            generated
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
        );

        assert_eq!(
            master_digest, generator_digest,
            "embedded master drifted from the in-memory generator"
        );
        assert_eq!(
            folder_digest, generator_digest,
            "committed packs/default drifted from the in-memory generator"
        );
        assert_eq!(
            generator_digest,
            default_pack_digest(),
            "the digest helper must agree with default_pack_digest"
        );
    }
}
