//! Dev-only pack file watch list and settle tracking.
//!
//! The config watcher has a compile-time list of six files; the pack watcher
//! has a dynamic list built from the frozen [`PackPresentation`]: every stack
//! manifest, the 25 block and 13 mob fixed names, one already-allocated v2
//! block-face asset per frozen slot, and one authored `[builtin_icons]` PNG
//! per override name. Nothing here touches the GPU, decodes a PNG, or reads
//! the filesystem: callers feed in fingerprints and react to the settled
//! indices. That keeps allocation frozen — a settled entry can only rewrite an
//! index that was recorded at build time.
//!
//! Paths are recorded as `packs_root/folder/relative`, while `display` is
//! always the pack-relative `packs/<folder>/<relative>` spelling used by the
//! overlay and `--creator-trace`, never an absolute path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::watch::{CONFIG_POLL_INTERVAL, CONFIG_SETTLE_INTERVAL};
use crate::content::names;
use crate::pack::presentation::PackPresentation;
use crate::pack::{LAYER_FILES, MOB_LAYER_FILES, PACKS_DIR};
use crate::render::texture::MOB_LAYER_BASE;
use crate::settle::SettleTracker;

/// Manifest file name inside one pack folder.
const MANIFEST_FILE: &str = "pack.toml";

/// What a watched pack file feeds.
///
/// `BaseLayer` uses the fixed texture-array index (`0..=24`, `32..=44`).
/// `Asset` uses the frozen pack-layer slot, whose GPU layer is
/// `PACK_LAYER_BASE + slot`. `BuiltinIcon` names the pinned procedural item
/// sprite layer and never consumes block-layer allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackFileKind {
    /// `pack.toml`; content/allocation changes need a restart.
    Manifest,
    /// One of the fixed-name block or mob layer files.
    BaseLayer(u8),
    /// One already-allocated v2 block-face asset.
    Asset { slot: u8 },
    /// One authored `[builtin_icons]` PNG, keyed by its canonical built-in
    /// sprite name. The name is resolved to a `'static` spelling at build time
    /// so the enum stays `Copy`.
    BuiltinIcon { name: &'static str },
}

/// One watched pack file with frozen provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackWatchEntry {
    /// Pack folder name inside the packs root.
    pub folder: String,
    /// Absolute-or-root-relative path used for fingerprinting.
    pub path: PathBuf,
    /// Pack-relative `packs/<folder>/<relative>` display path.
    pub display: String,
    /// Destination recorded at build time; never re-derived by the watcher.
    pub kind: PackFileKind,
}

/// Dynamic pack watch list plus one settle slot per entry.
pub struct PackWatch {
    entries: Vec<PackWatchEntry>,
    tracker: SettleTracker<u64>,
}

