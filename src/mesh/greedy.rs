use crate::content::faces::BlockFaces;
use crate::content::{BlockAlpha, BlockModel};
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
    /// M010 contract: padded mesh cells are raw `u16` ids so each mesh job's
    /// copy stays ~78 KB instead of ~157 KB.
    pub voxels: Box<[u16; PADDED_VOLUME]>,
    /// Per-cell sky light, 0-15. Defaults to all-15 (fully lit) so the T01
    /// placeholder keeps rendering identical until real light is wired in.
    pub light: Box<[u8; PADDED_VOLUME]>,
    pub has_any_solid: bool,
    /// True when at least one liquid cell was copied into the padded volume.
    /// Kept separate from `has_any_solid` (water is not solid) so the mesher
    /// and the manager's fast skip still see water-bearing chunks.
    pub has_any_liquid: bool,
    /// True when at least one non-Air cell was copied into the padded volume.
    /// This is the mesher's real skip predicate: plants (non-solid, non-liquid
    /// cross/crop geometry) and glass (solid blend geometry) must still be
    /// meshed, so neither solidity nor liquidity can stand in for it.
    pub has_any_render: bool,
}

impl PaddedChunk {
    pub fn new_empty() -> Self {
        Self {
            voxels: vec![0u16; PADDED_VOLUME]
                .into_boxed_slice()
                .try_into()
                .expect("PADDED_VOLUME sized array"),
            light: Box::new([15u8; PADDED_VOLUME]),
            has_any_solid: false,
            has_any_liquid: false,
            has_any_render: false,
        }
    }

