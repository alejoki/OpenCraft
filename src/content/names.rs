//! Game-side content name vocabulary: id mappings over the neutral tables.
//!
//! The frozen name tables, biome lookups and the pure `is_builtin_*` /
//! namespace predicates live in `opencraft-content::vocabulary` and are
//! re-exported here, so there is exactly one source of truth. This module adds
//! only the game-side reverse lookups that map a stable short name onto a
//! [`BlockId`], [`ItemId`], [`Material`], [`ToolTier`] or [`RecipeId`].
//!
//! Every table is index-aligned to the declaration order of its Rust enum
//! (or, for blocks, the frozen raw id order `0..=23`). Built-in names are
//! unqualified: they never contain a `:` namespace separator.

use crate::player::inventory::{ItemId, Material, RecipeId, ToolKind, ToolKindTiers, ToolTier};
use crate::world::block::BlockId;

pub use opencraft_content::vocabulary::*;

/// Reverse lookup for a built-in block short name.
pub fn builtin_block_id_by_name(name: &str) -> Option<BlockId> {
    BLOCK_ID_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .map(|raw| BlockId::from_raw(raw as u16))
}

/// Reverse lookup for a built-in material short name.
pub fn builtin_material_by_name(name: &str) -> Option<Material> {
    MATERIAL_ID_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| MATERIAL_VALUES[index])
}

/// Frozen table index of `material` in [`MATERIAL_ID_NAMES`].
///
/// This is the neutral catalog's `ItemRef::BuiltinMaterial` payload; the
/// table is complete by construction, so every `Material` has an index.
pub(crate) fn builtin_material_index(material: Material) -> u8 {
    let index = MATERIAL_VALUES
        .iter()
        .position(|candidate| *candidate == material)
        .expect("every material has a frozen vocabulary index");
    index as u8
}

/// The material at a frozen table index, or `None` when it is out of range.
pub(crate) fn builtin_material_by_index(index: u8) -> Option<Material> {
    MATERIAL_VALUES.get(usize::from(index)).copied()
}

/// Reverse lookup for a built-in tool tier short name.
pub fn builtin_tier_by_name(name: &str) -> Option<ToolTier> {
    TIER_ID_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| TIER_VALUES[index])
}

/// Frozen table index of `tier` in [`TIER_ID_NAMES`].
///
/// This is the neutral catalog's `ItemRef::BuiltinTool` tier payload; the
/// table is complete by construction, so every `ToolTier` has an index.
pub(crate) fn builtin_tier_index(tier: ToolTier) -> u8 {
    let index = TIER_VALUES
        .iter()
        .position(|candidate| *candidate == tier)
        .expect("every tier has a frozen vocabulary index");
    index as u8
}

/// The tier at a frozen table index, or `None` when it is out of range.
pub(crate) fn builtin_tier_by_index(index: u8) -> Option<ToolTier> {
    TIER_VALUES.get(usize::from(index)).copied()
}

/// Reverse lookup for a built-in constructible tool short name.
pub fn builtin_tool_id_by_name(name: &str) -> Option<ItemId> {
    for (index, candidate) in TOOL_ID_NAMES.iter().enumerate() {
        if *candidate != name {
            continue;
        }
        if index == 15 {
            return Some(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            });
        }
        let kind = KIND_VALUES[index % 5];
        let tier = TIER_VALUES[index / 5];
        return Some(ItemId::Tool { kind, tier });
    }
    None
}

/// Reverse lookup for a built-in recipe short name.
pub fn builtin_recipe_id_by_name(name: &str) -> Option<RecipeId> {
    RECIPE_ID_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| RECIPE_VALUES[index])
}

/// Resolve an unqualified built-in item name.
///
/// A block name maps to [`ItemId::Block`], a material name to
/// [`ItemId::Material`], and a tool name to [`ItemId::Tool`]. Namespaced pack
/// ids are not built-in and return `None`.
pub fn builtin_item_id_by_name(name: &str) -> Option<ItemId> {
    if let Some(block) = builtin_block_id_by_name(name) {
        return Some(ItemId::Block(block));
    }
    if let Some(material) = builtin_material_by_name(name) {
        return Some(ItemId::Material(material));
    }
    builtin_tool_id_by_name(name)
}

