//! Chunk dimensions, chunk coordinates, and dense-cell indexing.
//!
//! Deliberately storage-free: the game keeps its dense block-cell `Chunk`
//! type, while the neutral coordinate math lives here.

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
    fn local_coords_and_linear_index_round_trip_every_cell() {
        for idx in 0..CHUNK_VOLUME {
            let (x, y, z) = local_coords_from_index(idx as u16);
            assert!(
                x < CHUNK_SIZE && y < CHUNK_SIZE && z < CHUNK_SIZE,
                "index {idx} must decode inside the chunk"
            );
            assert_eq!(
                linear_index(x, y, z),
                idx,
                "index {idx} must survive a linear_index round trip"
            );
        }
    }
}
