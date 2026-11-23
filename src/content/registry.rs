//! Game adapter over the neutral `opencraft-content` catalog.
//!
//! [`build_registry`] converts the game's built-in tables into a neutral
//! [`Catalog`] base, lets `opencraft_content::catalog::build_catalog` fold the
//! ordered pack stack, and converts every resulting row back into game types.
//! Allocation order, patch precedence, placeholder allocation, the FNV-1a
//! content digest, the layer/icon caps and the changed-stack notice all live in
//! `opencraft-content`; this module owns only the field-for-field identity
//! mapping, so no game type (`BlockId`, `ItemRef`, `ContentRegistry`) crosses
//! the boundary.
//!
//! Nothing here touches the filesystem, renderer, audio, settings or the pack
//! loader: input is a list of [`PackIdentity`] + [`PackContentSpec`] pairs.

pub use opencraft_content::catalog::{ContentLoadReport, PackIdentity, WORLDGEN_RULE_CAP};

use std::collections::BTreeMap;

use opencraft_content::catalog::{
    self as neutral, BlockDef as NeutralBlockDef, Catalog, ItemDef as NeutralItemDef,
    ItemRef as NeutralItemRef, RecipeDef as NeutralRecipeDef, TierDef as NeutralTierDef,
    ToolDef as NeutralToolDef, WorldgenRule as NeutralWorldgenRule,
};

use crate::content::names;
use crate::content::spec::{FoodSpec, PackContentSpec};
use crate::content::{
    BlockDef, ContentRegistry, FoodDef, ItemDef, ItemRef, RecipeDef, TierDef, ToolDef, WorldgenRule,
};
use crate::player::inventory::ItemId;
use crate::world::block::BlockId;
use crate::world::terrain::Biome;

/// Fold an ordered pack stack over `base` into a fresh registry.
///
/// The returned registry is immutable; the caller owns any `Arc` freezing.
/// A malformed reference records a [`crate::content::Severity::Error`]
/// diagnostic and falls back to the safe value instead of panicking (the pack
/// parser normally rejects those entries before they reach a spec).
pub fn build_registry(
    base: &ContentRegistry,
    packs: &[(PackIdentity, PackContentSpec)],
) -> (ContentRegistry, ContentLoadReport) {
    let base = base_catalog(base);
    let (catalog, report) = neutral::build_catalog(&base, packs);
    (from_catalog(catalog), report)
}

/// Converts the game's built-in tables into a neutral base catalog.
///
/// Only the block, tier and recipe tables are populated: items, tools,
/// worldgen rules and every pack table start empty, exactly like today's
/// built-in-only registry. The game base `worldgen` table stays ignored.
fn base_catalog(base: &ContentRegistry) -> Catalog {
    Catalog::from_base(
        base.blocks().iter().map(block_to_catalog).collect(),
        base.tiers().iter().map(tier_to_catalog).collect(),
        base.recipes().iter().map(recipe_to_catalog).collect(),
    )
}

/// Converts every neutral catalog table back into a [`ContentRegistry`].
fn from_catalog(catalog: Catalog) -> ContentRegistry {
    ContentRegistry {
        blocks: catalog
            .blocks()
            .iter()
            .cloned()
            .map(block_from_catalog)
            .collect(),
        items: catalog
            .items()
            .iter()
            .cloned()
            .map(item_from_catalog)
            .collect(),
        tiers: catalog
            .tiers()
            .iter()
            .cloned()
            .map(tier_from_catalog)
            .collect(),
        tools: catalog
            .tools()
            .iter()
            .cloned()
            .map(tool_from_catalog)
            .collect(),
        recipes: catalog
            .recipes()
            .iter()
            .cloned()
            .map(recipe_from_catalog)
            .collect(),
        worldgen_rules: catalog
            .worldgen_rules()
            .iter()
            .cloned()
            .map(worldgen_from_catalog)
            .collect(),
        pack_layers: catalog.pack_layers().to_vec(),
        pack_icons: catalog.pack_icons().to_vec(),
        pack_layer_sources: catalog.pack_layer_sources().to_vec(),
        pack_icon_sources: catalog.pack_icon_sources().to_vec(),
        packs: catalog.packs().to_vec(),
        digest: catalog.digest(),
    }
}

