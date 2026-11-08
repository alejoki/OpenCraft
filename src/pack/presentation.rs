//! Frozen layer provenance and incremental pack presentation.
//!
//! A [`PackPresentation`] is built once per pack-stack apply from the resolved
//! [`LoadedPack`], the authored [`PackSpec`] stack, and the frozen
//! [`ContentRegistry`]. It records which `(pack, path)` produced every
//! fixed-name base layer and every allocated pack layer/icon, so a live edit
//! can rewrite exactly the affected GPU layer without re-deriving allocation
//! from a changed manifest.
//!
//! Mutation here is pure data only: no GPU or filesystem access. A change is
//! reported only when the *visible* bytes differ, which keeps shadowed-pack
//! edits and byte-identical writes from uploading anything. Invalid layer
//! indices debug-assert but never panic in release builds.

use std::collections::{BTreeMap, BTreeSet};

use crate::content::spec::PackAsset;
use crate::content::{names, ContentRegistry, LayerSource};
use crate::pack::spec::PackSpec;
use crate::pack::{LoadedPack, AUTHORABLE_TEX_LAYERS, LAYER_FILES, MOB_LAYER_FILES};
use crate::render::texture::{
    generate_all_texture_layers_rgba, MOB_AUTHORABLE_TEX_LAYERS, MOB_LAYER_BASE,
};

/// Bytes per 16x16 RGBA texture layer.
const LAYER_BYTES: usize = 16 * 16 * 4;

/// Frozen identity of the content definitions that produced an allocation.
///
/// Equal fingerprints mean "same digest and same pack layer/icon counts", so
/// a manifest edit that leaves the allocation untouched cannot force a
/// restart. A differing fingerprint means content or allocation changed:
/// callers must require a restart and keep every live layer and index.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContentFingerprint {
    /// Content digest of the registry that produced the allocation.
    pub digest: u64,
    /// Number of allocated pack block-face layers.
    pub pack_layers: usize,
    /// Number of allocated pack icon layers.
    pub pack_icons: usize,
}

impl ContentFingerprint {
    /// Snapshot the frozen identity of `registry`.
    pub fn from_registry(registry: &ContentRegistry) -> Self {
        Self {
            digest: registry.digest(),
            pack_layers: registry.pack_layers().len(),
            pack_icons: registry.pack_icons().len(),
        }
    }
}

/// New authored state for one fixed-name base layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerInput {
    /// The file now decodes to these resampled 16x16 RGBA bytes.
    Bytes(Vec<u8>),
    /// The file is gone; the layer falls back to the stack below it.
    Absent,
}

/// A visible base-layer change that needs exactly one GPU upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerChange {
    /// Texture-array layer whose visible bytes changed.
    pub layer: u8,
    /// New visible 16x16 RGBA bytes.
    pub bytes: Vec<u8>,
}

/// A visible built-in item sprite override change.
///
/// `bytes` and `source` are both `None` when the visible result is procedural,
/// and both `Some` when an authored override wins the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinIconChange {
    /// Built-in short name (`apple`, `wood_pickaxe`, ...).
    pub name: String,
    /// New visible 16x16 RGBA bytes, or `None` for procedural art.
    pub bytes: Option<Vec<u8>>,
    /// Authored winner, or `None` when procedural art is visible.
    pub source: Option<LayerSource>,
}

/// Result of re-reading one pack's whole `[builtin_icons]` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinIconTableChange {
    /// One entry per name whose *visible* bytes changed, including removals.
    pub changes: Vec<BuiltinIconChange>,
    /// Whether the authored name -> path map changed at all.
    pub paths_changed: bool,
}

/// Resolved pack state with frozen provenance for live texture reload.
///
/// `base.layers` is the live visible 48-layer presentation. `pack_layers` and
/// `pack_icons` are mutable copies of the frozen registry allocation; mutating
/// them never changes layer indices. `base_sources`, `pack_layer_sources` and
/// `icon_sources` name the authored `(folder, path)` behind each layer.
#[derive(Clone, Debug)]
pub struct PackPresentation {
    /// Resolved presentation; `layers` is kept in sync by live edits.
    pub base: LoadedPack,
    /// Authored specs in stack order, retained so live edits can re-merge.
    pub specs: Vec<PackSpec>,
    /// Procedural fallback bytes, one per base layer.
    pub procedural: Vec<Vec<u8>>,
    /// Copy of the registry's pack block-face layers (`48 + i`).
    pub pack_layers: Vec<Vec<u8>>,
    /// Copy of the registry's pack icons (`32 + i`).
    pub pack_icons: Vec<Vec<u8>>,
    /// Provenance per base layer; `None` for procedural or reserved layers.
    pub base_sources: Vec<Option<LayerSource>>,
    /// Provenance per `pack_layers` slot.
    pub pack_layer_sources: Vec<LayerSource>,
    /// Provenance per `pack_icons` slot.
    pub icon_sources: Vec<LayerSource>,
    /// Visible merged built-in icon override bytes, keyed by short name.
    pub builtin_icons: BTreeMap<String, Vec<u8>>,
    /// Visible winning provenance per built-in icon override.
    pub builtin_icon_sources: BTreeMap<String, LayerSource>,
    /// Per spec authored short name -> pack-relative path (stack order).
    builtin_icon_paths: Vec<BTreeMap<String, String>>,
    /// Per spec loaded 16x16 RGBA override bytes (stack order).
    authored_builtin_icons: Vec<BTreeMap<String, Vec<u8>>>,
    /// Frozen allocation identity captured at build time.
    pub fingerprint: ContentFingerprint,
}

