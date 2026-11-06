//! Deterministic pack-stack → registry builder.
//!
//! [`build_registry`] folds the ordered pack stack over the built-in content
//! table into one immutable [`ContentRegistry`]. Allocation is pure data:
//!
//! * every id is allocated in sorted order at a dense index (blocks from
//!   [`BUILTIN_BLOCK_COUNT`], items from 0, tiers from 3, recipes from 22),
//! * pack block faces and item/tool icons are appended to append-only layer
//!   tables, deduplicated by `(pack folder, authored path)`,
//! * worldgen rules keep authored stack order under an eight-rule cap, and
//! * a hand-rolled FNV-1a 64 digest covers the resulting definitions (never
//!   asset bytes), so `--validate-pack` and a game load agree.
//!
//! Nothing here touches the filesystem, renderer, audio, settings or the pack
//! loader: input is a list of [`PackIdentity`] + [`PackContentSpec`] pairs.

use std::collections::{BTreeMap, BTreeSet};

use crate::content::names;
use crate::content::spec::{
    BlockEntry, BlockPatch, BlockTextures, ContentDiagnostic, ItemEntry, PackAsset,
    PackContentSpec, RecipeEntry, RecipePatch, Severity, TierEntry, ToolEntry, WorldgenEntry,
    WorldgenKind,
};
use crate::content::{
    BlockDef, ContentRegistry, FoodDef, ItemDef, ItemRef, LayerSource, RecipeDef, TierDef, ToolDef,
    WorldgenRule, EMPTY_DIGEST, PACK_ICON_BASE, PACK_ICON_CAP, PACK_LAYER_BASE, PACK_LAYER_CAP,
    PLACEHOLDER_BLOCK_LAYER, PLACEHOLDER_ICON_LAYER,
};
use crate::player::inventory::{ItemId, ToolKind, ToolTier};
use crate::world::block::{BlockId, BUILTIN_BLOCK_COUNT};
use crate::world::terrain::Biome;

/// Maximum number of worldgen rules the stack may contribute.
pub const WORLDGEN_RULE_CAP: usize = 8;

/// Number of built-in tool tiers: wood, stone, iron.
pub const BUILTIN_TIER_COUNT: u16 = 3;

/// Identity of one pack in the stack, hashed into the registry digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackIdentity {
    /// Pack folder name (stable identity inside `packs/`).
    pub folder: String,
    /// Optional authored pack version.
    pub version: Option<String>,
}

/// Outcome of one [`build_registry`] call.
///
/// `entries` is the number of authored content entries considered across the
/// stack (blocks, items, tiers, tools, recipes, worldgen rules and patches);
/// it is the summary line the `--validate-pack` CLI prints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentLoadReport {
    /// Authored content entries considered.
    pub entries: usize,
    /// Per-entry problems collected while building the registry.
    pub diagnostics: Vec<ContentDiagnostic>,
}

impl ContentLoadReport {
    /// Number of error-severity diagnostics.
    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .count()
    }

    /// Number of warning-severity diagnostics.
    pub fn warnings(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .count()
    }
}

/// Resolved string → identity lookups shared by every reference field.
struct Resolver<'a> {
    block_ids: &'a BTreeMap<&'a str, BlockId>,
    item_indices: &'a BTreeMap<&'a str, u16>,
    tool_indices: &'a BTreeMap<&'a str, u16>,
}

impl Resolver<'_> {
    /// Resolve a block reference: pack block first, then a built-in name.
    fn block(&self, name: &str) -> Option<BlockId> {
        self.block_ids
            .get(name)
            .copied()
            .or_else(|| names::builtin_block_id_by_name(name))
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
        names::builtin_item_id_by_name(name).map(ItemRef::from_item_id)
    }
}

