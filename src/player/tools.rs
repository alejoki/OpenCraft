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

/// Creator-tunable per-tier mining speed multipliers.
///
/// Kept in this module so the tool authority stays free of session and config
/// types; defaults read the existing constants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolRules {
    /// Speed multiplier for wooden tools.
    pub wood_speed_multiplier: u8,
    /// Speed multiplier for stone tools.
    pub stone_speed_multiplier: u8,
    /// Speed multiplier for iron tools.
    pub iron_speed_multiplier: u8,
}

impl Default for ToolRules {
    fn default() -> Self {
        Self {
            wood_speed_multiplier: WOOD_SPEED_MULTIPLIER,
            stone_speed_multiplier: STONE_SPEED_MULTIPLIER,
            iron_speed_multiplier: IRON_SPEED_MULTIPLIER,
        }
    }
}

/// The tool kind that is intended for `block`, or `None` when no tool in this
/// game's set accelerates it (leaves, air, bedrock).
///
/// Classification is exact: a tool only accelerates blocks in its own class.
pub const fn block_tool_class(block: BlockId) -> Option<ToolKind> {
    match block {
        BlockId::Stone | BlockId::Cobblestone | BlockId::Brick => Some(ToolKind::Pickaxe),
        BlockId::CoalOre | BlockId::IronOre => Some(ToolKind::Pickaxe),
        BlockId::OakLog
        | BlockId::OakPlanks
        | BlockId::CraftingTable
        | BlockId::Chest
        | BlockId::LootChest => Some(ToolKind::Axe),
        BlockId::Grass | BlockId::Dirt | BlockId::Sand | BlockId::Snow | BlockId::Farmland => {
            Some(ToolKind::Shovel)
        }
        BlockId::Air
        | BlockId::OakLeaves
        | BlockId::Bedrock
        | BlockId::Water
        | BlockId::WheatStage0
        | BlockId::WheatStage1
        | BlockId::WheatStage2
        | BlockId::WheatStage3
        | BlockId::SocketMarker => None,
        // No acceleration until the registry resolves a tool class in S04:
        // fail closed rather than guessing a built-in material.
        BlockId::Modded(_) => None,
    }
}

/// Sword damage for a tool tier in Survival (4 / 5 / 6).
///
/// A pure data source for built-in tier damage: content definitions and the
/// melee rules both read it instead of duplicating the table.
pub const fn tier_sword_damage(tier: ToolTier) -> u16 {
    match tier {
        ToolTier::Wood => 4,
        ToolTier::Stone => 5,
        ToolTier::Iron => 6,
    }
}

/// Speed multiplier for a tool tier. Always `>= 1`, so a matched tool can only
/// ever shorten a break, never lengthen it.
pub fn tier_speed_multiplier(tier: ToolTier) -> u8 {
    tier_speed_multiplier_with_rules(tier, &ToolRules::default())
}

/// Speed multiplier for a tool tier under live [`ToolRules`].
///
/// The configured value is clamped to at least 1: a degenerate `0` can neither
/// divide by zero nor make a matched tool longer than bare hand.
pub fn tier_speed_multiplier_with_rules(tier: ToolTier, rules: &ToolRules) -> u8 {
    match tier {
        ToolTier::Wood => rules.wood_speed_multiplier,
        ToolTier::Stone => rules.stone_speed_multiplier,
        ToolTier::Iron => rules.iron_speed_multiplier,
    }
    .max(1)
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
    break_duration_with_rules(block, selected_item, &ToolRules::default())
}

/// [`break_duration`] with live [`ToolRules`] for the built-in tiers.
pub fn break_duration_with_rules(
    block: BlockId,
    selected_item: Option<Item>,
    rules: &ToolRules,
) -> Option<f32> {
    break_duration_for_with_rules(
        block.break_duration_secs(),
        block_tool_class(block),
        selected_item,
        rules,
    )
}

/// Registry-friendly core of [`break_duration`].
///
/// `hardness` and `tool_class` are the authored values from a [`BlockDef`], so
/// pack blocks get exactly the same tier math as built-ins. `None` hardness is
/// structurally unbreakable regardless of the held item; a mismatched,
/// non-tool, or empty selection keeps the authored hardness.
pub fn break_duration_for(
    hardness: Option<f32>,
    tool_class: Option<ToolKind>,
    selected_item: Option<Item>,
) -> Option<f32> {
    break_duration_for_with_rules(hardness, tool_class, selected_item, &ToolRules::default())
}