impl PackPresentation {
    /// Build a presentation from resolved pack data and the frozen registry.
    ///
    /// `base.layers` is recomputed from the procedural layer set folded over
    /// `specs` in stack order, so it is always identical to
    /// [`crate::pack::spec::resolve_presentation`] for the same stack. Other
    /// presentation fields (logo, tint, description, name, warnings) are kept
    /// from `base`.
    pub fn build(base: LoadedPack, specs: Vec<PackSpec>, registry: &ContentRegistry) -> Self {
        let procedural = generate_all_texture_layers_rgba();
        let mut layers = procedural.clone();
        let mut base_sources = vec![None; procedural.len()];

        for layer in authorable_base_layers() {
            let (bytes, source) = visible_base_layer(&specs, &procedural, layer);
            layers[usize::from(layer)] = bytes;
            base_sources[usize::from(layer)] = source;
        }

        let mut base = base;
        base.layers = layers;

        // Presentation-only built-in sprite overrides: paths come from the
        // authored spec table, bytes from the already-loaded 16x16 assets.
        // Invalid names are ignored defensively; the parser already rejects
        // them with a diagnostic.
        let builtin_icon_paths: Vec<BTreeMap<String, String>> = specs
            .iter()
            .map(|spec| {
                spec.content
                    .builtin_icons
                    .iter()
                    .filter(|(name, _)| names::builtin_item_sprite_id_by_name(name).is_some())
                    .map(|(name, path)| (name.clone(), path.clone()))
                    .collect()
            })
            .collect();
        let authored_builtin_icons: Vec<BTreeMap<String, Vec<u8>>> = specs
            .iter()
            .zip(&builtin_icon_paths)
            .map(|(spec, paths)| {
                paths
                    .iter()
                    .filter_map(|(name, path)| {
                        spec.content
                            .assets
                            .get(path)
                            .filter(|asset| asset.rgba.len() == LAYER_BYTES)
                            .map(|asset| (name.clone(), asset.rgba.clone()))
                    })
                    .collect()
            })
            .collect();
        let (builtin_icons, builtin_icon_sources) =
            merge_builtin_icon_stack(&specs, &builtin_icon_paths, &authored_builtin_icons);

        Self {
            base,
            specs,
            procedural,
            pack_layers: registry.pack_layers().to_vec(),
            pack_icons: registry.pack_icons().to_vec(),
            base_sources,
            pack_layer_sources: registry.pack_layer_sources().to_vec(),
            icon_sources: registry.pack_icon_sources().to_vec(),
            builtin_icons,
            builtin_icon_sources,
            builtin_icon_paths,
            authored_builtin_icons,
            fingerprint: ContentFingerprint::from_registry(registry),
        }
    }

