//! Pure, data-driven block/tool interaction authority.
//!
//! This module owns three rules and nothing else:
//!
//! 1. which blocks each [`ToolKind`] is *intended* for,
//! 2. the per-tier speed multiplier (Wood 2x, Stone 4x, Iron 6x), and
//! 3. the resulting [`break_duration`] for a block plus a selected item.
//!
//! It has no window, renderer, filesystem, or session dependency, so the
//! mining accumulator and the production loop can agree on timing without
//! either of them re-deriving rules. Durability is *not* handled here: a tool
//! that is merely selected is still fully durable as far as timing is
//! concerned, and wear belongs to the world-edit path.

use crate::player::inventory::{Item, ToolKind, ToolTier};
use crate::world::block::BlockId;

/// Speed multiplier for a wood tool: twice the bare-hand rate.
pub const WOOD_SPEED_MULTIPLIER: u8 = 2;
/// Speed multiplier for a stone tool: four times the bare-hand rate.
pub const STONE_SPEED_MULTIPLIER: u8 = 4;
/// Speed multiplier for an iron tool: six times the bare-hand rate.
pub const IRON_SPEED_MULTIPLIER: u8 = 6;

/// The tool kind that is intended for `block`, or `None` when no tool in this
/// game's set accelerates it (leaves, air, bedrock).
///
/// Classification is exact: a tool only accelerates blocks in its own class.
pub const fn block_tool_class(block: BlockId) -> Option<ToolKind> {
    match block {
        BlockId::Stone | BlockId::Cobblestone | BlockId::Brick => Some(ToolKind::Pickaxe),
        BlockId::CoalOre | BlockId::IronOre => Some(ToolKind::Pickaxe),
        BlockId::OakLog | BlockId::OakPlanks | BlockId::CraftingTable => Some(ToolKind::Axe),
        BlockId::Grass | BlockId::Dirt | BlockId::Sand | BlockId::Snow => Some(ToolKind::Shovel),
        BlockId::Air | BlockId::OakLeaves | BlockId::Bedrock => None,
    }
}

/// Speed multiplier for a tool tier. Always `>= 1`, so a matched tool can only
/// ever shorten a break, never lengthen it.
pub const fn tier_speed_multiplier(tier: ToolTier) -> u8 {
    match tier {
        ToolTier::Wood => WOOD_SPEED_MULTIPLIER,
        ToolTier::Stone => STONE_SPEED_MULTIPLIER,
        ToolTier::Iron => IRON_SPEED_MULTIPLIER,
    }
}

