use crate::world::block::BlockId;
use crate::world::chunk::{Chunk, ChunkPos, CHUNK_SIZE};

#[inline]
fn hash2d(x: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B9)
        ^ (z as u32).wrapping_mul(0x85EB_CA6B)
        ^ seed.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

#[inline]
fn hash3d(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B9)
        ^ (y as u32).wrapping_mul(0x85EB_CA6B)
        ^ (z as u32).wrapping_mul(0xC2B2_AE35)
        ^ seed.wrapping_mul(0x27D4_EB2D);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

#[inline]
fn smooth01(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn value_noise_3d(x: f32, y: f32, z: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let zi = z.floor() as i32;
    let xf = x - xi as f32;
    let yf = y - yi as f32;
    let zf = z - zi as f32;

    let u = smooth01(xf);
    let v = smooth01(yf);
    let w = smooth01(zf);

    let n000 = (hash3d(xi, yi, zi, seed) & 0xFFFF) as f32 / 65535.0;
    let n100 = (hash3d(xi + 1, yi, zi, seed) & 0xFFFF) as f32 / 65535.0;
    let n010 = (hash3d(xi, yi + 1, zi, seed) & 0xFFFF) as f32 / 65535.0;
    let n110 = (hash3d(xi + 1, yi + 1, zi, seed) & 0xFFFF) as f32 / 65535.0;
    let n001 = (hash3d(xi, yi, zi + 1, seed) & 0xFFFF) as f32 / 65535.0;
    let n101 = (hash3d(xi + 1, yi, zi + 1, seed) & 0xFFFF) as f32 / 65535.0;
    let n011 = (hash3d(xi, yi + 1, zi + 1, seed) & 0xFFFF) as f32 / 65535.0;
    let n111 = (hash3d(xi + 1, yi + 1, zi + 1, seed) & 0xFFFF) as f32 / 65535.0;

    let nx00 = n000 + u * (n100 - n000);
    let nx10 = n010 + u * (n110 - n010);
    let nx01 = n001 + u * (n101 - n001);
    let nx11 = n011 + u * (n111 - n011);
    let nxy0 = nx00 + v * (nx10 - nx00);
    let nxy1 = nx01 + v * (nx11 - nx01);
    nxy0 + w * (nxy1 - nxy0)
}

#[inline]
fn value_noise_2d(x: f32, z: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let zi = z.floor() as i32;
    let xf = x - xi as f32;
    let zf = z - zi as f32;

    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = zf * zf * (3.0 - 2.0 * zf);

    let n00 = (hash2d(xi, zi, seed) & 0xFFFF) as f32 / 65535.0;
    let n10 = (hash2d(xi + 1, zi, seed) & 0xFFFF) as f32 / 65535.0;
    let n01 = (hash2d(xi, zi + 1, seed) & 0xFFFF) as f32 / 65535.0;
    let n11 = (hash2d(xi + 1, zi + 1, seed) & 0xFFFF) as f32 / 65535.0;

    let nx0 = n00 + u * (n10 - n00);
    let nx1 = n01 + u * (n11 - n01);
    nx0 + v * (nx1 - nx0)
}

pub const DEFAULT_WORLD_SEED: u32 = 1337;

/// World-aligned cave lattice spacing in blocks. Sampling the density on a
/// 4-block grid (9 corners per 32-block chunk axis) keeps cave generation
/// linear in chunk volume while interpolation stays shared across chunk
/// borders because chunk bases are multiples of 32.
pub const CAVE_LATTICE: usize = 4;
pub const CAVE_THRESHOLD: f32 = 0.66;

const CAVE_CORNERS: usize = CHUNK_SIZE / CAVE_LATTICE + 1;

#[inline]
fn cave_density(wx: i32, wy: i32, wz: i32, seed: u32) -> f32 {
    let x = wx as f32;
    let y = wy as f32;
    let z = wz as f32;
    0.6 * value_noise_3d(x * 0.035, y * 0.05, z * 0.035, seed ^ 0x000A_11CE)
        + 0.4 * value_noise_3d(x * 0.090, y * 0.11, z * 0.090, seed ^ 0x0000_0B0B)
}

/// Per-chunk 3D cave density sampled on the world-aligned 4-block lattice.
/// Local to the rayon worker that generates the chunk: no shared state.
pub struct CaveField {
    densities: [[[f32; CAVE_CORNERS]; CAVE_CORNERS]; CAVE_CORNERS],
    max_density: f32,
}

impl CaveField {
    fn new(pos: ChunkPos, seed: u32) -> Self {
        let base_wx = pos.x * CHUNK_SIZE as i32;
        let base_wy = pos.y * CHUNK_SIZE as i32;
        let base_wz = pos.z * CHUNK_SIZE as i32;
        let mut densities = [[[0.0f32; CAVE_CORNERS]; CAVE_CORNERS]; CAVE_CORNERS];
        let mut max_density = f32::MIN;
        for (kz, z_corners) in densities.iter_mut().enumerate() {
            let wz = base_wz + (kz * CAVE_LATTICE) as i32;
            for (ky, y_corners) in z_corners.iter_mut().enumerate() {
                let wy = base_wy + (ky * CAVE_LATTICE) as i32;
                for (kx, density) in y_corners.iter_mut().enumerate() {
                    let wx = base_wx + (kx * CAVE_LATTICE) as i32;
                    let d = cave_density(wx, wy, wz, seed);
                    *density = d;
                    max_density = max_density.max(d);
                }
            }
        }
        Self {
            densities,
            max_density,
        }
    }

    /// Fast path: interpolation cannot exceed the largest corner sample, so a
    /// chunk with no corner above the threshold cannot contain a cave.
    #[inline]
    fn may_carve(&self) -> bool {
        self.max_density > CAVE_THRESHOLD
    }

    #[inline]
    fn is_carved(&self, lx: usize, ly: usize, lz: usize) -> bool {
        let fx = lx as f32 / CAVE_LATTICE as f32;
        let fy = ly as f32 / CAVE_LATTICE as f32;
        let fz = lz as f32 / CAVE_LATTICE as f32;
        let ix = (fx.floor() as usize).min(CAVE_CORNERS - 2);
        let iy = (fy.floor() as usize).min(CAVE_CORNERS - 2);
        let iz = (fz.floor() as usize).min(CAVE_CORNERS - 2);
        let tx = fx - ix as f32;
        let ty = fy - iy as f32;
        let tz = fz - iz as f32;

        let c000 = self.densities[iz][iy][ix];
        let c100 = self.densities[iz][iy][ix + 1];
        let c010 = self.densities[iz][iy + 1][ix];
        let c110 = self.densities[iz][iy + 1][ix + 1];
        let c001 = self.densities[iz + 1][iy][ix];
        let c101 = self.densities[iz + 1][iy][ix + 1];
        let c011 = self.densities[iz + 1][iy + 1][ix];
        let c111 = self.densities[iz + 1][iy + 1][ix + 1];

        let cx00 = c000 + tx * (c100 - c000);
        let cx10 = c010 + tx * (c110 - c010);
        let cx01 = c001 + tx * (c101 - c001);
        let cx11 = c011 + tx * (c111 - c011);
        let cxy0 = cx00 + ty * (cx10 - cx00);
        let cxy1 = cx01 + ty * (cx11 - cx01);
        (cxy0 + tz * (cxy1 - cxy0)) > CAVE_THRESHOLD
    }
}

/// A cave may only carve Stone that is strictly inside the stone band.
/// `surface` is the column's terrain height: keeping the top three blocks
/// solid stops caves from punching holes in Stone mountain surfaces.
#[inline]
fn is_cave_carvable(block: BlockId, wy: i32, surface: i32) -> bool {
    block == BlockId::Stone && wy > -32 && wy < surface - 3
}

#[inline]
pub fn terrain_height_at(wx: i32, wz: i32) -> i32 {
    terrain_height_at_seeded(wx, wz, DEFAULT_WORLD_SEED)
}

#[inline]
pub fn terrain_height_at_seeded(wx: i32, wz: i32, seed: u32) -> i32 {
    let x = wx as f32;
    let z = wz as f32;
    let s = seed.wrapping_mul(0x9E37_79B9);
    let continental = value_noise_2d(x * 0.004, z * 0.004, 101 ^ s);
    let hills = value_noise_2d(x * 0.015, z * 0.015, 202 ^ s);
    let detail = value_noise_2d(x * 0.05, z * 0.05, 303 ^ s);
    let ridge = (value_noise_2d(x * 0.008, z * 0.008, 404 ^ s) * 2.0 - 1.0).abs();

    let mountain_factor = (continental - 0.55).max(0.0) * 2.2;
    let base =
        18.0 + continental * 26.0 + hills * 14.0 + detail * 4.0 + mountain_factor * ridge * 48.0;
    base.round() as i32
}

pub fn generate_chunk(pos: ChunkPos) -> Chunk {
    generate_chunk_seeded(pos, DEFAULT_WORLD_SEED)
}

pub fn generate_chunk_seeded(pos: ChunkPos, seed: u32) -> Chunk {
    let base_wx = pos.x * CHUNK_SIZE as i32;
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let base_wz = pos.z * CHUNK_SIZE as i32;

    // Fast path for high sky chunks
    if base_wy > 105 {
        return Chunk::new_uniform(BlockId::Air);
    }

    // Precompute column heights in 32x32
    let mut heights = [[0i32; CHUNK_SIZE]; CHUNK_SIZE];
    let mut max_h = i32::MIN;
    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let h = terrain_height_at_seeded(base_wx + lx as i32, base_wz + lz as i32, seed);
            heights[lz][lx] = h;
            max_h = max_h.max(h);
        }
    }

    // If the entire chunk is well above max terrain height + max tree height (7)
    if base_wy > max_h + 7 {
        return Chunk::new_uniform(BlockId::Air);
    }
    // Deep chunks must still flow through the block loop so caves (and,
    // later, ores) can carve them; no uniform-stone fast path here.
    let cave_field = CaveField::new(pos, seed);

    let mut chunk = Chunk::new_uniform(BlockId::Air);
    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let wx = base_wx + lx as i32;
            let wz = base_wz + lz as i32;
            let h = heights[lz][lx];

            for ly in 0..CHUNK_SIZE {
                let wy = base_wy + ly as i32;
                let block = if wy <= -32 {
                    BlockId::Bedrock
                } else if wy < h - 3 {
                    BlockId::Stone
                } else if wy < h {
                    if h <= 22 {
                        BlockId::Sand
                    } else {
                        BlockId::Dirt
                    }
                } else if wy == h {
                    if h <= 22 {
                        BlockId::Sand
                    } else if h >= 68 {
                        BlockId::Snow
                    } else if h >= 58 {
                        BlockId::Stone
                    } else {
                        BlockId::Grass
                    }
                } else {
                    BlockId::Air
                };

                if is_cave_carvable(block, wy, h)
                    && cave_field.may_carve()
                    && cave_field.is_carved(lx, ly, lz)
                {
                    continue;
                }

                if block != BlockId::Air {
                    chunk.set(lx, ly, lz, block);
                }
            }

            // Place deterministic oak trees inside safe interior (lx, lz in 3..29) on grass
            if (3..29).contains(&lx) && (3..29).contains(&lz) && (24..54).contains(&h) {
                let tree_hash = hash2d(wx, wz, 777 ^ seed);
                if (tree_hash & 0xFF) == 0 {
                    let trunk_h = 4 + ((tree_hash >> 8) & 1) as i32;
                    for ty in (h + 1)..=(h + trunk_h) {
                        let ly_tree = ty - base_wy;
                        if (0..CHUNK_SIZE as i32).contains(&ly_tree) {
                            chunk.set(lx, ly_tree as usize, lz, BlockId::OakLog);
                        }
                    }
                    let canopy_base = h + trunk_h - 1;
                    let canopy_top = h + trunk_h + 1;
                    for cy in canopy_base..=canopy_top {
                        let ly_c = cy - base_wy;
                        if !(0..CHUNK_SIZE as i32).contains(&ly_c) {
                            continue;
                        }
                        let radius: i32 = if cy == canopy_top { 1 } else { 2 };
                        for dz in -radius..=radius {
                            for dx in -radius..=radius {
                                if dx == 0 && dz == 0 && cy <= h + trunk_h {
                                    continue;
                                }
                                if dx.abs() == 2 && dz.abs() == 2 && cy == canopy_base {
                                    continue;
                                }
                                let tlx = (lx as i32 + dx) as usize;
                                let tlz = (lz as i32 + dz) as usize;
                                if chunk.get(tlx, ly_c as usize, tlz) == BlockId::Air {
                                    chunk.set(tlx, ly_c as usize, tlz, BlockId::OakLeaves);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    chunk.try_compress();
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    #[test]
    fn test_cave_generation_deterministic_full_chunks() {
        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            for cy in -1..=1 {
                let pos = ChunkPos::new(0, cy, 0);
                let first = generate_chunk_seeded(pos, seed);
                let second = generate_chunk_seeded(pos, seed);
                for lz in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            assert_eq!(
                                first.get(lx, ly, lz),
                                second.get(lx, ly, lz),
                                "seed {seed} chunk {pos:?} differs at ({lx}, {ly}, {lz})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_caves_exist_as_air_below_stone_roof() {
        let seed = DEFAULT_WORLD_SEED;
        let mut carved = 0usize;
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in -1..=1 {
                    let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed);
                    let base_wx = cx * CHUNK_SIZE as i32;
                    let base_wy = cy * CHUNK_SIZE as i32;
                    let base_wz = cz * CHUNK_SIZE as i32;
                    for lz in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let h = terrain_height_at_seeded(
                                base_wx + lx as i32,
                                base_wz + lz as i32,
                                seed,
                            );
                            for ly in 0..CHUNK_SIZE {
                                let wy = base_wy + ly as i32;
                                if wy > -32 && wy < h - 3 && chunk.get(lx, ly, lz) == BlockId::Air {
                                    carved += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            carved > 0,
            "expected carved cave Air strictly below h-3, found none"
        );
    }

    #[test]
    fn test_mountain_top_stone_surface_not_carved() {
        let seed = DEFAULT_WORLD_SEED;
        // Find a genuine high-terrain column so this check is never vacuous.
        let mut highest: Option<(i32, i32, i32)> = None;
        for wz in (-1024..=1024).step_by(16) {
            for wx in (-1024..=1024).step_by(16) {
                let h = terrain_height_at_seeded(wx, wz, seed);
                if highest.is_none_or(|(_, _, best)| h > best) {
                    highest = Some((wx, wz, h));
                }
            }
        }
        let (wx, wz, h) = highest.expect("height search must yield a column");
        assert!(
            h >= 58,
            "height search did not reach mountain terrain (max h = {h})"
        );

        let pos = ChunkPos::new(
            wx.div_euclid(CHUNK_SIZE as i32),
            h.div_euclid(CHUNK_SIZE as i32),
            wz.div_euclid(CHUNK_SIZE as i32),
        );
        let chunk = generate_chunk_seeded(pos, seed);
        let lx = wx.rem_euclid(CHUNK_SIZE as i32) as usize;
        let ly = h.rem_euclid(CHUNK_SIZE as i32) as usize;
        let lz = wz.rem_euclid(CHUNK_SIZE as i32) as usize;
        assert_ne!(
            chunk.get(lx, ly, lz),
            BlockId::Air,
            "cave punched a hole in the mountain surface at ({wx}, {h}, {wz})"
        );
    }

    #[test]
    fn test_bedrock_floor_preserved_in_deep_chunk() {
        let chunk = generate_chunk_seeded(ChunkPos::new(0, -1, 0), DEFAULT_WORLD_SEED);
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                assert_eq!(
                    chunk.get(lx, 0, lz),
                    BlockId::Bedrock,
                    "wy == -32 must stay Bedrock at ({lx}, {lz})"
                );
            }
        }
    }

    #[test]
    fn test_surface_skin_never_carved() {
        let seed = DEFAULT_WORLD_SEED;
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in 0..=1 {
                    let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed);
                    let base_wx = cx * CHUNK_SIZE as i32;
                    let base_wy = cy * CHUNK_SIZE as i32;
                    let base_wz = cz * CHUNK_SIZE as i32;
                    let top_wy = base_wy + CHUNK_SIZE as i32 - 1;
                    for lz in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let h = terrain_height_at_seeded(
                                base_wx + lx as i32,
                                base_wz + lz as i32,
                                seed,
                            );
                            if !(base_wy..=top_wy).contains(&h) {
                                continue;
                            }
                            let from = (h - 3).max(base_wy);
                            let to = h.min(top_wy);
                            for wy in from..=to {
                                let ly = (wy - base_wy) as usize;
                                assert_ne!(
                                    chunk.get(lx, ly, lz),
                                    BlockId::Air,
                                    "stone-band column at world ({}, {wy}, {}) was carved",
                                    base_wx + lx as i32,
                                    base_wz + lz as i32
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_is_cave_carvable_guards_surface_and_bedrock() {
        let h = 40;
        assert!(!is_cave_carvable(BlockId::Stone, h, h));
        assert!(!is_cave_carvable(BlockId::Stone, h - 1, h));
        assert!(!is_cave_carvable(BlockId::Stone, h - 2, h));
        assert!(!is_cave_carvable(BlockId::Stone, h - 3, h));
        assert!(is_cave_carvable(BlockId::Stone, h - 4, h));
        assert!(is_cave_carvable(BlockId::Stone, 0, h));
        assert!(!is_cave_carvable(BlockId::Stone, -32, h));
        assert!(!is_cave_carvable(BlockId::Bedrock, h - 4, h));
        assert!(!is_cave_carvable(BlockId::Air, h - 4, h));
        assert!(!is_cave_carvable(BlockId::Dirt, h - 4, h));
    }
    #[test]
    fn test_terrain_generation_deterministic_and_layered() {
        let c1 = generate_chunk(ChunkPos::new(0, 0, 0));
        let c2 = generate_chunk(ChunkPos::new(0, 0, 0));
        assert_eq!(c1.get(10, 5, 10), c2.get(10, 5, 10));

        let bottom = generate_chunk(ChunkPos::new(0, -1, 0));
        assert_eq!(bottom.get(0, 0, 0), BlockId::Bedrock);

        let sky = generate_chunk(ChunkPos::new(0, 4, 0));
        assert_eq!(sky.is_uniform(), Some(BlockId::Air));
    }
}