    /// Authored `(folder, name, path)` built-in icon overrides in stack order.
    ///
    /// Missing or malformed assets still list their authored path so a watcher
    /// can notice the file appearing later without restarting.
    pub fn builtin_icon_paths(&self) -> impl Iterator<Item = (&str, &str, &str)> + '_ {
        self.specs
            .iter()
            .enumerate()
            .flat_map(move |(index, spec)| {
                self.builtin_icon_paths[index]
                    .iter()
                    .map(move |(name, path)| (spec.folder.as_str(), name.as_str(), path.as_str()))
            })
    }

    /// Apply new authored state to one pack's built-in icon override.
    ///
    /// Only specs whose folder matches *and* whose authored table contains
    /// `name` are updated; the visible winner is then re-merged across the
    /// stack. Returns `Some` only when the visible bytes changed, so a shadowed
    /// lower-pack edit keeps its bytes for a later reveal but requests no
    /// upload. Deleting an override reveals the next pack or procedural art
    /// (`bytes: None`). Invalid names are ignored without panicking.
    pub fn apply_builtin_icon(
        &mut self,
        folder: &str,
        name: &str,
        input: LayerInput,
    ) -> Option<BuiltinIconChange> {
        if names::builtin_item_sprite_id_by_name(name).is_none() {
            debug_assert!(false, "`{name}` is not an overridable builtin sprite");
            return None;
        }

        let mut matched = false;
        for (index, spec) in self.specs.iter().enumerate() {
            if spec.folder != folder || !self.builtin_icon_paths[index].contains_key(name) {
                continue;
            }
            match &input {
                LayerInput::Bytes(bytes) => {
                    self.authored_builtin_icons[index].insert(name.to_string(), bytes.clone());
                }
                LayerInput::Absent => {
                    self.authored_builtin_icons[index].remove(name);
                }
            }
            matched = true;
        }
        if !matched {
            return None;
        }

        let (bytes, source) = visible_builtin_icon(
            &self.specs,
            &self.builtin_icon_paths,
            &self.authored_builtin_icons,
            name,
        );
        let changed = self.builtin_icons.get(name) != bytes.as_ref();
        set_visible_builtin_icon(
            &mut self.builtin_icons,
            &mut self.builtin_icon_sources,
            name,
            &bytes,
            &source,
        );
        if !changed {
            return None;
        }
        Some(BuiltinIconChange {
            name: name.to_string(),
            bytes,
            source,
        })
    }

    /// Replace one pack's whole `[builtin_icons]` table and its loaded bytes.
    ///
    /// This is the manifest add/remove/change seam. Every matching spec gets
    /// the new path map (invalid names filtered defensively) and its authored
    /// bytes rebuilt from `assets`; then every name is re-merged across the
    /// stack. Returns one change per name whose visible bytes changed, with no
    /// duplicates. Allocation state (`base.layers`, `pack_layers`,
    /// `pack_icons`, block/mob layers, fingerprint) is untouched.
    pub fn apply_builtin_icon_table(
        &mut self,
        folder: &str,
        paths: &BTreeMap<String, String>,
        assets: &BTreeMap<String, PackAsset>,
    ) -> BuiltinIconTableChange {
        let sanitized: BTreeMap<String, String> = paths
            .iter()
            .filter(|(name, _)| names::builtin_item_sprite_id_by_name(name).is_some())
            .map(|(name, path)| (name.clone(), path.clone()))
            .collect();

        let mut paths_changed = false;
        let mut matched = false;
        for (index, spec) in self.specs.iter().enumerate() {
            if spec.folder != folder {
                continue;
            }
            matched = true;
            if self.builtin_icon_paths[index] != sanitized {
                paths_changed = true;
            }
            self.builtin_icon_paths[index] = sanitized.clone();
            self.authored_builtin_icons[index] = sanitized
                .iter()
                .filter_map(|(name, path)| {
                    assets
                        .get(path)
                        .filter(|asset| asset.rgba.len() == LAYER_BYTES)
                        .map(|asset| (name.clone(), asset.rgba.clone()))
                })
                .collect();
        }
        if !matched {
            return BuiltinIconTableChange::default();
        }

        // Names from every spec plus currently visible names, so a removal
        // from the only authoring pack still reports a procedural fallback.
        let mut names: BTreeSet<String> = BTreeSet::new();
        for map in &self.builtin_icon_paths {
            names.extend(map.keys().cloned());
        }
        names.extend(self.builtin_icons.keys().cloned());

        let mut changes = Vec::new();
        for name in names {
            let (bytes, source) = visible_builtin_icon(
                &self.specs,
                &self.builtin_icon_paths,
                &self.authored_builtin_icons,
                &name,
            );
            let changed = self.builtin_icons.get(&name) != bytes.as_ref();
            set_visible_builtin_icon(
                &mut self.builtin_icons,
                &mut self.builtin_icon_sources,
                &name,
                &bytes,
                &source,
            );
            if changed {
                changes.push(BuiltinIconChange {
                    name,
                    bytes,
                    source,
                });
            }
        }

        BuiltinIconTableChange {
            changes,
            paths_changed,
        }
    }

    /// Apply new authored state to one pack's fixed-name base layer.
    ///
    /// Only the spec(s) with `folder` are updated. The visible bytes are then
    /// re-merged across the whole stack (later packs win) and the provenance
    /// is refreshed. Returns `Some` only when the visible bytes changed, so a
    /// shadowed lower-pack edit is a no-op for uploads even though the
    /// authored state is retained for a later reveal.
    ///
    /// Layers outside the authorable block/mob ranges (reserved placeholders
    /// and spares) debug-assert and return `None`.
    pub fn apply_base_layer(
        &mut self,
        folder: &str,
        layer: u8,
        input: LayerInput,
    ) -> Option<LayerChange> {
        if layer_file(layer).is_none() {
            debug_assert!(false, "layer {layer} is not an authorable base layer");
            return None;
        }

        let mut matched = false;
        for spec in &mut self.specs {
            if spec.folder != folder {
                continue;
            }
            let slot = if usize::from(layer) < AUTHORABLE_TEX_LAYERS {
                spec.block_layers.get_mut(usize::from(layer))
            } else {
                let offset = usize::from(layer) - MOB_LAYER_BASE as usize;
                spec.mob_layers.get_mut(offset)
            };
            if let Some(slot) = slot {
                *slot = match &input {
                    LayerInput::Bytes(bytes) => Some(bytes.clone()),
                    LayerInput::Absent => None,
                };
                matched = true;
            }
        }

        if !matched {
            return None;
        }

        let index = usize::from(layer);
        let (bytes, source) = visible_base_layer(&self.specs, &self.procedural, layer);
        let changed = self.base.layers[index] != bytes;
        // Provenance follows the visible winner even when the bytes happen to
        // be identical (for example an upper pack now authors the same art).
        self.base_sources[index] = source;
        if !changed {
            return None;
        }
        self.base.layers[index] = bytes.clone();
        Some(LayerChange { layer, bytes })
    }

    /// Replace one allocated pack layer's bytes in place.
    ///
    /// The slot is frozen (`gpu_layer = PACK_LAYER_BASE + slot`), so this can
    /// never move allocation. Out-of-range slots and non-1024-byte inputs are
    /// rejected without allocating or panicking. Returns whether the bytes
    /// changed.
    pub fn apply_pack_asset(&mut self, slot: usize, rgba: &[u8]) -> bool {
        if slot >= self.pack_layers.len() || rgba.len() != LAYER_BYTES {
            return false;
        }
        if self.pack_layers[slot] == rgba {
            return false;
        }
        self.pack_layers[slot] = rgba.to_vec();
        true
    }
}