/// Seconds of held left-click needed to break `block` while `selected_item` is
/// the selected hotbar item.
///
/// Returns `None` for structurally unbreakable blocks (Air, Bedrock) — the
/// same fail-closed answer as [`BlockId::break_duration_secs`], regardless of
/// which tool is held. A matching tool divides the base hardness by its tier
/// multiplier; a mismatched tool, a non-tool, and an empty slot all keep the
/// base hardness.
pub fn break_duration(block: BlockId, selected_item: Option<Item>) -> Option<f32> {
    let base = block.break_duration_secs()?;
    let Some((kind, tier)) = selected_item.and_then(|item| item.id().tool()) else {
        return Some(base);
    };
    if block_tool_class(block) != Some(kind) {
        return Some(base);
    }
    Some(base / f32::from(tier_speed_multiplier(tier)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{ItemId, Material};

    const ALL_BLOCKS: [BlockId; 15] = [
        BlockId::Air,
        BlockId::Grass,
        BlockId::Dirt,
        BlockId::Stone,
        BlockId::Cobblestone,
        BlockId::Sand,
        BlockId::OakLog,
        BlockId::OakLeaves,
        BlockId::OakPlanks,
        BlockId::Brick,
        BlockId::Snow,
        BlockId::Bedrock,
        BlockId::CoalOre,
        BlockId::IronOre,
        BlockId::CraftingTable,
    ];

    const ALL_KINDS: [ToolKind; 3] = [ToolKind::Pickaxe, ToolKind::Axe, ToolKind::Shovel];
    const ALL_TIERS: [ToolTier; 3] = [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron];

    fn tool(kind: ToolKind, tier: ToolTier) -> Item {
        Item::new(ItemId::Tool { kind, tier }, 1).expect("valid tool")
    }

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn classification_covers_every_block_exactly_once() {
        for block in ALL_BLOCKS {
            let class = block_tool_class(block);
            match block {
                BlockId::Air | BlockId::Bedrock | BlockId::OakLeaves => {
                    assert_eq!(class, None, "{block:?} must not be accelerated by a tool");
                }
                _ => assert!(class.is_some(), "{block:?} must belong to a tool class"),
            }
        }

        for block in [
            BlockId::Stone,
            BlockId::Cobblestone,
            BlockId::Brick,
            BlockId::CoalOre,
            BlockId::IronOre,
        ] {
            assert_eq!(block_tool_class(block), Some(ToolKind::Pickaxe));
        }
        for block in [BlockId::OakLog, BlockId::OakPlanks, BlockId::CraftingTable] {
            assert_eq!(block_tool_class(block), Some(ToolKind::Axe));
        }
        for block in [BlockId::Grass, BlockId::Dirt, BlockId::Sand, BlockId::Snow] {
            assert_eq!(block_tool_class(block), Some(ToolKind::Shovel));
        }
    }

    #[test]
    fn tier_multipliers_are_two_four_six() {
        assert_eq!(tier_speed_multiplier(ToolTier::Wood), 2);
        assert_eq!(tier_speed_multiplier(ToolTier::Stone), 4);
        assert_eq!(tier_speed_multiplier(ToolTier::Iron), 6);
        assert_eq!(WOOD_SPEED_MULTIPLIER, 2);
        assert_eq!(STONE_SPEED_MULTIPLIER, 4);
        assert_eq!(IRON_SPEED_MULTIPLIER, 6);
    }

    #[test]
    fn unbreakable_blocks_return_no_duration_for_any_selection() {
        for block in [BlockId::Air, BlockId::Bedrock] {
            assert_eq!(break_duration(block, None), None);
            for kind in ALL_KINDS {
                for tier in ALL_TIERS {
                    assert_eq!(
                        break_duration(block, Some(tool(kind, tier))),
                        None,
                        "{block:?} must stay unbreakable with {kind:?}/{tier:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn bare_hand_and_non_tools_keep_base_hardness() {
        assert_eq!(break_duration(BlockId::Stone, None), Some(1.2));
        let planks = Item::new(ItemId::Block(BlockId::OakPlanks), 4).expect("valid block item");
        assert_eq!(break_duration(BlockId::Stone, Some(planks)), Some(1.2));
        let stick = Item::new(ItemId::Material(Material::Stick), 4).expect("valid material");
        assert_eq!(break_duration(BlockId::Stone, Some(stick)), Some(1.2));
    }

    #[test]
    fn matching_tier_speeds_are_exact() {
        // Stone base is 1.2s.
        approx(
            break_duration(
                BlockId::Stone,
                Some(tool(ToolKind::Pickaxe, ToolTier::Wood)),
            )
            .unwrap(),
            0.6,
        );
        approx(
            break_duration(
                BlockId::Stone,
                Some(tool(ToolKind::Pickaxe, ToolTier::Stone)),
            )
            .unwrap(),
            0.3,
        );
        approx(
            break_duration(
                BlockId::Stone,
                Some(tool(ToolKind::Pickaxe, ToolTier::Iron)),
            )
            .unwrap(),
            0.2,
        );
        // Log base is 0.9s -> 0.45 / 0.225 / 0.15.
        approx(
            break_duration(BlockId::OakLog, Some(tool(ToolKind::Axe, ToolTier::Wood))).unwrap(),
            0.45,
        );
        approx(
            break_duration(BlockId::OakLog, Some(tool(ToolKind::Axe, ToolTier::Stone))).unwrap(),
            0.225,
        );
        approx(
            break_duration(BlockId::OakLog, Some(tool(ToolKind::Axe, ToolTier::Iron))).unwrap(),
            0.15,
        );
        // Dirt base is 0.45s -> 0.225 / 0.1125 / 0.075.
        approx(
            break_duration(BlockId::Dirt, Some(tool(ToolKind::Shovel, ToolTier::Wood))).unwrap(),
            0.225,
        );
        approx(
            break_duration(BlockId::Dirt, Some(tool(ToolKind::Shovel, ToolTier::Iron))).unwrap(),
            0.075,
        );
    }

    #[test]
    fn complete_class_times_tier_matrix_matches_the_classification() {
        // Exhaustive: every block x every tool kind x every tier.
        for block in ALL_BLOCKS {
            let base = block.break_duration_secs();
            let class = block_tool_class(block);
            for kind in ALL_KINDS {
                for tier in ALL_TIERS {
                    let expected = match base {
                        None => None,
                        Some(base) if class == Some(kind) => {
                            Some(base / f32::from(tier_speed_multiplier(tier)))
                        }
                        Some(base) => Some(base),
                    };
                    let actual = break_duration(block, Some(tool(kind, tier)));
                    match (actual, expected) {
                        (None, None) => {}
                        (Some(actual), Some(expected)) => approx(actual, expected),
                        other => panic!(
                            "{block:?} with {kind:?}/{tier:?}: expected {expected:?}, got {other:?}"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn mismatched_tools_never_accelerate() {
        // A pickaxe on wood, an axe on stone, and a shovel on ore all fall
        // back to base hardness instead of partially helping.
        assert_eq!(
            break_duration(
                BlockId::OakLog,
                Some(tool(ToolKind::Pickaxe, ToolTier::Iron))
            ),
            Some(0.9)
        );
        assert_eq!(
            break_duration(BlockId::Stone, Some(tool(ToolKind::Axe, ToolTier::Iron))),
            Some(1.2)
        );
        assert_eq!(
            break_duration(
                BlockId::IronOre,
                Some(tool(ToolKind::Shovel, ToolTier::Stone))
            ),
            Some(2.2)
        );
        // A matched tool on a block with no tool class stays at base speed.
        assert_eq!(
            break_duration(
                BlockId::OakLeaves,
                Some(tool(ToolKind::Axe, ToolTier::Iron))
            ),
            Some(0.2)
        );
    }

    #[test]
    fn tool_adjusted_durations_stay_positive_and_never_longer_than_base() {
        for block in ALL_BLOCKS {
            let Some(base) = block.break_duration_secs() else {
                continue;
            };
            for kind in ALL_KINDS {
                for tier in ALL_TIERS {
                    let duration = break_duration(block, Some(tool(kind, tier)))
                        .expect("breakable block always has a duration");
                    assert!(
                        duration > 0.0,
                        "{block:?}/{kind:?}/{tier:?} must be positive"
                    );
                    assert!(
                        duration <= base,
                        "{block:?}/{kind:?}/{tier:?} must not be slower than bare hand"
                    );
                }
            }
        }
    }
}
