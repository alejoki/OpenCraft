//! Deterministic pack-stack → neutral catalog builder.
//!
//! [`build_catalog`] folds the ordered pack stack over the supplied base
//! catalog into one immutable [`Catalog`]. Allocation is pure data:
//!
//! * every id is allocated in sorted order at a dense index (blocks after the
//!   base block table, items and tools from 0, tiers after the base tier
//!   table, pack recipes after the base recipe table),
//! * pack block faces and item/tool icons are appended to append-only layer
//!   tables, deduplicated by `(pack folder, authored path)`,
//! * worldgen rules keep authored stack order under an eight-rule cap, and
//! * the FNV-1a 64 digest in [`super::digest`] covers the resulting pack
//!   definitions (never asset bytes), so `--validate-pack` and a game load
//!   agree.
//!
//! Nothing here touches the filesystem, renderer, audio, settings or the pack
//! loader: input is a list of [`PackIdentity`] + [`PackContentSpec`] pairs.

use std::collections::{BTreeMap, BTreeSet};

use crate::spec::{
    face_direction, BlockEntry, BlockPatch, ContentDiagnostic, FaceSource, FoodSpec, ItemEntry,
    PackAsset, PackContentSpec, RecipeEntry, RecipePatch, Severity, TierEntry, ToolEntry, ToolKind,
    WorldgenEntry, WorldgenKind,
};
use crate::vocabulary::{BLOCK_ID_NAMES, MATERIAL_ID_NAMES, TIER_ID_NAMES, TOOL_ID_NAMES};

use super::digest::{compute_digest, face_texture, DigestInputs};
use super::{
    BiomeTag, BlockDef, Catalog, ContentLoadReport, ItemDef, ItemRef, LayerSource, PackIdentity,
    RecipeDef, TierDef, ToolDef, WorldgenRule, PACK_ICON_BASE, PACK_ICON_CAP, PACK_LAYER_BASE,
    PACK_LAYER_CAP, PLACEHOLDER_BLOCK_LAYER, PLACEHOLDER_ICON_LAYER, WORLDGEN_RULE_CAP,
};

/// The five constructible tool kinds, in the frozen `TOOL_ID_NAMES` order.
const KIND_VALUES: [ToolKind; 5] = [
    ToolKind::Pickaxe,
    ToolKind::Axe,
    ToolKind::Shovel,
    ToolKind::Hoe,
    ToolKind::Sword,
];

/// Resolved string → identity lookups shared by every reference field.
struct Resolver<'a> {
    block_ids: &'a BTreeMap<&'a str, u16>,
    item_indices: &'a BTreeMap<&'a str, u16>,
    tool_indices: &'a BTreeMap<&'a str, u16>,
}

impl Resolver<'_> {
    /// Resolve a block reference: pack block first, then a built-in name.
    fn block(&self, name: &str) -> Option<u16> {
        self.block_ids
            .get(name)
            .copied()
            .or_else(|| builtin_block_raw(name))
    }

    /// Resolve a generic item reference.
    ///
    /// Pack blocks/items/tools shadow nothing (their ids are namespaced), so
    /// the pack tables are consulted first and the frozen built-in vocabulary
    /// is the fallback.
    fn item_ref(&self, name: &str) -> Option<ItemRef> {
        if let Some(block) = self.block_ids.get(name) {
            return Some(ItemRef::Block(*block));
        }
        if let Some(index) = self.item_indices.get(name) {
            return Some(ItemRef::Item(*index));
        }
        if let Some(index) = self.tool_indices.get(name) {
            return Some(ItemRef::Tool(*index));
        }
        builtin_item_ref(name)
    }
}

/// Raw id for a built-in block short name.
fn builtin_block_raw(name: &str) -> Option<u16> {
    BLOCK_ID_NAMES
        .iter()
        .position(|known| *known == name)
        .and_then(|index| u16::try_from(index).ok())
}

/// Neutral item reference for an unqualified built-in name.
fn builtin_item_ref(name: &str) -> Option<ItemRef> {
    if let Some(block) = builtin_block_raw(name) {
        return Some(ItemRef::Block(block));
    }
    if let Some(index) = MATERIAL_ID_NAMES.iter().position(|known| *known == name) {
        return Some(ItemRef::BuiltinMaterial(u8::try_from(index).ok()?));
    }
    builtin_tool_ref(name)
}

/// Neutral item reference for a built-in constructible tool name.
///
/// Index 15 is the wood-only bow; every other index packs `(kind, tier)` as
/// `kind = index % 5`, `tier = index / 5`.
fn builtin_tool_ref(name: &str) -> Option<ItemRef> {
    let index = TOOL_ID_NAMES.iter().position(|known| *known == name)?;
    if index == 15 {
        return Some(ItemRef::BuiltinTool {
            kind: ToolKind::Bow,
            tier: 0,
        });
    }
    let kind = KIND_VALUES[index % 5];
    let tier = u8::try_from(index / 5).ok()?;
    Some(ItemRef::BuiltinTool { kind, tier })
}

