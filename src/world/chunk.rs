use crate::world::block::BlockId;

pub const CHUNK_SIZE: usize = 32;
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl ChunkPos {
    #[inline]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

#[derive(Clone, Debug)]
pub enum ChunkStorage {
    Uniform(BlockId),
    Dense(Box<[BlockId; CHUNK_VOLUME]>),
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub storage: ChunkStorage,
}

#[inline]
pub const fn linear_index(x: usize, y: usize, z: usize) -> usize {
    debug_assert!(x < CHUNK_SIZE && y < CHUNK_SIZE && z < CHUNK_SIZE);
    x + (z << 5) + (y << 10)
}

impl Chunk {
    #[inline]
    pub fn new_uniform(block: BlockId) -> Self {
        Self {
            storage: ChunkStorage::Uniform(block),
        }
    }

    #[inline]
    pub fn is_uniform(&self) -> Option<BlockId> {
        match self.storage {
            ChunkStorage::Uniform(b) => Some(b),
            ChunkStorage::Dense(_) => None,
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockId {
        match &self.storage {
            ChunkStorage::Uniform(b) => *b,
            ChunkStorage::Dense(arr) => arr[linear_index(x, y, z)],
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: BlockId) {
        let idx = linear_index(x, y, z);
        match &mut self.storage {
            ChunkStorage::Uniform(u) => {
                if *u == block {
                    return;
                }
                let fill = *u;
                let mut boxed = Box::new([fill; CHUNK_VOLUME]);
                boxed[idx] = block;
                self.storage = ChunkStorage::Dense(boxed);
            }
            ChunkStorage::Dense(arr) => {
                arr[idx] = block;
            }
        }
    }

    pub fn try_compress(&mut self) -> bool {
        match &self.storage {
            ChunkStorage::Uniform(_) => true,
            ChunkStorage::Dense(arr) => {
                let first = arr[0];
                if arr.iter().all(|&b| b == first) {
                    self.storage = ChunkStorage::Uniform(first);
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[inline]
pub fn world_to_chunk_and_local(wx: i32, wy: i32, wz: i32) -> (ChunkPos, (usize, usize, usize)) {
    let cs = CHUNK_SIZE as i32;
    let cx = wx.div_euclid(cs);
    let cy = wy.div_euclid(cs);
    let cz = wz.div_euclid(cs);
    let lx = wx.rem_euclid(cs) as usize;
    let ly = wy.rem_euclid(cs) as usize;
    let lz = wz.rem_euclid(cs) as usize;
    (ChunkPos::new(cx, cy, cz), (lx, ly, lz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    #[test]
    fn test_negative_world_coordinates() {
        assert_eq!(
            world_to_chunk_and_local(0, 32, -1),
            (ChunkPos::new(0, 1, -1), (0, 0, 31))
        );
        assert_eq!(
            world_to_chunk_and_local(-32, -33, 63),
            (ChunkPos::new(-1, -2, 1), (0, 31, 31))
        );
    }

    #[test]
    fn test_chunk_uniform_promotion_and_compression() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        assert_eq!(chunk.is_uniform(), Some(BlockId::Air));
        chunk.set(3, 4, 5, BlockId::Stone);
        assert_eq!(chunk.is_uniform(), None);
        assert_eq!(chunk.get(3, 4, 5), BlockId::Stone);
        assert_eq!(chunk.get(0, 0, 0), BlockId::Air);
        chunk.set(3, 4, 5, BlockId::Air);
        assert!(chunk.try_compress());
        assert_eq!(chunk.is_uniform(), Some(BlockId::Air));
    }
}