/// Every authorable fixed-name base layer: 0..=24 and 32..=44.
fn authorable_base_layers() -> impl Iterator<Item = u8> {
    (0..AUTHORABLE_TEX_LAYERS as u8)
        .chain((MOB_LAYER_BASE as u8)..(MOB_LAYER_BASE as u8 + MOB_AUTHORABLE_TEX_LAYERS as u8))
}

/// The authored file name for an authorable base layer, else `None`.
fn layer_file(layer: u8) -> Option<&'static str> {
    if usize::from(layer) < AUTHORABLE_TEX_LAYERS {
        return LAYER_FILES.get(usize::from(layer)).copied();
    }
    let offset = usize::from(layer).checked_sub(MOB_LAYER_BASE as usize)?;
    MOB_LAYER_FILES.get(offset).copied()
}

/// Merge every built-in icon override name across the stack.
fn merge_builtin_icon_stack(
    specs: &[PackSpec],
    paths: &[BTreeMap<String, String>],
    authored: &[BTreeMap<String, Vec<u8>>],
) -> (BTreeMap<String, Vec<u8>>, BTreeMap<String, LayerSource>) {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for map in paths {
        names.extend(map.keys().map(String::as_str));
    }

    let mut visible = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for name in names {
        let (bytes, source) = visible_builtin_icon(specs, paths, authored, name);
        if let (Some(bytes), Some(source)) = (bytes, source) {
            visible.insert(name.to_string(), bytes);
            sources.insert(name.to_string(), source);
        }
    }
    (visible, sources)
}

/// Visible bytes and provenance for one built-in icon name across the stack.
///
/// Later packs win only when they carry loaded bytes: a missing or malformed
/// asset contributes nothing, so a lower valid override stays visible.
fn visible_builtin_icon(
    specs: &[PackSpec],
    paths: &[BTreeMap<String, String>],
    authored: &[BTreeMap<String, Vec<u8>>],
    name: &str,
) -> (Option<Vec<u8>>, Option<LayerSource>) {
    let mut winner: Option<(&Vec<u8>, &str, &str)> = None;
    for (index, spec) in specs.iter().enumerate() {
        if let Some(bytes) = authored[index].get(name) {
            let path = paths[index]
                .get(name)
                .map(String::as_str)
                .unwrap_or_default();
            winner = Some((bytes, spec.folder.as_str(), path));
        }
    }

    match winner {
        Some((bytes, folder, path)) => (
            Some(bytes.clone()),
            Some(LayerSource {
                folder: folder.to_string(),
                path: path.to_string(),
            }),
        ),
        None => (None, None),
    }
}

/// Store (or clear) the visible bytes and provenance for one built-in icon.
fn set_visible_builtin_icon(
    visible: &mut BTreeMap<String, Vec<u8>>,
    sources: &mut BTreeMap<String, LayerSource>,
    name: &str,
    bytes: &Option<Vec<u8>>,
    source: &Option<LayerSource>,
) {
    match (bytes, source) {
        (Some(bytes), Some(source)) => {
            visible.insert(name.to_string(), bytes.clone());
            sources.insert(name.to_string(), source.clone());
        }
        _ => {
            visible.remove(name);
            sources.remove(name);
        }
    }
}

/// This spec's authored bytes for `layer`, or `None` when it does not
/// override the layer (or the layer is reserved).
fn authored_base_bytes(spec: &PackSpec, layer: u8) -> Option<&Vec<u8>> {
    if usize::from(layer) < AUTHORABLE_TEX_LAYERS {
        return spec
            .block_layers
            .get(usize::from(layer))
            .and_then(Option::as_ref);
    }
    let offset = usize::from(layer).checked_sub(MOB_LAYER_BASE as usize)?;
    spec.mob_layers.get(offset).and_then(Option::as_ref)
}