impl PackWatch {
    /// Build the watch list from a frozen presentation and the active stack.
    ///
    /// Every stack name gets a manifest entry, including a folder that does
    /// not exist yet, so a later appearance is observed. Every loaded spec
    /// contributes all 38 fixed names, whether or not the files exist at
    /// launch. Only `pack_layer_sources` entries whose authored asset actually
    /// loaded are watched, in frozen slot order and deduplicated by
    /// `(folder, path)`.
    pub fn build(
        presentation: &PackPresentation,
        packs_root: &Path,
        stack: &[String],
        now: Instant,
    ) -> Self {
        let mut entries = Vec::new();

        for folder in stack {
            entries.push(PackWatchEntry {
                folder: folder.clone(),
                path: packs_root.join(folder).join(MANIFEST_FILE),
                display: format!("{PACKS_DIR}/{folder}/{MANIFEST_FILE}"),
                kind: PackFileKind::Manifest,
            });
        }

        for spec in &presentation.specs {
            let folder_dir = packs_root.join(&spec.folder);
            for (layer, file) in LAYER_FILES.iter().enumerate() {
                entries.push(PackWatchEntry {
                    folder: spec.folder.clone(),
                    path: folder_dir.join(file),
                    display: format!("{PACKS_DIR}/{}/{file}", spec.folder),
                    kind: PackFileKind::BaseLayer(layer as u8),
                });
            }
            for (offset, file) in MOB_LAYER_FILES.iter().enumerate() {
                entries.push(PackWatchEntry {
                    folder: spec.folder.clone(),
                    path: folder_dir.join(file),
                    display: format!("{PACKS_DIR}/{}/{file}", spec.folder),
                    kind: PackFileKind::BaseLayer(MOB_LAYER_BASE as u8 + offset as u8),
                });
            }
        }

        // Slots follow the frozen registry allocation order. The same authored
        // path can only be watched once even if a future allocator records it
        // for more than one slot.
        let mut seen = BTreeSet::new();
        for (slot, source) in presentation.pack_layer_sources.iter().enumerate() {
            let loaded = presentation.specs.iter().any(|spec| {
                spec.folder == source.folder && spec.content.assets.contains_key(&source.path)
            });
            if !loaded || !seen.insert((source.folder.clone(), source.path.clone())) {
                continue;
            }
            let Ok(slot) = u8::try_from(slot) else {
                debug_assert!(false, "pack layer slot {slot} does not fit in a u8");
                continue;
            };
            entries.push(PackWatchEntry {
                folder: source.folder.clone(),
                path: packs_root.join(&source.folder).join(&source.path),
                display: format!("{PACKS_DIR}/{}/{}", source.folder, source.path),
                kind: PackFileKind::Asset { slot },
            });
        }

        // One entry per authored override, including a path that is missing
        // at launch: the file appearing later must settle like any edit. The
        // same authored path may back two names, so these entries are
        // deliberately NOT deduplicated — each destination drives its own
        // pinned sprite layer.
        for (folder, name, path) in presentation.builtin_icon_paths() {
            let Some(name) = names::builtin_item_sprite_name(name) else {
                debug_assert!(false, "`{name}` is not an overridable builtin sprite");
                continue;
            };
            entries.push(PackWatchEntry {
                folder: folder.to_string(),
                path: packs_root.join(folder).join(path),
                display: format!("{PACKS_DIR}/{folder}/{path}"),
                kind: PackFileKind::BuiltinIcon { name },
            });
        }

        let tracker = SettleTracker::new(
            entries.len(),
            CONFIG_POLL_INTERVAL,
            CONFIG_SETTLE_INTERVAL,
            now,
        );
        Self { entries, tracker }
    }

    /// The complete frozen watch list, in build order.
    pub fn entries(&self) -> &[PackWatchEntry] {
        &self.entries
    }

    /// Number of watched entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// One entry by index, if the entry list ever shrinks underneath a caller.
    pub fn entry(&self, index: usize) -> Option<&PackWatchEntry> {
        self.entries.get(index)
    }

    /// Observe one entry's latest content fingerprint.
    ///
    /// Shares the config watcher's settle rules exactly: the first observation
    /// baselines, a new snapshot restarts the settle timer, and a snapshot
    /// stable for [`CONFIG_SETTLE_INTERVAL`] reports `true` once. `None` means
    /// missing or unreadable and is a legal snapshot.
    pub fn observe(&mut self, index: usize, snapshot: Option<u64>, now: Instant) -> bool {
        self.tracker.observe(index, snapshot, now)
    }

    /// Whether the next scheduled poll is due.
    pub fn is_due(&self, now: Instant) -> bool {
        self.tracker.is_due(now)
    }

    /// Entry indices with an unsettled write, in watch-list order.
    ///
    /// Mirrors [`crate::config::watch::WatchState::pending_files`] so the app
    /// can recompose the overlay only when the pending set actually changes.
    pub fn pending_indices(&self) -> Vec<usize> {
        self.tracker.pending_indices()
    }

    /// Schedule the next poll one shared poll interval after `now`.
    pub fn mark_polled(&mut self, now: Instant) {
        self.tracker.mark_polled(now);
    }

    /// When the next poll becomes due.
    pub fn next_poll_at(&self) -> Instant {
        self.tracker.next_poll_at()
    }
}