/// Fold an ordered pack stack over `base` into a fresh registry.
///
/// The returned registry is immutable; the caller owns any `Arc` freezing.
/// A malformed reference records a [`Severity::Error`] diagnostic and falls
/// back to the safe value instead of panicking (the pack parser normally
/// rejects those entries before they reach a spec).
pub fn build_registry(
    base: &ContentRegistry,
    packs: &[(PackIdentity, PackContentSpec)],
) -> (ContentRegistry, ContentLoadReport) {
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

    // Dense block ids start at the frozen built-in count, in sorted id order.
    let block_ids: BTreeMap<&str, BlockId> = block_entries
        .keys()
        .enumerate()
        .map(|(index, id)| (*id, BlockId::Modded(BUILTIN_BLOCK_COUNT + index as u16)))
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
        .map(|(index, id)| (*id, BUILTIN_TIER_COUNT + index as u16))
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
        let Some(id) = names::builtin_block_id_by_name(&patch.id) else {
            error(
                &mut diagnostics,
                format!("patch {}", patch.id),
                format!("pack `{folder}` patches an unknown block"),
            );
            continue;
        };
        let Some(def) = blocks.get_mut(usize::from(id.to_raw())) else {
            continue;
        };
        if def.id != id {
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
            food: entry.food.as_ref().map(|food| FoodDef {
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
        names::builtin_tier_by_name(name).map(|tier| match tier {
            ToolTier::Wood => 0,
            ToolTier::Stone => 1,
            ToolTier::Iron => 2,
        })
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
        let index = recipes.len() as u16;
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
                    match biome_by_name(name) {
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
    for (id, (folder, entry)) in &block_entries {
        let raw = block_ids[id];
        let def_index = usize::from(raw.to_raw());
        for face in 0..6 {
            let Some(path) = face_texture(&entry.textures, face) else {
                error(
                    &mut diagnostics,
                    format!("block {id}"),
                    format!("face {face} has no authored texture"),
                );
                blocks[def_index].faces[face] = PLACEHOLDER_BLOCK_LAYER;
                continue;
            };
            let key = (folder.to_string(), path.clone());
            if !assets.contains_key(&key) {
                if reported_paths.insert((id.to_string(), path.clone())) {
                    error(
                        &mut diagnostics,
                        format!("block {id}"),
                        format!("texture `{path}` is not a loaded asset"),
                    );
                }
                blocks[def_index].faces[face] = PLACEHOLDER_BLOCK_LAYER;
                continue;
            }
            let layer = if let Some(existing) = layer_slots.get(&key) {
                *existing
            } else {
                let slot = pack_layers.len();
                if PACK_LAYER_BASE as usize + slot >= PACK_LAYER_CAP as usize {
                    if reported_paths.insert((id.to_string(), path.clone())) {
                        error(
                            &mut diagnostics,
                            format!("block {id}"),
                            format!(
                                "pack layer space exhausted at {PACK_LAYER_CAP}; `{path}` falls back to layer {PLACEHOLDER_BLOCK_LAYER}"
                            ),
                        );
                    }
                    blocks[def_index].faces[face] = PLACEHOLDER_BLOCK_LAYER;
                    continue;
                }
                let layer = PACK_LAYER_BASE + slot as u8;
                pack_layers.push(assets[&key].rgba.clone());
                pack_layer_sources.push(LayerSource {
                    folder: folder.to_string(),
                    path: path.clone(),
                });
                layer_slots.insert(key, layer);
                layer
            };
            blocks[def_index].faces[face] = layer;
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

    let registry = ContentRegistry {
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
        registry,
        ContentLoadReport {
            entries,
            diagnostics,
        },
    )
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
    /// [`PACK_ICON_BASE`] up to [`PACK_ICON_CAP`]. A missing asset or an exhausted
    /// icon space records a diagnostic and returns the built-in sprite placeholder
    /// instead of panicking.
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

/// Resolve the authored texture path for face index 0..6 (+X,-X,+Y,-Y,+Z,-Z).
fn face_texture(textures: &BlockTextures, face: usize) -> Option<&String> {
    let specific = match face {
        0 | 1 | 4 | 5 => textures.side.as_ref(),
        2 => textures.top.as_ref(),
        3 => textures.bottom.as_ref(),
        _ => None,
    };
    specific.or(textures.all.as_ref())
}

/// Map an authored biome name to the world enum.
fn biome_by_name(name: &str) -> Option<Biome> {
    match name {
        "plains" => Some(Biome::Plains),
        "forest" => Some(Biome::Forest),
        "mountains" => Some(Biome::Mountains),
        "ocean" => Some(Biome::Ocean),
        _ => None,
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

// ---------------------------------------------------------------------------
// Digest
// ---------------------------------------------------------------------------

/// Minimal FNV-1a 64 sink over a canonical little-endian byte stream.
///
/// Never uses `DefaultHasher`: the digest is part of the pack contract and must
/// be reproducible across processes, platforms and Rust versions.
struct Digest(u64);

impl Digest {
    fn new() -> Digest {
        Digest(EMPTY_DIGEST)
    }

    fn finish(self) -> u64 {
        self.0
    }

    fn byte(&mut self, value: u8) {
        self.0 ^= u64::from(value);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn bytes(&mut self, value: &[u8]) {
        for &byte in value {
            self.byte(byte);
        }
    }

    fn u8(&mut self, value: u8) {
        self.byte(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.bytes(&value.to_bits().to_le_bytes());
    }

    fn str(&mut self, value: &str) {
        self.u16(u16::try_from(value.len()).unwrap_or(u16::MAX));
        self.bytes(value.as_bytes());
    }

    fn opt_str(&mut self, value: Option<&str>) {
        match value {
            None => self.u8(0),
            Some(text) => {
                self.u8(1);
                self.str(text);
            }
        }
    }

    fn opt_f32(&mut self, value: Option<f32>) {
        match value {
            None => self.u8(0),
            Some(number) => {
                self.u8(1);
                self.f32(number);
            }
        }
    }

    fn opt_u16(&mut self, value: Option<u16>) {
        match value {
            None => self.u8(0),
            Some(number) => {
                self.u8(1);
                self.u16(number);
            }
        }
    }

    fn opt_tool_kind(&mut self, value: Option<ToolKind>) {
        match value {
            None => self.u8(0),
            Some(kind) => {
                self.u8(1);
                self.u8(tool_kind_tag(kind));
            }
        }
    }

    fn block(&mut self, block: BlockId) {
        self.u16(block.to_raw());
    }

    fn item_id(&mut self, id: ItemId) {
        match id {
            ItemId::Block(block) => {
                self.u8(0);
                self.block(block);
            }
            ItemId::Material(material) => {
                self.u8(1);
                self.str(names::builtin_material_id_name(material));
            }
            ItemId::Tool { kind, tier } => {
                self.u8(2);
                self.u8(tool_kind_tag(kind));
                self.u8(tool_tier_tag(tier));
            }
            // A widened pack identity hashes by its dense item index. It
            // cannot appear in a built-in recipe output, so existing digests
            // are unchanged.
            ItemId::Item(index) => {
                self.u8(3);
                self.u16(index);
            }
            // A pack tool hashes by its dense tool index. Built-in recipes
            // cannot produce one, so existing digests are unchanged.
            ItemId::ModTool(index) => {
                self.u8(4);
                self.u16(index);
            }
        }
    }

    fn item_ref(&mut self, reference: ItemRef) {
        match reference {
            ItemRef::Block(block) => {
                self.u8(0);
                self.block(block);
            }
            ItemRef::Builtin(id) => {
                self.u8(1);
                self.item_id(id);
            }
            ItemRef::Item(index) => {
                self.u8(2);
                self.u16(index);
            }
            ItemRef::Tool(index) => {
                self.u8(3);
                self.u16(index);
            }
        }
    }

    fn opt_item_ref(&mut self, reference: Option<ItemRef>) {
        match reference {
            None => self.u8(0),
            Some(reference) => {
                self.u8(1);
                self.item_ref(reference);
            }
        }
    }
}

fn tool_kind_tag(kind: ToolKind) -> u8 {
    match kind {
        ToolKind::Pickaxe => 0,
        ToolKind::Axe => 1,
        ToolKind::Shovel => 2,
        ToolKind::Hoe => 3,
        ToolKind::Sword => 4,
        ToolKind::Bow => 5,
    }
}

fn tool_tier_tag(tier: ToolTier) -> u8 {
    match tier {
        ToolTier::Wood => 0,
        ToolTier::Stone => 1,
        ToolTier::Iron => 2,
    }
}

/// Everything the deterministic registry digest folds in.
///
/// The builder's working set (resolved definitions, authored patch lists, and
/// the winning entry map per content kind) travels as one bundle so the digest
/// stays a single entry point as new content kinds appear.
#[derive(Clone, Copy)]
struct DigestInputs<'a> {
    packs: &'a [(PackIdentity, PackContentSpec)],
    blocks: &'a [BlockDef],
    items: &'a [ItemDef],
    tiers: &'a [TierDef],
    tools: &'a [ToolDef],
    recipes: &'a [RecipeDef],
    builtin_recipe_count: usize,
    worldgen_rules: &'a [WorldgenRule],
    block_entries: &'a BTreeMap<&'a str, (&'a str, &'a BlockEntry)>,
    item_entries: &'a BTreeMap<&'a str, (&'a str, &'a ItemEntry)>,
    tier_entries: &'a BTreeMap<&'a str, (&'a str, &'a TierEntry)>,
    tool_entries: &'a BTreeMap<&'a str, (&'a str, &'a ToolEntry)>,
    block_patches: &'a [(&'a str, &'a BlockPatch)],
    recipe_patches: &'a [(&'a str, &'a RecipePatch)],
}

fn compute_digest(input: &DigestInputs<'_>) -> u64 {
    let DigestInputs {
        packs,
        blocks,
        items,
        tiers,
        tools,
        recipes,
        builtin_recipe_count,
        worldgen_rules,
        block_entries,
        item_entries,
        tier_entries,
        tool_entries,
        block_patches,
        recipe_patches,
    } = *input;
    // No packs means no pack definitions: report the empty-stack digest so a
    // rebuilt stack with zero packs matches the built-in registry exactly.
    if packs.is_empty() {
        return EMPTY_DIGEST;
    }
    let mut digest = Digest::new();

    // Pack identities in folder order.
    let mut identities: Vec<&PackIdentity> = packs.iter().map(|(identity, _)| identity).collect();
    identities.sort_by(|left, right| left.folder.cmp(&right.folder));
    digest.u16(identities.len() as u16);
    for identity in identities {
        digest.str(&identity.folder);
        digest.opt_str(identity.version.as_deref());
    }

    // Pack block definitions in index (sorted id) order, including the
    // authored texture paths so a rename moves the digest even when the
    // allocated layer index is unchanged.
    digest.u16(block_entries.len() as u16);
    for (id, (_folder, entry)) in block_entries {
        let resolved = blocks
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected block has a definition");
        digest.str(id);
        digest.u16(resolved.id.to_raw());
        digest.opt_str(entry.display_name.as_deref());
        digest.f32(entry.hardness);
        digest.opt_str(entry.drop.as_deref());
        digest.opt_tool_kind(entry.tool);
        digest.u16(entry.stack_limit);
        for face in 0..6 {
            digest.opt_str(face_texture(&entry.textures, face).map(String::as_str));
        }
        digest.opt_item_ref(resolved.drop);
    }

    // Items in index order.
    digest.u16(items.len() as u16);
    for (id, (_folder, entry)) in item_entries {
        let resolved = items
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected item has a definition");
        digest.str(id);
        digest.u16(resolved.index);
        digest.opt_str(entry.display_name.as_deref());
        digest.str(&entry.icon);
        digest.u16(entry.stack_limit);
        match &entry.food {
            None => digest.u8(0),
            Some(food) => {
                digest.u8(1);
                digest.u16(food.nutrition);
                digest.f32(food.eat_seconds);
            }
        }
    }

    // Pack tiers in index order (built-ins are excluded).
    digest.u16(tier_entries.len() as u16);
    for (id, (_folder, entry)) in tier_entries {
        let resolved = tiers
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected tier has a definition");
        digest.str(id);
        digest.u16(resolved.index);
        digest.opt_str(entry.display_name.as_deref());
        digest.f32(entry.speed);
        digest.u16(entry.durability);
        digest.u16(entry.damage);
    }

    // Tools in index order.
    digest.u16(tools.len() as u16);
    for (id, (_folder, entry)) in tool_entries {
        let resolved = tools
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected tool has a definition");
        digest.str(id);
        digest.u16(resolved.index);
        digest.opt_str(entry.display_name.as_deref());
        digest.u8(tool_kind_tag(entry.kind));
        digest.u16(resolved.tier);
        digest.str(&entry.icon);
    }

    // Pack recipes in index order.
    let pack_recipes = &recipes[builtin_recipe_count..];
    digest.u16(pack_recipes.len() as u16);
    for recipe in pack_recipes {
        digest.u16(recipe.index);
        digest.str(&recipe.name);
        digest.u16(recipe.pattern.len() as u16);
        for row in &recipe.pattern {
            digest.str(row);
        }
        digest.u16(recipe.key.len() as u16);
        for (character, reference) in &recipe.key {
            digest.u8(*character as u8);
            digest.item_ref(*reference);
        }
        digest.item_ref(recipe.output);
        digest.u16(recipe.count);
        digest.u8(u8::from(recipe.enabled));
    }

    // Worldgen rules in authored order.
    digest.u16(worldgen_rules.len() as u16);
    for rule in worldgen_rules {
        match rule {
            WorldgenRule::Ore {
                block,
                replace,
                min_y,
                max_y,
                vein,
                attempts,
            } => {
                digest.u8(0);
                digest.block(*block);
                digest.block(*replace);
                digest.i32(*min_y);
                digest.i32(*max_y);
                digest.u8(*vein);
                digest.u8(*attempts);
            }
            WorldgenRule::Patch {
                block,
                replace,
                biomes,
                min_y,
                max_y,
                chance,
            } => {
                digest.u8(1);
                digest.block(*block);
                digest.block(*replace);
                digest.u16(biomes.len() as u16);
                for biome in biomes {
                    digest.u8(biome_tag(*biome));
                }
                digest.i32(*min_y);
                digest.i32(*max_y);
                digest.f32(*chance);
            }
        }
    }

    // Authored patches in stack order.
    digest.u16(block_patches.len() as u16);
    for (_folder, patch) in block_patches {
        digest.str(&patch.id);
        digest.opt_str(patch.display_name.as_deref());
        digest.opt_f32(patch.hardness);
        digest.opt_str(patch.drop.as_deref());
        digest.opt_tool_kind(patch.tool);
        digest.opt_u16(patch.stack_limit);
    }
    digest.u16(recipe_patches.len() as u16);
    for (_folder, patch) in recipe_patches {
        digest.str(&patch.id);
        digest.u8(u8::from(patch.enabled));
    }

    digest.finish()
}

fn biome_tag(biome: Biome) -> u8 {
    match biome {
        Biome::Plains => 0,
        Biome::Forest => 1,
        Biome::Mountains => 2,
        Biome::Ocean => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::spec::{
        BlockTextures, FoodSpec, ItemEntry, RecipeEntry, TierEntry, ToolEntry,
    };

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

    fn simple_pack(folder: &str) -> (PackIdentity, PackContentSpec) {
        let mut spec = PackContentSpec::default();
        spec.blocks
            .push(block_entry("mymod:ruby_ore", "ruby_ore.png"));
        spec.assets.insert("ruby_ore.png".to_string(), asset(0x11));
        (identity(folder), spec)
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

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        let def = registry
            .block(BlockId::Stone)
            .expect("stone stays registered");
        assert_eq!(def.id, BlockId::Stone);
        assert_eq!(def.name, "stone");
        assert_eq!(def.display_name, "Granite");
        assert_eq!(def.hardness, Some(2.5));
        assert_eq!(def.drop, Some(ItemRef::Block(BlockId::Cobblestone)));
        assert_eq!(def.tool_class, Some(ToolKind::Shovel));
        assert_eq!(def.stack_limit, 32);
        assert_eq!(registry.blocks().len(), usize::from(BUILTIN_BLOCK_COUNT));
    }

    #[test]
    fn recipe_patch_disables_a_builtin_recipe() {
        let mut spec = PackContentSpec::default();
        spec.recipe_patches.push(RecipePatch {
            id: "planks".to_string(),
            enabled: false,
        });

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        let recipe = registry.recipe_by_name("planks").expect("planks exists");
        assert!(!recipe.enabled);
        assert_eq!(recipe.index, 0);
        assert_eq!(recipe.output, ItemRef::Block(BlockId::OakPlanks));
        assert!(
            registry
                .recipe_by_name("sticks")
                .expect("sticks exists")
                .enabled
        );
    }

    #[test]
    fn modded_blocks_start_at_23_in_sorted_id_order() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:zeta", "z.png"));
        spec.blocks.push(block_entry("mymod:alpha", "a.png"));
        spec.assets.insert("a.png".to_string(), asset(1));
        spec.assets.insert("z.png".to_string(), asset(2));

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(
            registry.blocks().len(),
            usize::from(BUILTIN_BLOCK_COUNT) + 2
        );

        let alpha = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("alpha at 23");
        let zeta = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT + 1))
            .expect("zeta at 24");
        assert_eq!(alpha.name, "mymod:alpha");
        assert_eq!(zeta.name, "mymod:zeta");
        assert_eq!(alpha.id.to_raw(), 23);
        assert_eq!(zeta.id.to_raw(), 24);
        assert_eq!(BlockId::from_raw(23), alpha.id);
        assert_eq!(BlockId::from_raw(24), zeta.id);
        assert_eq!(registry.items().len(), 0);
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

        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(identity("first"), first), (identity("second"), second)],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(report.warnings(), 1);
        assert!(report.diagnostics[0].reason.contains("`second`"));

        assert_eq!(
            registry.blocks().len(),
            usize::from(BUILTIN_BLOCK_COUNT) + 2
        );
        let alpha = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("alpha stays at 23");
        let zeta = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT + 1))
            .expect("zeta stays at 24");
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
        };
        spec.blocks.push(entry);
        spec.assets.insert("shared.png".to_string(), asset(0x55));

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(registry.pack_layers().len(), 1);
        assert_eq!(
            registry.pack_layer_sources(),
            &[LayerSource {
                folder: "p".to_string(),
                path: "shared.png".to_string(),
            }],
            "a shared asset is deduplicated into one layer and one source"
        );
        let def = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("pack block");
        assert_eq!(def.faces, [PACK_LAYER_BASE; 6]);
        assert_eq!(
            registry.pack_layer(PACK_LAYER_BASE),
            Some(&[0x55u8; 1024][..])
        );
        assert!(registry.pack_layer(PACK_LAYER_BASE - 1).is_none());
    }

    #[test]
    fn block_layers_append_from_48_and_cap_at_208_with_fallback_25() {
        let mut spec = PackContentSpec::default();
        for index in 0..161 {
            let texture = format!("layer_{index:03}.png");
            spec.assets.insert(texture.clone(), asset(index as u8));
            spec.blocks
                .push(block_entry(&format!("mymod:block_{index:03}"), &texture));
        }

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(registry.pack_layers().len(), 160);
        assert_eq!(
            PACK_LAYER_BASE as usize + registry.pack_layers().len(),
            PACK_LAYER_CAP as usize
        );
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);

        let first = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("first pack block");
        assert_eq!(first.name, "mymod:block_000");
        assert_eq!(first.faces, [PACK_LAYER_BASE; 6]);
        assert_eq!(registry.pack_layer(PACK_LAYER_BASE), Some(&[0u8; 1024][..]));

        let last = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT + 160))
            .expect("161st pack block");
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

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(registry.pack_icons().len(), 192);
        assert_eq!(
            PACK_ICON_BASE as usize + registry.pack_icons().len(),
            PACK_ICON_CAP as usize
        );
        assert_eq!(report.errors(), 1, "{:?}", report.diagnostics);
        assert_eq!(registry.items()[0].icon_layer, PACK_ICON_BASE);
        assert_eq!(
            registry.item(0).map(|def| def.icon_layer),
            Some(PACK_ICON_BASE)
        );
        assert_eq!(registry.items()[192].icon_layer, PLACEHOLDER_ICON_LAYER);
        assert_eq!(registry.pack_icon(PACK_ICON_BASE), Some(&[0u8; 1024][..]));
    }

    #[test]
    fn layer_and_icon_sources_align_with_allocated_bytes() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:block", "block.png"));
        spec.assets.insert("block.png".to_string(), asset(0x21));
        spec.items.push(item_entry("mymod:item", "icon.png"));
        spec.assets.insert("icon.png".to_string(), asset(0x31));

        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(identity("pack-a"), spec.clone())],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        assert_eq!(registry.pack_layers().len(), 1);
        assert_eq!(registry.pack_layers()[0], vec![0x21; 1024]);
        assert_eq!(
            registry.pack_layer_sources(),
            &[LayerSource {
                folder: "pack-a".to_string(),
                path: "block.png".to_string(),
            }]
        );
        assert_eq!(registry.pack_icons().len(), 1);
        assert_eq!(registry.pack_icons()[0], vec![0x31; 1024]);
        assert_eq!(
            registry.pack_icon_sources(),
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
        let (recolored, report) = build_registry(
            ContentRegistry::builtin(),
            &[(identity("pack-a"), recolored)],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(recolored.digest(), registry.digest());
        assert_eq!(
            recolored.pack_layer_sources(),
            registry.pack_layer_sources()
        );
        assert_eq!(recolored.pack_icon_sources(), registry.pack_icon_sources());
        assert_eq!(recolored.pack_layers()[0], vec![0xEE; 1024]);
        assert_eq!(recolored.pack_icons()[0], vec![0xFF; 1024]);
    }

    #[test]
    fn digest_is_stable_and_excludes_asset_bytes() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(block_entry("mymod:ore", "ore.png"));
        spec.assets.insert("ore.png".to_string(), asset(1));
        let base = build_registry(ContentRegistry::builtin(), &[(identity("p"), spec.clone())]).0;
        let again = build_registry(ContentRegistry::builtin(), &[(identity("p"), spec.clone())]).0;
        assert_eq!(base.digest(), again.digest());

        // Asset bytes are excluded: same path, different RGBA bytes.
        let mut recolored = spec.clone();
        recolored.assets.insert("ore.png".to_string(), asset(0xEE));
        let recolored = build_registry(ContentRegistry::builtin(), &[(identity("p"), recolored)]).0;
        assert_eq!(base.digest(), recolored.digest());

        // A definition change moves the digest.
        let mut renamed = spec.clone();
        renamed.blocks[0].display_name = Some("Ruby Ore".to_string());
        let renamed = build_registry(ContentRegistry::builtin(), &[(identity("p"), renamed)]).0;
        assert_ne!(base.digest(), renamed.digest());

        // A texture path change moves the digest even when the layer index is
        // unchanged.
        let mut repathed = spec.clone();
        repathed.blocks[0].textures.all = Some("ore2.png".to_string());
        repathed.assets.insert("ore2.png".to_string(), asset(1));
        let repathed = build_registry(ContentRegistry::builtin(), &[(identity("p"), repathed)]).0;
        assert_eq!(repathed.pack_layers().len(), base.pack_layers().len());
        assert_ne!(base.digest(), repathed.digest());

        // A pack version change moves the digest.
        let mut versioned_identity = identity("p");
        versioned_identity.version = Some("2.0.0".to_string());
        let versioned = build_registry(
            ContentRegistry::builtin(),
            &[(versioned_identity, spec.clone())],
        )
        .0;
        assert_ne!(versioned.digest(), base.digest());
    }

    #[test]
    fn forged_modded_alias_is_still_rejected() {
        let (registry, _) = build_registry(ContentRegistry::builtin(), &[simple_pack("p")]);
        assert!(registry.block(BlockId::Modded(5)).is_none());
        assert!(registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT + 1))
            .is_none());
        let first = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("first pack block resolves");
        assert_eq!(first.name, "mymod:ruby_ore");
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

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        assert_eq!(registry.worldgen_rules().len(), 8);
        assert_eq!(report.errors(), 2, "{:?}", report.diagnostics);
        for (index, rule) in registry.worldgen_rules().iter().enumerate() {
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

        let pack = (identity("p"), spec);
        let (first, first_report) =
            build_registry(ContentRegistry::builtin(), std::slice::from_ref(&pack));
        let (second, second_report) =
            build_registry(ContentRegistry::builtin(), std::slice::from_ref(&pack));

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
        assert_eq!(
            first.recipes()[22].key[&'S'],
            ItemRef::Builtin(ItemId::Material(crate::player::inventory::Material::Stick))
        );
        assert_eq!(first.tools()[0].tier, 3);
        assert_eq!(first.tiers().len(), 4);
        assert_eq!(first.items()[0].name, "mymod:gem");
        assert_eq!(
            first.items()[1].food,
            Some(FoodDef {
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

        let (_, report) = build_registry(
            ContentRegistry::builtin(),
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

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        let def = registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("block still exists");
        assert_eq!(def.drop, None);
        assert_eq!(report.errors(), 2, "{:?}", report.diagnostics);
        assert!(registry.recipe_by_name("not_a_recipe").is_none());
    }

    #[test]
    fn empty_stack_is_the_builtin_registry() {
        let (registry, report) = build_registry(ContentRegistry::builtin(), &[]);
        assert_eq!(report, ContentLoadReport::default());
        assert_eq!(registry.blocks(), ContentRegistry::builtin().blocks());
        assert_eq!(registry.recipes(), ContentRegistry::builtin().recipes());
        assert_eq!(registry.digest(), EMPTY_DIGEST);
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

        let (plain_registry, plain_report) = build_registry(
            ContentRegistry::builtin(),
            &[(identity("p"), plain.clone())],
        );
        let (override_registry, override_report) = build_registry(
            ContentRegistry::builtin(),
            &[(identity("p"), with_override.clone())],
        );

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
        assert_eq!(override_registry.digest(), plain_registry.digest());
        assert_eq!(
            override_registry.pack_layers(),
            plain_registry.pack_layers()
        );
        assert_eq!(override_registry.pack_icons(), plain_registry.pack_icons());
        assert_eq!(
            override_registry.pack_icon_sources(),
            plain_registry.pack_icon_sources()
        );
        assert_eq!(
            override_registry.pack_layers().len(),
            plain_registry.pack_layers().len()
        );
        assert_eq!(
            override_registry.pack_icons().len(),
            plain_registry.pack_icons().len()
        );

        // Removing the override again is the same identity.
        let (restored, restored_report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), plain)]);
        assert_eq!(restored.digest(), override_registry.digest());
        assert_eq!(restored_report.entries, override_report.entries);
    }

    #[test]
    fn builtin_icon_with_unloaded_asset_warns_without_changing_identity() {
        let mut spec = PackContentSpec::default();
        spec.builtin_icons
            .insert("apple".to_string(), "missing.png".to_string());

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);
        let (empty, empty_report) = build_registry(
            ContentRegistry::builtin(),
            &[(identity("p"), PackContentSpec::default())],
        );

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
        assert_eq!(registry.digest(), empty.digest());
        assert_eq!(registry.pack_layers().len(), 0);
        assert_eq!(registry.pack_icons().len(), 0);
    }
}
