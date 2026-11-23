//! Frozen built-in name vocabulary for content identity.
//!
//! This module is pure data plus lookups: no I/O, GPU, window, audio or
//! filesystem dependency. It is the single authoritative string vocabulary
//! that the pack parser and validation layers resolve against, so names are
//! frozen here before any pack data exists. The game re-exports these tables
//! and adds its id-mapping lookups on top; there is exactly one source of
//! truth.
//!
//! Every table is index-aligned to the declaration order of its Rust enum
//! (or, for blocks, the frozen raw id order `0..=24`). Built-in names are
//! unqualified: they never contain a `:` namespace separator.

/// Longest content name accepted anywhere a name is persisted or namespaced.
///
/// This is the shared cap for the v8 save content string table and pack ids,
/// so a name accepted by the pack parser can always be written to a save and
/// vice versa. Bytes, not chars: the save table stores UTF-8 length.
pub const MAX_CONTENT_NAME_LEN: usize = 255;

/// Built-in block id names, index-aligned to raw ids `0..=24`.
pub const BLOCK_ID_NAMES: [&str; 25] = [
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
    "glass",
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

/// Whether `name` is a built-in block short name.
pub fn is_builtin_block_name(name: &str) -> bool {
    BLOCK_ID_NAMES.contains(&name)
}

/// Whether `name` is a built-in material short name.
pub fn is_builtin_material_name(name: &str) -> bool {
    MATERIAL_ID_NAMES.contains(&name)
}

/// Whether `name` is a built-in tool tier short name.
pub fn is_builtin_tier_name(name: &str) -> bool {
    TIER_ID_NAMES.contains(&name)
}

/// Whether `name` is a built-in constructible tool short name.
pub fn is_builtin_tool_name(name: &str) -> bool {
    TOOL_ID_NAMES.contains(&name)
}

/// Whether `name` is a built-in recipe short name.
pub fn is_builtin_recipe_name(name: &str) -> bool {
    RECIPE_ID_NAMES.contains(&name)
}

/// Whether `name` is an unqualified built-in item name: block, material or
/// tool. Namespaced pack ids are not built-in and answer `false`.
pub fn is_builtin_item_name(name: &str) -> bool {
    is_builtin_block_name(name) || is_builtin_material_name(name) || is_builtin_tool_name(name)
}

/// Whether `name` is a built-in identity that owns a procedural item sprite:
/// a material or constructible tool.
///
/// Blocks are rejected: their icons are sampled from the block texture array,
/// not the item sprite atlas.
pub fn is_builtin_item_sprite_name(name: &str) -> bool {
    is_builtin_material_name(name) || is_builtin_tool_name(name)
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

    #[test]
    fn builtin_predicates_agree_with_tables_and_reject_foreign_names() {
        for name in BLOCK_ID_NAMES {
            assert!(is_builtin_block_name(name), "{name} is a built-in block");
            assert!(is_builtin_item_name(name), "{name} is a built-in item");
            assert!(
                !is_builtin_item_sprite_name(name),
                "{name} is a block: blocks have no item sprite"
            );
            assert!(!is_builtin_material_name(name));
            assert!(!is_builtin_tool_name(name));
        }
        for name in MATERIAL_ID_NAMES {
            assert!(is_builtin_material_name(name), "{name} is a material");
            assert!(is_builtin_item_name(name));
            assert!(is_builtin_item_sprite_name(name));
            assert!(!is_builtin_block_name(name));
            assert!(!is_builtin_tool_name(name));
        }
        for name in TIER_ID_NAMES {
            assert!(is_builtin_tier_name(name), "{name} is a tier");
            assert!(!is_builtin_material_name(name));
            assert!(!is_builtin_tool_name(name));
            assert!(!is_builtin_item_sprite_name(name));
        }
        for name in TOOL_ID_NAMES {
            assert!(is_builtin_tool_name(name), "{name} is a tool");
            assert!(is_builtin_item_name(name));
            assert!(is_builtin_item_sprite_name(name));
            assert!(!is_builtin_block_name(name));
            assert!(!is_builtin_material_name(name));
        }
        for name in RECIPE_ID_NAMES {
            assert!(is_builtin_recipe_name(name), "{name} is a recipe");
        }

        // The tables overlap on purpose (`stone` is a block and a tier,
        // `chest` a block and a recipe, `wood_sword` a tool and a recipe),
        // so the negative cross-checks above only cover non-overlapping
        // tables; the predicate/table agreement stays exact per predicate.
        for foreign in [
            "mymod:ruby",
            "mymod:ruby_ore",
            "opencraft:stone",
            "not_a_thing",
            "",
            "Stone",
            "stone ",
            " wood_pickaxe",
        ] {
            assert!(!is_builtin_block_name(foreign), "{foreign}");
            assert!(!is_builtin_material_name(foreign), "{foreign}");
            assert!(!is_builtin_tier_name(foreign), "{foreign}");
            assert!(!is_builtin_tool_name(foreign), "{foreign}");
            assert!(!is_builtin_recipe_name(foreign), "{foreign}");
            assert!(!is_builtin_item_name(foreign), "{foreign}");
            assert!(!is_builtin_item_sprite_name(foreign), "{foreign}");
        }
    }
}