/// Converts one neutral block row into the game definition.
///
/// Shared with placeholder allocation in later slices so a placeholder block
/// has exactly one construction path.
pub(crate) fn block_from_catalog(row: NeutralBlockDef) -> BlockDef {
    BlockDef {
        id: BlockId::from_raw(row.id),
        name: row.name,
        display_name: row.display_name,
        faces: row.faces,
        hardness: row.hardness,
        drop: row.drop.map(item_ref_from_catalog),
        tool_class: row.tool_class,
        stack_limit: row.stack_limit,
        model: row.model,
        alpha: row.alpha,
        placeholder: row.placeholder,
    }
}

/// Converts one neutral generic item row into the game definition.
///
/// Shared with placeholder allocation in later slices so a placeholder item
/// has exactly one construction path.
pub(crate) fn item_from_catalog(row: NeutralItemDef) -> ItemDef {
    ItemDef {
        index: row.index,
        name: row.name,
        display_name: row.display_name,
        icon_layer: row.icon_layer,
        stack_limit: row.stack_limit,
        food: row.food.map(food_from_catalog),
        placeholder: row.placeholder,
        placeholder_tool: row.placeholder_tool,
    }
}

/// Converts one neutral tier row into the game definition.
fn tier_from_catalog(row: NeutralTierDef) -> TierDef {
    TierDef {
        index: row.index,
        name: row.name,
        display_name: row.display_name,
        speed: row.speed,
        durability: row.durability,
        damage: row.damage,
    }
}

/// Converts one neutral tool row into the game definition.
fn tool_from_catalog(row: NeutralToolDef) -> ToolDef {
    ToolDef {
        index: row.index,
        name: row.name,
        display_name: row.display_name,
        kind: row.kind,
        tier: row.tier,
        icon_layer: row.icon_layer,
    }
}

/// Converts one neutral recipe row into the game definition.
fn recipe_from_catalog(row: NeutralRecipeDef) -> RecipeDef {
    RecipeDef {
        index: row.index,
        name: row.name,
        pattern: row.pattern,
        key: row
            .key
            .into_iter()
            .map(|(character, reference)| (character, item_ref_from_catalog(reference)))
            .collect(),
        output: item_ref_from_catalog(row.output),
        count: row.count,
        enabled: row.enabled,
        placeholder: row.placeholder,
    }
}

/// Converts one neutral worldgen rule into the game rule.
fn worldgen_from_catalog(row: NeutralWorldgenRule) -> WorldgenRule {
    match row {
        NeutralWorldgenRule::Ore {
            block,
            replace,
            min_y,
            max_y,
            vein,
            attempts,
        } => WorldgenRule::Ore {
            block: BlockId::from_raw(block),
            replace: BlockId::from_raw(replace),
            min_y,
            max_y,
            vein,
            attempts,
        },
        NeutralWorldgenRule::Patch {
            block,
            replace,
            biomes,
            min_y,
            max_y,
            chance,
        } => WorldgenRule::Patch {
            block: BlockId::from_raw(block),
            replace: BlockId::from_raw(replace),
            biomes: biomes.iter().map(|tag| biome_from_tag(*tag)).collect(),
            min_y,
            max_y,
            chance,
        },
    }
}