/// Fold an ordered pack stack over `base` into a fresh neutral catalog.
///
/// The returned catalog is immutable; the caller owns any `Arc` freezing.
/// A malformed reference records a [`Severity::Error`] diagnostic and falls
/// back to the safe value instead of panicking (the pack parser normally
/// rejects those entries before they reach a spec).
pub fn build_catalog(
    base: &Catalog,
    packs: &[(PackIdentity, PackContentSpec)],
) -> (Catalog, ContentLoadReport) {
    let mut diagnostics = Vec::new();
    let mut entries = 0usize;

    // Winning authored entry per id, sorted by id (BTreeMap) while later packs
    // replace earlier ones in stack order.
    let mut block_entries: BTreeMap<&str, (&str, &BlockEntry)> = BTreeMap::new();
    let mut item_entries: BTreeMap<&str, (&str, &ItemEntry)> = BTreeMap::new();
    let mut tier_entries: BTreeMap<&str, (&str, &TierEntry)> = BTreeMap::new();
    let mut tool_entries: BTreeMap<&str, (&str, &ToolEntry)> = BTreeMap::new();
    let mut recipe_entries: BTreeMap<&str, (&str, &RecipeEntry)> = BTreeMap::new();
    let mut assets: BTreeMap<(String, String), &PackAsset> = BTreeMap::new();
    let mut block_patches: Vec<(&str, &BlockPatch)> = Vec::new();
    let mut recipe_patches: Vec<(&str, &RecipePatch)> = Vec::new();
    let mut worldgen: Vec<(&str, &WorldgenEntry)> = Vec::new();

    for (identity, spec) in packs {
        entries += spec.blocks.len()
            + spec.items.len()
            + spec.tiers.len()
            + spec.tools.len()
            + spec.recipes.len()
            + spec.worldgen.len()
            + spec.block_patches.len()
            + spec.recipe_patches.len();

        let folder = identity.folder.as_str();
        for (path, asset) in &spec.assets {
            assets.insert((folder.to_string(), path.clone()), asset);
        }
        // `[builtin_icons]` is presentation-only: it never allocates a pack
        // icon slot or enters the digest. A path that failed to load is still
        // worth explaining, because the built-in sprite silently stays.
        for (name, path) in &spec.builtin_icons {
            if !spec.assets.contains_key(path) {
                warning(
                    &mut diagnostics,
                    format!("builtin_icon {name}"),
                    format!(
                        "icon `{path}` is not a loaded asset; the built-in sprite stays visible"
                    ),
                );
            }
        }
        for block in &spec.blocks {
            if let Some((previous, _)) = block_entries.insert(block.id.as_str(), (folder, block)) {
                warning(
                    &mut diagnostics,
                    format!("block {}", block.id),
                    format!("pack `{folder}` replaces the definition from pack `{previous}`"),
                );
            }
        }
        for item in &spec.items {
            if let Some((previous, _)) = item_entries.insert(item.id.as_str(), (folder, item)) {
                warning(
                    &mut diagnostics,
                    format!("item {}", item.id),
                    format!("pack `{folder}` replaces the definition from pack `{previous}`"),
                );
            }
        }
        for tier in &spec.tiers {
            if let Some((previous, _)) = tier_entries.insert(tier.id.as_str(), (folder, tier)) {
                warning(
                    &mut diagnostics,
                    format!("tier {}", tier.id),
                    format!("pack `{folder}` replaces the definition from pack `{previous}`"),
                );
            }
        }
        for tool in &spec.tools {
            if let Some((previous, _)) = tool_entries.insert(tool.id.as_str(), (folder, tool)) {
                warning(
                    &mut diagnostics,
                    format!("tool {}", tool.id),
                    format!("pack `{folder}` replaces the definition from pack `{previous}`"),
                );
            }
        }
        for recipe in &spec.recipes {
            if let Some((previous, _)) = recipe_entries.insert(recipe.id.as_str(), (folder, recipe))
            {
                warning(
                    &mut diagnostics,
                    format!("recipe {}", recipe.id),
                    format!("pack `{folder}` replaces the definition from pack `{previous}`"),
                );
            }
        }
        for patch in &spec.block_patches {
            block_patches.push((folder, patch));
        }
        for patch in &spec.recipe_patches {
            recipe_patches.push((folder, patch));
        }
        for rule in &spec.worldgen {
            worldgen.push((folder, rule));
        }
    }

    let base_block_count = u16::try_from(base.blocks().len())
        .expect("base catalog block count exceeds the u16 id space");
    let base_tier_count = u16::try_from(base.tiers().len())
        .expect("base catalog tier count exceeds the u16 id space");

    // Dense block ids start after the supplied base table, in sorted id order.
    let block_ids: BTreeMap<&str, u16> = block_entries
        .keys()
        .enumerate()
        .map(|(index, id)| {
            let raw = u16::try_from(index).expect("pack block count exceeds the u16 id space");
            (*id, base_block_count + raw)
        })
        .collect();
    let item_indices: BTreeMap<&str, u16> = item_entries
        .keys()
        .enumerate()
        .map(|(index, id)| (*id, index as u16))
        .collect();
    let tool_indices: BTreeMap<&str, u16> = tool_entries
        .keys()
        .enumerate()
        .map(|(index, id)| (*id, index as u16))
        .collect();
    let tier_indices: BTreeMap<&str, u16> = tier_entries
        .keys()
        .enumerate()
        .map(|(index, id)| {
            let offset = u16::try_from(index).expect("pack tier count exceeds the u16 id space");
            (*id, base_tier_count + offset)
        })
        .collect();

    let resolver = Resolver {
        block_ids: &block_ids,
        item_indices: &item_indices,
        tool_indices: &tool_indices,
    };

    // ---- blocks ---------------------------------------------------------
    let mut blocks: Vec<BlockDef> = base.blocks().to_vec();

    // Patches apply in stack order; later fields win. They can change
    // presentation and gameplay fields but never `name`/`id` (save vocabulary).
    for (folder, patch) in &block_patches {
        let Some(raw) = builtin_block_raw(&patch.id) else {
            error(
                &mut diagnostics,
                format!("patch {}", patch.id),
                format!("pack `{folder}` patches an unknown block"),
            );
            continue;
        };
        let Some(def) = blocks.get_mut(usize::from(raw)) else {
            continue;
        };
        if def.id != raw {
            continue;
        }
        if let Some(display_name) = &patch.display_name {
            def.display_name = display_name.clone();
        }
        if let Some(hardness) = patch.hardness {
            def.hardness = Some(hardness);
        }
        if let Some(tool) = patch.tool {
            def.tool_class = Some(tool);
        }
        if let Some(limit) = patch.stack_limit {
            def.stack_limit = limit;
        }
        if let Some(drop) = &patch.drop {
            match resolver.item_ref(drop) {
                Some(reference) => def.drop = Some(reference),
                None => error(
                    &mut diagnostics,
                    format!("patch {}", patch.id),
                    format!("unresolved drop item `{drop}`"),
                ),
            }
        }
    }

    // Pack blocks append in sorted id order so raw ids round-trip.
    for (id, (_folder, entry)) in &block_entries {
        let block_id = *block_ids
            .get(id)
            .expect("every collected block id is allocated");
        let drop = match &entry.drop {
            Some(target) => match resolver.item_ref(target) {
                Some(reference) => Some(reference),
                None => {
                    error(
                        &mut diagnostics,
                        format!("block {id}"),
                        format!("unresolved drop item `{target}`"),
                    );
                    None
                }
            },
            None => Some(ItemRef::Block(block_id)),
        };
        blocks.push(BlockDef {
            id: block_id,
            name: (*id).to_string(),
            display_name: entry
                .display_name
                .clone()
                .unwrap_or_else(|| derive_display_name(id)),
            // Assigned by the layer allocator below.
            faces: [PLACEHOLDER_BLOCK_LAYER; 6],
            hardness: Some(entry.hardness),
            drop,
            tool_class: entry.tool,
            stack_limit: entry.stack_limit,
            placeholder: false,
        });
    }

    // ---- items ----------------------------------------------------------
    let mut items: Vec<ItemDef> = Vec::with_capacity(item_entries.len());
    for (id, (_folder, entry)) in &item_entries {
        items.push(ItemDef {
            index: item_indices[id],
            name: (*id).to_string(),
            display_name: entry
                .display_name
                .clone()
                .unwrap_or_else(|| derive_display_name(id)),
            icon_layer: PLACEHOLDER_ICON_LAYER,
            stack_limit: entry.stack_limit,
            food: entry.food.as_ref().map(|food| FoodSpec {
                nutrition: food.nutrition,
                eat_seconds: food.eat_seconds,
            }),
            placeholder: false,
            placeholder_tool: false,
        });
    }

    // ---- tiers ----------------------------------------------------------
    let mut tiers: Vec<TierDef> = base.tiers().to_vec();
    for (id, (_folder, entry)) in &tier_entries {
        tiers.push(TierDef {
            index: tier_indices[id],
            name: (*id).to_string(),
            display_name: entry
                .display_name
                .clone()
                .unwrap_or_else(|| derive_display_name(id)),
            speed: entry.speed,
            durability: entry.durability,
            damage: entry.damage,
        });
    }
    let resolve_tier = |name: &str| -> Option<u16> {
        TIER_ID_NAMES
            .iter()
            .position(|known| *known == name)
            .and_then(|index| u16::try_from(index).ok())
    };

    // ---- tools ----------------------------------------------------------
    let mut tools: Vec<ToolDef> = Vec::with_capacity(tool_entries.len());
    for (id, (_folder, entry)) in &tool_entries {
        let tier = match resolve_tier(&entry.tier)
            .or_else(|| tier_indices.get(entry.tier.as_str()).copied())
        {
            Some(index) => index,
            None => {
                error(
                    &mut diagnostics,
                    format!("tool {id}"),
                    format!("unknown tier `{}`", entry.tier),
                );
                0
            }
        };
        tools.push(ToolDef {
            index: tool_indices[id],
            name: (*id).to_string(),
            display_name: entry
                .display_name
                .clone()
                .unwrap_or_else(|| derive_display_name(id)),
            kind: entry.kind,
            tier,
            icon_layer: PLACEHOLDER_ICON_LAYER,
        });
    }

    // ---- recipes --------------------------------------------------------
    let mut recipes: Vec<RecipeDef> = base.recipes().to_vec();
    let builtin_recipe_count = recipes.len();
    for (folder, patch) in &recipe_patches {
        match recipes.iter_mut().find(|recipe| recipe.name == patch.id) {
            Some(recipe) => recipe.enabled = patch.enabled,
            None => error(
                &mut diagnostics,
                format!("patch {}", patch.id),
                format!("pack `{folder}` patches an unknown recipe"),
            ),
        }
    }
    for (id, (_folder, entry)) in &recipe_entries {
        let Some(output) = resolver.item_ref(&entry.output) else {
            error(
                &mut diagnostics,
                format!("recipe {id}"),
                format!("unresolved output item `{}`", entry.output),
            );
            continue;
        };
        let mut key = BTreeMap::new();
        let mut malformed = false;
        for (character, target) in &entry.key {
            match resolver.item_ref(target) {
                Some(reference) => {
                    key.insert(*character, reference);
                }
                None => {
                    error(
                        &mut diagnostics,
                        format!("recipe {id}"),
                        format!("unresolved key item `{target}`"),
                    );
                    malformed = true;
                }
            }
        }
        if malformed {
            continue;
        }
        let index = u16::try_from(recipes.len()).expect("recipe count exceeds the u16 id space");
        recipes.push(RecipeDef {
            index,
            name: (*id).to_string(),
            pattern: entry.pattern.clone(),
            key,
            output,
            count: entry.count,
            enabled: true,
            placeholder: false,
        });
    }

    // ---- worldgen -------------------------------------------------------
    let mut worldgen_rules: Vec<WorldgenRule> = Vec::new();
    for (folder, entry) in &worldgen {
        if worldgen_rules.len() >= WORLDGEN_RULE_CAP {
            error(
                &mut diagnostics,
                format!("worldgen {folder}"),
                format!("stack already holds {WORLDGEN_RULE_CAP} worldgen rules; rule dropped"),
            );
            continue;
        }
        match &entry.kind {
            WorldgenKind::Ore {
                block,
                replace,
                min_y,
                max_y,
                vein,
                attempts,
            } => {
                let Some(block) = resolver.block(block) else {
                    error(
                        &mut diagnostics,
                        format!("worldgen {folder}"),
                        format!("unresolved ore block `{}`", entry_kind_block(entry)),
                    );
                    continue;
                };
                let Some(replace) = resolver.block(replace) else {
                    error(
                        &mut diagnostics,
                        format!("worldgen {folder}"),
                        "unresolved ore host block".to_string(),
                    );
                    continue;
                };
                worldgen_rules.push(WorldgenRule::Ore {
                    block,
                    replace,
                    min_y: *min_y,
                    max_y: *max_y,
                    vein: *vein,
                    attempts: *attempts,
                });
            }
            WorldgenKind::Patch {
                block,
                replace,
                biomes,
                min_y,
                max_y,
                chance,
            } => {
                let Some(block) = resolver.block(block) else {
                    error(
                        &mut diagnostics,
                        format!("worldgen {folder}"),
                        "unresolved patch block".to_string(),
                    );
                    continue;
                };
                let Some(replace) = resolver.block(replace) else {
                    error(
                        &mut diagnostics,
                        format!("worldgen {folder}"),
                        "unresolved patch host block".to_string(),
                    );
                    continue;
                };
                let mut resolved_biomes = Vec::with_capacity(biomes.len());
                let mut bad_biome = false;
                for name in biomes {
                    match BiomeTag::from_name(name) {
                        Some(biome) => resolved_biomes.push(biome),
                        None => {
                            error(
                                &mut diagnostics,
                                format!("worldgen {folder}"),
                                format!("unknown biome `{name}`"),
                            );
                            bad_biome = true;
                        }
                    }
                }
                if bad_biome || resolved_biomes.is_empty() {
                    continue;
                }
                worldgen_rules.push(WorldgenRule::Patch {
                    block,
                    replace,
                    biomes: resolved_biomes,
                    min_y: *min_y,
                    max_y: *max_y,
                    chance: *chance,
                });
            }
        }
    }

    // ---- block face layers ---------------------------------------------
    let mut pack_layers: Vec<Vec<u8>> = Vec::new();
    let mut pack_layer_sources: Vec<LayerSource> = Vec::new();
    let mut layer_slots: BTreeMap<(String, String), u8> = BTreeMap::new();
    // One diagnostic per (entry, path): `all` resolves to six identical faces
    // and a broken asset must not be reported six times.
    let mut reported_paths: BTreeSet<(String, String)> = BTreeSet::new();
    {
        let mut faces = FaceAllocation {
            assets: &assets,
            pack_layers: &mut pack_layers,
            pack_layer_sources: &mut pack_layer_sources,
            layer_slots: &mut layer_slots,
            reported_paths: &mut reported_paths,
            diagnostics: &mut diagnostics,
        };
        for (id, (folder, entry)) in &block_entries {
            let raw = block_ids[id];
            let def_index = usize::from(raw);
            for face in 0..6 {
                let Some(path) = face_texture(&entry.textures, face) else {
                    error(
                        faces.diagnostics,
                        format!("block {id}"),
                        format!("face {face} has no authored texture"),
                    );
                    blocks[def_index].faces[face] = PLACEHOLDER_BLOCK_LAYER;
                    continue;
                };
                let layer = faces.allocate(folder, path, &format!("block {id}"));
                blocks[def_index].faces[face] = layer.unwrap_or(PLACEHOLDER_BLOCK_LAYER);
            }
        }

        // Loader-synthesized per-direction face plans for built-in blocks.
        // They apply in stack order after pack-defined blocks, so a later
        // pack's face wins without moving any pack-block layer index (still
        // 48+i).
        for (identity, spec) in packs {
            let folder = identity.folder.as_str();
            for entry in &spec.builtin_faces {
                let label = format!("face {}", entry.block);
                let Some(raw) = builtin_block_raw(&entry.block) else {
                    error(
                        faces.diagnostics,
                        label,
                        format!("`{}` is not a built-in block", entry.block),
                    );
                    continue;
                };
                let def_index = usize::from(raw);
                for (face, source) in entry.faces.iter().enumerate() {
                    match source {
                        // Keep the built-in `face_tex_layer` default.
                        None => {}
                        // Existing fixed layer: no allocation, no content
                        // entry.
                        Some(FaceSource::Layer(layer)) => blocks[def_index].faces[face] = *layer,
                        Some(FaceSource::Authored(path)) => {
                            // Each authored face slot is an authored content
                            // entry.
                            entries += 1;
                            let face_entry = match face_direction(face) {
                                Some(direction) => {
                                    format!("face {} {direction}", entry.block)
                                }
                                None => format!("face {} {face}", entry.block),
                            };
                            let layer = faces.allocate(folder, path, &face_entry);
                            blocks[def_index].faces[face] =
                                layer.unwrap_or(PLACEHOLDER_BLOCK_LAYER);
                        }
                    }
                }
            }
        }
    }

    // ---- icons ----------------------------------------------------------
    let mut pack_icons: Vec<Vec<u8>> = Vec::new();
    let mut pack_icon_sources: Vec<LayerSource> = Vec::new();
    let mut icon_slots: BTreeMap<(String, String), u8> = BTreeMap::new();

    let mut icons = IconAllocation {
        assets: &assets,
        pack_icons: &mut pack_icons,
        pack_icon_sources: &mut pack_icon_sources,
        icon_slots: &mut icon_slots,
        diagnostics: &mut diagnostics,
    };

    for (id, (folder, entry)) in &item_entries {
        let layer = icons.allocate(folder, &entry.icon, &format!("item {id}"));
        items[usize::from(item_indices[id])].icon_layer = layer;
    }
    for (id, (folder, entry)) in &tool_entries {
        let layer = icons.allocate(folder, &entry.icon, &format!("tool {id}"));
        tools[usize::from(tool_indices[id])].icon_layer = layer;
    }

    // ---- digest ---------------------------------------------------------
    let digest = compute_digest(&DigestInputs {
        packs,
        blocks: &blocks,
        items: &items,
        tiers: &tiers,
        tools: &tools,
        recipes: &recipes,
        builtin_recipe_count,
        worldgen_rules: &worldgen_rules,
        block_entries: &block_entries,
        item_entries: &item_entries,
        tier_entries: &tier_entries,
        tool_entries: &tool_entries,
        block_patches: &block_patches,
        recipe_patches: &recipe_patches,
    });

    let identity_list: Vec<PackIdentity> =
        packs.iter().map(|(identity, _)| identity.clone()).collect();

    let catalog = Catalog {
        blocks,
        items,
        tiers,
        tools,
        recipes,
        worldgen_rules,
        pack_layers,
        pack_icons,
        pack_layer_sources,
        pack_icon_sources,
        packs: identity_list,
        digest,
    };
    (
        catalog,
        ContentLoadReport {
            entries,
            diagnostics,
        },
    )
}