/// Resolve `name` to a built-in item identity that owns a procedural sprite.
///
/// Blocks are rejected: their icons are sampled from the block texture array,
/// not the item sprite atlas. Forged tool pairs are rejected too, so only
/// constructible `(kind, tier)` tools (the wood-only bow included) can be
/// overridden.
#[allow(dead_code)] // Consumed by the S07 parser and presentation layers.
pub fn builtin_item_sprite_id_by_name(name: &str) -> Option<ItemId> {
    sprite_identity(builtin_item_id_by_name(name)?)
}

/// Canonical short name for a built-in item identity that owns a sprite.
///
/// This is the exact inverse of [`builtin_item_sprite_id_by_name`] for every
/// entry of the pinned item sprite table: materials resolve through
/// [`builtin_material_id_name`] and constructible tools (the wood-only bow
/// included) through [`builtin_tool_id_name`]. Blocks, registry items, forged
/// tool pairs and unconstructible bow ties return `None`.
#[allow(dead_code)] // Consumed by the S02 default-pack exporter.
pub fn builtin_sprite_name_for(id: ItemId) -> Option<&'static str> {
    match id {
        ItemId::Material(material) => Some(builtin_material_id_name(material)),
        ItemId::Tool { kind, tier } if kind.allowed_tiers().contains(&tier) => {
            builtin_tool_id_name(kind, tier)
        }
        _ => None,
    }
}

/// Canonical short name for a built-in material/tool sprite identity.
#[allow(dead_code)] // Consumed by the S07 presentation layer.
pub fn builtin_item_sprite_name(name: &str) -> Option<&'static str> {
    match sprite_identity(builtin_item_id_by_name(name)?)? {
        ItemId::Material(material) => Some(builtin_material_id_name(material)),
        ItemId::Tool { kind, tier } => builtin_tool_id_name(kind, tier),
        _ => None,
    }
}

/// Keep only identities backed by a procedural item sprite layer.
fn sprite_identity(id: ItemId) -> Option<ItemId> {
    match id {
        id @ ItemId::Material(_) => Some(id),
        id @ ItemId::Tool { kind, tier } if kind.allowed_tiers().contains(&tier) => Some(id),
        _ => None,
    }
}

const MATERIAL_VALUES: [Material; 11] = [
    Material::Stick,
    Material::Apple,
    Material::Wheat,
    Material::WheatSeeds,
    Material::Bread,
    Material::Leather,
    Material::RawPorkchop,
    Material::RawBeef,
    Material::RottenFlesh,
    Material::Bone,
    Material::Arrow,
];

const TIER_VALUES: [ToolTier; 3] = [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron];

const KIND_VALUES: [ToolKind; 5] = [
    ToolKind::Pickaxe,
    ToolKind::Axe,
    ToolKind::Shovel,
    ToolKind::Hoe,
    ToolKind::Sword,
];

const RECIPE_VALUES: [RecipeId; 22] = [
    RecipeId::Planks,
    RecipeId::Sticks,
    RecipeId::CraftingTable,
    RecipeId::Chest,
    RecipeId::WoodPickaxe,
    RecipeId::WoodAxe,
    RecipeId::WoodShovel,
    RecipeId::StonePickaxe,
    RecipeId::StoneAxe,
    RecipeId::StoneShovel,
    RecipeId::IronPickaxe,
    RecipeId::IronAxe,
    RecipeId::IronShovel,
    RecipeId::Bread,
    RecipeId::WoodHoe,
    RecipeId::StoneHoe,
    RecipeId::IronHoe,
    RecipeId::WoodSword,
    RecipeId::StoneSword,
    RecipeId::IronSword,
    RecipeId::Shortbow,
    RecipeId::Arrows,
];