/// Converts one game worldgen rule into the neutral catalog rule.
///
/// Exact inverse of [`worldgen_from_catalog`]: block ids become raw `u16`,
/// biomes become neutral tags, and every field and vector entry keeps its
/// authored order. The empty rule stack is not special-cased here; the
/// terrain adapter borrows it without calling this function.
pub(crate) fn worldgen_to_catalog(rule: &WorldgenRule) -> NeutralWorldgenRule {
    match rule {
        WorldgenRule::Ore {
            block,
            replace,
            min_y,
            max_y,
            vein,
            attempts,
        } => NeutralWorldgenRule::Ore {
            block: block.to_raw(),
            replace: replace.to_raw(),
            min_y: *min_y,
            max_y: *max_y,
            vein: *vein,
            attempts: *attempts,
        },
        WorldgenRule::Patch {
            block,
            replace,
            biomes,
            min_y,
            max_y,
            chance,
        } => NeutralWorldgenRule::Patch {
            block: block.to_raw(),
            replace: replace.to_raw(),
            biomes: biomes.iter().map(|biome| biome.tag()).collect(),
            min_y: *min_y,
            max_y: *max_y,
            chance: *chance,
        },
    }
}

/// Converts a game block definition into its neutral row.
fn block_to_catalog(def: &BlockDef) -> NeutralBlockDef {
    NeutralBlockDef {
        id: def.id.to_raw(),
        name: def.name.clone(),
        display_name: def.display_name.clone(),
        faces: def.faces,
        hardness: def.hardness,
        drop: def.drop.map(item_ref_to_catalog),
        tool_class: def.tool_class,
        stack_limit: def.stack_limit,
        model: def.model,
        alpha: def.alpha,
        placeholder: def.placeholder,
    }
}

/// Converts a game tier definition into its neutral row.
fn tier_to_catalog(def: &TierDef) -> NeutralTierDef {
    NeutralTierDef {
        index: def.index,
        name: def.name.clone(),
        display_name: def.display_name.clone(),
        speed: def.speed,
        durability: def.durability,
        damage: def.damage,
    }
}

/// Converts a game recipe definition into its neutral row.
fn recipe_to_catalog(def: &RecipeDef) -> NeutralRecipeDef {
    NeutralRecipeDef {
        index: def.index,
        name: def.name.clone(),
        pattern: def.pattern.clone(),
        key: def
            .key
            .iter()
            .map(|(character, reference)| (*character, item_ref_to_catalog(*reference)))
            .collect::<BTreeMap<_, _>>(),
        output: item_ref_to_catalog(def.output),
        count: def.count,
        enabled: def.enabled,
        placeholder: def.placeholder,
    }
}

/// Converts a neutral item reference into the game reference.
///
/// `BuiltinMaterial` indexes the frozen [`crate::content::names`] material
/// table and `BuiltinTool` the frozen tier table, so built-in identity (and
/// therefore the digest bytes) survives the round trip.
fn item_ref_from_catalog(reference: NeutralItemRef) -> ItemRef {
    match reference {
        NeutralItemRef::Block(raw) => ItemRef::Block(BlockId::from_raw(raw)),
        NeutralItemRef::BuiltinMaterial(index) => {
            let material = names::builtin_material_by_index(index)
                .expect("the catalog only allocates in-range material indexes");
            ItemRef::Builtin(ItemId::Material(material))
        }
        NeutralItemRef::BuiltinTool { kind, tier } => {
            let tier = names::builtin_tier_by_index(tier)
                .expect("the catalog only allocates in-range tool tier indexes");
            ItemRef::Builtin(ItemId::Tool { kind, tier })
        }
        NeutralItemRef::Item(index) => ItemRef::Item(index),
        NeutralItemRef::Tool(index) => ItemRef::Tool(index),
    }
}

/// Converts a game item reference into the neutral reference.
fn item_ref_to_catalog(reference: ItemRef) -> NeutralItemRef {
    match reference {
        ItemRef::Block(block) => NeutralItemRef::Block(block.to_raw()),
        ItemRef::Builtin(id) => item_id_to_catalog(id),
        ItemRef::Item(index) => NeutralItemRef::Item(index),
        ItemRef::Tool(index) => NeutralItemRef::Tool(index),
    }
}

