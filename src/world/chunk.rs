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

/// M010 contract: dense chunk cells are raw `u16` ids, independent of the
/// in-memory width of [`BlockId`]. A dense chunk is exactly 65,536 bytes.
const _: () = assert!(std::mem::size_of::<u16>() * CHUNK_VOLUME == 65_536);

#[derive(Clone, Debug)]
pub enum ChunkStorage {
    Uniform(BlockId),
    Dense(Box<[u16; CHUNK_VOLUME]>),
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

#[inline]
pub const fn local_coords_from_index(idx: u16) -> (usize, usize, usize) {
    let i = idx as usize;
    let x = i & 31;
    let z = (i >> 5) & 31;
    let y = (i >> 10) & 31;
    (x, y, z)
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
            ChunkStorage::Dense(arr) => BlockId::from_raw(arr[linear_index(x, y, z)]),
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: BlockId) {
        self.set_by_index(linear_index(x, y, z) as u16, block);
    }

    #[inline]
    pub fn set_by_index(&mut self, idx: u16, block: BlockId) {
        let idx = idx as usize;
        debug_assert!(idx < CHUNK_VOLUME);
        match &mut self.storage {
            ChunkStorage::Uniform(u) => {
                if *u == block {
                    return;
                }
                let fill = u.to_raw();
                // Avoid `Box::new([fill; CHUNK_VOLUME])`: that materializes a
                // transient 64 KB stack array in every rayon worker.
                let mut boxed: Box<[u16; CHUNK_VOLUME]> = vec![fill; CHUNK_VOLUME]
                    .into_boxed_slice()
                    .try_into()
                    .expect("CHUNK_VOLUME sized array");
                boxed[idx] = block.to_raw();
                self.storage = ChunkStorage::Dense(boxed);
            }
            ChunkStorage::Dense(arr) => {
                arr[idx] = block.to_raw();
            }
        }
    }

    pub fn try_compress(&mut self) -> bool {
        match &self.storage {
            ChunkStorage::Uniform(_) => true,
            ChunkStorage::Dense(arr) => {
                let first = arr[0];
                if arr.iter().all(|&raw| raw == first) {
                    self.storage = ChunkStorage::Uniform(BlockId::from_raw(first));
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

    #[test]
    fn dense_chunk_storage_is_two_bytes_per_block() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(1, 2, 3, BlockId::Stone);
        match &chunk.storage {
            ChunkStorage::Dense(arr) => {
                assert_eq!(std::mem::size_of_val(&**arr), 65_536);
                assert_eq!(std::mem::size_of_val(&**arr), CHUNK_VOLUME * 2);
            }
            ChunkStorage::Uniform(_) => panic!("one non-uniform cell must promote to dense"),
        }
    }

    #[test]
    fn chunk_round_trips_every_builtin_and_modded_id() {
        // Identity is total and bijective over the whole u16 space.
        for raw in 0..=u16::MAX {
            assert_eq!(BlockId::from_raw(raw).to_raw(), raw);
        }
        // Sampled built-in plus modded ids round-trip through dense storage,
        // and promotion never touches the neighbouring cell.
        for raw in (0..=crate::world::block::BUILTIN_BLOCK_COUNT).chain([1000, u16::MAX]) {
            let mut chunk = Chunk::new_uniform(BlockId::Air);
            chunk.set(1, 2, 3, BlockId::from_raw(raw));
            assert_eq!(chunk.get(1, 2, 3), BlockId::from_raw(raw));
            assert_eq!(chunk.get(2, 2, 3), BlockId::Air);
        }
        // Exhaustive storage round-trip: a dense chunk holds every raw id.
        let mut low = Chunk::new_uniform(BlockId::Air);
        let mut high = Chunk::new_uniform(BlockId::Air);
        for idx in 0..CHUNK_VOLUME {
            low.set_by_index(idx as u16, BlockId::from_raw(idx as u16));
            high.set_by_index(
                idx as u16,
                BlockId::from_raw(idx as u16 + CHUNK_VOLUME as u16),
            );
        }
        for idx in 0..CHUNK_VOLUME {
            let (x, y, z) = local_coords_from_index(idx as u16);
            assert_eq!(low.get(x, y, z), BlockId::from_raw(idx as u16));
            assert_eq!(
                high.get(x, y, z),
                BlockId::from_raw(idx as u16 + CHUNK_VOLUME as u16)
            );
        }
    }

    #[test]
    fn uniform_promotion_and_compression_use_raw_ids() {
        let modded = BlockId::Modded(crate::world::block::BUILTIN_BLOCK_COUNT);
        let mut chunk = Chunk::new_uniform(modded);
        assert_eq!(chunk.is_uniform(), Some(modded));
        chunk.set(4, 5, 6, BlockId::Stone);
        assert_eq!(chunk.is_uniform(), None);
        assert_eq!(chunk.get(4, 5, 6), BlockId::Stone);
        assert_eq!(chunk.get(0, 0, 0), modded);
        chunk.set(4, 5, 6, modded);
        assert!(chunk.try_compress());
        assert_eq!(chunk.is_uniform(), Some(modded));
    }
}