/// Stable short name for a built-in block id, or `None` for [`BlockId::Modded`].
pub fn builtin_block_id_name(id: BlockId) -> Option<&'static str> {
    if !id.is_builtin() {
        return None;
    }
    BLOCK_ID_NAMES.get(usize::from(id.to_raw())).copied()
}

/// Stable short name for a material.
pub fn builtin_material_id_name(material: Material) -> &'static str {
    MATERIAL_ID_NAMES[match material {
        Material::Stick => 0,
        Material::Apple => 1,
        Material::Wheat => 2,
        Material::WheatSeeds => 3,
        Material::Bread => 4,
        Material::Leather => 5,
        Material::RawPorkchop => 6,
        Material::RawBeef => 7,
        Material::RottenFlesh => 8,
        Material::Bone => 9,
        Material::Arrow => 10,
    }]
}

/// Stable short name for a tool tier.
pub fn builtin_tier_id_name(tier: ToolTier) -> &'static str {
    TIER_ID_NAMES[match tier {
        ToolTier::Wood => 0,
        ToolTier::Stone => 1,
        ToolTier::Iron => 2,
    }]
}

/// Stable short name for a constructible `(kind, tier)` tool pair.
///
/// Returns `None` for the two invalid bow combinations, `(Bow, Stone)` and
/// `(Bow, Iron)`: the bow is wood-only (D133/MEM396).
pub fn builtin_tool_id_name(kind: ToolKind, tier: ToolTier) -> Option<&'static str> {
    let tier_index = match tier {
        ToolTier::Wood => 0usize,
        ToolTier::Stone => 1,
        ToolTier::Iron => 2,
    };
    let kind_index = match kind {
        ToolKind::Pickaxe => 0usize,
        ToolKind::Axe => 1,
        ToolKind::Shovel => 2,
        ToolKind::Hoe => 3,
        ToolKind::Sword => 4,
        ToolKind::Bow => {
            return (tier == ToolTier::Wood).then_some(TOOL_ID_NAMES[15]);
        }
    };
    Some(TOOL_ID_NAMES[tier_index * 5 + kind_index])
}

/// Stable short name for a recipe, or `None` for a registry pack recipe.
pub fn builtin_recipe_id_name(recipe: RecipeId) -> Option<&'static str> {
    RECIPE_ID_NAMES.get(builtin_recipe_index(recipe)?).copied()
}

/// The recipe at `index` in the frozen built-in recipe order.
pub fn builtin_recipe_by_index(index: u16) -> Option<RecipeId> {
    RECIPE_VALUES.get(usize::from(index)).copied()
}