/// Mutable block-face layer allocation state for one pack-stack build.
///
/// Pack-defined block faces and loader-synthesized built-in faces share this
/// state so `(folder, path)` deduplication, the `48 + i` slot order, the
/// 160-layer budget and provenance all stay identical for both sources.
struct FaceAllocation<'a> {
    assets: &'a BTreeMap<(String, String), &'a PackAsset>,
    pack_layers: &'a mut Vec<Vec<u8>>,
    pack_layer_sources: &'a mut Vec<LayerSource>,
    layer_slots: &'a mut BTreeMap<(String, String), u8>,
    reported_paths: &'a mut BTreeSet<(String, String)>,
    diagnostics: &'a mut Vec<ContentDiagnostic>,
}

impl FaceAllocation<'_> {
    /// Allocate one block-face layer for an authored path.
    ///
    /// Returns `None` when the face must use [`PLACEHOLDER_BLOCK_LAYER`]: a
    /// missing asset or an exhausted layer space records at most one
    /// diagnostic per `(entry, path)` because `all` can feed six faces.
    fn allocate(&mut self, folder: &str, path: &str, entry: &str) -> Option<u8> {
        let key = (folder.to_string(), path.to_string());
        if !self.assets.contains_key(&key) {
            if self
                .reported_paths
                .insert((entry.to_string(), path.to_string()))
            {
                error(
                    self.diagnostics,
                    entry.to_string(),
                    format!("texture `{path}` is not a loaded asset"),
                );
            }
            return None;
        }
        if let Some(existing) = self.layer_slots.get(&key) {
            return Some(*existing);
        }
        let slot = self.pack_layers.len();
        if PACK_LAYER_BASE as usize + slot >= PACK_LAYER_CAP as usize {
            if self
                .reported_paths
                .insert((entry.to_string(), path.to_string()))
            {
                error(
                    self.diagnostics,
                    entry.to_string(),
                    format!(
                        "pack layer space exhausted at {PACK_LAYER_CAP}; `{path}` falls back to layer {PLACEHOLDER_BLOCK_LAYER}"
                    ),
                );
            }
            return None;
        }
        let layer = PACK_LAYER_BASE + slot as u8;
        self.pack_layers.push(self.assets[&key].rgba.clone());
        self.pack_layer_sources.push(LayerSource {
            folder: folder.to_string(),
            path: path.to_string(),
        });
        self.layer_slots.insert(key, layer);
        Some(layer)
    }
}