/// Collapse every run of whitespace so a trace line is always one line.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stable `--creator-trace` line for one live texture upload.
///
/// `display` is the pack-relative path, `layer` the frozen texture-array
/// index, and `digest` the content digest at application time. Uses the same
/// `{:#018x}` spelling as the config trace.
pub fn applied_line(display: &str, layer: u8, digest: u64) -> String {
    format!("creator: texture applied path={display} layer={layer} digest={digest:#018x}")
}

/// Stable `--creator-trace` line for one rejected texture edit.
pub fn rejected_line(display: &str, reason: &str) -> String {
    format!(
        "creator: texture rejected path={display} reason={}",
        single_line(reason)
    )
}

/// Stable `--creator-trace` line for a manifest change that needs a restart.
pub fn restart_line(reason: &str) -> String {
    format!("creator: restart-required reason={}", single_line(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    use crate::content::registry::{build_registry, PackIdentity};
    use crate::content::spec::{BlockEntry, BlockTextures, PackAsset, PackContentSpec};
    use crate::content::ContentRegistry;
    use crate::pack::manifest;
    use crate::pack::sound::SoundSpec;
    use crate::pack::spec::{resolve_presentation, PackSpec};
    use crate::pack::AUTHORABLE_TEX_LAYERS;
    use crate::render::texture::MOB_AUTHORABLE_TEX_LAYERS;

    const LAYER_BYTES: usize = 16 * 16 * 4;

    fn blank_spec(folder: &str) -> PackSpec {
        PackSpec {
            folder: folder.to_string(),
            format_version: manifest::PackFormat::V1,
            version: None,
            manifest: manifest::PackManifest::default(),
            block_layers: vec![None; AUTHORABLE_TEX_LAYERS],
            mob_layers: vec![None; MOB_AUTHORABLE_TEX_LAYERS],
            logo: None,
            sound: SoundSpec::default(),
            content: PackContentSpec::default(),
            diagnostics: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn presentation_with(specs: Vec<PackSpec>, registry: &ContentRegistry) -> PackPresentation {
        let base = resolve_presentation(&specs);
        PackPresentation::build(base, specs, registry)
    }

    /// One v2 pack whose blocks reference `used` and whose content also ships
    /// `unused` assets. Returns the authored spec and the built registry.
    fn v2_fixture(folder: &str, used: &[&str], unused: &[&str]) -> (PackSpec, ContentRegistry) {
        let mut content = PackContentSpec::default();
        for (index, path) in used.iter().enumerate() {
            content.blocks.push(BlockEntry {
                id: format!("mymod:block{index}"),
                display_name: None,
                textures: BlockTextures {
                    all: Some((*path).to_string()),
                    ..BlockTextures::default()
                },
                hardness: 1.0,
                drop: None,
                tool: None,
                stack_limit: 64,
            });
        }
        for (index, path) in used.iter().chain(unused.iter()).enumerate() {
            content.assets.insert(
                (*path).to_string(),
                PackAsset {
                    rgba: vec![index as u8; LAYER_BYTES],
                    source_side: 16,
                },
            );
        }

        let mut spec = blank_spec(folder);
        spec.format_version = manifest::PackFormat::V2;
        spec.content = content.clone();

        let identity = PackIdentity {
            folder: folder.to_string(),
            version: Some("1.0.0".to_string()),
        };
        let registry = build_registry(ContentRegistry::builtin(), &[(identity, content)]).0;
        (spec, registry)
    }

    fn base_layer_indices(watch: &PackWatch) -> Vec<u8> {
        watch
            .entries()
            .iter()
            .filter_map(|entry| match entry.kind {
                PackFileKind::BaseLayer(layer) => Some(layer),
                _ => None,
            })
            .collect()
    }

    fn asset_entries(watch: &PackWatch) -> Vec<&PackWatchEntry> {
        watch
            .entries()
            .iter()
            .filter(|entry| matches!(entry.kind, PackFileKind::Asset { .. }))
            .collect()
    }

    fn builtin_icon_entries(watch: &PackWatch) -> Vec<&PackWatchEntry> {
        watch
            .entries()
            .iter()
            .filter(|entry| matches!(entry.kind, PackFileKind::BuiltinIcon { .. }))
            .collect()
    }

    /// A v2 spec with a `[builtin_icons]` table and no content arrays. The
    /// authored PNGs are deliberately not in `assets`, so every override is
    /// "missing at launch" for watch purposes.
    fn builtin_icon_fixture(
        folder: &str,
        overrides: &[(&str, &str)],
    ) -> (PackSpec, ContentRegistry) {
        let (mut spec, registry) = v2_fixture(folder, &[], &[]);
        for (name, path) in overrides {
            spec.content
                .builtin_icons
                .insert((*name).to_string(), (*path).to_string());
        }
        (spec, registry)
    }

    #[test]
    fn v1_pack_watches_manifest_and_all_thirty_eight_fixed_names() {
        let root = tempfile::tempdir().unwrap();
        let folder = "v1";
        fs::create_dir_all(root.path().join(folder)).unwrap();
        let presentation = presentation_with(vec![blank_spec(folder)], ContentRegistry::builtin());
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &[folder.to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1 + 25 + 13);
        assert_eq!(watch.entries().len(), watch.entry_count());
        assert!(watch.entry(watch.entry_count()).is_none());

        let manifest = watch.entry(0).expect("manifest entry");
        assert_eq!(manifest.kind, PackFileKind::Manifest);
        assert_eq!(manifest.folder, "v1");
        assert_eq!(manifest.display, "packs/v1/pack.toml");
        assert_eq!(manifest.path, root.path().join("v1").join("pack.toml"));

        let expected_layers: Vec<u8> = (0..25u8).chain(32..=44).collect();
        assert_eq!(base_layer_indices(&watch), expected_layers);

        let stone = watch
            .entries()
            .iter()
            .find(|entry| entry.kind == PackFileKind::BaseLayer(3))
            .expect("stone layer entry");
        assert_eq!(stone.display, "packs/v1/stone.png");
        assert_eq!(stone.path, root.path().join("v1").join("stone.png"));

        let pig = watch
            .entries()
            .iter()
            .find(|entry| entry.kind == PackFileKind::BaseLayer(32))
            .expect("pig body layer entry");
        assert_eq!(pig.display, "packs/v1/pig_body.png");
        assert_eq!(pig.path, root.path().join("v1").join("pig_body.png"));

        // Display paths are pack-relative even though `packs_root` is a
        // process-absolute tempdir.
        assert!(
            watch
                .entries()
                .iter()
                .all(|entry| entry.display.starts_with("packs/")),
            "every display path must be pack-relative"
        );
    }

    #[test]
    fn v2_pack_watches_allocated_assets_in_frozen_slot_order_and_skips_unused() {
        let root = tempfile::tempdir().unwrap();
        let folder = "v2";
        fs::create_dir_all(root.path().join(folder)).unwrap();
        let (spec, registry) = v2_fixture(
            folder,
            &["ruby.png", "textures/emerald.png"],
            &["unused.png"],
        );
        assert_eq!(registry.pack_layer_sources().len(), 2);
        assert_eq!(registry.pack_layer_sources()[0].path, "ruby.png");
        assert_eq!(
            registry.pack_layer_sources()[1].path,
            "textures/emerald.png"
        );

        let presentation = presentation_with(vec![spec], &registry);
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &[folder.to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1 + 38 + 2);
        let assets = asset_entries(&watch);
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].kind, PackFileKind::Asset { slot: 0 });
        assert_eq!(assets[0].folder, "v2");
        assert_eq!(assets[0].display, "packs/v2/ruby.png");
        assert_eq!(assets[0].path, root.path().join("v2").join("ruby.png"));
        assert_eq!(assets[1].kind, PackFileKind::Asset { slot: 1 });
        assert_eq!(assets[1].display, "packs/v2/textures/emerald.png");
        assert_eq!(
            assets[1].path,
            root.path().join("v2").join("textures").join("emerald.png")
        );

        // An authored-but-unreferenced asset consumes no registry slot and is
        // never watched.
        assert!(watch
            .entries()
            .iter()
            .all(|entry| !entry.display.contains("unused.png")));
    }

    #[test]
    fn duplicate_asset_path_is_watched_once() {
        let root = tempfile::tempdir().unwrap();
        let folder = "dup";
        let (spec, registry) = v2_fixture(folder, &["ore.png"], &[]);
        let mut presentation = presentation_with(vec![spec], &registry);
        assert_eq!(presentation.pack_layer_sources.len(), 1);

        // Model two frozen slots that name the same authored file. The watch
        // list must not fingerprint or settle the same path twice.
        presentation
            .pack_layers
            .push(presentation.pack_layers[0].clone());
        presentation
            .pack_layer_sources
            .push(presentation.pack_layer_sources[0].clone());

        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &[folder.to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1 + 38 + 1);
        let assets = asset_entries(&watch);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].kind, PackFileKind::Asset { slot: 0 });
    }

    #[test]
    fn v2_pack_watches_builtin_icon_overrides_even_when_missing_at_launch() {
        let root = tempfile::tempdir().unwrap();
        let folder = "icons";
        fs::create_dir_all(root.path().join(folder)).unwrap();
        let (spec, registry) = builtin_icon_fixture(
            folder,
            &[
                ("apple", "apple.png"),
                ("wood_pickaxe", "textures/pickaxe.png"),
            ],
        );

        // The files are authored but do not exist at launch.
        assert!(!root.path().join(folder).join("apple.png").exists());
        assert!(!root
            .path()
            .join(folder)
            .join("textures")
            .join("pickaxe.png")
            .exists());

        let presentation = presentation_with(vec![spec], &registry);
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &[folder.to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1 + 38 + 2);
        let icons = builtin_icon_entries(&watch);
        assert_eq!(icons.len(), 2, "one entry per authored override name");
        assert_eq!(icons[0].kind, PackFileKind::BuiltinIcon { name: "apple" });
        assert_eq!(icons[0].folder, "icons");
        assert_eq!(icons[0].display, "packs/icons/apple.png");
        assert_eq!(icons[0].path, root.path().join("icons").join("apple.png"));
        assert_eq!(
            icons[1].kind,
            PackFileKind::BuiltinIcon {
                name: "wood_pickaxe"
            }
        );
        assert_eq!(icons[1].display, "packs/icons/textures/pickaxe.png");
        assert_eq!(
            icons[1].path,
            root.path()
                .join("icons")
                .join("textures")
                .join("pickaxe.png")
        );
        assert!(
            icons
                .iter()
                .all(|entry| entry.display.starts_with("packs/")),
            "override display paths stay pack-relative"
        );
    }

    #[test]
    fn v2_pack_without_builtin_icons_keeps_the_existing_entry_count() {
        let root = tempfile::tempdir().unwrap();
        let folder = "plain";
        let (spec, registry) = v2_fixture(folder, &["ruby.png"], &[]);
        let presentation = presentation_with(vec![spec], &registry);
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &[folder.to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1 + 38 + 1);
        assert!(
            builtin_icon_entries(&watch).is_empty(),
            "a table-less v2 pack adds no sprite entries"
        );
    }

    #[test]
    fn two_builtin_icon_names_sharing_one_path_produce_two_entries() {
        let root = tempfile::tempdir().unwrap();
        let folder = "shared";
        let (spec, registry) = builtin_icon_fixture(
            folder,
            &[("apple", "sprite.png"), ("wood_pickaxe", "sprite.png")],
        );
        let presentation = presentation_with(vec![spec], &registry);
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &[folder.to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1 + 38 + 2);
        let icons = builtin_icon_entries(&watch);
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0].display, "packs/shared/sprite.png");
        assert_eq!(icons[1].display, "packs/shared/sprite.png");
        assert_ne!(
            icons[0].kind, icons[1].kind,
            "the two names remain distinct destinations"
        );
    }

    #[test]
    fn every_stack_name_gets_a_manifest_entry_and_missing_folders_get_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let presentation =
            presentation_with(vec![blank_spec("present")], ContentRegistry::builtin());
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &["present".to_string(), "ghost".to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 2 + 38);

        let manifests: Vec<&PackWatchEntry> = watch
            .entries()
            .iter()
            .filter(|entry| entry.kind == PackFileKind::Manifest)
            .collect();
        assert_eq!(manifests.len(), 2);
        assert_eq!(manifests[0].folder, "present");
        assert_eq!(manifests[1].folder, "ghost");
        assert_eq!(manifests[1].display, "packs/ghost/pack.toml");
        assert_eq!(
            manifests[1].path,
            root.path().join("ghost").join("pack.toml")
        );

        // The missing folder contributes no base-layer slots; only its
        // manifest can settle if the folder later appears.
        assert!(
            watch
                .entries()
                .iter()
                .filter(|entry| entry.folder == "ghost")
                .count()
                == 1
        );
    }

    #[test]
    fn missing_presentation_folder_gets_only_its_manifest_entry() {
        let root = tempfile::tempdir().unwrap();
        let presentation = presentation_with(Vec::new(), ContentRegistry::builtin());
        let watch = PackWatch::build(
            &presentation,
            root.path(),
            &["ghost".to_string()],
            Instant::now(),
        );

        assert_eq!(watch.entry_count(), 1);
        let entry = watch.entry(0).expect("manifest entry");
        assert_eq!(entry.kind, PackFileKind::Manifest);
        assert_eq!(entry.display, "packs/ghost/pack.toml");
        assert!(!entry
            .display
            .contains(root.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn observe_wraps_the_shared_settle_machine() {
        let base = Instant::now();
        let presentation = presentation_with(Vec::new(), ContentRegistry::builtin());
        let mut watch = PackWatch::build(
            &presentation,
            Path::new(PACKS_DIR),
            &["ghost".to_string()],
            base,
        );

        assert_eq!(watch.entry_count(), 1);
        assert!(watch.is_due(base));
        assert_eq!(watch.next_poll_at(), base);
        watch.mark_polled(base);
        assert_eq!(watch.next_poll_at(), base + CONFIG_POLL_INTERVAL);
        assert!(!watch.is_due(base + Duration::from_millis(99)));
        assert!(watch.is_due(base + CONFIG_POLL_INTERVAL));

        // Missing at baseline, then the folder appears and its bytes settle.
        assert!(!watch.observe(0, None, base));
        assert!(!watch.observe(0, Some(7), base + Duration::from_millis(10)));
        assert!(!watch.observe(0, Some(7), base + Duration::from_millis(259)));
        assert!(watch.observe(0, Some(7), base + Duration::from_millis(260)));
        assert!(!watch.observe(0, Some(7), base + Duration::from_millis(400)));
    }

    #[test]
    fn formatters_are_exact_and_collapse_whitespace() {
        assert_eq!(
            applied_line("packs/v1/stone.png", 3, 0x0000_0000_0000_00ab),
            "creator: texture applied path=packs/v1/stone.png layer=3 digest=0x00000000000000ab"
        );
        assert_eq!(
            applied_line("packs/v2/textures/ruby.png", 48, u64::MAX),
            "creator: texture applied path=packs/v2/textures/ruby.png layer=48 digest=0xffffffffffffffff"
        );

        let rejected = rejected_line(
            "packs/v1/stone.png",
            "failed to decode PNG\nline\ttwo   three ",
        );
        assert_eq!(
            rejected,
            "creator: texture rejected path=packs/v1/stone.png \
             reason=failed to decode PNG line two three"
        );
        assert!(!rejected.contains('\n') && !rejected.contains('\t'));

        let restart = restart_line("v1: content changed;\nrestart to apply");
        assert_eq!(
            restart,
            "creator: restart-required reason=v1: content changed; restart to apply"
        );
        assert!(!restart.contains('\n'));
    }
}