/// [`break_duration_for`] with live [`ToolRules`] for the built-in tiers.
pub fn break_duration_for_with_rules(
    hardness: Option<f32>,
    tool_class: Option<ToolKind>,
    selected_item: Option<Item>,
    rules: &ToolRules,
) -> Option<f32> {
    let base = hardness?;
    let Some((kind, tier)) = selected_item.and_then(|item| item.id().tool()) else {
        return Some(base);
    };
    if tool_class != Some(kind) {
        return Some(base);
    }
    Some(base / f32::from(tier_speed_multiplier_with_rules(tier, rules)))
}

/// Registry-friendly core for a pack tool tier.
///
/// `kind` and `speed` are the authored values from the resolved [`ToolDef`]
/// and its [`TierDef`], so a pack tier gets the same class gate and hardness
/// division as the built-in `Tool { kind, tier }` arm. A mismatched tool class
/// keeps the authored hardness, and a missing/degenerate authored speed fails
/// closed to bare-hand timing instead of producing a non-finite or
/// longer-than-base duration.
///
/// [`ToolDef`]: crate::content::ToolDef
/// [`TierDef`]: crate::content::TierDef
pub fn break_duration_for_pack_tier(
    hardness: Option<f32>,
    tool_class: Option<ToolKind>,
    kind: ToolKind,
    speed: f32,
) -> Option<f32> {
    let base = hardness?;
    if tool_class != Some(kind) || !speed.is_finite() || speed < 1.0 {
        return Some(base);
    }
    Some(base / speed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{ItemId, Material};

    #[test]
    fn tool_rules_default_matches_constants() {
        let rules = ToolRules::default();
        assert_eq!(rules.wood_speed_multiplier, WOOD_SPEED_MULTIPLIER);
        assert_eq!(rules.stone_speed_multiplier, STONE_SPEED_MULTIPLIER);
        assert_eq!(rules.iron_speed_multiplier, IRON_SPEED_MULTIPLIER);
    }

    const ALL_BLOCKS: [BlockId; 24] = [
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
        BlockId::Water,
        BlockId::Farmland,
        BlockId::WheatStage0,
        BlockId::WheatStage1,
        BlockId::WheatStage2,
        BlockId::WheatStage3,
        BlockId::Chest,
        BlockId::LootChest,
        BlockId::SocketMarker,
    ];

    const ALL_KINDS: [ToolKind; 6] = [
        ToolKind::Pickaxe,
        ToolKind::Axe,
        ToolKind::Shovel,
        // Hoe, Sword, and Bow are real tool identities but have no block
        // class: they are mined with but never accelerate a block, so the
        // exhaustive matrix below correctly expects base hardness for them.
        ToolKind::Hoe,
        ToolKind::Sword,
        ToolKind::Bow,
    ];

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
                BlockId::Air
                | BlockId::Bedrock
                | BlockId::OakLeaves
                | BlockId::Water
                | BlockId::WheatStage0
                | BlockId::WheatStage1
                | BlockId::WheatStage2
                | BlockId::WheatStage3
                | BlockId::SocketMarker => {
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
        for block in [
            BlockId::OakLog,
            BlockId::OakPlanks,
            BlockId::CraftingTable,
            BlockId::Chest,
            BlockId::LootChest,
        ] {
            assert_eq!(block_tool_class(block), Some(ToolKind::Axe));
        }
        for block in [
            BlockId::Grass,
            BlockId::Dirt,
            BlockId::Sand,
            BlockId::Snow,
            BlockId::Farmland,
        ] {
            assert_eq!(block_tool_class(block), Some(ToolKind::Shovel));
        }
        for block in [
            BlockId::WheatStage0,
            BlockId::WheatStage1,
            BlockId::WheatStage2,
            BlockId::WheatStage3,
            BlockId::SocketMarker,
        ] {
            assert_eq!(block_tool_class(block), None);
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
        for block in [BlockId::Air, BlockId::Bedrock, BlockId::Water] {
            assert_eq!(break_duration(block, None), None);
            for kind in ALL_KINDS {
                for &tier in kind.allowed_tiers() {
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
                for &tier in kind.allowed_tiers() {
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
    fn break_duration_for_matches_builtin_answers_exhaustively() {
        // The registry path must ask the same core rule with the same authored
        // fields, so every built-in answer is byte-identical.
        for block in ALL_BLOCKS {
            let hardness = block.break_duration_secs();
            let class = block_tool_class(block);
            assert_eq!(
                break_duration_for(hardness, class, None),
                break_duration(block, None),
                "bare-hand mismatch for {block:?}"
            );
            for kind in ALL_KINDS {
                for &tier in kind.allowed_tiers() {
                    let item = Some(tool(kind, tier));
                    assert_eq!(
                        break_duration_for(hardness, class, item),
                        break_duration(block, item),
                        "{block:?} with {kind:?}/{tier:?} diverged"
                    );
                }
            }
        }
        assert_eq!(
            break_duration_for(None, Some(ToolKind::Pickaxe), None),
            None
        );
    }

    #[test]
    fn sword_and_bow_never_accelerate_any_block() {
        // Swords and bows are combat trades: no block in the catalogue may
        // ever get a shorter break from holding one, at any tier.
        for block in ALL_BLOCKS {
            let base = block.break_duration_secs();
            for item in [
                tool(ToolKind::Sword, ToolTier::Wood),
                tool(ToolKind::Sword, ToolTier::Stone),
                tool(ToolKind::Sword, ToolTier::Iron),
                tool(ToolKind::Bow, ToolTier::Wood),
            ] {
                assert_eq!(
                    break_duration(block, Some(item)),
                    base,
                    "{block:?} must keep base hardness with {:?}",
                    item.id()
                );
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
    fn tuned_tier_multipliers_drive_builtin_break_durations() {
        let rules = ToolRules {
            wood_speed_multiplier: 8,
            stone_speed_multiplier: 12,
            iron_speed_multiplier: 16,
        };
        // Stone base is 1.2 s, so tuned wood/stone/iron are 1.2/8, 1.2/12,
        // 1.2/16 instead of the pinned 0.6/0.3/0.2.
        for (tier, multiplier) in [
            (ToolTier::Wood, 8u8),
            (ToolTier::Stone, 12),
            (ToolTier::Iron, 16),
        ] {
            assert_eq!(tier_speed_multiplier_with_rules(tier, &rules), multiplier);
            approx(
                break_duration_with_rules(
                    BlockId::Stone,
                    Some(tool(ToolKind::Pickaxe, tier)),
                    &rules,
                )
                .unwrap(),
                1.2 / f32::from(multiplier),
            );
        }

        // The default wrapper still pins the pre-tuning 2x wood timing.
        assert_eq!(
            break_duration(
                BlockId::Stone,
                Some(tool(ToolKind::Pickaxe, ToolTier::Wood))
            ),
            Some(1.2 / 2.0)
        );
        assert_eq!(tier_speed_multiplier(ToolTier::Wood), WOOD_SPEED_MULTIPLIER);
    }

    #[test]
    fn degenerate_tuned_multipliers_fail_closed_to_bare_hand() {
        let rules = ToolRules {
            wood_speed_multiplier: 0,
            stone_speed_multiplier: 0,
            iron_speed_multiplier: 0,
        };
        for tier in [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron] {
            assert_eq!(
                tier_speed_multiplier_with_rules(tier, &rules),
                1,
                "a zero multiplier must clamp to 1x, never divide by zero"
            );
            let duration = break_duration_with_rules(
                BlockId::Stone,
                Some(tool(ToolKind::Pickaxe, tier)),
                &rules,
            )
            .expect("stone stays breakable");
            approx(duration, 1.2);
        }
        // Unbreakable blocks stay unbreakable under any tuning.
        assert_eq!(
            break_duration_with_rules(
                BlockId::Bedrock,
                Some(tool(ToolKind::Pickaxe, ToolTier::Iron)),
                &rules
            ),
            None
        );
        // A mismatched class is still not accelerated by a tuned tier.
        assert_eq!(
            break_duration_with_rules(
                BlockId::OakLog,
                Some(tool(ToolKind::Pickaxe, ToolTier::Iron)),
                &rules
            ),
            Some(0.9)
        );
    }

    #[test]
    fn pack_tier_break_speed_matches_the_authored_multiplier() {
        approx(
            break_duration_for_pack_tier(
                Some(3.0),
                Some(ToolKind::Pickaxe),
                ToolKind::Pickaxe,
                9.0,
            )
            .unwrap(),
            3.0 / 9.0,
        );
        // The class gate is exact: a pack sword never accelerates a pickaxe block.
        assert_eq!(
            break_duration_for_pack_tier(Some(3.0), Some(ToolKind::Pickaxe), ToolKind::Sword, 9.0),
            Some(3.0)
        );
        // Degenerate authored speeds fail closed to bare-hand timing.
        for speed in [0.0, -1.0, 0.5, f32::NAN, f32::INFINITY] {
            assert_eq!(
                break_duration_for_pack_tier(
                    Some(3.0),
                    Some(ToolKind::Pickaxe),
                    ToolKind::Pickaxe,
                    speed
                ),
                Some(3.0),
                "speed {speed} must not produce a degenerate duration"
            );
        }
        assert_eq!(
            break_duration_for_pack_tier(None, Some(ToolKind::Pickaxe), ToolKind::Pickaxe, 9.0),
            None
        );
    }

    #[test]
    fn tool_adjusted_durations_stay_positive_and_never_longer_than_base() {
        for block in ALL_BLOCKS {
            let Some(base) = block.break_duration_secs() else {
                continue;
            };
            for kind in ALL_KINDS {
                for &tier in kind.allowed_tiers() {
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