/// Merge the visible bytes and provenance for one base layer across the stack.
fn visible_base_layer(
    specs: &[PackSpec],
    procedural: &[Vec<u8>],
    layer: u8,
) -> (Vec<u8>, Option<LayerSource>) {
    let mut winner: Option<(&PackSpec, &Vec<u8>)> = None;
    for spec in specs {
        if let Some(bytes) = authored_base_bytes(spec, layer) {
            winner = Some((spec, bytes));
        }
    }

    match winner {
        Some((spec, bytes)) => (
            bytes.clone(),
            Some(LayerSource {
                folder: spec.folder.clone(),
                path: layer_file(layer)
                    .expect("an authored base layer always has a fixed file name")
                    .to_string(),
            }),
        ),
        None => (procedural[usize::from(layer)].clone(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::content::registry::{build_registry, PackIdentity};
    use crate::content::spec::{BlockEntry, BlockTextures, PackAsset, PackContentSpec};
    use crate::pack::manifest;
    use crate::pack::resample;
    use crate::pack::sound::SoundSpec;
    use crate::pack::spec::{load_spec_from_dir, resolve_presentation};
    use crate::pack::MOB_LAYER_FILES;
    use crate::render::texture::NUM_TEX_LAYERS;

    fn procedural() -> Vec<Vec<u8>> {
        generate_all_texture_layers_rgba()
    }

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

    fn spec_with_layer(folder: &str, layer: usize, bytes: &[u8]) -> PackSpec {
        let mut spec = blank_spec(folder);
        spec.block_layers[layer] = Some(bytes.to_vec());
        spec
    }

    fn builtin_spec(folder: &str, icons: &[(&str, &str)], assets: &[(&str, u8)]) -> PackSpec {
        let mut spec = blank_spec(folder);
        for (name, path) in icons {
            spec.content
                .builtin_icons
                .insert((*name).to_string(), (*path).to_string());
        }
        for (path, byte) in assets {
            spec.content.assets.insert(
                (*path).to_string(),
                PackAsset {
                    rgba: bytes(*byte),
                    source_side: 16,
                },
            );
        }
        spec
    }

    fn bytes(byte: u8) -> Vec<u8> {
        vec![byte; LAYER_BYTES]
    }

    fn presentation(specs: Vec<PackSpec>) -> PackPresentation {
        let base = resolve_presentation(&specs);
        PackPresentation::build(base, specs, ContentRegistry::builtin())
    }

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

    fn write_png(path: &Path, dim: u32) {
        let data = gradient(dim, dim);
        let file = fs::File::create(path).expect("create fixture png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), dim, dim);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        writer
            .write_image_data(&data)
            .expect("write png image data");
    }

    fn authored_layer(dir: &Path, file: &str, dim: u32) -> Vec<u8> {
        write_png(&dir.join(file), dim);
        resample::resample_to_16(&gradient(dim, dim), dim)
    }

    /// Build a one-block v2 registry with one allocated pack block-face layer.
    fn registry_with_asset(path: &str, byte: u8) -> ContentRegistry {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(BlockEntry {
            id: "mymod:block".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some(path.to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.0,
            drop: None,
            tool: None,
            stack_limit: 64,
        });
        spec.assets.insert(
            path.to_string(),
            PackAsset {
                rgba: bytes(byte),
                source_side: 16,
            },
        );
        let identity = PackIdentity {
            folder: "assets".to_string(),
            version: Some("1.0.0".to_string()),
        };
        build_registry(ContentRegistry::builtin(), &[(identity, spec)]).0
    }

    #[test]
    fn later_pack_wins_and_sources_name_the_winner() {
        let first = spec_with_layer("first", 3, &bytes(1));
        let second = spec_with_layer("second", 3, &bytes(2));

        let presentation = presentation(vec![first, second]);

        assert_eq!(presentation.base.layers[3], bytes(2));
        assert_eq!(
            presentation.base_sources[3],
            Some(LayerSource {
                folder: "second".to_string(),
                path: "stone.png".to_string(),
            })
        );
        // Untouched authorable layers stay procedural and unsourced.
        assert_eq!(presentation.base.layers[2], presentation.procedural[2]);
        assert_eq!(presentation.base_sources[2], None);
    }

    #[test]
    fn reserved_layers_have_no_source() {
        let presentation = presentation(vec![spec_with_layer("p", 3, &bytes(1))]);

        // Layer 25 is built-in socket-marker art: no pack may override it.
        assert_eq!(presentation.base_sources[25], None, "layer 25");
        assert_eq!(
            presentation.base.layers[25], presentation.procedural[25],
            "layer 25 must stay the built-in marker art"
        );

        for layer in 0..presentation.base_sources.len() {
            let reserved = (26..MOB_LAYER_BASE as usize).contains(&layer)
                || layer >= MOB_LAYER_BASE as usize + MOB_AUTHORABLE_TEX_LAYERS;
            if reserved {
                assert_eq!(presentation.base_sources[layer], None, "layer {layer}");
                assert_eq!(
                    presentation.base.layers[layer],
                    presentation.procedural[layer]
                );
            }
        }
    }

    #[test]
    fn shadowed_lower_pack_edit_is_a_no_op_until_the_winner_disappears() {
        let first = spec_with_layer("first", 3, &bytes(1));
        let second = spec_with_layer("second", 3, &bytes(2));
        let mut presentation = presentation(vec![first, second]);

        // Updating the shadowed pack keeps the visible bytes, so no upload.
        let shadowed = bytes(9);
        assert_eq!(
            presentation.apply_base_layer("first", 3, LayerInput::Bytes(shadowed.clone())),
            None
        );
        assert_eq!(presentation.base.layers[3], bytes(2));
        assert_eq!(
            presentation.base_sources[3].as_ref().unwrap().folder,
            "second"
        );

        // Deleting the winner reveals the retained lower-pack edit live.
        let change = presentation
            .apply_base_layer("second", 3, LayerInput::Absent)
            .expect("revealing the lower pack must produce a change");
        assert_eq!(change.layer, 3);
        assert_eq!(change.bytes, shadowed);
        assert_eq!(presentation.base.layers[3], shadowed);
        assert_eq!(
            presentation.base_sources[3],
            Some(LayerSource {
                folder: "first".to_string(),
                path: "stone.png".to_string(),
            })
        );
    }

    #[test]
    fn deletion_reveals_procedural_art_and_clears_provenance() {
        let spec = spec_with_layer("only", 3, &bytes(7));
        let procedural = procedural();
        let mut presentation = presentation(vec![spec]);

        let change = presentation
            .apply_base_layer("only", 3, LayerInput::Absent)
            .expect("deleting the only author must reveal procedural art");
        assert_eq!(change.layer, 3);
        assert_eq!(change.bytes, procedural[3]);
        assert_eq!(presentation.base.layers[3], procedural[3]);
        assert_eq!(presentation.base_sources[3], None);
    }

    #[test]
    fn fixed_name_and_mob_edits_return_the_changed_bytes() {
        let mut presentation = presentation(vec![blank_spec("p")]);
        let stone = bytes(0x33);
        let change = presentation
            .apply_base_layer("p", 3, LayerInput::Bytes(stone.clone()))
            .expect("new authored stone art must change layer 3");
        assert_eq!(
            change,
            LayerChange {
                layer: 3,
                bytes: stone.clone()
            }
        );
        assert_eq!(presentation.base.layers[3], stone);
        assert_eq!(
            presentation.base_sources[3],
            Some(LayerSource {
                folder: "p".to_string(),
                path: LAYER_FILES[3].to_string(),
            })
        );

        // The same bytes again change nothing.
        assert_eq!(
            presentation.apply_base_layer("p", 3, LayerInput::Bytes(stone)),
            None
        );

        // Mob layers use MOB_LAYER_FILES and the 32..=44 range.
        let pig = bytes(0x44);
        let mob_layer = MOB_LAYER_BASE as u8;
        let change = presentation
            .apply_base_layer("p", mob_layer, LayerInput::Bytes(pig.clone()))
            .expect("new authored pig art must change layer 32");
        assert_eq!(change.layer, mob_layer);
        assert_eq!(change.bytes, pig);
        assert_eq!(presentation.base.layers[usize::from(mob_layer)], pig);
        assert_eq!(
            presentation.base_sources[usize::from(mob_layer)]
                .as_ref()
                .map(|source| source.path.as_str()),
            Some(MOB_LAYER_FILES[0])
        );
    }

    #[test]
    fn pack_asset_slot_mutation_and_fingerprint_stability() {
        let registry = registry_with_asset("ore.png", 0x11);
        assert_eq!(registry.pack_layers().len(), 1);

        let specs = vec![blank_spec("assets")];
        let base = resolve_presentation(&specs);
        let mut presentation = PackPresentation::build(base, specs, &registry);
        let fingerprint = presentation.fingerprint;
        assert_eq!(fingerprint, ContentFingerprint::from_registry(&registry));
        assert_eq!(presentation.pack_layers, vec![bytes(0x11)]);
        assert_eq!(
            presentation.pack_layer_sources,
            vec![LayerSource {
                folder: "assets".to_string(),
                path: "ore.png".to_string(),
            }]
        );

        let recolored = bytes(0x77);
        assert!(presentation.apply_pack_asset(0, &recolored));
        assert_eq!(presentation.pack_layers[0], recolored);
        // Asset bytes are not part of the digest or the allocation counts.
        assert_eq!(presentation.fingerprint, fingerprint);

        assert!(
            !presentation.apply_pack_asset(0, &recolored),
            "same bytes must not request an upload"
        );
        assert!(
            !presentation.apply_pack_asset(1, &recolored),
            "out-of-range slots are rejected, never allocated"
        );
        assert!(
            !presentation.apply_pack_asset(0, &[0u8; 4]),
            "non-1024-byte inputs are rejected"
        );
        assert_eq!(presentation.fingerprint, fingerprint);
    }

    #[test]
    fn base_layers_match_resolve_presentation_for_the_same_stack() {
        let root = tempfile::tempdir().unwrap();
        let earlier = pack_dir(&root, "earlier");
        let later = pack_dir(&root, "later");
        fs::write(earlier.join("pack.toml"), "name = \"Earlier\"\n").unwrap();
        fs::write(later.join("pack.toml"), "name = \"Later\"\n").unwrap();

        authored_layer(&earlier, LAYER_FILES[3], 32);
        authored_layer(&earlier, MOB_LAYER_FILES[0], 16);
        authored_layer(&later, LAYER_FILES[3], 64);

        let specs = vec![load_spec_from_dir(&earlier), load_spec_from_dir(&later)];
        let resolved = resolve_presentation(&specs);
        let presentation = PackPresentation::build(
            resolve_presentation(&specs),
            specs.clone(),
            ContentRegistry::builtin(),
        );

        assert_eq!(presentation.base.layers.len(), NUM_TEX_LAYERS as usize);
        assert_eq!(presentation.base.layers, resolved.layers);
        assert_eq!(presentation.base.display_name, "Later");
    }

    #[test]
    fn builtin_icons_merge_later_packs_and_missing_assets_fall_through() {
        let first = builtin_spec(
            "first",
            &[("apple", "apple.png"), ("wood_pickaxe", "pickaxe.png")],
            &[("apple.png", 0x11), ("pickaxe.png", 0x22)],
        );
        let second = builtin_spec(
            "second",
            &[("apple", "apple2.png"), ("wood_pickaxe", "missing.png")],
            &[("apple2.png", 0x33)],
        );

        let presentation = presentation(vec![first, second]);

        assert_eq!(presentation.builtin_icons["apple"], bytes(0x33));
        assert_eq!(
            presentation.builtin_icon_sources["apple"],
            LayerSource {
                folder: "second".to_string(),
                path: "apple2.png".to_string(),
            }
        );
        // The later pack's missing asset contributes no bytes, so the lower
        // valid override stays visible.
        assert_eq!(presentation.builtin_icons["wood_pickaxe"], bytes(0x22));
        assert_eq!(
            presentation.builtin_icon_sources["wood_pickaxe"].folder,
            "first"
        );
        let authored: Vec<(&str, &str, &str)> = presentation.builtin_icon_paths().collect();
        assert_eq!(
            authored,
            vec![
                ("first", "apple", "apple.png"),
                ("first", "wood_pickaxe", "pickaxe.png"),
                ("second", "apple", "apple2.png"),
                ("second", "wood_pickaxe", "missing.png"),
            ]
        );
    }

    #[test]
    fn builtin_icon_missing_or_malformed_asset_keeps_procedural() {
        let missing = builtin_spec("missing", &[("apple", "apple.png")], &[]);
        let missing_presentation = presentation(vec![missing]);
        assert!(!missing_presentation.builtin_icons.contains_key("apple"));
        assert!(!missing_presentation
            .builtin_icon_sources
            .contains_key("apple"));

        let mut malformed = blank_spec("malformed");
        malformed
            .content
            .builtin_icons
            .insert("apple".to_string(), "apple.png".to_string());
        malformed.content.assets.insert(
            "apple.png".to_string(),
            PackAsset {
                rgba: vec![0x55; 100],
                source_side: 16,
            },
        );
        let presentation = presentation(vec![malformed]);
        assert!(!presentation.builtin_icons.contains_key("apple"));
        assert!(!presentation.builtin_icon_sources.contains_key("apple"));
    }

    #[test]
    fn apply_builtin_icon_adds_updates_and_deletes_a_live_override() {
        let spec = builtin_spec("p", &[("apple", "apple.png")], &[]);
        let mut presentation = presentation(vec![spec]);

        let apple = bytes(0x21);
        let change = presentation
            .apply_builtin_icon("p", "apple", LayerInput::Bytes(apple.clone()))
            .expect("a newly authored apple must change the visible override");
        assert_eq!(
            change,
            BuiltinIconChange {
                name: "apple".to_string(),
                bytes: Some(apple.clone()),
                source: Some(LayerSource {
                    folder: "p".to_string(),
                    path: "apple.png".to_string(),
                }),
            }
        );
        assert_eq!(presentation.builtin_icons["apple"], apple);

        // Identical bytes must not request an upload.
        assert_eq!(
            presentation.apply_builtin_icon("p", "apple", LayerInput::Bytes(apple)),
            None
        );

        let updated = bytes(0x22);
        let change = presentation
            .apply_builtin_icon("p", "apple", LayerInput::Bytes(updated.clone()))
            .expect("changed bytes must produce a change");
        assert_eq!(change.bytes, Some(updated));

        let change = presentation
            .apply_builtin_icon("p", "apple", LayerInput::Absent)
            .expect("deleting the only override must reveal procedural art");
        assert_eq!(change.bytes, None);
        assert_eq!(change.source, None);
        assert!(!presentation.builtin_icons.contains_key("apple"));
        assert!(!presentation.builtin_icon_sources.contains_key("apple"));
    }

    #[test]
    fn shadowed_builtin_icon_edit_returns_none_until_revealed() {
        let first = builtin_spec("first", &[("apple", "apple.png")], &[("apple.png", 0x11)]);
        let second = builtin_spec(
            "second",
            &[("apple", "apple2.png")],
            &[("apple2.png", 0x22)],
        );
        let mut presentation = presentation(vec![first, second]);

        let shadowed = bytes(0x44);
        assert_eq!(
            presentation.apply_builtin_icon("first", "apple", LayerInput::Bytes(shadowed.clone())),
            None
        );
        assert_eq!(presentation.builtin_icons["apple"], bytes(0x22));
        assert_eq!(presentation.builtin_icon_sources["apple"].folder, "second");

        let change = presentation
            .apply_builtin_icon("second", "apple", LayerInput::Absent)
            .expect("removing the winner must reveal the retained lower override");
        assert_eq!(change.bytes, Some(shadowed));
        assert_eq!(change.source.unwrap().folder, "first");
    }

    #[test]
    fn apply_builtin_icon_table_add_remove_and_change() {
        let mut presentation = presentation(vec![builtin_spec("p", &[], &[])]);

        let mut paths = BTreeMap::new();
        paths.insert("apple".to_string(), "apple.png".to_string());
        let mut assets = BTreeMap::new();
        assets.insert(
            "apple.png".to_string(),
            PackAsset {
                rgba: bytes(0x11),
                source_side: 16,
            },
        );

        let change = presentation.apply_builtin_icon_table("p", &paths, &assets);
        assert!(change.paths_changed);
        assert_eq!(change.changes.len(), 1);
        assert_eq!(change.changes[0].name, "apple");
        assert_eq!(change.changes[0].bytes, Some(bytes(0x11)));
        assert_eq!(change.changes[0].source.as_ref().unwrap().path, "apple.png");

        // Re-applying the same table changes no paths and no visible bytes.
        let change = presentation.apply_builtin_icon_table("p", &paths, &assets);
        assert!(!change.paths_changed);
        assert!(change.changes.is_empty());

        // Same path, new bytes: a pixel edit without a manifest path change.
        assets.insert(
            "apple.png".to_string(),
            PackAsset {
                rgba: bytes(0x22),
                source_side: 16,
            },
        );
        let change = presentation.apply_builtin_icon_table("p", &paths, &assets);
        assert!(!change.paths_changed);
        assert_eq!(change.changes.len(), 1);
        assert_eq!(change.changes[0].bytes, Some(bytes(0x22)));

        // Changed path with new bytes.
        paths.insert("apple".to_string(), "apple2.png".to_string());
        assets.insert(
            "apple2.png".to_string(),
            PackAsset {
                rgba: bytes(0x33),
                source_side: 16,
            },
        );
        let change = presentation.apply_builtin_icon_table("p", &paths, &assets);
        assert!(change.paths_changed);
        assert_eq!(change.changes.len(), 1);
        assert_eq!(change.changes[0].bytes, Some(bytes(0x33)));
        assert_eq!(
            change.changes[0].source.as_ref().unwrap().path,
            "apple2.png"
        );

        // Removing the entry reports the procedural fallback exactly once.
        let change = presentation.apply_builtin_icon_table("p", &BTreeMap::new(), &BTreeMap::new());
        assert!(change.paths_changed);
        assert_eq!(change.changes.len(), 1);
        assert_eq!(change.changes[0].name, "apple");
        assert_eq!(change.changes[0].bytes, None);
        assert_eq!(change.changes[0].source, None);
        assert!(!presentation.builtin_icons.contains_key("apple"));
    }

    #[test]
    fn apply_builtin_icon_table_leaves_allocation_and_base_layers_untouched() {
        let registry = registry_with_asset("ore.png", 0x11);
        let mut spec = blank_spec("assets");
        spec.block_layers[3] = Some(bytes(0x55));
        spec.content
            .builtin_icons
            .insert("apple".to_string(), "apple.png".to_string());
        let specs = vec![spec];
        let base = resolve_presentation(&specs);
        let mut presentation = PackPresentation::build(base, specs, &registry);

        let fingerprint = presentation.fingerprint;
        let base_layers = presentation.base.layers.clone();
        let pack_layers = presentation.pack_layers.clone();
        let pack_icons = presentation.pack_icons.clone();
        let block_layers = presentation.specs[0].block_layers.clone();
        let mob_layers = presentation.specs[0].mob_layers.clone();

        let mut paths = BTreeMap::new();
        paths.insert("apple".to_string(), "apple.png".to_string());
        let mut assets = BTreeMap::new();
        assets.insert(
            "apple.png".to_string(),
            PackAsset {
                rgba: bytes(0x99),
                source_side: 16,
            },
        );
        let change = presentation.apply_builtin_icon_table("assets", &paths, &assets);
        assert_eq!(change.changes.len(), 1);

        assert_eq!(presentation.fingerprint, fingerprint);
        assert_eq!(presentation.base.layers, base_layers);
        assert_eq!(presentation.pack_layers, pack_layers);
        assert_eq!(presentation.pack_icons, pack_icons);
        assert_eq!(presentation.specs[0].block_layers, block_layers);
        assert_eq!(presentation.specs[0].mob_layers, mob_layers);
    }

    #[test]
    fn invalid_builtin_icon_names_are_ignored() {
        let spec = builtin_spec(
            "p",
            &[("stone", "stone.png"), ("diamond", "diamond.png")],
            &[("stone.png", 0x11), ("diamond.png", 0x22)],
        );
        let mut presentation = presentation(vec![spec]);
        assert!(presentation.builtin_icons.is_empty());
        assert!(presentation.builtin_icon_sources.is_empty());
        assert!(presentation.builtin_icon_paths().next().is_none());

        let mut paths = BTreeMap::new();
        paths.insert("stone".to_string(), "stone.png".to_string());
        let change = presentation.apply_builtin_icon_table("p", &paths, &BTreeMap::new());
        assert!(!change.paths_changed);
        assert!(change.changes.is_empty());
        assert!(presentation.builtin_icons.is_empty());
    }
}