/// Converts a built-in [`ItemId`] into the neutral reference variants.
///
/// `ItemId::Block` stays a block; materials and tools use the frozen
/// vocabulary indexes so the digest hashes the same bytes as before.
fn item_id_to_catalog(id: ItemId) -> NeutralItemRef {
    match id {
        ItemId::Block(block) => NeutralItemRef::Block(block.to_raw()),
        ItemId::Material(material) => {
            NeutralItemRef::BuiltinMaterial(names::builtin_material_index(material))
        }
        ItemId::Tool { kind, tier } => NeutralItemRef::BuiltinTool {
            kind,
            tier: names::builtin_tier_index(tier),
        },
        ItemId::Item(index) => NeutralItemRef::Item(index),
        ItemId::ModTool(index) => NeutralItemRef::Tool(index),
    }
}

/// Converts neutral authored food behaviour into the game definition.
fn food_from_catalog(food: FoodSpec) -> FoodDef {
    FoodDef {
        nutrition: food.nutrition,
        eat_seconds: food.eat_seconds,
    }
}

/// Maps a neutral biome tag onto the world enum.
fn biome_from_tag(tag: opencraft_content::catalog::BiomeTag) -> Biome {
    match tag {
        opencraft_content::catalog::BiomeTag::Plains => Biome::Plains,
        opencraft_content::catalog::BiomeTag::Forest => Biome::Forest,
        opencraft_content::catalog::BiomeTag::Mountains => Biome::Mountains,
        opencraft_content::catalog::BiomeTag::Ocean => Biome::Ocean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::spec::{
        BlockAlpha, BlockEntry, BlockModel, BlockPatch, BlockTextures, ItemEntry, RecipeEntry,
        TierEntry, ToolEntry, WorldgenEntry, WorldgenKind,
    };
    use crate::content::EMPTY_DIGEST;
    use crate::player::inventory::{Material, ToolKind};
    use crate::world::block::BUILTIN_BLOCK_COUNT;

    fn identity(folder: &str) -> PackIdentity {
        PackIdentity {
            folder: folder.to_string(),
            version: Some("1.0.0".to_string()),
        }
    }

    fn asset(byte: u8) -> crate::content::spec::PackAsset {
        crate::content::spec::PackAsset {
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
            model: BlockModel::Cube,
            alpha: BlockAlpha::Opaque,
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

    /// One stack that exercises every content kind the adapter converts.
    fn representative_pack() -> (PackIdentity, PackContentSpec) {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_ore".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ruby_ore.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 3.0,
            drop: Some("mymod:ruby".to_string()),
            tool: Some(ToolKind::Pickaxe),
            stack_limit: 64,
            model: BlockModel::Cube,
            alpha: BlockAlpha::Opaque,
        });
        spec.assets.insert("ruby_ore.png".to_string(), asset(0x11));
        spec.items.push(item_entry("mymod:ruby", "ruby.png"));
        spec.assets.insert("ruby.png".to_string(), asset(0x22));
        spec.tiers.push(tier_entry("mymod:ruby"));
        spec.tools.push(tool_entry(
            "mymod:ruby_pickaxe",
            "mymod:ruby",
            "ruby_pickaxe.png",
        ));
        spec.assets
            .insert("ruby_pickaxe.png".to_string(), asset(0x33));
        let mut key = BTreeMap::new();
        key.insert('R', "mymod:ruby".to_string());
        key.insert('S', "stick".to_string());
        spec.recipes.push(RecipeEntry {
            id: "mymod:ruby_pickaxe".to_string(),
            pattern: vec!["RRR".to_string(), " S ".to_string(), " S ".to_string()],
            key,
            output: "mymod:ruby_pickaxe".to_string(),
            count: 1,
        });
        spec.block_patches.push(BlockPatch {
            id: "stone".to_string(),
            display_name: Some("Granite".to_string()),
            hardness: Some(2.5),
            drop: None,
            tool: None,
            stack_limit: None,
            model: None,
            alpha: None,
        });
        spec.worldgen.push(WorldgenEntry {
            kind: WorldgenKind::Ore {
                block: "mymod:ruby_ore".to_string(),
                replace: "stone".to_string(),
                min_y: 4,
                max_y: 40,
                vein: 4,
                attempts: 8,
            },
        });
        spec.worldgen.push(WorldgenEntry {
            kind: WorldgenKind::Patch {
                block: "mymod:ruby_ore".to_string(),
                replace: "grass".to_string(),
                biomes: vec!["plains".to_string(), "forest".to_string()],
                min_y: 40,
                max_y: 110,
                chance: 0.02,
            },
        });
        (identity("mymod-ruby"), spec)
    }

    /// Compares a neutral worldgen rule with its game conversion.
    fn same_worldgen_rule(neutral: &NeutralWorldgenRule, game: &WorldgenRule) -> bool {
        match (neutral, game) {
            (
                NeutralWorldgenRule::Ore {
                    block,
                    replace,
                    min_y,
                    max_y,
                    vein,
                    attempts,
                },
                WorldgenRule::Ore {
                    block: game_block,
                    replace: game_replace,
                    min_y: game_min_y,
                    max_y: game_max_y,
                    vein: game_vein,
                    attempts: game_attempts,
                },
            ) => {
                *block == game_block.to_raw()
                    && *replace == game_replace.to_raw()
                    && *min_y == *game_min_y
                    && *max_y == *game_max_y
                    && *vein == *game_vein
                    && *attempts == *game_attempts
            }
            (
                NeutralWorldgenRule::Patch {
                    block,
                    replace,
                    biomes,
                    min_y,
                    max_y,
                    chance,
                },
                WorldgenRule::Patch {
                    block: game_block,
                    replace: game_replace,
                    biomes: game_biomes,
                    min_y: game_min_y,
                    max_y: game_max_y,
                    chance: game_chance,
                },
            ) => {
                *block == game_block.to_raw()
                    && *replace == game_replace.to_raw()
                    && biomes
                        .iter()
                        .map(|tag| biome_from_tag(*tag))
                        .eq(game_biomes.iter().copied())
                    && *min_y == *game_min_y
                    && *max_y == *game_max_y
                    && *chance == *game_chance
            }
            _ => false,
        }
    }

    #[test]
    fn malformed_references_report_through_the_adapter() {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(BlockEntry {
            id: "mymod:ore".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ore.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.0,
            drop: Some("mymod:missing".to_string()),
            tool: None,
            stack_limit: 64,
            model: BlockModel::Cube,
            alpha: BlockAlpha::Opaque,
        });
        spec.assets.insert("ore.png".to_string(), asset(0x11));
        spec.recipe_patches.push(crate::content::spec::RecipePatch {
            id: "not_a_recipe".to_string(),
            enabled: false,
        });

        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[(identity("p"), spec)]);

        assert_eq!(report.errors(), 2, "{:?}", report.diagnostics);
        let block = registry
            .block_by_name("mymod:ore")
            .expect("the block is still allocated");
        assert_eq!(block.drop, None, "an unresolved drop falls back to none");
        assert!(registry.recipe_by_name("not_a_recipe").is_none());
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
    fn empty_stack_rebuilds_the_builtin_registry() {
        let builtin = ContentRegistry::builtin();
        let (registry, report) = build_registry(builtin, &[]);

        assert_eq!(registry.blocks(), builtin.blocks());
        assert_eq!(registry.tiers(), builtin.tiers());
        assert_eq!(registry.recipes(), builtin.recipes());
        assert!(registry.items().is_empty());
        assert!(registry.tools().is_empty());
        assert!(registry.worldgen_rules().is_empty());
        assert_eq!(
            report,
            crate::content::registry::ContentLoadReport::default()
        );
        assert_eq!(registry.digest(), EMPTY_DIGEST);
    }

    #[test]
    fn catalog_conversion_is_lossless_for_builtin_tables() {
        let builtin = ContentRegistry::builtin();
        let catalog = base_catalog(builtin);
        let round_tripped = from_catalog(catalog);

        assert_eq!(round_tripped.blocks(), builtin.blocks());
        assert_eq!(round_tripped.tiers(), builtin.tiers());
        assert_eq!(round_tripped.recipes(), builtin.recipes());
        assert!(round_tripped.items().is_empty());
        assert!(round_tripped.tools().is_empty());
        assert!(round_tripped.worldgen_rules().is_empty());
        assert!(round_tripped.pack_layers().is_empty());
        assert!(round_tripped.pack_icons().is_empty());
        assert!(round_tripped.packs().is_empty());
        assert_eq!(round_tripped.digest(), EMPTY_DIGEST);
    }

    #[test]
    fn game_digest_matches_the_neutral_catalog_for_a_representative_pack() {
        let packs = [representative_pack()];

        let (registry, report) = build_registry(ContentRegistry::builtin(), &packs);
        let base = base_catalog(ContentRegistry::builtin());
        let (catalog, neutral_report) = neutral::build_catalog(&base, &packs);

        // The adapter is a field-for-field conversion: every observable value,
        // including the digest, matches the neutral catalog byte for byte.
        assert_eq!(report, neutral_report);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(registry.digest(), catalog.digest());
        assert_eq!(registry.packs(), catalog.packs());
        assert_eq!(registry.pack_layers(), catalog.pack_layers());
        assert_eq!(registry.pack_icons(), catalog.pack_icons());
        assert_eq!(registry.pack_layer_sources(), catalog.pack_layer_sources());
        assert_eq!(registry.pack_icon_sources(), catalog.pack_icon_sources());
        assert_eq!(registry.worldgen_rules().len(), 2);
        assert_eq!(
            registry.worldgen_rules().len(),
            catalog.worldgen_rules().len()
        );
        for (game, neutral) in registry
            .worldgen_rules()
            .iter()
            .zip(catalog.worldgen_rules())
        {
            assert!(
                same_worldgen_rule(neutral, game),
                "worldgen rule conversion is lossless: {game:?}"
            );
            // The inverse direction must also be exact, or the terrain
            // adapter could reorder or rewrite a rule on its way back into
            // the crate (pack ores/patches would drift).
            assert_eq!(
                worldgen_from_catalog(worldgen_to_catalog(game)),
                *game,
                "game -> neutral -> game must round-trip every rule field"
            );
        }

        let block = registry
            .block_by_name("mymod:ruby_ore")
            .expect("the pack block is registered");
        assert_eq!(block.id, BlockId::Modded(BUILTIN_BLOCK_COUNT));
        assert_eq!(block.faces, [48; 6]);
        assert_eq!(block.drop, Some(ItemRef::Item(0)));
        assert_eq!(block.tool_class, Some(ToolKind::Pickaxe));

        let item = registry
            .item_by_name("mymod:ruby")
            .expect("the pack item is registered");
        assert_eq!(item.index, 0);
        assert_eq!(item.icon_layer, 32);

        assert_eq!(
            registry.tier(3).map(|tier| tier.name.as_str()),
            Some("mymod:ruby")
        );
        let tool = registry
            .tool_by_name("mymod:ruby_pickaxe")
            .expect("the pack tool is registered");
        assert_eq!(tool.index, 0);
        assert_eq!(tool.tier, 3);

        let recipe = registry
            .recipe_by_name("mymod:ruby_pickaxe")
            .expect("the pack recipe is registered");
        assert_eq!(recipe.index, 22);
        assert_eq!(recipe.output, ItemRef::Tool(0));
        assert_eq!(recipe.key.get(&'R'), Some(&ItemRef::Item(0)));
        assert_eq!(
            recipe.key.get(&'S'),
            Some(&ItemRef::Builtin(ItemId::Material(Material::Stick)))
        );

        // The block patch survives in both directions and does not move the
        // game-side identity.
        let stone = registry
            .block(BlockId::Stone)
            .expect("stone stays registered");
        assert_eq!(stone.display_name, "Granite");
        assert_eq!(stone.hardness, Some(2.5));
        assert_eq!(stone.id, BlockId::Stone);
    }
}