/// Mutable icon-layer allocation state for one pack-stack build.
struct IconAllocation<'a> {
    assets: &'a BTreeMap<(String, String), &'a PackAsset>,
    pack_icons: &'a mut Vec<Vec<u8>>,
    pack_icon_sources: &'a mut Vec<LayerSource>,
    icon_slots: &'a mut BTreeMap<(String, String), u8>,
    diagnostics: &'a mut Vec<ContentDiagnostic>,
}

impl IconAllocation<'_> {
    /// Allocate one icon layer for an authored icon path.
    ///
    /// Icons are deduplicated by `(pack folder, path)` and appended from
    /// [`PACK_ICON_BASE`] up to [`PACK_ICON_CAP`]. A missing asset or an
    /// exhausted icon space records a diagnostic and returns the built-in
    /// sprite placeholder instead of panicking.
    fn allocate(&mut self, folder: &str, path: &str, entry: &str) -> u8 {
        let key = (folder.to_string(), path.to_string());
        if !self.assets.contains_key(&key) {
            error(
                self.diagnostics,
                entry.to_string(),
                format!("icon `{path}` is not a loaded asset"),
            );
            return PLACEHOLDER_ICON_LAYER;
        }
        if let Some(existing) = self.icon_slots.get(&key) {
            return *existing;
        }
        let slot = self.pack_icons.len();
        if PACK_ICON_BASE as usize + slot >= PACK_ICON_CAP as usize {
            error(
                self.diagnostics,
                entry.to_string(),
                format!(
                    "icon space exhausted at {PACK_ICON_CAP}; `{path}` falls back to layer {PLACEHOLDER_ICON_LAYER}"
                ),
            );
            return PLACEHOLDER_ICON_LAYER;
        }
        let layer = PACK_ICON_BASE + slot as u8;
        self.pack_icons.push(self.assets[&key].rgba.clone());
        self.pack_icon_sources.push(LayerSource {
            folder: folder.to_string(),
            path: path.to_string(),
        });
        self.icon_slots.insert(key, layer);
        layer
    }
}

fn entry_kind_block(entry: &WorldgenEntry) -> &'static str {
    match entry.kind {
        WorldgenKind::Ore { .. } => "ore",
        WorldgenKind::Patch { .. } => "patch",
    }
}

