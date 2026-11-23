//! Raw `u16` block ids for the neutral worldgen vocabulary.
//!
//! Every constant is index-aligned to
//! [`opencraft_content::vocabulary::BLOCK_ID_NAMES`], the frozen built-in
//! vocabulary owned by `opencraft-content`. This crate never imports the game
//! `BlockId`; the game adapter maps these raw ids at its own boundary.

pub const BLOCK_AIR: u16 = 0;
pub const BLOCK_GRASS: u16 = 1;
pub const BLOCK_DIRT: u16 = 2;
pub const BLOCK_STONE: u16 = 3;
pub const BLOCK_COBBLESTONE: u16 = 4;
pub const BLOCK_SAND: u16 = 5;
pub const BLOCK_OAK_LOG: u16 = 6;
pub const BLOCK_OAK_LEAVES: u16 = 7;
pub const BLOCK_OAK_PLANKS: u16 = 8;
pub const BLOCK_BRICK: u16 = 9;
pub const BLOCK_SNOW: u16 = 10;
pub const BLOCK_BEDROCK: u16 = 11;
pub const BLOCK_COAL_ORE: u16 = 12;
pub const BLOCK_IRON_ORE: u16 = 13;
pub const BLOCK_CRAFTING_TABLE: u16 = 14;
pub const BLOCK_WATER: u16 = 15;
pub const BLOCK_FARMLAND: u16 = 16;
pub const BLOCK_WHEAT_STAGE_0: u16 = 17;
pub const BLOCK_WHEAT_STAGE_1: u16 = 18;
pub const BLOCK_WHEAT_STAGE_2: u16 = 19;
pub const BLOCK_WHEAT_STAGE_3: u16 = 20;
pub const BLOCK_CHEST: u16 = 21;
pub const BLOCK_LOOT_CHEST: u16 = 22;
pub const BLOCK_SOCKET_MARKER: u16 = 23;
pub const BLOCK_GLASS: u16 = 24;

/// Number of built-in block ids: `0..=24` are the frozen vocabulary and every
/// modded registry index starts here.
pub const BUILTIN_BLOCK_COUNT: u16 = 25;

#[cfg(test)]
mod tests {
    use super::*;
    use opencraft_content::vocabulary::BLOCK_ID_NAMES;

    #[test]
    fn block_constants_are_index_aligned_to_the_frozen_vocabulary() {
        let ids: [u16; 25] = [
            BLOCK_AIR,
            BLOCK_GRASS,
            BLOCK_DIRT,
            BLOCK_STONE,
            BLOCK_COBBLESTONE,
            BLOCK_SAND,
            BLOCK_OAK_LOG,
            BLOCK_OAK_LEAVES,
            BLOCK_OAK_PLANKS,
            BLOCK_BRICK,
            BLOCK_SNOW,
            BLOCK_BEDROCK,
            BLOCK_COAL_ORE,
            BLOCK_IRON_ORE,
            BLOCK_CRAFTING_TABLE,
            BLOCK_WATER,
            BLOCK_FARMLAND,
            BLOCK_WHEAT_STAGE_0,
            BLOCK_WHEAT_STAGE_1,
            BLOCK_WHEAT_STAGE_2,
            BLOCK_WHEAT_STAGE_3,
            BLOCK_CHEST,
            BLOCK_LOOT_CHEST,
            BLOCK_SOCKET_MARKER,
            BLOCK_GLASS,
        ];
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(
                *id as usize, index,
                "constant for {} is not index-aligned",
                BLOCK_ID_NAMES[index]
            );
        }
        assert_eq!(
            ids.len(),
            BLOCK_ID_NAMES.len(),
            "constant count must match the vocabulary"
        );
    }

    #[test]
    fn builtin_block_count_matches_the_vocabulary() {
        assert_eq!(BUILTIN_BLOCK_COUNT as usize, BLOCK_ID_NAMES.len());
    }
}
