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
    /// Per-cell sky light, 0-15. Defaults to all-15 (fully lit) so the T01
    /// placeholder keeps rendering identical until real light is wired in.
    pub light: Box<[u8; PADDED_VOLUME]>,
    pub has_any_solid: bool,
}

impl PaddedChunk {
    pub fn new_empty() -> Self {
        Self {
            voxels: Box::new([BlockId::Air; PADDED_VOLUME]),
            light: Box::new([15u8; PADDED_VOLUME]),
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

    #[inline]
    pub fn set_light(&mut self, px: usize, py: usize, pz: usize, light: u8) {
        debug_assert!(light <= 15);
        self.light[padded_index(px, py, pz)] = light & 15;
    }

    #[inline]
    pub fn light_at(&self, px: usize, py: usize, pz: usize) -> u8 {
        self.light[padded_index(px, py, pz)] & 15
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
fn neighbor_light(padded: &PaddedChunk, axis: usize, d: usize, u: usize, v: usize) -> u8 {
    let (px, py, pz) = get_axis_coord(axis, d, u, v);
    padded.light_at(px, py, pz)
}

/// Smooth (integer, rounded-down) average of the non-solid light samples for a
/// quad corner. Solid neighbor cells are `None` and excluded; if every sample is
/// solid the result is 0.
#[inline]
fn average_light(samples: [Option<u8>; 4]) -> u8 {
    let mut sum = 0u32;
    let mut count = 0u32;
    for value in samples.into_iter().flatten() {
        sum += value as u32;
        count += 1;
    }
    sum.checked_div(count).unwrap_or(0) as u8
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
        for (pv, mask_row) in solid_masks.iter_mut().enumerate() {
            for (pu, cell) in mask_row.iter_mut().enumerate() {
                let mut mask = 0u64;
                for pd in 0..PADDED_SIZE {
                    let (px, py, pz) = get_axis_coord(axis, pd, pu, pv);
                    if padded.get(px, py, pz).is_solid() {
                        mask |= 1u64 << pd;
                    }
                }
                *cell = mask;
            }
        }

        // Process positive (is_pos = true -> face_dir = axis*2) and negative (is_pos = false -> face_dir = axis*2 + 1)
        for is_pos in [true, false] {
            let face_dir = (axis * 2 + usize::from(!is_pos)) as u8;

            // slice_keys[v][u][d] for d in 0..32, v in 0..32, u in 0..32
            // 0 means no visible face; non-zero is
            // ((tex_layer as u32 + 1) | ((ao_packed as u32) << 8) | ((light_packed as u32) << 16))
            let mut slice_keys = [[[0u32; CHUNK_SIZE]; CHUNK_SIZE]; CHUNK_SIZE];
            let mut slice_row_masks = [[0u32; CHUNK_SIZE]; CHUNK_SIZE];

            for (v, keys_v) in slice_keys.iter_mut().enumerate() {
                let pv = v + 1;
                for (u, keys_v_u) in keys_v.iter_mut().enumerate() {
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

                        // Corner light uses the same air-side neighbor geometry as AO:
                        // the center air cell plus each corner's two edge neighbors and
                        // diagonal, with solid samples excluded from the average.
                        let center_light = neighbor_light(padded, axis, air_d, pu, pv);
                        let light_um = if s_um {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu - 1, pv))
                        };
                        let light_up = if s_up {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu + 1, pv))
                        };
                        let light_vm = if s_vm {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu, pv - 1))
                        };
                        let light_vp = if s_vp {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu, pv + 1))
                        };
                        let light_c00 = if c00 {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu - 1, pv - 1))
                        };
                        let light_c10 = if c10 {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu + 1, pv - 1))
                        };
                        let light_c01 = if c01 {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu - 1, pv + 1))
                        };
                        let light_c11 = if c11 {
                            None
                        } else {
                            Some(neighbor_light(padded, axis, air_d, pu + 1, pv + 1))
                        };

                        let l00 = average_light([
                            Some(center_light),
                            light_um,
                            light_vm,
                            light_c00,
                        ]);
                        let l10 = average_light([
                            Some(center_light),
                            light_up,
                            light_vm,
                            light_c10,
                        ]);
                        let l01 = average_light([
                            Some(center_light),
                            light_um,
                            light_vp,
                            light_c01,
                        ]);
                        let l11 = average_light([
                            Some(center_light),
                            light_up,
                            light_vp,
                            light_c11,
                        ]);
                        let light_packed = (l00 as u32)
                            | ((l10 as u32) << 4)
                            | ((l01 as u32) << 8)
                            | ((l11 as u32) << 12);

                        let key = (tex_layer as u32 + 1)
                            | ((ao_packed as u32) << 8)
                            | (light_packed << 16);
                        keys_v_u[d] = key;
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
                        let key = slice_keys[v][u][d];

                        // Expand width w along u
                        let mut w = 1usize;
                        while u + w < CHUNK_SIZE
                            && (row_masks[v] & (1u32 << (u + w))) != 0
                            && slice_keys[v][u + w][d] == key
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
                            if slice_keys[v + h][u..(u + w)].iter().any(|k| k[d] != key) {
                                break 'height;
                            }
                            h += 1;
                        }

                        // Clear merged bits from row_masks
                        for mask in &mut row_masks[v..(v + h)] {
                            *mask &= !span_mask;
                        }

                        let tex_layer = ((key & 0xFF) - 1) as u8;
                        let ao_packed = (key >> 8) as u8;
                        let light_packed = (key >> 16) as u16;
                        let (qx, qy, qz) = get_axis_coord(axis, d, u, v);
                        bucket.push(PackedQuad::new(
                            [qx as u32, qy as u32, qz as u32],
                            [w as u32, h as u32],
                            face_dir as u32,
                            tex_layer,
                            ao_packed,
                            light_packed,
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
            let (_, _, _, w, h, fdir, _, _, _) = mesh.buckets[dir][0].unpack();
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
            let (_, _, _, w, h, _, _, _, _) = mesh.buckets[dir][0].unpack();
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

    #[test]
    fn test_placeholder_all_lit_light_is_uniform_15() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);
        assert!(padded.light.iter().all(|&l| l == 15));
        let mesh = mesh_chunk(&padded);
        for bucket in &mesh.buckets {
            for quad in bucket {
                assert_eq!(quad.unpack().8, 0xFFFF, "all corners fully lit");
            }
        }
    }

    #[test]
    fn test_corner_light_averages_non_solid_neighbors() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Stone);
        let mut padded = PaddedChunk::from_center_only(&chunk);
        // Start dark so every sample is explicit, then fill the +X air plane
        // around the single voxel's PosX face (padded x=7, y=7, z=8).
        *padded.light = [0u8; PADDED_VOLUME];
        padded.set_light(7, 7, 8, 15); // air-side center (u=z 8, v=y 7)
        padded.set_light(7, 7, 7, 12); // u- neighbor
        padded.set_light(7, 6, 8, 8); // v- neighbor
        padded.set_light(7, 6, 7, 4); // u-v- corner

        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.buckets[0].len(), 1);
        let light_packed = mesh.buckets[0][0].unpack().8;
        // l00 = (15 + 12 + 8 + 4) / 4 = 9 (round down)
        // l10 = (15 + 0 + 8 + 0) / 4 = 5
        // l01 = (15 + 12 + 0 + 0) / 4 = 6
        // l11 = (15 + 0 + 0 + 0) / 4 = 3
        assert_eq!(light_packed, 9 | (5 << 4) | (6 << 8) | (3 << 12));
    }

    #[test]
    fn test_average_light_excludes_solid_samples_and_floors() {
        assert_eq!(average_light([None, None, None, None]), 0);
        assert_eq!(average_light([Some(15), Some(8), None, None]), 11);
        assert_eq!(average_light([Some(15), None, None, None]), 15);
        assert_eq!(average_light([Some(0), Some(0), Some(0), Some(0)]), 0);
    }

    #[test]
    fn test_set_light_masks_to_nibble() {
        let mut padded = PaddedChunk::new_empty();
        padded.set_light(1, 1, 1, 7);
        assert_eq!(padded.light_at(1, 1, 1), 7);
        padded.set_light(1, 1, 1, 15);
        assert_eq!(padded.light_at(1, 1, 1), 15);
        // Defensive masking keeps packed nibbles in 0-15 even without debug asserts.
        padded.light[padded_index(1, 1, 1)] = 0xFF;
        assert_eq!(padded.light_at(1, 1, 1), 15);
    }
}
