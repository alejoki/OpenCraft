#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BlockId {
    Air = 0,
    Grass = 1,
    Dirt = 2,
    Stone = 3,
    Cobblestone = 4,
    Sand = 5,
    OakLog = 6,
    OakLeaves = 7,
    OakPlanks = 8,
    Brick = 9,
    Snow = 10,
    Bedrock = 11,
    CoalOre = 12,
    IronOre = 13,
}

impl BlockId {
    pub const HOTBAR: [BlockId; 9] = [
        BlockId::Grass,
        BlockId::Dirt,
        BlockId::Stone,
        BlockId::Cobblestone,
        BlockId::Sand,
        BlockId::OakLog,
        BlockId::OakLeaves,
        BlockId::OakPlanks,
        BlockId::Brick,
    ];

    #[inline]
    pub const fn is_solid(self) -> bool {
        !matches!(self, BlockId::Air)
    }

    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(BlockId::Air),
            1 => Some(BlockId::Grass),
            2 => Some(BlockId::Dirt),
            3 => Some(BlockId::Stone),
            4 => Some(BlockId::Cobblestone),
            5 => Some(BlockId::Sand),
            6 => Some(BlockId::OakLog),
            7 => Some(BlockId::OakLeaves),
            8 => Some(BlockId::OakPlanks),
            9 => Some(BlockId::Brick),
            10 => Some(BlockId::Snow),
            11 => Some(BlockId::Bedrock),
            12 => Some(BlockId::CoalOre),
            13 => Some(BlockId::IronOre),
            _ => None,
        }
    }

    /// Returns texture array layer index (0..16) for a face direction:
    /// 0 = +X, 1 = -X, 2 = +Y (top), 3 = -Y (bottom), 4 = +Z, 5 = -Z
    #[inline]
    pub const fn face_tex_layer(self, face_dir: u8) -> u8 {
        match self {
            BlockId::Air => 0,
            BlockId::Grass => match face_dir {
                2 => 0, // grass top
                3 => 2, // dirt bottom
                _ => 1, // grass side
            },
            BlockId::Dirt => 2,
            BlockId::Stone => 3,
            BlockId::Cobblestone => 4,
            BlockId::Sand => 5,
            BlockId::OakLog => match face_dir {
                2 | 3 => 6, // log top/bottom
                _ => 7,     // log bark side
            },
            BlockId::OakLeaves => 8,
            BlockId::OakPlanks => 9,
            BlockId::Brick => 10,
            BlockId::Snow => match face_dir {
                2 => 11, // snow top
                3 => 2,  // dirt bottom
                _ => 13, // snow grass side
            },
            BlockId::Bedrock => 12,
            BlockId::CoalOre => 14,
            BlockId::IronOre => 15,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            BlockId::Air => "Air",
            BlockId::Grass => "Grass",
            BlockId::Dirt => "Dirt",
            BlockId::Stone => "Stone",
            BlockId::Cobblestone => "Cobblestone",
            BlockId::Sand => "Sand",
            BlockId::OakLog => "Oak Log",
            BlockId::OakLeaves => "Oak Leaves",
            BlockId::OakPlanks => "Oak Planks",
            BlockId::Brick => "Brick",
            BlockId::Snow => "Snow",
            BlockId::Bedrock => "Bedrock",
            BlockId::CoalOre => "Coal Ore",
            BlockId::IronOre => "Iron Ore",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_solidity_and_texture_layers() {
        assert!(!BlockId::Air.is_solid());
        assert!(BlockId::Grass.is_solid());
        assert_eq!(BlockId::Grass.face_tex_layer(2), 0);
        assert_eq!(BlockId::Grass.face_tex_layer(3), 2);
        assert_eq!(BlockId::Grass.face_tex_layer(0), 1);
        assert_eq!(BlockId::OakLog.face_tex_layer(2), 6);
        assert_eq!(BlockId::OakLog.face_tex_layer(4), 7);
        assert_eq!(BlockId::Stone.name(), "Stone");
    }

    #[test]
    fn test_block_ids_roundtrip_and_ore_metadata() {
        for id in 0u8..=13 {
            let block = BlockId::from_u8(id).expect("known block id must decode");
            assert_eq!(block as u8, id, "block id {id} must round-trip");
        }
        assert_eq!(BlockId::from_u8(14), None);
        assert_eq!(BlockId::from_u8(99), None);

        assert_eq!(BlockId::CoalOre.name(), "Coal Ore");
        assert_eq!(BlockId::IronOre.name(), "Iron Ore");
        assert!(BlockId::CoalOre.is_solid());
        assert!(BlockId::IronOre.is_solid());

        for face_dir in 0u8..6 {
            assert_eq!(BlockId::CoalOre.face_tex_layer(face_dir), 14);
            assert_eq!(BlockId::IronOre.face_tex_layer(face_dir), 15);
        }
    }
}
