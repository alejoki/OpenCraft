use crate::mesh::quad::{ChunkMesh, PackedQuad};
use crate::world::block::BlockId;
use crate::world::chunk::{Chunk, CHUNK_SIZE};

pub const PADDED_SIZE: usize = CHUNK_SIZE + 2;
pub const PADDED_VOLUME: usize = PADDED_SIZE * PADDED_SIZE * PADDED_SIZE;

#[inline]
pub const fn padded_index(px: usize, py: usize, pz: usize) -> usize {
    debug_assert!(px < PADDED_SIZE && py < PADDED_SIZE && pz < PADDED_SIZE);
    px + pz * PADDED_SIZE + py * PADDED_SIZE * PADDED_SIZE
}

#[derive(Clone)]
pub struct PaddedChunk {
    pub voxels: Box<[BlockId; PADDED_VOLUME]>,
    pub has_any_solid: bool,
}

impl PaddedChunk {
    pub fn new_empty() -> Self {
        Self {
            voxels: Box::new([BlockId::Air; PADDED_VOLUME]),
            has_any_solid: false,
        }
    }

    pub fn from_center_only(chunk: &Chunk) -> Self {
        let mut padded = Self::new_empty();
        if let Some(b) = chunk.is_uniform() {
            if !b.is_solid() {
                return padded;
            }
            padded.has_any_solid = true;
            for py in 1..=CHUNK_SIZE {
                for pz in 1..=CHUNK_SIZE {
                    for px in 1..=CHUNK_SIZE {
                        padded.voxels[padded_index(px, py, pz)] = b;
                    }
                }
            }
            return padded;
        }

        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let b = chunk.get(lx, ly, lz);
                    if b.is_solid() {
                        padded.has_any_solid = true;
                        padded.voxels[padded_index(lx + 1, ly + 1, lz + 1)] = b;
                    }
                }
            }
        }
        padded
    }

    #[inline]
    pub fn set(&mut self, px: usize, py: usize, pz: usize, block: BlockId) {
        if block.is_solid() {
            self.has_any_solid = true;
        }
        self.voxels[padded_index(px, py, pz)] = block;
    }

    #[inline]
    pub fn get(&self, px: usize, py: usize, pz: usize) -> BlockId {
        self.voxels[padded_index(px, py, pz)]
    }
}

#[inline]
const fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        0
    } else {
        3 - (side1 as u8 + side2 as u8 + corner as u8)
    }
}

#[inline]
fn get_axis_coord(axis: usize, d: usize, u: usize, v: usize) -> (usize, usize, usize) {
    match axis {
        // Axis 0 (X): d = x, u = z, v = y
        0 => (d, v, u),
        // Axis 1 (Y): d = y, u = x, v = z
        1 => (u, d, v),
        // Axis 2 (Z): d = z, u = x, v = y
        _ => (u, v, d),
    }
}

