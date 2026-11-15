//! Neutral block classification and the voxel query contract.
//!
//! Engine crates sample the world through [`BlockQuery`] so neither physics nor
//! pathfinding has to know a game block identity.

/// What a sampled voxel cell is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Passable, non-liquid cell (air and any other empty cell).
    Empty,
    /// Opaque, collidable cell.
    Solid,
    /// Passable liquid volume.
    Liquid,
}

impl BlockKind {
    #[inline]
    pub const fn is_solid(self) -> bool {
        matches!(self, BlockKind::Solid)
    }

    #[inline]
    pub const fn is_liquid(self) -> bool {
        matches!(self, BlockKind::Liquid)
    }
}

/// One-call voxel probe used by the engine crates.
///
/// The blanket closure impl keeps tests and synthetic worlds cheap:
///
/// ```
/// use opencraft_core::block::{BlockKind, BlockQuery};
///
/// let floor = |_x: i32, y: i32, _z: i32| {
///     if y < 0 { BlockKind::Solid } else { BlockKind::Empty }
/// };
/// assert_eq!(floor.block_kind(0, -1, 0), BlockKind::Solid);
/// ```
pub trait BlockQuery {
    fn block_kind(&self, x: i32, y: i32, z: i32) -> BlockKind;
}

impl<F> BlockQuery for F
where
    F: Fn(i32, i32, i32) -> BlockKind,
{
    #[inline]
    fn block_kind(&self, x: i32, y: i32, z: i32) -> BlockKind {
        self(x, y, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_kind_helpers_classify_only_their_own_variant() {
        assert!(BlockKind::Solid.is_solid());
        assert!(!BlockKind::Solid.is_liquid());

        assert!(BlockKind::Liquid.is_liquid());
        assert!(!BlockKind::Liquid.is_solid());

        assert!(!BlockKind::Empty.is_solid());
        assert!(!BlockKind::Empty.is_liquid());
    }

    #[test]
    fn closure_fake_answers_coordinates_through_the_trait() {
        // A synthetic world: bedrock floor, a liquid pool on the y = 0 ring,
        // empty everywhere else.
        let world = |x: i32, y: i32, z: i32| {
            if y < 0 {
                BlockKind::Solid
            } else if y == 0 && x.rem_euclid(2) == 0 && z.rem_euclid(2) == 0 {
                BlockKind::Liquid
            } else {
                BlockKind::Empty
            }
        };

        fn sample<Q: BlockQuery>(world: &Q, x: i32, y: i32, z: i32) -> BlockKind {
            world.block_kind(x, y, z)
        }

        assert_eq!(sample(&world, 0, -1, 0), BlockKind::Solid);
        assert_eq!(sample(&world, -4, -7, 9), BlockKind::Solid);
        assert_eq!(sample(&world, 0, 0, 0), BlockKind::Liquid);
        assert_eq!(sample(&world, -2, 0, 4), BlockKind::Liquid);
        assert_eq!(sample(&world, 1, 0, 0), BlockKind::Empty);
        assert_eq!(sample(&world, 0, 1, 0), BlockKind::Empty);
    }
}