    pub fn from_center_only(chunk: &Chunk) -> Self {
        let mut padded = Self::new_empty();
        if let Some(b) = chunk.is_uniform() {
            // Uniform Air is the only volume with no geometry at all. Every
            // other uniform block (stone, water, wheat, glass) must fill the
            // padded voxels: a uniform wheat chunk that returned empty here
            // would silently lose its two cross quads per cell.
            if b == BlockId::Air {
                return padded;
            }
            padded.has_any_render = true;
            if b.is_solid() {
                padded.has_any_solid = true;
            } else if b.is_liquid() {
                padded.has_any_liquid = true;
            }
            for py in 1..=CHUNK_SIZE {
                for pz in 1..=CHUNK_SIZE {
                    for px in 1..=CHUNK_SIZE {
                        padded.voxels[padded_index(px, py, pz)] = b.to_raw();
                    }
                }
            }
            return padded;
        }

        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let b = chunk.get(lx, ly, lz);
                    if b == BlockId::Air {
                        continue;
                    }
                    padded.has_any_render = true;
                    if b.is_solid() {
                        padded.has_any_solid = true;
                    } else if b.is_liquid() {
                        padded.has_any_liquid = true;
                    }
                    padded.voxels[padded_index(lx + 1, ly + 1, lz + 1)] = b.to_raw();
                }
            }
        }
        padded
    }

    #[inline]
    pub fn set(&mut self, px: usize, py: usize, pz: usize, block: BlockId) {
        if block != BlockId::Air {
            self.has_any_render = true;
        }
        if block.is_solid() {
            self.has_any_solid = true;
        } else if block.is_liquid() {
            self.has_any_liquid = true;
        }
        self.voxels[padded_index(px, py, pz)] = block.to_raw();
    }

    #[inline]
    pub fn get(&self, px: usize, py: usize, pz: usize) -> BlockId {
        BlockId::from_raw(self.voxels[padded_index(px, py, pz)])
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

/// Meshes `padded` against the built-in face-layer table.
///
/// Every pre-M010 caller keeps this signature and therefore its exact output:
/// [`BlockFaces::builtin`] is derived from [`BlockId::face_tex_layer`].
pub fn mesh_chunk(padded: &PaddedChunk) -> ChunkMesh {
    mesh_chunk_with(padded, &BlockFaces::builtin())
}

/// Meshes `padded` resolving each visible face through `faces`.
///
/// The session-owned table is built from the frozen [`crate::content::ContentRegistry`]
/// so pack-authored blocks reach the world without threading the registry
/// into rayon workers. Each block's authored `(model, alpha)` render class
/// selects both the geometry and the destination container:
///
/// * `cube + opaque` and `cube + cutout` -> the six [`ChunkMesh::buckets`]
///   (cutout is an alpha-test state of the same opaque draw, not a new buffer);
/// * `cube + blend` -> [`ChunkMesh::blend_quads`] for the sorted translucent pass;
/// * `cross | crop` -> [`ChunkMesh::cross_quads`], two quads per block and
///   never a cube face.
///
/// The merge key is unchanged: `(tex_layer + 1) | (ao << 8) | (light << 16)`.
/// Each class is swept separately, so quads from different classes can never
/// merge across a class boundary.
pub fn mesh_chunk_with(padded: &PaddedChunk, faces: &BlockFaces) -> ChunkMesh {
    let mut mesh = ChunkMesh::new();
    // `has_any_render` covers every geometry class: solids, water, blend glass
    // and non-solid cross plants. Only a volume with no non-Air cell at all
    // can produce no faces.
    if !padded.has_any_render {
        return mesh;
    }

    // col_masks[axis][v][u] holds 64-bit class masks along d in 0..34.
    // For each axis (0=X, 1=Y, 2=Z):
    for axis in 0..3 {
        let mut opaque_masks = [[0u64; PADDED_SIZE]; PADDED_SIZE];
        let mut cutout_masks = [[0u64; PADDED_SIZE]; PADDED_SIZE];
        let mut blend_masks = [[0u64; PADDED_SIZE]; PADDED_SIZE];

        for (pv, mask_row) in opaque_masks.iter_mut().enumerate() {
            for (pu, opaque_cell) in mask_row.iter_mut().enumerate() {
                let mut opaque = 0u64;
                let mut cutout = 0u64;
                let mut blend = 0u64;
                for pd in 0..PADDED_SIZE {
                    let (px, py, pz) = get_axis_coord(axis, pd, pu, pv);
                    let block = padded.get(px, py, pz);
                    // A cube-class mask holds only solid cube blocks: water is
                    // its own pass, and cross/crop plants are swept later.
                    if !block.is_solid() {
                        continue;
                    }
                    let render = faces.render(block);
                    if render.model != BlockModel::Cube {
                        continue;
                    }
                    let bit = 1u64 << pd;
                    match render.alpha {
                        BlockAlpha::Opaque => opaque |= bit,
                        BlockAlpha::Cutout => cutout |= bit,
                        BlockAlpha::Blend => blend |= bit,
                    }
                }
                *opaque_cell = opaque;
                cutout_masks[pv][pu] = cutout;
                blend_masks[pv][pu] = blend;
            }
        }

        // Process positive (is_pos = true -> face_dir = axis*2) and negative
        // (is_pos = false -> face_dir = axis*2 + 1).
        for is_pos in [true, false] {
            let face_dir = axis * 2 + usize::from(!is_pos);

            // Opaque: every face culls against another opaque face.
            emit_class_faces(
                padded,
                faces,
                axis,
                is_pos,
                &opaque_masks,
                &opaque_masks,
                true,
                &mut mesh.buckets[face_dir],
            );
            // Cutout: a leaf face is only hidden by an opaque neighbour. Two
            // adjacent cutout cells keep their outward faces so alpha holes
            // never expose a culled interior.
            emit_class_faces(
                padded,
                faces,
                axis,
                is_pos,
                &cutout_masks,
                &opaque_masks,
                false,
                &mut mesh.buckets[face_dir],
            );
            // Blend: glass culls against glass and opaque, so a glass pane
            // pressed to stone loses only the face touching the wall.
            emit_class_faces(
                padded,
                faces,
                axis,
                is_pos,
                &blend_masks,
                &opaque_masks,
                true,
                &mut mesh.blend_quads,
            );
        }
    }

    mesh_cross(padded, faces, &mut mesh);
    mesh_water(padded, &mut mesh);

    mesh
}

/// Emit greedy-merged cube faces for one render class in one axis direction.
///
/// `class_mask` holds the cells whose class is being emitted; `occluders` is
/// the opaque mask, optionally OR-ed with `class_mask` itself when the class
/// culls against its own members. The AO/light sampling deliberately uses the
/// opaque mask only, so cutout leaves and blend glass never bake occlusion into
/// neighbouring opaque quads.
#[allow(clippy::too_many_arguments)]
fn emit_class_faces(
    padded: &PaddedChunk,
    faces: &BlockFaces,
    axis: usize,
    is_pos: bool,
    class_mask: &[[u64; PADDED_SIZE]; PADDED_SIZE],
    opaque_masks: &[[u64; PADDED_SIZE]; PADDED_SIZE],
    cull_with_class: bool,
    target: &mut Vec<PackedQuad>,
) {
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
            let class_col = class_mask[pv][pu];
            if class_col == 0 {
                continue;
            }
            let mut occluders = opaque_masks[pv][pu];
            if cull_with_class {
                occluders |= class_col;
            }
            // Bits 1..=32 shifted down to 0..31
            let mut visible_mask = if is_pos {
                ((class_col & !(occluders >> 1)) >> 1) as u32
            } else {
                ((class_col & !(occluders << 1)) >> 1) as u32
            };

            while visible_mask != 0 {
                let d = visible_mask.trailing_zeros() as usize;
                visible_mask &= visible_mask - 1;

                let pd = d + 1;
                let (px, py, pz) = get_axis_coord(axis, pd, pu, pv);
                let block = padded.get(px, py, pz);
                let tex_layer = faces.layer(block, face_dir);

                let air_d = if is_pos { pd + 1 } else { pd - 1 };
                let air_bit = 1u64 << air_d;

                // AO and corner light sample only opaque cells: cutout and
                // blend geometry must not darken a neighbouring opaque face.
                let s_um = (opaque_masks[pv][pu - 1] & air_bit) != 0;
                let s_up = (opaque_masks[pv][pu + 1] & air_bit) != 0;
                let s_vm = (opaque_masks[pv - 1][pu] & air_bit) != 0;
                let s_vp = (opaque_masks[pv + 1][pu] & air_bit) != 0;
                let c00 = (opaque_masks[pv - 1][pu - 1] & air_bit) != 0;
                let c10 = (opaque_masks[pv - 1][pu + 1] & air_bit) != 0;
                let c01 = (opaque_masks[pv + 1][pu - 1] & air_bit) != 0;
                let c11 = (opaque_masks[pv + 1][pu + 1] & air_bit) != 0;

                let ao00 = vertex_ao(s_um, s_vm, c00);
                let ao10 = vertex_ao(s_up, s_vm, c10);
                let ao01 = vertex_ao(s_um, s_vp, c01);
                let ao11 = vertex_ao(s_up, s_vp, c11);
                let ao_packed = ao00 | (ao10 << 2) | (ao01 << 4) | (ao11 << 6);

                // Corner light uses the same air-side neighbor geometry as AO:
                // the center air cell plus each corner's two edge neighbors and
                // diagonal, with opaque samples excluded from the average.
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

                let l00 = average_light([Some(center_light), light_um, light_vm, light_c00]);
                let l10 = average_light([Some(center_light), light_up, light_vm, light_c10]);
                let l01 = average_light([Some(center_light), light_um, light_vp, light_c01]);
                let l11 = average_light([Some(center_light), light_up, light_vp, light_c11]);
                let light_packed =
                    (l00 as u32) | ((l10 as u32) << 4) | ((l01 as u32) << 8) | ((l11 as u32) << 12);

                let key = (tex_layer as u32 + 1) | ((ao_packed as u32) << 8) | (light_packed << 16);
                keys_v_u[d] = key;
                slice_row_masks[d][v] |= 1u32 << u;
            }
        }
    }

    // Greedy 2D merge per slice d in 0..32
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
                target.push(PackedQuad::new(
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

/// Emit the crossed geometry for every `cross | crop` block.
///
/// Plants never enter a cube mask, so this 32x32x32 sweep is their only source
/// of geometry: exactly two unit quads (`face_dir` 6 and 7) per block, sampled
/// from the +X (`face = 0`) texture layer. Cross art is unshaded: every corner
/// gets full AO (3) and the cell's light in all four nibbles.
fn mesh_cross(padded: &PaddedChunk, faces: &BlockFaces, mesh: &mut ChunkMesh) {
    for py in 1..=CHUNK_SIZE {
        for pz in 1..=CHUNK_SIZE {
            for px in 1..=CHUNK_SIZE {
                let block = padded.get(px, py, pz);
                if !matches!(faces.model(block), BlockModel::Cross | BlockModel::Crop) {
                    continue;
                }
                // Crop stages select their art through the side/all (+X) face.
                let tex_layer = faces.layer(block, 0);
                let light = padded.light_at(px, py, pz) as u16;
                let light_packed = light | (light << 4) | (light << 8) | (light << 12);
                let pos = [(px - 1) as u32, (py - 1) as u32, (pz - 1) as u32];
                mesh.cross_quads.push(PackedQuad::new(
                    pos,
                    [1, 1],
                    6,
                    tex_layer,
                    0xFF,
                    light_packed,
                ));
                mesh.cross_quads.push(PackedQuad::new(
                    pos,
                    [1, 1],
                    7,
                    tex_layer,
                    0xFF,
                    light_packed,
                ));
            }
        }
    }
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
                for (v, row) in visible.iter_mut().enumerate() {
                    for (u, cell) in row.iter_mut().enumerate() {
                        let (px, py, pz) = get_axis_coord(axis, d + 1, u + 1, v + 1);
                        if !padded.get(px, py, pz).is_liquid() {
                            continue;
                        }
                        let air_d = if is_pos { d + 2 } else { d };
                        let (ax, ay, az) = get_axis_coord(axis, air_d, u + 1, v + 1);
                        if padded.get(ax, ay, az) == BlockId::Air {
                            *cell = true;
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
                            if !visible[v + h][u..(u + w)].iter().all(|&cell| cell) {
                                break 'height;
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
        use crate::world::season::Season;
        use crate::world::terrain::{generate_chunk_seeded, SEA_LEVEL};

        const SEED: u32 = 1337;
        let mut found = None;
        'scan: for cx in -4..=4 {
            for cz in -4..=4 {
                let pos = ChunkPos::new(cx, 0, cz);
                let chunk = generate_chunk_seeded(pos, SEED, Season::Summer);
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

    #[test]
    fn padded_chunk_stores_raw_u16_and_round_trips() {
        let mut padded = PaddedChunk::new_empty();
        assert_eq!(padded.get(1, 1, 1), BlockId::Air);
        padded.set(
            1,
            1,
            1,
            BlockId::Modded(crate::world::block::BUILTIN_BLOCK_COUNT),
        );
        assert_eq!(
            padded.get(1, 1, 1),
            BlockId::Modded(crate::world::block::BUILTIN_BLOCK_COUNT)
        );
        assert!(padded.has_any_solid, "modded solid id keeps the solid flag");
        assert!(!padded.has_any_liquid);
        assert_eq!(std::mem::size_of_val(&*padded.voxels), PADDED_VOLUME * 2);

        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(
            3,
            4,
            5,
            BlockId::Modded(crate::world::block::BUILTIN_BLOCK_COUNT),
        );
        let copied = PaddedChunk::from_center_only(&chunk);
        assert_eq!(
            copied.get(4, 5, 6),
            BlockId::Modded(crate::world::block::BUILTIN_BLOCK_COUNT)
        );
        assert!(copied.has_any_solid);
    }

    /// A face table built from a one-block pack stack: `mymod:ruby_block`
    /// occupies raw id [`BUILTIN_BLOCK_COUNT`] and every face is layer 48.
    fn pack_faces() -> crate::content::faces::BlockFaces {
        use crate::content::spec::{BlockAlpha, BlockEntry, BlockModel, BlockTextures, PackAsset};
        use crate::content::{build_registry, ContentRegistry, PackContentSpec, PackIdentity};

        let mut spec = PackContentSpec::default();
        spec.assets.insert(
            "ruby.png".to_string(),
            PackAsset {
                rgba: vec![0x5A; 1024],
                source_side: 16,
            },
        );
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_block".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ruby.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.5,
            drop: None,
            tool: None,
            stack_limit: 64,

            model: BlockModel::Cube,
            alpha: BlockAlpha::Opaque,
        });
        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "mymod".to_string(),
                    version: None,
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        crate::content::faces::BlockFaces::from_registry(&registry)
    }

    #[test]
    fn modded_block_meshes_at_its_authored_pack_layer() {
        use crate::content::PACK_LAYER_BASE;
        use crate::world::block::BUILTIN_BLOCK_COUNT;

        let faces = pack_faces();
        let modded = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, modded);
        let padded = PaddedChunk::from_center_only(&chunk);

        let mesh = mesh_chunk_with(&padded, &faces);
        assert_eq!(mesh.total_quads(), 6, "one isolated cell emits six faces");
        for (dir, bucket) in mesh.buckets.iter().enumerate() {
            assert_eq!(bucket.len(), 1, "bucket {dir} must hold one quad");
            assert_eq!(
                bucket[0].unpack().6,
                PACK_LAYER_BASE,
                "bucket {dir} must sample the authored pack layer"
            );
        }
        assert_eq!(mesh.water_quad_count(), 0);

        // The built-in wrapper keeps the pre-M010 placeholder policy: a modded
        // id has no built-in table entry and falls back to the checker layer.
        let builtin_mesh = mesh_chunk(&padded);
        assert_eq!(builtin_mesh.total_quads(), 6);
        for bucket in &builtin_mesh.buckets {
            assert_eq!(
                bucket[0].unpack().6,
                crate::content::PLACEHOLDER_BLOCK_LAYER
            );
        }
    }

    #[test]
    fn builtin_wrapper_matches_mesh_chunk_with_builtin_faces() {
        let chunk = Chunk::new_uniform(BlockId::OakLog);
        let padded = PaddedChunk::from_center_only(&chunk);
        let wrapper = mesh_chunk(&padded);
        let explicit = mesh_chunk_with(&padded, &BlockFaces::builtin());
        for dir in 0..6 {
            assert_eq!(wrapper.buckets[dir], explicit.buckets[dir], "dir {dir}");
        }
        assert_eq!(wrapper.water_quads, explicit.water_quads);
    }

    #[test]
    fn wheat_chunk_emits_two_cross_quads_and_no_cube_faces() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::WheatStage3);
        let padded = PaddedChunk::from_center_only(&chunk);
        assert!(padded.has_any_render, "the plant cell is renderable");
        assert!(!padded.has_any_solid, "a plant is not solid");
        assert!(!padded.has_any_liquid);

        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 0, "plants never emit cube faces");
        assert_eq!(mesh.blend_quad_count(), 0);
        assert_eq!(mesh.cross_quad_count(), 2);
        let mut dirs: Vec<u32> = mesh.cross_quads.iter().map(|q| q.unpack().5).collect();
        dirs.sort_unstable();
        assert_eq!(dirs, vec![6, 7], "exactly the two crossed planes");
        for quad in &mesh.cross_quads {
            let (x, y, z, w, h, _, layer, ao, light) = quad.unpack();
            assert_eq!((x, y, z, w, h), (5, 6, 7, 1, 1));
            assert_eq!(layer, BlockId::WheatStage3.face_tex_layer(0));
            assert_eq!(ao, 0xFF, "cross art is unshaded: every corner AO = 3");
            assert_eq!(light, 0xFFFF, "the cell's light fills all four nibbles");
        }
    }

    #[test]
    fn uniform_wheat_chunk_fills_its_voxels_instead_of_returning_empty() {
        let padded = PaddedChunk::from_center_only(&Chunk::new_uniform(BlockId::WheatStage1));
        assert!(padded.has_any_render, "a uniform plant chunk is renderable");
        assert!(!padded.has_any_solid && !padded.has_any_liquid);
        assert_eq!(padded.get(1, 1, 1), BlockId::WheatStage1);
        assert_eq!(
            padded.get(CHUNK_SIZE, CHUNK_SIZE, CHUNK_SIZE),
            BlockId::WheatStage1
        );

        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 0);
        assert_eq!(
            mesh.cross_quad_count(),
            2 * CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE,
            "every cell emits its two crossed planes"
        );
    }

    #[test]
    fn single_cutout_leaves_block_emits_six_cutout_faces_in_buckets() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::OakLeaves);
        let padded = PaddedChunk::from_center_only(&chunk);
        assert!(padded.has_any_render && padded.has_any_solid);

        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 6, "cutout shares the opaque draw");
        assert_eq!(mesh.blend_quad_count(), 0);
        assert_eq!(mesh.cross_quad_count(), 0);
        for (dir, bucket) in mesh.buckets.iter().enumerate() {
            assert_eq!(bucket.len(), 1, "bucket {dir} must hold the leaf face");
            assert_eq!(
                bucket[0].unpack().6,
                BlockId::OakLeaves.face_tex_layer(dir as u8)
            );
        }
    }

    #[test]
    fn single_glass_block_emits_only_blend_faces() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Glass);
        let padded = PaddedChunk::from_center_only(&chunk);
        assert!(padded.has_any_render, "a glass-only chunk is renderable");
        assert!(padded.has_any_solid, "glass still collides");

        let mesh = mesh_chunk(&padded);
        assert_eq!(
            mesh.total_quads(),
            0,
            "glass never enters the opaque upload"
        );
        assert_eq!(mesh.cross_quad_count(), 0);
        assert_eq!(mesh.blend_quad_count(), 6);
        let mut dirs: Vec<u32> = mesh.blend_quads.iter().map(|q| q.unpack().5).collect();
        dirs.sort_unstable();
        assert_eq!(dirs, vec![0, 1, 2, 3, 4, 5]);
        for quad in &mesh.blend_quads {
            assert_eq!(quad.unpack().6, BlockId::Glass.face_tex_layer(0));
        }
    }

    #[test]
    fn glass_against_stone_keeps_the_stone_face_and_culls_the_touching_glass_face() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Glass);
        chunk.set(6, 6, 7, BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);

        let mesh = mesh_chunk(&padded);
        // Stone culls only against opaque neighbours, so all six of its faces
        // survive; the -X one looks through the glass.
        assert_eq!(mesh.total_quads(), 6);
        assert_eq!(mesh.buckets[1].len(), 1, "the stone's -X face survives");
        // Glass culls against the opaque wall: its +X face is hidden.
        assert_eq!(mesh.blend_quad_count(), 5);
        assert!(
            mesh.blend_quads.iter().all(|q| q.unpack().5 != 0),
            "the glass face pressed to stone must be culled"
        );
    }

    #[test]
    fn glass_against_glass_culls_internal_faces() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Glass);
        chunk.set(6, 6, 7, BlockId::Glass);
        let padded = PaddedChunk::from_center_only(&chunk);

        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 0);
        // Two adjacent cells expose 5 faces each. The four side directions
        // greedy-merge the pair into one 2x1 quad each, leaving 6 quads that
        // cover 10 cube faces (not the 12 an unculled pair would emit).
        assert_eq!(mesh.blend_quad_count(), 6);
        let area: u32 = mesh
            .blend_quads
            .iter()
            .map(|q| {
                let (_, _, _, w, h, _, _, _, _) = q.unpack();
                w * h
            })
            .sum();
        assert_eq!(area, 10, "glass-glass internal faces stay invisible");
        assert!(
            !mesh
                .blend_quads
                .iter()
                .any(|q| matches!(q.unpack(), (5, _, _, _, _, 0, _, _, _))),
            "no +X quad at the internal x=5 boundary"
        );
        assert!(
            !mesh
                .blend_quads
                .iter()
                .any(|q| matches!(q.unpack(), (6, _, _, _, _, 1, _, _, _))),
            "no -X quad at the internal x=6 boundary"
        );
    }

    /// FNV-1a 64 over every mesh quad stream in a fixed order: bucket count,
    /// each bucket's length, every opaque/cutout quad word (dir 0..6), then
    /// the water count and its words, then the blend count and its words, then
    /// the cross count and its words. Folding every class keeps the pin honest:
    /// a change to glass destinations or plant geometry fails here.
    fn fnv1a64_mesh_baseline(mesh: &ChunkMesh) -> ([usize; 6], usize, usize, usize, usize, u64) {
        fn feed(hash: &mut u64, bytes: &[u8]) {
            for &byte in bytes {
                *hash ^= byte as u64;
                *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }

        fn feed_quads(hash: &mut u64, quads: &[PackedQuad]) -> usize {
            feed(hash, &(quads.len() as u64).to_le_bytes());
            for quad in quads {
                feed(hash, &quad.0[0].to_le_bytes());
                feed(hash, &quad.0[1].to_le_bytes());
            }
            quads.len()
        }

        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let bucket_counts = [0, 1, 2, 3, 4, 5].map(|dir| mesh.buckets[dir].len());
        feed(&mut hash, &(mesh.buckets.len() as u64).to_le_bytes());
        for count in bucket_counts {
            feed(&mut hash, &(count as u64).to_le_bytes());
        }

        let mut opaque_count = 0usize;
        for bucket in &mesh.buckets {
            for quad in bucket {
                feed(&mut hash, &quad.0[0].to_le_bytes());
                feed(&mut hash, &quad.0[1].to_le_bytes());
                opaque_count += 1;
            }
        }

        let water_count = feed_quads(&mut hash, &mesh.water_quads);
        let blend_count = feed_quads(&mut hash, &mesh.blend_quads);
        let cross_count = feed_quads(&mut hash, &mesh.cross_quads);

        (
            bucket_counts,
            opaque_count,
            water_count,
            blend_count,
            cross_count,
            hash,
        )
    }

    #[test]
    fn pinned_greedy_mesh_matches_pre_m010_baseline() {
        use crate::world::chunk::ChunkPos;
        use crate::world::season::Season;
        use crate::world::terrain::{generate_chunk_seeded, DEFAULT_WORLD_SEED};

        // Case A: a real generated chunk pushed through the real halo/mesher.
        let generated =
            generate_chunk_seeded(ChunkPos::new(0, -1, 0), DEFAULT_WORLD_SEED, Season::Summer);
        let mesh_a = mesh_chunk(&PaddedChunk::from_center_only(&generated));

        // Case B: a synthetic chunk with one isolated cell per interesting
        // block family (opaque, log faces, liquid, wheat, brick).
        let mut synthetic = Chunk::new_uniform(BlockId::Air);
        synthetic.set(5, 6, 7, BlockId::Stone);
        synthetic.set(8, 9, 10, BlockId::Grass);
        synthetic.set(12, 13, 14, BlockId::OakLog);
        synthetic.set(15, 16, 17, BlockId::Water);
        synthetic.set(20, 21, 22, BlockId::WheatStage3);
        synthetic.set(25, 26, 27, BlockId::Brick);
        let mesh_b = mesh_chunk(&PaddedChunk::from_center_only(&synthetic));

        // Captured from the pre-M010 mesher and re-pinned once per documented
        // geometry change. Any greedy-merge, AO/light, quad packing or
        // water/blend/cross-emission drift fails here.
        //
        // S03-T02 re-pin: wheat became a non-solid `BlockKind::Plant`, so the
        // synthetic wheat cell stopped emitting cube faces.
        // S03-T04 re-pin: wheat is now `model = crop`, so the synthetic cell
        // emits two cross quads and no cubes; the hash also folds the blend and
        // cross buckets, which are empty in both cases except for that pair.
        let expected: [([usize; 6], usize, usize, usize, usize, u64); 2] = [
            (
                [370, 482, 402, 471, 274, 471],
                2_470,
                0,
                0,
                0,
                9_147_626_324_952_005_047,
            ),
            ([4, 4, 4, 4, 4, 4], 24, 6, 0, 2, 11_984_414_958_114_010_917),
        ];
        let actual = [
            fnv1a64_mesh_baseline(&mesh_a),
            fnv1a64_mesh_baseline(&mesh_b),
        ];
        assert_eq!(actual, expected, "pre-M010 greedy-mesh baseline drifted");
    }
}
