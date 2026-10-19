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
    let base = 18.0 + continental * 26.0 + hills * 14.0 + detail * 4.0 + mountain_factor * ridge * 48.0;
    base.round() as i32
}

pub fn generate_chunk(pos: ChunkPos) -> Chunk {
    generate_chunk_seeded(pos, DEFAULT_WORLD_SEED)
}

pub fn generate_chunk_seeded(pos: ChunkPos, seed: u32) -> Chunk {
    let base_wx = pos.x * CHUNK_SIZE as i32;
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let base_wz = pos.z * CHUNK_SIZE as i32;
    let top_wy = base_wy + CHUNK_SIZE as i32 - 1;

    // Fast path for high sky chunks
    if base_wy > 105 {
        return Chunk::new_uniform(BlockId::Air);
    }

    // Precompute column heights in 32x32
    let mut heights = [[0i32; CHUNK_SIZE]; CHUNK_SIZE];
    let mut min_h = i32::MAX;
    let mut max_h = i32::MIN;
    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let h = terrain_height_at_seeded(base_wx + lx as i32, base_wz + lz as i32, seed);
            heights[lz][lx] = h;
            min_h = min_h.min(h);
            max_h = max_h.max(h);
        }
    }

    // If the entire chunk is well above max terrain height + max tree height (7)
    if base_wy > max_h + 7 {
        return Chunk::new_uniform(BlockId::Air);
    }
    // If the entire chunk is deep underground (above bedrock -32, below stone layer)
    if base_wy > -32 && top_wy < min_h - 4 {
        return Chunk::new_uniform(BlockId::Stone);
    }

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