/// The frozen index of a built-in recipe, or `None` for [`RecipeId::Modded`].
pub fn builtin_recipe_index(recipe: RecipeId) -> Option<usize> {
    RECIPE_VALUES
        .iter()
        .position(|candidate| *candidate == recipe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn is_snake_case(value: &str) -> bool {
        !value.is_empty()
            && !value.starts_with('_')
            && !value.ends_with('_')
            && !value.contains("__")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    }

    fn assert_unique(names: &[&str]) {
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "duplicate names in {names:?}");
    }

    fn snake_case(camel: &str) -> String {
        let mut out = String::new();
        for (index, ch) in camel.char_indices() {
            if ch.is_ascii_uppercase() {
                if index > 0 {
                    out.push('_');
                }
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn builtin_block_names_align_with_raw_order() {
        let expected = [
            "air",
            "grass",
            "dirt",
            "stone",
            "cobblestone",
            "sand",
            "oak_log",
            "oak_leaves",
            "oak_planks",
            "brick",
            "snow",
            "bedrock",
            "coal_ore",
            "iron_ore",
            "crafting_table",
            "water",
            "farmland",
            "wheat_stage_0",
            "wheat_stage_1",
            "wheat_stage_2",
            "wheat_stage_3",
            "chest",
            "loot_chest",
            "socket_marker",
        ];
        assert_eq!(expected, BLOCK_ID_NAMES);
        for name in BLOCK_ID_NAMES {
            assert!(!name.contains(':'), "{name} must be unqualified");
            assert!(is_snake_case(name), "{name} must be lowercase snake_case");
        }
    }

    #[test]
    fn material_tier_tool_recipe_tables_are_index_aligned_and_unique() {
        let expected_materials = [
            "stick",
            "apple",
            "wheat",
            "wheat_seeds",
            "bread",
            "leather",
            "raw_porkchop",
            "raw_beef",
            "rotten_flesh",
            "bone",
            "arrow",
        ];
        let expected_tiers = ["wood", "stone", "iron"];
        let expected_tools = [
            "wood_pickaxe",
            "wood_axe",
            "wood_shovel",
            "wood_hoe",
            "wood_sword",
            "stone_pickaxe",
            "stone_axe",
            "stone_shovel",
            "stone_hoe",
            "stone_sword",
            "iron_pickaxe",
            "iron_axe",
            "iron_shovel",
            "iron_hoe",
            "iron_sword",
            "wood_bow",
        ];
        let expected_recipes = [
            "planks",
            "sticks",
            "crafting_table",
            "chest",
            "wood_pickaxe",
            "wood_axe",
            "wood_shovel",
            "stone_pickaxe",
            "stone_axe",
            "stone_shovel",
            "iron_pickaxe",
            "iron_axe",
            "iron_shovel",
            "bread",
            "wood_hoe",
            "stone_hoe",
            "iron_hoe",
            "wood_sword",
            "stone_sword",
            "iron_sword",
            "shortbow",
            "arrows",
        ];

        assert_eq!(expected_materials, MATERIAL_ID_NAMES);
        assert_eq!(expected_tiers, TIER_ID_NAMES);
        assert_eq!(expected_tools, TOOL_ID_NAMES);
        assert_eq!(expected_recipes, RECIPE_ID_NAMES);
        assert_eq!(MATERIAL_ID_NAMES.len(), 11);
        assert_eq!(TIER_ID_NAMES.len(), 3);
        assert_eq!(TOOL_ID_NAMES.len(), 16);
        assert_eq!(RECIPE_ID_NAMES.len(), 22);

        assert_unique(&MATERIAL_ID_NAMES);
        assert_unique(&TIER_ID_NAMES);
        assert_unique(&TOOL_ID_NAMES);
        assert_unique(&RECIPE_ID_NAMES);

        let materials = [
            Material::Stick,
            Material::Apple,
            Material::Wheat,
            Material::WheatSeeds,
            Material::Bread,
            Material::Leather,
            Material::RawPorkchop,
            Material::RawBeef,
            Material::RottenFlesh,
            Material::Bone,
            Material::Arrow,
        ];
        for (index, material) in materials.into_iter().enumerate() {
            assert_eq!(
                builtin_material_id_name(material),
                MATERIAL_ID_NAMES[index],
                "material table misaligned at {index}"
            );
        }

        let tiers = [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron];
        for (index, tier) in tiers.into_iter().enumerate() {
            assert_eq!(builtin_tier_id_name(tier), TIER_ID_NAMES[index]);
        }

        let kinds = [
            ToolKind::Pickaxe,
            ToolKind::Axe,
            ToolKind::Shovel,
            ToolKind::Hoe,
            ToolKind::Sword,
            ToolKind::Bow,
        ];
        let mut combos = 0;
        for kind in kinds {
            for &tier in kind.allowed_tiers() {
                let name = builtin_tool_id_name(kind, tier)
                    .unwrap_or_else(|| panic!("missing tool name for {kind:?}/{tier:?}"));
                if kind == ToolKind::Bow {
                    assert_eq!(name, "wood_bow");
                } else {
                    assert_eq!(
                        name,
                        format!("{}_{}", snake_case(tier.name()), snake_case(kind.name())),
                        "tool name misaligned for {kind:?}/{tier:?}"
                    );
                }
                combos += 1;
            }
        }
        assert_eq!(combos, 16);
        assert_eq!(builtin_tool_id_name(ToolKind::Bow, ToolTier::Stone), None);
        assert_eq!(builtin_tool_id_name(ToolKind::Bow, ToolTier::Iron), None);

        let recipes = [
            RecipeId::Planks,
            RecipeId::Sticks,
            RecipeId::CraftingTable,
            RecipeId::Chest,
            RecipeId::WoodPickaxe,
            RecipeId::WoodAxe,
            RecipeId::WoodShovel,
            RecipeId::StonePickaxe,
            RecipeId::StoneAxe,
            RecipeId::StoneShovel,
            RecipeId::IronPickaxe,
            RecipeId::IronAxe,
            RecipeId::IronShovel,
            RecipeId::Bread,
            RecipeId::WoodHoe,
            RecipeId::StoneHoe,
            RecipeId::IronHoe,
            RecipeId::WoodSword,
            RecipeId::StoneSword,
            RecipeId::IronSword,
            RecipeId::Shortbow,
            RecipeId::Arrows,
        ];
        for (index, recipe) in recipes.into_iter().enumerate() {
            assert_eq!(
                builtin_recipe_id_name(recipe),
                Some(RECIPE_ID_NAMES[index]),
                "recipe table misaligned at {index}"
            );
        }
        assert_eq!(builtin_recipe_id_name(RecipeId::Modded(22)), None);
    }

    #[test]
    fn builtin_sprite_identities_reject_blocks_forged_tools_and_unknown_names() {
        assert_eq!(
            builtin_item_sprite_id_by_name("apple"),
            Some(ItemId::Material(Material::Apple))
        );
        assert_eq!(
            builtin_item_sprite_id_by_name("wood_pickaxe"),
            Some(ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            })
        );
        assert_eq!(
            builtin_item_sprite_id_by_name("wood_bow"),
            Some(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            })
        );
        assert_eq!(
            builtin_item_sprite_name("apple"),
            Some("apple"),
            "the canonical name round-trips"
        );
        assert_eq!(builtin_item_sprite_name("wood_bow"), Some("wood_bow"));

        // Blocks are not sprite identities: their art lives in the block
        // texture array.
        assert_eq!(builtin_item_sprite_id_by_name("stone"), None);
        assert_eq!(builtin_item_sprite_name("stone"), None);

        // Forged tool names normalize to the same rejection; the inner guard
        // also rejects forged `(Bow, Stone/Iron)` identities directly.
        assert_eq!(builtin_item_sprite_id_by_name("stone_bow"), None);
        assert_eq!(builtin_item_sprite_id_by_name("iron_bow"), None);
        assert_eq!(
            sprite_identity(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Stone,
            }),
            None
        );
        assert_eq!(
            sprite_identity(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Iron,
            }),
            None
        );
        assert_eq!(sprite_identity(ItemId::Block(BlockId::Stone)), None);
        assert_eq!(sprite_identity(ItemId::Item(0)), None);
        assert_eq!(sprite_identity(ItemId::ModTool(0)), None);

        assert_eq!(builtin_item_sprite_id_by_name("diamond"), None);
        assert_eq!(builtin_item_sprite_name("not_a_thing"), None);

        // Every table entry with a procedural sprite round-trips to its
        // canonical short name.
        for name in MATERIAL_ID_NAMES {
            assert_eq!(builtin_item_sprite_name(name), Some(name));
        }
        for name in TOOL_ID_NAMES {
            assert_eq!(builtin_item_sprite_name(name), Some(name));
        }
    }

    #[test]
    fn builtin_sprite_name_for_is_the_exact_inverse_of_the_sprite_lookup() {
        use crate::ui::item_icon::item_sprite_table;

        let mut checked = 0;
        for (id, _layer) in item_sprite_table() {
            let name = builtin_sprite_name_for(*id)
                .unwrap_or_else(|| panic!("table identity {id:?} has no canonical name"));
            assert_eq!(
                builtin_item_sprite_id_by_name(name),
                Some(*id),
                "name {name:?} must round-trip back to {id:?}"
            );
            checked += 1;
        }
        assert_eq!(checked, 27, "every pinned table entry is covered");

        // Identities outside the table have no canonical sprite name.
        assert_eq!(builtin_sprite_name_for(ItemId::Block(BlockId::Stone)), None);
        assert_eq!(builtin_sprite_name_for(ItemId::Item(0)), None);
        assert_eq!(builtin_sprite_name_for(ItemId::ModTool(0)), None);
        assert_eq!(
            builtin_sprite_name_for(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Stone,
            }),
            None
        );
    }

    #[test]
    fn reverse_lookups_cover_every_table_entry() {
        for (raw, name) in BLOCK_ID_NAMES.iter().enumerate() {
            let id = builtin_block_id_by_name(name)
                .unwrap_or_else(|| panic!("missing block lookup for {name}"));
            assert_eq!(usize::from(id.to_raw()), raw);
            assert_eq!(builtin_item_id_by_name(name), Some(ItemId::Block(id)));
        }
        assert_eq!(builtin_block_id_by_name("mymod:ruby"), None);

        for (index, name) in MATERIAL_ID_NAMES.iter().enumerate() {
            let material = builtin_material_by_name(name)
                .unwrap_or_else(|| panic!("missing material lookup for {name}"));
            assert_eq!(builtin_material_id_name(material), MATERIAL_ID_NAMES[index]);
            assert_eq!(
                builtin_item_id_by_name(name),
                Some(ItemId::Material(material))
            );
        }

        for (index, name) in TIER_ID_NAMES.iter().enumerate() {
            let tier = builtin_tier_by_name(name)
                .unwrap_or_else(|| panic!("missing tier lookup for {name}"));
            assert_eq!(builtin_tier_id_name(tier), TIER_ID_NAMES[index]);
        }

        for (index, name) in TOOL_ID_NAMES.iter().enumerate() {
            let tool = builtin_tool_id_by_name(name)
                .unwrap_or_else(|| panic!("missing tool lookup for {name}"));
            assert_eq!(tool.name(), builtin_item_id_by_name(name).unwrap().name());
            if index != 15 {
                assert_eq!(
                    builtin_tool_id_name(
                        match tool {
                            ItemId::Tool { kind, .. } => kind,
                            _ => unreachable!(),
                        },
                        match tool {
                            ItemId::Tool { tier, .. } => tier,
                            _ => unreachable!(),
                        },
                    ),
                    Some(*name)
                );
            }
        }
        assert_eq!(builtin_tool_id_by_name("mymod:ruby_pickaxe"), None);

        for (index, name) in RECIPE_ID_NAMES.iter().enumerate() {
            let recipe = builtin_recipe_id_by_name(name)
                .unwrap_or_else(|| panic!("missing recipe lookup for {name}"));
            assert_eq!(builtin_recipe_id_name(recipe), Some(RECIPE_ID_NAMES[index]));
        }
        assert_eq!(builtin_recipe_id_by_name("mymod:ruby"), None);
    }

    #[test]
    fn builtin_recipe_indices_round_trip_and_reject_modded() {
        for (index, name) in RECIPE_ID_NAMES.iter().enumerate() {
            let recipe = builtin_recipe_by_index(index as u16)
                .unwrap_or_else(|| panic!("missing recipe index {index}"));
            assert_eq!(builtin_recipe_index(recipe), Some(index));
            assert_eq!(builtin_recipe_id_name(recipe), Some(*name));
            assert_eq!(builtin_recipe_id_by_name(name), Some(recipe));
        }
        assert_eq!(builtin_recipe_by_index(22), None);
        assert_eq!(builtin_recipe_by_index(u16::MAX), None);
        assert_eq!(builtin_recipe_index(RecipeId::Modded(0)), None);
        assert_eq!(builtin_recipe_index(RecipeId::Modded(22)), None);
        assert_eq!(builtin_recipe_id_name(RecipeId::Modded(22)), None);
    }
}