pub fn mesh_chunk(padded: &PaddedChunk) -> ChunkMesh {
    let mut mesh = ChunkMesh::new();
    if !padded.has_any_solid {
        return mesh;
    }

    // col_masks[axis][v][u] holds 64-bit solidity mask along d in 0..34
    // For each axis (0=X, 1=Y, 2=Z):
    for axis in 0..3 {
        // Precompute 34x34 solidity masks along d (0..34) so AO and face culling are fast
        let mut solid_masks = [[0u64; PADDED_SIZE]; PADDED_SIZE];
        for pv in 0..PADDED_SIZE {
            for pu in 0..PADDED_SIZE {
                let mut mask = 0u64;
                for pd in 0..PADDED_SIZE {
                    let (px, py, pz) = get_axis_coord(axis, pd, pu, pv);
                    if padded.get(px, py, pz).is_solid() {
                        mask |= 1u64 << pd;
                    }
                }
                solid_masks[pv][pu] = mask;
            }
        }

        // Process positive (is_pos = true -> face_dir = axis*2) and negative (is_pos = false -> face_dir = axis*2 + 1)
        for is_pos in [true, false] {
            let face_dir = (axis * 2 + usize::from(!is_pos)) as u8;

            // slice_keys[d][v][u] for d in 0..32, v in 0..32, u in 0..32
            // 0 means no visible face; non-zero is ((tex_layer as u16 + 1) | ((ao_packed as u16) << 8))
            let mut slice_keys = [[[0u16; CHUNK_SIZE]; CHUNK_SIZE]; CHUNK_SIZE];
            let mut slice_row_masks = [[0u32; CHUNK_SIZE]; CHUNK_SIZE];

            for v in 0..CHUNK_SIZE {
                let pv = v + 1;
                for u in 0..CHUNK_SIZE {
                    let pu = u + 1;
                    let col_mask = solid_masks[pv][pu];
                    if col_mask == 0 {
                        continue;
                    }
                    // Bits 1..=32 shifted down to 0..31
                    let mut visible_mask = if is_pos {
                        ((col_mask & !(col_mask >> 1)) >> 1) as u32
                    } else {
                        ((col_mask & !(col_mask << 1)) >> 1) as u32
                    };

                    while visible_mask != 0 {
                        let d = visible_mask.trailing_zeros() as usize;
                        visible_mask &= visible_mask - 1;

                        let pd = d + 1;
                        let (px, py, pz) = get_axis_coord(axis, pd, pu, pv);
                        let block = padded.get(px, py, pz);
                        let tex_layer = block.face_tex_layer(face_dir);

                        let air_d = if is_pos { pd + 1 } else { pd - 1 };
                        let air_bit = 1u64 << air_d;

                        let s_um = (solid_masks[pv][pu - 1] & air_bit) != 0;
                        let s_up = (solid_masks[pv][pu + 1] & air_bit) != 0;
                        let s_vm = (solid_masks[pv - 1][pu] & air_bit) != 0;
                        let s_vp = (solid_masks[pv + 1][pu] & air_bit) != 0;
                        let c00 = (solid_masks[pv - 1][pu - 1] & air_bit) != 0;
                        let c10 = (solid_masks[pv - 1][pu + 1] & air_bit) != 0;
                        let c01 = (solid_masks[pv + 1][pu - 1] & air_bit) != 0;
                        let c11 = (solid_masks[pv + 1][pu + 1] & air_bit) != 0;

                        let ao00 = vertex_ao(s_um, s_vm, c00);
                        let ao10 = vertex_ao(s_up, s_vm, c10);
                        let ao01 = vertex_ao(s_um, s_vp, c01);
                        let ao11 = vertex_ao(s_up, s_vp, c11);
                        let ao_packed = ao00 | (ao10 << 2) | (ao01 << 4) | (ao11 << 6);

                        let key = (tex_layer as u16 + 1) | ((ao_packed as u16) << 8);
                        slice_keys[d][v][u] = key;
                        slice_row_masks[d][v] |= 1u32 << u;
                    }
                }
            }

            // Greedy 2D merge per slice d in 0..32
            let bucket = &mut mesh.buckets[face_dir as usize];
            for d in 0..CHUNK_SIZE {
                let mut row_masks = slice_row_masks[d];
                for v in 0..CHUNK_SIZE {
                    while row_masks[v] != 0 {
                        let u = row_masks[v].trailing_zeros() as usize;
                        let key = slice_keys[d][v][u];

                        // Expand width w along u
                        let mut w = 1usize;
                        while u + w < CHUNK_SIZE
                            && (row_masks[v] & (1u32 << (u + w))) != 0
                            && slice_keys[d][v][u + w] == key
                        {
                            w += 1;
                        }

                        // Mask for bits u..(u+w)
                        let span_mask = if w == 32 {
                            u32::MAX
                        } else {
                            ((1u32 << w) - 1) << u
                        };

                        // Expand height h along v
                        let mut h = 1usize;
                        'height: while v + h < CHUNK_SIZE {
                            if (row_masks[v + h] & span_mask) != span_mask {
                                break;
                            }
                            for ku in u..(u + w) {
                                if slice_keys[d][v + h][ku] != key {
                                    break 'height;
                                }
                            }
                            h += 1;
                        }

                        // Clear merged bits from row_masks
                        for kv in v..(v + h) {
                            row_masks[kv] &= !span_mask;
                        }

                        let tex_layer = ((key & 0xFF) - 1) as u8;
                        let ao_packed = (key >> 8) as u8;
                        let (qx, qy, qz) = get_axis_coord(axis, d, u, v);
                        bucket.push(PackedQuad::new(
                            qx as u32,
                            qy as u32,
                            qz as u32,
                            w as u32,
                            h as u32,
                            face_dir as u32,
                            tex_layer,
                            ao_packed,
                        ));
                    }
                }
            }
        }
    }

    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;
    use crate::world::chunk::Chunk;

    #[test]
    fn test_single_voxel_produces_six_unit_quads() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 6);
        for dir in 0..6 {
            assert_eq!(mesh.buckets[dir].len(), 1);
            let (_, _, _, w, h, fdir, _, _) = mesh.buckets[dir][0].unpack();
            assert_eq!((w, h, fdir), (1, 1, dir as u32));
        }
    }

    #[test]
    fn test_full_solid_chunk_greedy_merges_to_six_quads() {
        let chunk = Chunk::new_uniform(BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 6);
        for dir in 0..6 {
            assert_eq!(mesh.buckets[dir].len(), 1);
            let (_, _, _, w, h, _, _, _) = mesh.buckets[dir][0].unpack();
            assert_eq!((w, h), (32, 32));
        }
    }

    #[test]
    fn test_halo_culls_chunk_boundary_face() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(31, 10, 10, BlockId::Stone);
        let mut padded = PaddedChunk::from_center_only(&chunk);
        padded.set(33, 11, 11, BlockId::Stone);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.buckets[0].len(), 0);
        assert_eq!(mesh.total_quads(), 5);
    }
}
