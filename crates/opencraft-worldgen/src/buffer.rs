//! Neutral raw-`u16` chunk buffer and its uniform/dense hot path.
//!
//! Deliberately an exact mirror of the game `ChunkStorage`/`Chunk` semantics:
//! a uniform buffer only holds one id, promotion allocates a boxed 64 KiB fill
//! and writes the single changed cell, and [`ChunkBuffer::try_compress`] folds
//! a dense buffer whose cells are all equal back to uniform. The generator
//! therefore produces the same storage shape (and the same pinned hashes) as
//! the game's own chunk type, without this crate importing a game type.

use opencraft_core::chunk::{linear_index, CHUNK_VOLUME};

/// Dense buffers are exactly 65,536 bytes (one `u16` per cell).
const _: () = assert!(std::mem::size_of::<u16>() * CHUNK_VOLUME == 65_536);

/// One 32x32x32 block buffer addressed by raw `u16` block ids.
#[derive(Clone, Debug)]
pub enum ChunkBuffer {
    Uniform(u16),
    Dense(Box<[u16; CHUNK_VOLUME]>),
}

impl ChunkBuffer {
    #[inline]
    pub fn uniform(block: u16) -> Self {
        Self::Uniform(block)
    }

    #[inline]
    pub fn is_uniform(&self) -> Option<u16> {
        match self {
            ChunkBuffer::Uniform(block) => Some(*block),
            ChunkBuffer::Dense(_) => None,
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> u16 {
        match self {
            ChunkBuffer::Uniform(block) => *block,
            ChunkBuffer::Dense(cells) => cells[linear_index(x, y, z)],
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: u16) {
        self.set_by_index(linear_index(x, y, z) as u16, block);
    }

    #[inline]
    pub fn set_by_index(&mut self, idx: u16, block: u16) {
        let idx = idx as usize;
        debug_assert!(idx < CHUNK_VOLUME);
        match self {
            ChunkBuffer::Uniform(current) => {
                if *current == block {
                    return;
                }
                let fill = *current;
                // Never `Box::new([fill; CHUNK_VOLUME])`: that materializes a
                // transient 64 KB stack array in every worker.
                let mut boxed: Box<[u16; CHUNK_VOLUME]> = vec![fill; CHUNK_VOLUME]
                    .into_boxed_slice()
                    .try_into()
                    .expect("CHUNK_VOLUME sized array");
                boxed[idx] = block;
                *self = ChunkBuffer::Dense(boxed);
            }
            ChunkBuffer::Dense(cells) => {
                cells[idx] = block;
            }
        }
    }

    /// Fold a dense buffer into a uniform one when every cell is equal.
    ///
    /// Returns `true` when the buffer is (now) uniform, matching the game
    /// `Chunk::try_compress` contract so the pinned all-Air sky case keeps its
    /// `Some(Air)` shape.
    pub fn try_compress(&mut self) -> bool {
        match self {
            ChunkBuffer::Uniform(_) => true,
            ChunkBuffer::Dense(cells) => {
                let first = cells[0];
                if cells.iter().all(|&block| block == first) {
                    *self = ChunkBuffer::Uniform(first);
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BLOCK_AIR, BLOCK_STONE};

    #[test]
    fn uniform_get_and_promotion_match_the_game_chunk() {
        let mut buffer = ChunkBuffer::uniform(BLOCK_AIR);
        assert_eq!(buffer.is_uniform(), Some(BLOCK_AIR));
        // Setting the uniform value back onto itself is a no-op, not a
        // promotion.
        buffer.set(0, 0, 0, BLOCK_AIR);
        assert_eq!(buffer.is_uniform(), Some(BLOCK_AIR));

        buffer.set(3, 4, 5, BLOCK_STONE);
        assert_eq!(buffer.is_uniform(), None);
        assert_eq!(buffer.get(3, 4, 5), BLOCK_STONE);
        assert_eq!(buffer.get(0, 0, 0), BLOCK_AIR);
        match &buffer {
            ChunkBuffer::Dense(cells) => {
                assert_eq!(std::mem::size_of_val(&**cells), CHUNK_VOLUME * 2);
            }
            ChunkBuffer::Uniform(_) => panic!("one changed cell must promote to dense"),
        }
    }

    #[test]
    fn dense_set_and_get_round_trip() {
        let mut buffer = ChunkBuffer::uniform(BLOCK_AIR);
        buffer.set(1, 2, 3, BLOCK_STONE);
        assert_eq!(buffer.get(1, 2, 3), BLOCK_STONE);
        assert_eq!(buffer.get(2, 2, 3), BLOCK_AIR);
        buffer.set_by_index(linear_index(9, 8, 7) as u16, 42);
        assert_eq!(buffer.get(9, 8, 7), 42);
        // Index-addressed writes agree with coordinate-addressed ones.
        buffer.set(9, 8, 7, 43);
        assert_eq!(buffer.get(9, 8, 7), 43);
    }

    #[test]
    fn try_compress_folds_uniform_and_rejects_mixed() {
        let mut buffer = ChunkBuffer::uniform(BLOCK_STONE);
        assert!(buffer.try_compress(), "uniform buffer is already compact");
        assert_eq!(buffer.is_uniform(), Some(BLOCK_STONE));

        buffer.set(0, 0, 0, BLOCK_AIR);
        assert!(!buffer.try_compress(), "a mixed dense buffer stays dense");
        assert_eq!(buffer.is_uniform(), None);

        buffer.set(0, 0, 0, BLOCK_STONE);
        assert!(buffer.try_compress());
        assert_eq!(buffer.is_uniform(), Some(BLOCK_STONE));
    }
}
