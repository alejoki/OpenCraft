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
    /// True when at least one liquid cell was copied into the padded volume.
    /// Kept separate from `has_any_solid` (water is not solid) so the mesher
    /// and the manager's fast skip still see water-bearing chunks.
    pub has_any_liquid: bool,
}

impl PaddedChunk {
    pub fn new_empty() -> Self {
        Self {
            voxels: Box::new([BlockId::Air; PADDED_VOLUME]),
            light: Box::new([15u8; PADDED_VOLUME]),
            has_any_solid: false,
            has_any_liquid: false,
        }
    }

    pub fn from_center_only(chunk: &Chunk) -> Self {
        let mut padded = Self::new_empty();
        if let Some(b) = chunk.is_uniform() {
            if b.is_liquid() {
                padded.has_any_liquid = true;
                for py in 1..=CHUNK_SIZE {
                    for pz in 1..=CHUNK_SIZE {
                        for px in 1..=CHUNK_SIZE {
                            padded.voxels[padded_index(px, py, pz)] = b;
                        }
                    }
                }
                return padded;
            }
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
                    } else if b.is_liquid() {
                        padded.has_any_liquid = true;
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
        } else if block.is_liquid() {
            self.has_any_liquid = true;
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
    // Water-only chunks must still be meshed (open ocean surface); only a
    // chunk with neither solid nor liquid geometry can produce no faces.
    if !padded.has_any_solid && !padded.has_any_liquid {
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

                        let l00 =
                            average_light([Some(center_light), light_um, light_vm, light_c00]);
                        let l10 =
                            average_light([Some(center_light), light_up, light_vm, light_c10]);
                        let l01 =
                            average_light([Some(center_light), light_um, light_vp, light_c01]);
                        let l11 =
                            average_light([Some(center_light), light_up, light_vp, light_c11]);
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

    mesh_water(padded, &mut mesh);

    mesh
}

/// Emit air-facing liquid faces into `ChunkMesh::water_quads` (D070).
///
/// Water is its own mask: it is never part of `solid_masks`, so a water cell
/// can neither cull a stone face nor be culled by one — the seabed keeps real
/// opaque faces and water quads never enter the opaque buckets. Only faces
/// with Air on the far side are emitted (the open top surface at
/// `SEA_LEVEL`, plus rare lateral air pockets); water↔water and water↔stone
/// boundaries emit nothing here.
fn mesh_water(padded: &PaddedChunk, mesh: &mut ChunkMesh) {
    if !padded.has_any_liquid {
        return;
    }

    // Water is unlit by the opaque sky-light model and uses a constant bright
    // key: tex layer 0 (placeholder until S04 ships the water texture), all
    // four AO corners full (3) and every corner at full light (15).
    const WATER_TEX_LAYER: u8 = 0;
    const WATER_AO: u8 = 0xFF;
    const WATER_LIGHT: u16 = 0xFFFF;

    for axis in 0..3 {
        for is_pos in [true, false] {
            let face_dir = (axis * 2 + usize::from(!is_pos)) as u8;
            for d in 0..CHUNK_SIZE {
                // visible[v][u]: a liquid cell at this slice has Air across
                // the face. Kept strictly separate from `solid_masks`.
                let mut visible = [[false; CHUNK_SIZE]; CHUNK_SIZE];
                let mut any_visible = false;
                for v in 0..CHUNK_SIZE {
                    for u in 0..CHUNK_SIZE {
                        let (px, py, pz) = get_axis_coord(axis, d + 1, u + 1, v + 1);
                        if !padded.get(px, py, pz).is_liquid() {
                            continue;
                        }
                        let air_d = if is_pos { d + 2 } else { d };
                        let (ax, ay, az) = get_axis_coord(axis, air_d, u + 1, v + 1);
                        if padded.get(ax, ay, az) == BlockId::Air {
                            visible[v][u] = true;
                            any_visible = true;
                        }
                    }
                }
                if !any_visible {
                    continue;
                }

                // Greedy merge within the slice. Every water face shares the
                // same key, so the merge only has to grow rectangles.
                for v in 0..CHUNK_SIZE {
                    let mut u = 0usize;
                    while u < CHUNK_SIZE {
                        if !visible[v][u] {
                            u += 1;
                            continue;
                        }
                        let mut w = 1usize;
                        while u + w < CHUNK_SIZE && visible[v][u + w] {
                            w += 1;
                        }
                        let mut h = 1usize;
                        'height: while v + h < CHUNK_SIZE {
                            for uu in u..(u + w) {
                                if !visible[v + h][uu] {
                                    break 'height;
                                }
                            }
                            h += 1;
                        }
                        for row in &mut visible[v..(v + h)] {
                            row[u..(u + w)].fill(false);
                        }
                        let (qx, qy, qz) = get_axis_coord(axis, d, u, v);
                        mesh.water_quads.push(PackedQuad::new(
                            [qx as u32, qy as u32, qz as u32],
                            [w as u32, h as u32],
                            face_dir as u32,
                            WATER_TEX_LAYER,
                            WATER_AO,
                            WATER_LIGHT,
                        ));
                        u += w;
                    }
                }
            }
        }
    }
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
        assert_eq!(mesh.water_quad_count(), 0, "solid chunks carry no water");
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

    #[test]
    fn real_ocean_chunk_emits_water_surface_quads_above_real_seabed_faces() {
        use crate::world::chunk::ChunkPos;
        use crate::world::terrain::{generate_chunk_seeded, SEA_LEVEL};

        const SEED: u32 = 1337;
        let mut found = None;
        'scan: for cx in -4..=4 {
            for cz in -4..=4 {
                let pos = ChunkPos::new(cx, 0, cz);
                let chunk = generate_chunk_seeded(pos, SEED);
                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        // Chunk layer 0 starts at wy 0, so local y == world y
                        // and SEA_LEVEL is inside this layer.
                        if chunk.get(lx, SEA_LEVEL as usize, lz) == BlockId::Water {
                            found = Some(chunk);
                            break 'scan;
                        }
                    }
                }
            }
        }
        let chunk = found.expect("near-origin scan must contain an ocean chunk");

        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);
        assert!(
            mesh.total_quads() > 0,
            "the real seabed keeps opaque stone/sand faces"
        );
        assert!(
            mesh.water_quad_count() > 0,
            "a real ocean chunk emits water surface quads"
        );
        // The open top surface at local y = SEA_LEVEL is a PosY (face_dir 2)
        // water quad. `from_center_only` leaves the halo Air, so side faces at
        // the chunk border are emitted too; the top face is the one that must
        // exist for the ocean to read as water.
        let has_top = mesh
            .water_quads
            .iter()
            .any(|q| q.unpack().5 == 2 && q.unpack().1 == SEA_LEVEL as u32);
        assert!(has_top, "ocean chunk must contain a top water surface quad");
    }

    #[test]
    fn water_only_padded_chunk_emits_water_quads_and_no_opaque_quads() {
        let padded = PaddedChunk::from_center_only(&Chunk::new_uniform(BlockId::Water));
        assert!(padded.has_any_liquid, "uniform water is recorded as liquid");
        assert!(!padded.has_any_solid, "water is not solid");

        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 0, "water never enters an opaque bucket");
        for (dir, bucket) in mesh.buckets.iter().enumerate() {
            assert!(
                bucket.is_empty(),
                "opaque bucket {dir} received a water quad"
            );
        }
        // A 32x32x32 water cube exposed on all six sides greedy-merges to one
        // water quad per face direction.
        assert_eq!(mesh.water_quad_count(), 6);
        let mut dirs: Vec<u32> = mesh.water_quads.iter().map(|q| q.unpack().5).collect();
        dirs.sort_unstable();
        assert_eq!(dirs, vec![0, 1, 2, 3, 4, 5]);
        for quad in &mesh.water_quads {
            let (_, _, _, w, h, _, _, _, light) = quad.unpack();
            assert_eq!((w, h), (32, 32));
            assert_eq!(light, 0xFFFF, "water quads carry a constant bright key");
        }
    }

    #[test]
    fn water_does_not_cull_stone_faces() {
        // A stone chunk with one cell replaced by water: the six stone faces
        // around the water cell must still be emitted (water is not solid and
        // is never in `solid_masks`), and the water cell emits nothing because
        // every neighbour is stone rather than Air.
        let mut chunk = Chunk::new_uniform(BlockId::Stone);
        chunk.set(5, 6, 7, BlockId::Water);
        let padded = PaddedChunk::from_center_only(&chunk);
        assert!(padded.has_any_solid && padded.has_any_liquid);

        let mesh = mesh_chunk(&padded);
        assert_eq!(
            mesh.total_quads(),
            12,
            "6 outer stone faces + 6 cavity walls facing the water cell"
        );
        assert_eq!(
            mesh.water_quad_count(),
            0,
            "water against stone is invisible"
        );
    }

    #[test]
    fn water_surrounded_by_air_emits_one_quad_per_face() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Water);
        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 0);
        assert_eq!(mesh.water_quad_count(), 6);
        for quad in &mesh.water_quads {
            let (_, _, _, w, h, _, _, _, _) = quad.unpack();
            assert_eq!((w, h), (1, 1));
        }
    }

    #[test]
    fn water_water_and_water_stone_boundaries_emit_no_faces() {
        // Two adjacent water cells share a face (no internal surface), and a
        // water cell pressed against stone emits nothing. Only the shared
        // air-facing faces appear.
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(4, 6, 7, BlockId::Water);
        chunk.set(5, 6, 7, BlockId::Water);
        chunk.set(6, 6, 7, BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);

        // The two-cell water pair has 9 air-facing unit faces: 5 for the
        // -X cell and 4 for the +X cell (its shared -X face is water-water,
        // its +X face is water-stone). Greedy merging across the pair turns
        // the Y and Z faces into 2x1 quads: 1 + 2 + 2 = 5 quads for 9 units.
        assert_eq!(mesh.water_quad_count(), 5);
        let water_area: u32 = mesh
            .water_quads
            .iter()
            .map(|q| {
                let (_, _, _, w, h, _, _, _, _) = q.unpack();
                w * h
            })
            .sum();
        assert_eq!(
            water_area, 9,
            "water-water and water-stone faces stay invisible"
        );
        // The stone cell is a single voxel against Air on five sides and water
        // on the -X side; water is not solid, so all six faces are opaque.
        assert_eq!(mesh.total_quads(), 6);
    }
}