/// Derive a human-readable name from a namespaced id (`mymod:ruby_ore` →
/// `Ruby Ore`). Authored `display_name` always wins.
fn derive_display_name(id: &str) -> String {
    let short = id.rsplit(':').next().unwrap_or(id);
    short
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn error(
    diagnostics: &mut Vec<ContentDiagnostic>,
    entry: impl Into<String>,
    reason: impl Into<String>,
) {
    diagnostics.push(ContentDiagnostic {
        severity: Severity::Error,
        entry: entry.into(),
        reason: reason.into(),
    });
}

fn warning(
    diagnostics: &mut Vec<ContentDiagnostic>,
    entry: impl Into<String>,
    reason: impl Into<String>,
) {
    diagnostics.push(ContentDiagnostic {
        severity: Severity::Warning,
        entry: entry.into(),
        reason: reason.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::super::EMPTY_DIGEST;
    use super::*;
    use crate::spec::{
        BlockTextures, BuiltinFaceEntry, FoodSpec, ItemEntry, RecipeEntry, TierEntry, ToolEntry,
    };
    use crate::vocabulary::RECIPE_ID_NAMES;

    fn identity(folder: &str) -> PackIdentity {
        PackIdentity {
            folder: folder.to_string(),
            version: Some("1.0.0".to_string()),
        }
    }

    fn asset(byte: u8) -> PackAsset {
        PackAsset {
            rgba: vec![byte; 1024],
            source_side: 16,
        }
    }

    fn block_entry(id: &str, texture: &str) -> BlockEntry {
        BlockEntry {
            id: id.to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some(texture.to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.0,
            drop: None,
            tool: None,
            stack_limit: 64,
        }
    }

    fn item_entry(id: &str, icon: &str) -> ItemEntry {
        ItemEntry {
            id: id.to_string(),
            display_name: None,
            icon: icon.to_string(),
            stack_limit: 64,
            food: None,
        }
    }

    fn tier_entry(id: &str) -> TierEntry {
        TierEntry {
            id: id.to_string(),
            display_name: None,
            speed: 9.0,
            durability: 900,
            damage: 6,
        }
    }

    fn tool_entry(id: &str, tier: &str, icon: &str) -> ToolEntry {
        ToolEntry {
            id: id.to_string(),
            display_name: None,
            kind: ToolKind::Pickaxe,
            tier: tier.to_string(),
            icon: icon.to_string(),
        }
    }

    /// Synthetic built-in base with the frozen default tables.
    ///
    /// The crate has no game `ContentRegistry`, so the ported builder tests
    /// supply the same neutral base: 24 blocks raw `0..24`, three tiers and 22
    /// inert recipes named from the vocabulary. Recipe contents are not part
    /// of the digest and only their names (plus the `planks` output asserted
    /// below) are consumed.
    fn builtin_base() -> Catalog {
        let blocks = BLOCK_ID_NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| BlockDef {
                id: index as u16,
                name: (*name).to_string(),
                display_name: derive_display_name(name),
                faces: [0; 6],
                hardness: None,
                drop: None,
                tool_class: None,
                stack_limit: 64,
                placeholder: false,
            })
            .collect();
        let tiers = TIER_ID_NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| TierDef {
                index: index as u16,
                name: (*name).to_string(),
                display_name: derive_display_name(name),
                speed: 1.0,
                durability: 59,
                damage: 3,
            })
            .collect();
        let recipes = RECIPE_ID_NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| RecipeDef {
                index: index as u16,
                name: (*name).to_string(),
                pattern: Vec::new(),
                key: BTreeMap::new(),
                output: if index == 0 {
                    ItemRef::Block(8)
                } else {
                    ItemRef::Block(0)
                },
                count: 1,
                enabled: true,
                placeholder: false,
            })
            .collect();
        Catalog::from_base(blocks, tiers, recipes)
    }

    #[test]
    fn builtin_patches_change_fields_but_never_discriminants_or_stable_names() {
        let mut spec = PackContentSpec::default();
        spec.block_patches.push(BlockPatch {
            id: "stone".to_string(),
            display_name: Some("Granite".to_string()),
            hardness: Some(2.5),
            drop: Some("cobblestone".to_string()),
            tool: Some(ToolKind::Shovel),
            stack_limit: Some(32),
        });

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        let def = catalog.block(3).expect("stone stays registered");
        assert_eq!(def.id, 3);
        assert_eq!(def.name, "stone");
        assert_eq!(def.display_name, "Granite");
        assert_eq!(def.hardness, Some(2.5));
        assert_eq!(def.drop, Some(ItemRef::Block(4)));
        assert_eq!(def.tool_class, Some(ToolKind::Shovel));
        assert_eq!(def.stack_limit, 32);
        assert_eq!(catalog.blocks().len(), 24);
    }

    #[test]
    fn recipe_patch_disables_a_builtin_recipe() {
        let mut spec = PackContentSpec::default();
        spec.recipe_patches.push(RecipePatch {
            id: "planks".to_string(),
            enabled: false,
        });

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        let recipe = catalog.recipe_by_name("planks").expect("planks exists");
        assert!(!recipe.enabled);
        assert_eq!(recipe.index, 0);
        assert_eq!(recipe.output, ItemRef::Block(8));
        assert!(
            catalog
                .recipe_by_name("sticks")
                .expect("sticks exists")
                .enabled
        );
    }

    #[test]
    fn modded_blocks_start_at_builtin_count_in_sorted_id_order() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:zeta", "z.png"));
        spec.blocks.push(block_entry("mymod:alpha", "a.png"));
        spec.assets.insert("a.png".to_string(), asset(1));
        spec.assets.insert("z.png".to_string(), asset(2));

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(catalog.blocks().len(), 24 + 2);

        let alpha = catalog.block(24).expect("alpha at the first modded index");
        let zeta = catalog.block(25).expect("zeta at the second modded index");
        assert_eq!(alpha.name, "mymod:alpha");
        assert_eq!(zeta.name, "mymod:zeta");
        assert_eq!(alpha.id, 24);
        assert_eq!(zeta.id, 25);
        assert_eq!(catalog.items().len(), 0);
    }

    #[test]
    fn later_pack_replaces_a_definition_without_moving_other_indices() {
        let mut first = PackContentSpec::default();
        first.blocks.push(block_entry("mymod:alpha", "a.png"));
        first.blocks.push(block_entry("mymod:zeta", "z.png"));
        first.assets.insert("a.png".to_string(), asset(1));
        first.assets.insert("z.png".to_string(), asset(2));

        let mut second = PackContentSpec::default();
        let mut replacement = block_entry("mymod:alpha", "a2.png");
        replacement.display_name = Some("Alpha Two".to_string());
        second.blocks.push(replacement);
        second.assets.insert("a2.png".to_string(), asset(3));

        let (catalog, report) = build_catalog(
            &builtin_base(),
            &[(identity("first"), first), (identity("second"), second)],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.warnings(), 1);
        assert!(report.diagnostics[0].reason.contains("`second`"));

        assert_eq!(catalog.blocks().len(), 24 + 2);
        let alpha = catalog
            .block(24)
            .expect("alpha stays at the first modded index");
        let zeta = catalog
            .block(25)
            .expect("zeta stays at the second modded index");
        assert_eq!(alpha.name, "mymod:alpha");
        assert_eq!(alpha.display_name, "Alpha Two");
        assert_eq!(zeta.name, "mymod:zeta");
    }

    #[test]
    fn shared_assets_get_one_layer() {
        let mut spec = PackContentSpec::default();
        let mut entry = block_entry("mymod:all_sides", "shared.png");
        entry.textures = BlockTextures {
            all: Some("shared.png".to_string()),
            top: Some("shared.png".to_string()),
            bottom: Some("shared.png".to_string()),
            side: Some("shared.png".to_string()),
            ..BlockTextures::default()
        };
        spec.blocks.push(entry);
        spec.assets.insert("shared.png".to_string(), asset(0x55));

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(catalog.pack_layers().len(), 1);
        assert_eq!(
            catalog.pack_layer_sources(),
            &[LayerSource {
                folder: "p".to_string(),
                path: "shared.png".to_string(),
            }],
            "a shared asset is deduplicated into one layer and one source"
        );
        let def = catalog.block(24).expect("pack block");
        assert_eq!(def.faces, [PACK_LAYER_BASE; 6]);
        assert_eq!(catalog.pack_layers()[0], vec![0x55u8; 1024]);
        assert_eq!(catalog.pack_layers().len(), 1);
    }

    #[test]
    fn builtin_face_authored_top_allocates_48_and_records_its_source() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: [
                None,
                None,
                Some(FaceSource::Authored("stone_top.png".to_string())),
                None,
                None,
                None,
            ],
        });
        spec.assets.insert("stone_top.png".to_string(), asset(0x77));

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.entries, 1, "one authored face slot is one entry");
        assert_eq!(
            catalog
                .block(3)
                .expect("builtin stone stays registered")
                .faces,
            [0, 0, PACK_LAYER_BASE, 0, 0, 0],
            "authored top allocates the first pack layer; other faces keep the default"
        );
        assert_eq!(catalog.pack_layers().len(), 1);
        assert_eq!(catalog.pack_layers()[0], vec![0x77u8; 1024]);
        assert_eq!(
            catalog.pack_layer_sources(),
            &[LayerSource {
                folder: "p".to_string(),
                path: "stone_top.png".to_string(),
            }]
        );
    }

    #[test]
    fn builtin_authored_face_allocates_after_pack_block_faces() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:alpha", "alpha.png"));
        spec.assets.insert("alpha.png".to_string(), asset(0x11));
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: [
                None,
                None,
                Some(FaceSource::Authored("stone_top.png".to_string())),
                None,
                None,
                None,
            ],
        });
        spec.assets.insert("stone_top.png".to_string(), asset(0x22));

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.entries, 2, "pack block + authored built-in face");
        assert_eq!(
            catalog.block(24).expect("pack block").faces,
            [PACK_LAYER_BASE; 6],
            "pack-defined blocks still start at 48"
        );
        assert_eq!(
            catalog.block(3).expect("builtin stone").faces[2],
            PACK_LAYER_BASE + 1,
            "a built-in authored face appends after pack-block layers"
        );
    }

    #[test]
    fn builtin_layer_face_assigns_without_allocating_or_counting() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: [
                Some(FaceSource::Layer(20)),
                None,
                Some(FaceSource::Layer(21)),
                Some(FaceSource::Layer(22)),
                None,
                Some(FaceSource::Layer(23)),
            ],
        });

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.entries, 0, "Layer faces are not authored entries");
        assert_eq!(catalog.pack_layers().len(), 0);
        assert_eq!(
            catalog.block(3).expect("builtin stone").faces,
            [20, 0, 21, 22, 0, 23]
        );
    }

    #[test]
    fn builtin_face_unknown_block_records_one_error() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "not_a_block".to_string(),
            faces: std::array::from_fn(|_| Some(FaceSource::Authored("x.png".to_string()))),
        });

        let (_catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);
        assert_eq!(report.entries, 0, "a skipped entry allocates nothing");
        assert_eq!(report.diagnostics[0].entry, "face not_a_block");
    }

    #[test]
    fn builtin_face_missing_asset_uses_placeholder_and_reports_once_per_face_entry() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: [
                None,
                None,
                Some(FaceSource::Authored("missing.png".to_string())),
                None,
                None,
                None,
            ],
        });

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);
        assert_eq!(report.diagnostics[0].entry, "face stone top");
        assert_eq!(
            catalog.block(3).expect("builtin stone").faces[2],
            PLACEHOLDER_BLOCK_LAYER
        );
        assert_eq!(catalog.pack_layers().len(), 0);
    }

    #[test]
    fn block_layers_append_from_48_and_cap_at_208_with_fallback_26() {
        let mut spec = PackContentSpec::default();
        for index in 0..161 {
            let texture = format!("layer_{index:03}.png");
            spec.assets.insert(texture.clone(), asset(index as u8));
            spec.blocks
                .push(block_entry(&format!("mymod:block_{index:03}"), &texture));
        }

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(catalog.pack_layers().len(), 160);
        assert_eq!(
            PACK_LAYER_BASE as usize + catalog.pack_layers().len(),
            PACK_LAYER_CAP as usize
        );
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);

        let first = catalog.block(24).expect("first pack block");
        assert_eq!(first.name, "mymod:block_000");
        assert_eq!(first.faces, [PACK_LAYER_BASE; 6]);
        assert_eq!(catalog.pack_layers()[0], vec![0u8; 1024]);

        let last = catalog.block(24 + 160).expect("161st pack block");
        assert_eq!(last.name, "mymod:block_160");
        assert_eq!(last.faces, [PLACEHOLDER_BLOCK_LAYER; 6]);
    }

    #[test]
    fn icons_append_from_32_and_cap_at_224_with_fallback_27() {
        let mut spec = PackContentSpec::default();
        for index in 0..193 {
            let icon = format!("icon_{index:03}.png");
            spec.assets.insert(icon.clone(), asset(index as u8));
            spec.items
                .push(item_entry(&format!("mymod:item_{index:03}"), &icon));
        }

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(catalog.pack_icons().len(), 192);
        assert_eq!(
            PACK_ICON_BASE as usize + catalog.pack_icons().len(),
            PACK_ICON_CAP as usize
        );
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);
        assert_eq!(catalog.items()[0].icon_layer, PACK_ICON_BASE);
        assert_eq!(
            catalog.item(0).map(|def| def.icon_layer),
            Some(PACK_ICON_BASE)
        );
        assert_eq!(catalog.items()[192].icon_layer, PLACEHOLDER_ICON_LAYER);
        assert_eq!(catalog.pack_icons()[0], vec![0u8; 1024]);
    }

    #[test]
    fn layer_and_icon_sources_align_with_allocated_bytes() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:block", "block.png"));
        spec.assets.insert("block.png".to_string(), asset(0x21));
        spec.items.push(item_entry("mymod:item", "icon.png"));
        spec.assets.insert("icon.png".to_string(), asset(0x31));

        let (catalog, report) =
            build_catalog(&builtin_base(), &[(identity("pack-a"), spec.clone())]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        assert_eq!(catalog.pack_layers().len(), 1);
        assert_eq!(catalog.pack_layers()[0], vec![0x21; 1024]);
        assert_eq!(
            catalog.pack_layer_sources(),
            &[LayerSource {
                folder: "pack-a".to_string(),
                path: "block.png".to_string(),
            }]
        );
        assert_eq!(catalog.pack_icons().len(), 1);
        assert_eq!(catalog.pack_icons()[0], vec![0x31; 1024]);
        assert_eq!(
            catalog.pack_icon_sources(),
            &[LayerSource {
                folder: "pack-a".to_string(),
                path: "icon.png".to_string(),
            }]
        );

        // Asset bytes are not digest input: a recolor leaves both the digest
        // and the frozen provenance unchanged.
        let mut recolored = spec;
        recolored
            .assets
            .insert("block.png".to_string(), asset(0xEE));
        recolored.assets.insert("icon.png".to_string(), asset(0xFF));
        let (recolored, report) =
            build_catalog(&builtin_base(), &[(identity("pack-a"), recolored)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(recolored.digest(), catalog.digest());
        assert_eq!(recolored.pack_layer_sources(), catalog.pack_layer_sources());
        assert_eq!(recolored.pack_icon_sources(), catalog.pack_icon_sources());
        assert_eq!(recolored.pack_layers()[0], vec![0xEE; 1024]);
        assert_eq!(recolored.pack_icons()[0], vec![0xFF; 1024]);
    }

    #[test]
    fn digest_is_stable_and_excludes_asset_bytes() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:ore", "ore.png"));
        spec.assets.insert("ore.png".to_string(), asset(1));
        let base = build_catalog(&builtin_base(), &[(identity("p"), spec.clone())]).0;
        let again = build_catalog(&builtin_base(), &[(identity("p"), spec.clone())]).0;
        assert_eq!(base.digest(), again.digest());

        // Asset bytes are excluded: same path, different RGBA bytes.
        let mut recolored = spec.clone();
        recolored.assets.insert("ore.png".to_string(), asset(0xEE));
        let recolored = build_catalog(&builtin_base(), &[(identity("p"), recolored)]).0;
        assert_eq!(base.digest(), recolored.digest());

        // A definition change moves the digest.
        let mut renamed = spec.clone();
        renamed.blocks[0].display_name = Some("Ruby Ore".to_string());
        let renamed = build_catalog(&builtin_base(), &[(identity("p"), renamed)]).0;
        assert_ne!(base.digest(), renamed.digest());

        // A texture path change moves the digest even when the layer index is
        // unchanged.
        let mut repathed = spec.clone();
        repathed.blocks[0].textures.all = Some("ore2.png".to_string());
        repathed.assets.insert("ore2.png".to_string(), asset(1));
        let repathed = build_catalog(&builtin_base(), &[(identity("p"), repathed)]).0;
        assert_eq!(repathed.pack_layers().len(), base.pack_layers().len());
        assert_ne!(base.digest(), repathed.digest());

        // A pack version change moves the digest.
        let mut versioned_identity = identity("p");
        versioned_identity.version = Some("2.0.0".to_string());
        let versioned = build_catalog(&builtin_base(), &[(versioned_identity, spec.clone())]).0;
        assert_ne!(versioned.digest(), base.digest());
    }

    #[test]
    fn worldgen_rules_keep_authored_order_and_cap_at_8() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:ore", "ore.png"));
        spec.assets.insert("ore.png".to_string(), asset(1));
        for index in 0..10 {
            spec.worldgen.push(WorldgenEntry {
                kind: WorldgenKind::Ore {
                    block: "mymod:ore".to_string(),
                    replace: "stone".to_string(),
                    min_y: index,
                    max_y: 128,
                    vein: 4,
                    attempts: 8,
                },
            });
        }

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        assert_eq!(catalog.worldgen_rules().len(), 8);
        assert_eq!(report.errors(), 2, "{:?}", report.diagnostics);
        for (index, rule) in catalog.worldgen_rules().iter().enumerate() {
            match rule {
                WorldgenRule::Ore { min_y, .. } => assert_eq!(*min_y, index as i32),
                other => panic!("expected an ore rule, got {other:?}"),
            }
        }
    }

    #[test]
    fn registry_rebuild_is_deterministic() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:ore", "ore.png"));
        spec.blocks
            .push(block_entry("mymod:gem_block", "gem_block.png"));
        spec.assets.insert("ore.png".to_string(), asset(1));
        spec.assets.insert("gem_block.png".to_string(), asset(2));
        spec.items.push(item_entry("mymod:gem", "gem.png"));
        spec.assets.insert("gem.png".to_string(), asset(3));
        spec.tiers.push(tier_entry("mymod:ruby"));
        spec.tools
            .push(tool_entry("mymod:ruby_pickaxe", "mymod:ruby", "pick.png"));
        spec.assets.insert("pick.png".to_string(), asset(4));
        let mut food_item = item_entry("mymod:snack", "snack.png");
        food_item.food = Some(FoodSpec {
            nutrition: 4,
            eat_seconds: 0.8,
        });
        food_item.display_name = Some("Snack".to_string());
        spec.items.push(food_item);
        spec.assets.insert("snack.png".to_string(), asset(5));
        spec.recipes.push(RecipeEntry {
            id: "mymod:ruby_pickaxe".to_string(),
            pattern: vec!["RRR".to_string(), " S ".to_string(), " S ".to_string()],
            key: [('R', "mymod:gem".to_string()), ('S', "stick".to_string())]
                .into_iter()
                .collect(),
            output: "mymod:ruby_pickaxe".to_string(),
            count: 1,
        });
        spec.worldgen.push(WorldgenEntry {
            kind: WorldgenKind::Patch {
                block: "mymod:gem_block".to_string(),
                replace: "grass".to_string(),
                biomes: vec!["plains".to_string(), "forest".to_string()],
                min_y: 60,
                max_y: 80,
                chance: 0.01,
            },
        });

        let base = builtin_base();
        let pack = (identity("p"), spec);
        let (first, first_report) = build_catalog(&base, std::slice::from_ref(&pack));
        let (second, second_report) = build_catalog(&base, std::slice::from_ref(&pack));

        assert_eq!(first_report, second_report);
        assert_eq!(first.digest(), second.digest());
        assert_eq!(
            first
                .blocks()
                .iter()
                .map(|def| (def.id, def.faces))
                .collect::<Vec<_>>(),
            second
                .blocks()
                .iter()
                .map(|def| (def.id, def.faces))
                .collect::<Vec<_>>()
        );
        assert_eq!(first.pack_layers(), second.pack_layers());
        assert_eq!(first.pack_icons(), second.pack_icons());
        assert_eq!(first.recipes().len(), 23);
        assert_eq!(first.recipes()[22].name, "mymod:ruby_pickaxe");
        assert_eq!(first.recipes()[22].output, ItemRef::Tool(0));
        assert_eq!(first.recipes()[22].key[&'R'], ItemRef::Item(0));
        assert_eq!(first.recipes()[22].key[&'S'], ItemRef::BuiltinMaterial(0));
        assert_eq!(first.tools()[0].tier, 3);
        assert_eq!(first.tiers().len(), 4);
        assert_eq!(first.items()[0].name, "mymod:gem");
        assert_eq!(
            first.items()[1].food,
            Some(FoodSpec {
                nutrition: 4,
                eat_seconds: 0.8
            })
        );
    }

    #[test]
    fn content_report_counts_errors_and_warnings() {
        let mut first = PackContentSpec::default();
        first.blocks.push(block_entry("mymod:ore", "ore.png"));
        first.assets.insert("ore.png".to_string(), asset(1));
        first.worldgen.push(WorldgenEntry {
            kind: WorldgenKind::Ore {
                block: "mymod:ore".to_string(),
                replace: "stone".to_string(),
                min_y: -16,
                max_y: 16,
                vein: 4,
                attempts: 8,
            },
        });

        let mut second = PackContentSpec::default();
        // Replaces the earlier definition (warning) and references a missing
        // asset (error).
        second.blocks.push(block_entry("mymod:ore", "missing.png"));

        let (_, report) = build_catalog(
            &builtin_base(),
            &[(identity("first"), first), (identity("second"), second)],
        );
        assert_eq!(report.entries, 3);
        assert_eq!(report.warnings(), 1);
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);
        assert_eq!(
            report.diagnostics.len(),
            report.warnings() + report.errors()
        );
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error
                && diagnostic.reason.contains("missing.png")));
    }

    #[test]
    fn unresolved_references_fall_back_instead_of_panicking() {
        let mut spec = PackContentSpec::default();
        let mut entry = block_entry("mymod:broken", "broken.png");
        entry.drop = Some("mymod:does_not_exist".to_string());
        spec.blocks.push(entry);
        spec.assets.insert("broken.png".to_string(), asset(1));
        spec.recipe_patches.push(RecipePatch {
            id: "not_a_recipe".to_string(),
            enabled: false,
        });

        let (catalog, report) = build_catalog(&builtin_base(), &[(identity("p"), spec)]);
        let def = catalog.block(24).expect("block still exists");
        assert_eq!(def.drop, None);
        assert_eq!(report.errors(), 2, "{:?}", report.diagnostics);
        assert!(catalog.recipe_by_name("not_a_recipe").is_none());
    }

    #[test]
    fn empty_stack_is_the_builtin_registry() {
        let base = builtin_base();
        let (catalog, report) = build_catalog(&base, &[]);
        assert_eq!(report, ContentLoadReport::default());
        assert_eq!(catalog.blocks(), base.blocks());
        assert_eq!(catalog.recipes(), base.recipes());
        assert_eq!(catalog.digest(), EMPTY_DIGEST);
    }

    #[test]
    fn display_names_are_derived_from_namespaced_ids() {
        assert_eq!(derive_display_name("mymod:ruby_ore"), "Ruby Ore");
        assert_eq!(derive_display_name("mymod:Iron"), "Iron");
        assert_eq!(derive_display_name("stone"), "Stone");
    }

    #[test]
    fn builtin_icons_do_not_move_digest_allocation_or_entry_count() {
        let mut plain = PackContentSpec::default();
        plain.items.push(item_entry("mymod:iconful", "icon.png"));
        plain.assets.insert("icon.png".to_string(), asset(0x22));

        let mut with_override = plain.clone();
        with_override
            .builtin_icons
            .insert("apple".to_string(), "apple.png".to_string());
        with_override
            .builtin_icons
            .insert("wood_pickaxe".to_string(), "pickaxe.png".to_string());
        with_override
            .assets
            .insert("apple.png".to_string(), asset(0x33));
        with_override
            .assets
            .insert("pickaxe.png".to_string(), asset(0x44));

        let base = builtin_base();
        let (plain_catalog, plain_report) = build_catalog(&base, &[(identity("p"), plain.clone())]);
        let (override_catalog, override_report) =
            build_catalog(&base, &[(identity("p"), with_override.clone())]);

        assert_eq!(override_report.entries, plain_report.entries);
        assert_eq!(
            override_report.errors(),
            0,
            "{:?}",
            override_report.diagnostics
        );
        assert_eq!(
            override_report.warnings(),
            0,
            "{:?}",
            override_report.diagnostics
        );
        assert_eq!(override_catalog.digest(), plain_catalog.digest());
        assert_eq!(override_catalog.pack_layers(), plain_catalog.pack_layers());
        assert_eq!(override_catalog.pack_icons(), plain_catalog.pack_icons());
        assert_eq!(
            override_catalog.pack_icon_sources(),
            plain_catalog.pack_icon_sources()
        );
        assert_eq!(
            override_catalog.pack_layers().len(),
            plain_catalog.pack_layers().len()
        );
        assert_eq!(
            override_catalog.pack_icons().len(),
            plain_catalog.pack_icons().len()
        );

        // Removing the override again is the same identity.
        let (restored, restored_report) = build_catalog(&base, &[(identity("p"), plain)]);
        assert_eq!(restored.digest(), override_catalog.digest());
        assert_eq!(restored_report.entries, override_report.entries);
    }

    #[test]
    fn builtin_icon_with_unloaded_asset_warns_without_changing_identity() {
        let mut spec = PackContentSpec::default();
        spec.builtin_icons
            .insert("apple".to_string(), "missing.png".to_string());

        let base = builtin_base();
        let (catalog, report) = build_catalog(&base, &[(identity("p"), spec)]);
        let (empty, empty_report) =
            build_catalog(&base, &[(identity("p"), PackContentSpec::default())]);

        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.warnings(), 1, "{:?}", report.diagnostics);
        assert_eq!(report.diagnostics[0].severity, Severity::Warning);
        assert_eq!(report.diagnostics[0].entry, "builtin_icon apple");
        assert!(
            report.diagnostics[0].reason.contains("not a loaded asset"),
            "{:?}",
            report.diagnostics
        );
        assert_eq!(report.entries, empty_report.entries);
        assert_eq!(catalog.digest(), empty.digest());
        assert_eq!(catalog.pack_layers().len(), 0);
        assert_eq!(catalog.pack_icons().len(), 0);
    }

    #[test]
    fn reference_pack_matches_the_pre_adapter_oracle() {
        const REFERENCE_PACK: &str =
            include_str!("../../../../tests/fixtures/packs/mymod-ruby/pack.toml");

        let (manifest, root, warnings) = crate::manifest::parse_pack_file(REFERENCE_PACK);
        assert!(warnings.is_empty(), "{warnings:?}");
        let root = root.expect("the reference pack parses as TOML");
        let (mut spec, warnings, diagnostics) =
            crate::v3::parse_content(&root, manifest.format_version);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        // The checked-in PNGs are not needed: the digest excludes asset bytes.
        for path in [
            "textures/ruby_ore.png",
            "textures/ruby_block.png",
            "textures/ruby.png",
            "textures/ruby_stew.png",
            "textures/ruby_pickaxe.png",
            "textures/ruby_sword.png",
        ] {
            spec.assets.insert(
                path.to_string(),
                PackAsset {
                    rgba: vec![0xAB; 1024],
                    source_side: 16,
                },
            );
        }

        let identity = PackIdentity {
            folder: "mymod-ruby".to_string(),
            version: Some("1.0.0".to_string()),
        };
        let (catalog, report) = build_catalog(&builtin_base(), &[(identity, spec)]);

        assert_eq!(report.entries, 12);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.warnings(), 0, "{:?}", report.diagnostics);

        // Independently validated pre-change oracle: a mismatch is a builder
        // defect, do not re-derive it.
        assert_eq!(catalog.digest(), 0xbacb_c160_bdf7_6d08);

        let ruby_block = catalog.block(24).expect("mymod:ruby_block at raw 24");
        assert_eq!(ruby_block.name, "mymod:ruby_block");
        assert_eq!(ruby_block.faces, [48; 6]);
        let ruby_ore = catalog.block(25).expect("mymod:ruby_ore at raw 25");
        assert_eq!(ruby_ore.name, "mymod:ruby_ore");
        assert_eq!(ruby_ore.faces, [49; 6]);

        assert_eq!(catalog.items()[0].name, "mymod:ruby");
        assert_eq!(catalog.items()[0].icon_layer, 32);
        assert_eq!(catalog.tiers()[3].name, "mymod:ruby");
        assert_eq!(catalog.tools()[0].tier, 3);
        assert_eq!(catalog.tools()[0].icon_layer, 34);
        assert_eq!(catalog.tools()[1].tier, 3);
        assert_eq!(catalog.tools()[1].icon_layer, 35);

        assert_eq!(catalog.recipes()[22].name, "mymod:ruby_pickaxe");
        assert_eq!(catalog.recipes()[23].name, "mymod:ruby_stew");

        assert_eq!(catalog.worldgen_rules().len(), 2);
        assert!(matches!(
            &catalog.worldgen_rules()[0],
            WorldgenRule::Ore { .. }
        ));
        assert_eq!(
            catalog.worldgen_rules()[1],
            WorldgenRule::Patch {
                block: 25,
                replace: 1,
                biomes: vec![BiomeTag::Plains, BiomeTag::Forest],
                min_y: 40,
                max_y: 110,
                chance: 0.02,
            }
        );

        assert!(
            !catalog
                .recipe_by_name("shortbow")
                .expect("shortbow is a built-in recipe")
                .enabled
        );
    }
}
