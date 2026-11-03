//! Frozen built-in name vocabulary for content identity.
//!
//! This module is pure data plus lookups: no I/O, GPU, window, audio or
//! filesystem dependency. It is the single authoritative string vocabulary
//! that the pack parser (S02) and the v8 save writer (S03) resolve against, so
//! names are frozen here before any pack data exists.
//!
//! Every table is index-aligned to the declaration order of its Rust enum
//! (or, for blocks, the frozen raw id order `0..=22`). Built-in names are
//! unqualified: they never contain a `:` namespace separator.

use crate::player::inventory::{ItemId, Material, RecipeId, ToolKind, ToolTier};
use crate::world::block::BlockId;

/// Built-in block id names, index-aligned to raw ids `0..=22`.
pub const BLOCK_ID_NAMES: [&str; 23] = [
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
];

/// Material names in `Material` declaration order.
pub const MATERIAL_ID_NAMES: [&str; 11] = [
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

/// Tool tier names in `ToolTier` declaration order.
pub const TIER_ID_NAMES: [&str; 3] = ["wood", "stone", "iron"];

/// Tool names: tier-major, then kind, with the wood-only bow last.
pub const TOOL_ID_NAMES: [&str; 16] = [
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

/// Recipe names in `RecipeId` declaration order.
pub const RECIPE_ID_NAMES: [&str; 22] = [
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

/// Biome names accepted by declarative worldgen patches.
pub const BIOME_NAMES: [&str; 4] = ["plains", "forest", "mountains", "ocean"];

/// Namespaces reserved for the engine and built-in content.
pub const RESERVED_NAMESPACES: [&str; 3] = ["opencraft", "builtin", "minecraft"];

/// Canonical biome name for `name`, or `None` when it is unknown.
pub fn biome_from_name(name: &str) -> Option<&'static str> {
    BIOME_NAMES.iter().copied().find(|known| *known == name)
}

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

/// Reverse lookup for a built-in tool tier short name.
pub fn builtin_tier_by_name(name: &str) -> Option<ToolTier> {
    TIER_ID_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| TIER_VALUES[index])
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

/// Stable short name for a recipe.
pub fn builtin_recipe_id_name(recipe: RecipeId) -> &'static str {
    RECIPE_ID_NAMES[match recipe {
        RecipeId::Planks => 0,
        RecipeId::Sticks => 1,
        RecipeId::CraftingTable => 2,
        RecipeId::Chest => 3,
        RecipeId::WoodPickaxe => 4,
        RecipeId::WoodAxe => 5,
        RecipeId::WoodShovel => 6,
        RecipeId::StonePickaxe => 7,
        RecipeId::StoneAxe => 8,
        RecipeId::StoneShovel => 9,
        RecipeId::IronPickaxe => 10,
        RecipeId::IronAxe => 11,
        RecipeId::IronShovel => 12,
        RecipeId::Bread => 13,
        RecipeId::WoodHoe => 14,
        RecipeId::StoneHoe => 15,
        RecipeId::IronHoe => 16,
        RecipeId::WoodSword => 17,
        RecipeId::StoneSword => 18,
        RecipeId::IronSword => 19,
        RecipeId::Shortbow => 20,
        RecipeId::Arrows => 21,
    }]
}

/// Whether `namespace` is reserved for the engine or built-in content.
pub fn is_reserved_namespace(namespace: &str) -> bool {
    RESERVED_NAMESPACES.contains(&namespace)
}

/// Whether `value` is a well-formed namespaced id: exactly one `:`, both
/// sides non-empty, and each side composed only of `a-z`, `0-9`, and `_`.
///
/// Uppercase letters, spaces, and extra colons are rejected.
pub fn is_namespaced_id(value: &str) -> bool {
    let mut parts = value.split(':');
    let Some(namespace) = parts.next() else {
        return false;
    };
    let Some(path) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    !namespace.is_empty() && !path.is_empty() && is_id_segment(namespace) && is_id_segment(path)
}

fn is_id_segment(segment: &str) -> bool {
    segment
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
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
                RECIPE_ID_NAMES[index],
                "recipe table misaligned at {index}"
            );
        }
    }

    #[test]
    fn reserved_namespaces_and_namespaced_ids() {
        assert!(is_reserved_namespace("opencraft"));
        assert!(is_reserved_namespace("builtin"));
        assert!(is_reserved_namespace("minecraft"));
        assert!(!is_reserved_namespace("mymod"));

        for valid in ["mymod:ruby", "mymod:ruby_ore"] {
            assert!(is_namespaced_id(valid), "{valid} should be valid");
        }
        for invalid in [
            "mymod",
            ":ruby",
            "mymod:",
            "my:mod:ruby",
            "MyMod:ruby",
            "mymod:Ruby",
            "my mod:ruby",
        ] {
            assert!(!is_namespaced_id(invalid), "{invalid} should be invalid");
        }
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
            assert_eq!(builtin_recipe_id_name(recipe), RECIPE_ID_NAMES[index]);
        }
        assert_eq!(builtin_recipe_id_by_name("mymod:ruby"), None);
    }

    #[test]
    fn biome_names_are_canonical_and_reject_unknown() {
        assert_eq!(BIOME_NAMES, ["plains", "forest", "mountains", "ocean"]);
        for name in BIOME_NAMES {
            assert_eq!(biome_from_name(name), Some(name));
        }
        assert_eq!(biome_from_name("desert"), None);
        assert_eq!(biome_from_name("Plains"), None);
    }

    #[test]
    fn builtin_names_are_unqualified() {
        let tables: [&[&str]; 5] = [
            &BLOCK_ID_NAMES,
            &MATERIAL_ID_NAMES,
            &TIER_ID_NAMES,
            &TOOL_ID_NAMES,
            &RECIPE_ID_NAMES,
        ];
        for table in tables {
            for name in table {
                assert!(!name.contains(':'), "{name} must be unqualified");
            }
        }
    }
}
