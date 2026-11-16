//! Deterministic synthetic worlds for the external reuse proof.
//!
//! Every constructor returns a [`ProofWorld`] built from a neutral floor rule
//! plus a small override map. Cells are answered as
//! [`BlockKind`] only, so the consumer never needs a game block identity.

use std::collections::HashMap;

use opencraft_core::block::{BlockKind, BlockQuery};

/// A deterministic voxel world for the proof scenarios.
///
/// The floor rule makes every cell below `floor_below_y` solid; an override
/// wins over the floor rule, so a pit can remove floor cells. There is no
/// randomness, no I/O and no game identity.
pub struct ProofWorld {
    floor_below_y: i32,
    overrides: HashMap<(i32, i32, i32), BlockKind>,
}

impl ProofWorld {
    fn with_floor(floor_below_y: i32) -> Self {
        Self {
            floor_below_y,
            overrides: HashMap::new(),
        }
    }

    fn set(&mut self, x: i32, y: i32, z: i32, kind: BlockKind) {
        self.overrides.insert((x, y, z), kind);
    }

    /// Solid below `y = 0`, empty above.
    pub fn flat_floor() -> Self {
        Self::with_floor(0)
    }

    /// Flat floor plus a one-block step at `y = 0`, `z = 2` across the walk
    /// lane, so a +Z walker has to step up.
    pub fn floor_with_step() -> Self {
        let mut world = Self::with_floor(0);
        for x in -1..=1 {
            world.set(x, 0, 2, BlockKind::Solid);
        }
        world
    }

    /// Flat floor plus a two-block wall at `x = 3` across the walk lane, so
    /// both a +X ray at feet level and a +X walker meet it.
    pub fn floor_with_wall() -> Self {
        let mut world = Self::with_floor(0);
        for y in 0..=1 {
            for z in -1..=1 {
                world.set(3, y, z, BlockKind::Solid);
            }
        }
        world
    }

    /// Flat floor with the two blocks below `z >= 2` removed across the walk
    /// lane, leaving a two-block drop that the cliff guard must refuse.
    pub fn floor_with_pit() -> Self {
        let mut world = Self::with_floor(0);
        for y in -2..=-1 {
            for z in 2..=32 {
                for x in -1..=1 {
                    world.set(x, y, z, BlockKind::Empty);
                }
            }
        }
        world
    }
}

impl BlockQuery for ProofWorld {
    fn block_kind(&self, x: i32, y: i32, z: i32) -> BlockKind {
        if let Some(kind) = self.overrides.get(&(x, y, z)) {
            return *kind;
        }
        if y < self.floor_below_y {
            BlockKind::Solid
        } else {
            BlockKind::Empty
        }
    }
}
