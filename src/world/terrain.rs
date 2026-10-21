use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, local_coords_from_index, Chunk, ChunkPos, CHUNK_SIZE};

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

/// A block is strictly inside the stone band when it is above the bedrock
/// floor and at least four blocks below the column surface. The top three
/// surface blocks stay solid so caves and ores never punch holes in Stone
/// mountain surfaces.
#[inline]
fn is_inside_stone_band(wy: i32, surface: i32) -> bool {
    wy > -32 && wy < surface - 3
}

/// A cave may only carve Stone that is strictly inside the stone band.
#[inline]
fn is_cave_carvable(block: BlockId, wy: i32, surface: i32) -> bool {
    block == BlockId::Stone && is_inside_stone_band(wy, surface)
}

/// Deterministic per-chunk xorshift32 stream. No wall-clock or global state,
/// so the same chunk position and seed always produce the same ore layout.
struct Xorshift32(u32);

impl Xorshift32 {
    #[inline]
    fn new(seed: u32) -> Self {
        // xorshift32 locks up at zero, so force a non-zero state.
        Self(seed | 1)
    }

    #[inline]
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

const NEIGHBOR_OFFSETS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

const COAL_DEPTH_BAND: (i32, i32) = (4, 36);
const IRON_DEPTH_BAND: (i32, i32) = (12, 90);

#[inline]
fn ore_depth_band(ore: BlockId) -> Option<(i32, i32)> {
    match ore {
        BlockId::CoalOre => Some(COAL_DEPTH_BAND),
        BlockId::IronOre => Some(IRON_DEPTH_BAND),
        _ => None,
    }
}

/// Every ore replacement must satisfy all of these guards: the target is
/// still Stone, it is strictly inside the stone band, and its depth from the
/// column surface is inside the ore's band. Re-checking per growth cell keeps
/// veins from climbing into the surface skin or drifting out of their band.
#[inline]
fn can_place_ore(block: BlockId, wy: i32, surface: i32, ore: BlockId) -> bool {
    if block != BlockId::Stone || !is_inside_stone_band(wy, surface) {
        return false;
    }
    match ore_depth_band(ore) {
        Some((min_depth, max_depth)) => {
            let depth = surface - wy;
            (min_depth..=max_depth).contains(&depth)
        }
        None => false,
    }
}

/// Stone cells with at least one in-chunk Air neighbor that is itself below
/// the neighbor column's surface skin. These are cave walls and underground
/// cliffs, not open sky; veins started from this list are visible when a
/// player digs into a cave.
fn exposed_stone_cells(
    chunk: &Chunk,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    pos: ChunkPos,
) -> Vec<u16> {
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let max_local = CHUNK_SIZE as i32 - 1;
    let mut exposed = Vec::new();

    for lz in 0..CHUNK_SIZE {
        for ly in 0..CHUNK_SIZE {
            let wy = base_wy + ly as i32;
            for lx in 0..CHUNK_SIZE {
                if chunk.get(lx, ly, lz) != BlockId::Stone
                    || !is_inside_stone_band(wy, heights[lz][lx])
                {
                    continue;
                }
                let touches_cave_air = NEIGHBOR_OFFSETS.iter().any(|&(dx, dy, dz)| {
                    let nlx = lx as i32 + dx;
                    let nly = ly as i32 + dy;
                    let nlz = lz as i32 + dz;
                    if !(0..=max_local).contains(&nlx)
                        || !(0..=max_local).contains(&nly)
                        || !(0..=max_local).contains(&nlz)
                    {
                        return false;
                    }
                    let (nlx, nly, nlz) = (nlx as usize, nly as usize, nlz as usize);
                    if chunk.get(nlx, nly, nlz) != BlockId::Air {
                        return false;
                    }
                    let nwy = base_wy + nly as i32;
                    nwy < heights[nlz][nlx] - 3
                });
                if touches_cave_air {
                    exposed.push(linear_index(lx, ly, lz) as u16);
                }
            }
        }
    }

    exposed
}

/// Place deterministic coal and iron veins, biased onto cave walls so the
/// ores are visible from opened caves rather than only after blind mining.
/// The pass is linear in chunk volume: one exposed-cell scan plus a bounded
/// number of attempts, with no per-voxel noise calls.
fn place_ore_veins(
    chunk: &mut Chunk,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    pos: ChunkPos,
    seed: u32,
) {
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let mut rng = Xorshift32::new(hash3d(pos.x, pos.y, pos.z, seed ^ 0x000F_E51D));
    let exposed = exposed_stone_cells(chunk, heights, pos);
    let max_local = CHUNK_SIZE as i32 - 1;

    for (ore, attempts) in [(BlockId::CoalOre, 14u32), (BlockId::IronOre, 10u32)] {
        for _ in 0..attempts {
            let (mut lx, mut ly, mut lz) = if (rng.next() & 1) == 0 && !exposed.is_empty() {
                let idx = exposed[(rng.next() as usize) % exposed.len()];
                local_coords_from_index(idx)
            } else {
                (
                    (rng.next() as usize) & 31,
                    (rng.next() as usize) & 31,
                    (rng.next() as usize) & 31,
                )
            };

            let start_wy = base_wy + ly as i32;
            if !can_place_ore(chunk.get(lx, ly, lz), start_wy, heights[lz][lx], ore) {
                continue;
            }

            let steps = 3 + (rng.next() % 6) as usize;
            for step in 0..steps {
                let wy = base_wy + ly as i32;
                if can_place_ore(chunk.get(lx, ly, lz), wy, heights[lz][lx], ore) {
                    chunk.set(lx, ly, lz, ore);
                }
                if step + 1 == steps {
                    break;
                }
                let (dx, dy, dz) = NEIGHBOR_OFFSETS[(rng.next() % 6) as usize];
                lx = (lx as i32 + dx).clamp(0, max_local) as usize;
                ly = (ly as i32 + dy).clamp(0, max_local) as usize;
                lz = (lz as i32 + dz).clamp(0, max_local) as usize;
            }
        }
    }
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
    for (lz, row) in heights.iter_mut().enumerate() {
        for (lx, height) in row.iter_mut().enumerate() {
            let h = terrain_height_at_seeded(base_wx + lx as i32, base_wz + lz as i32, seed);
            *height = h;
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
    for (lz, row) in heights.iter().enumerate() {
        for (lx, &h) in row.iter().enumerate() {
            let wx = base_wx + lx as i32;
            let wz = base_wz + lz as i32;

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

    place_ore_veins(&mut chunk, &heights, pos, seed);
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

    fn has_subsurface_air_neighbor(
        chunk: &Chunk,
        pos: ChunkPos,
        lx: usize,
        ly: usize,
        lz: usize,
        seed: u32,
    ) -> bool {
        let base_wx = pos.x * CHUNK_SIZE as i32;
        let base_wy = pos.y * CHUNK_SIZE as i32;
        let base_wz = pos.z * CHUNK_SIZE as i32;
        let max_local = CHUNK_SIZE as i32 - 1;
        NEIGHBOR_OFFSETS.iter().any(|&(dx, dy, dz)| {
            let nlx = lx as i32 + dx;
            let nly = ly as i32 + dy;
            let nlz = lz as i32 + dz;
            if !(0..=max_local).contains(&nlx)
                || !(0..=max_local).contains(&nly)
                || !(0..=max_local).contains(&nlz)
            {
                return false;
            }
            let (nlx, nly, nlz) = (nlx as usize, nly as usize, nlz as usize);
            if chunk.get(nlx, nly, nlz) != BlockId::Air {
                return false;
            }
            let nwx = base_wx + nlx as i32;
            let nwy = base_wy + nly as i32;
            let nwz = base_wz + nlz as i32;
            nwy < terrain_height_at_seeded(nwx, nwz, seed) - 3
        })
    }

    #[test]
    fn test_ore_generation_deterministic_full_chunks() {
        let mut ore_count = 0usize;
        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            for cy in -1..=1 {
                for cz in -1..=1 {
                    for cx in -1..=1 {
                        let pos = ChunkPos::new(cx, cy, cz);
                        let first = generate_chunk_seeded(pos, seed);
                        let second = generate_chunk_seeded(pos, seed);
                        for lz in 0..CHUNK_SIZE {
                            for ly in 0..CHUNK_SIZE {
                                for lx in 0..CHUNK_SIZE {
                                    let a = first.get(lx, ly, lz);
                                    assert_eq!(
                                        a,
                                        second.get(lx, ly, lz),
                                        "seed {seed} chunk {pos:?} differs at ({lx}, {ly}, {lz})"
                                    );
                                    if matches!(a, BlockId::CoalOre | BlockId::IronOre) {
                                        ore_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            ore_count > 0,
            "expected non-vacuous ore placement across sampled chunks"
        );
    }

    #[test]
    fn test_both_ore_types_present_at_default_seed() {
        let seed = DEFAULT_WORLD_SEED;
        let mut coal = 0usize;
        let mut iron = 0usize;
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in -1..=1 {
                    let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                match chunk.get(lx, ly, lz) {
                                    BlockId::CoalOre => coal += 1,
                                    BlockId::IronOre => iron += 1,
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(coal > 0, "seed {seed} generated no coal ore");
        assert!(iron > 0, "seed {seed} generated no iron ore");
    }

    #[test]
    fn test_ore_blocks_respect_surface_and_depth_bands() {
        let seed = DEFAULT_WORLD_SEED;
        let mut checked = 0usize;
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in -1..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, seed);
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
                                let block = chunk.get(lx, ly, lz);
                                let Some((min_depth, max_depth)) = ore_depth_band(block) else {
                                    continue;
                                };
                                let wy = base_wy + ly as i32;
                                let depth = h - wy;
                                assert!(
                                    wy < h - 3,
                                    "{block:?} at world ({}, {wy}, {}) is not below h-3 (h = {h})",
                                    base_wx + lx as i32,
                                    base_wz + lz as i32
                                );
                                assert!(
                                    wy > -32,
                                    "{block:?} at world y {wy} intrudes into bedrock"
                                );
                                assert!(
                                    (min_depth..=max_depth).contains(&depth),
                                    "{block:?} depth {depth} at ({}, {wy}, {}) outside band {min_depth}..={max_depth}",
                                    base_wx + lx as i32,
                                    base_wz + lz as i32
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 0, "no ore blocks found to validate");
    }

    #[test]
    fn test_both_ores_visible_from_subsurface_air() {
        let seed = DEFAULT_WORLD_SEED;
        let mut coal_exposed = 0usize;
        let mut iron_exposed = 0usize;
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in -1..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, seed);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                match chunk.get(lx, ly, lz) {
                                    BlockId::CoalOre
                                        if has_subsurface_air_neighbor(
                                            &chunk, pos, lx, ly, lz, seed,
                                        ) =>
                                    {
                                        coal_exposed += 1;
                                    }
                                    BlockId::IronOre
                                        if has_subsurface_air_neighbor(
                                            &chunk, pos, lx, ly, lz, seed,
                                        ) =>
                                    {
                                        iron_exposed += 1;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            coal_exposed > 0,
            "no coal ore adjacent to subsurface cave Air"
        );
        assert!(
            iron_exposed > 0,
            "no iron ore adjacent to subsurface cave Air"
        );
    }

    #[test]
    fn test_different_seeds_change_ore_layout() {
        let mut layout_a = Vec::new();
        let mut layout_b = Vec::new();
        for cy in -1..=1 {
            for cz in -1..=1 {
                for cx in -1..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    for (seed, layout) in [(1337u32, &mut layout_a), (424_242u32, &mut layout_b)] {
                        let chunk = generate_chunk_seeded(pos, seed);
                        for lz in 0..CHUNK_SIZE {
                            for ly in 0..CHUNK_SIZE {
                                for lx in 0..CHUNK_SIZE {
                                    let block = chunk.get(lx, ly, lz);
                                    if matches!(block, BlockId::CoalOre | BlockId::IronOre) {
                                        layout.push((pos.x, pos.y, pos.z, lx, ly, lz, block as u8));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(!layout_a.is_empty(), "seed 1337 produced no ores");
        assert!(!layout_b.is_empty(), "seed 424242 produced no ores");
        assert_ne!(
            layout_a, layout_b,
            "different seeds produced identical ore layouts"
        );
    }

    #[test]
    fn test_can_place_ore_guards_surface_bedrock_and_bands() {
        let h = 40;
        assert!(can_place_ore(BlockId::Stone, h - 4, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Stone, h - 3, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Stone, h, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Air, h - 4, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Dirt, h - 4, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Sand, h - 4, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Bedrock, h - 4, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::OakLog, h - 4, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Stone, -32, h, BlockId::CoalOre));

        // Coal depth band is 4..=36.
        assert!(can_place_ore(BlockId::Stone, h - 4, h, BlockId::CoalOre));
        assert!(can_place_ore(BlockId::Stone, h - 36, h, BlockId::CoalOre));
        assert!(!can_place_ore(BlockId::Stone, h - 37, h, BlockId::CoalOre));

        // Iron depth band is 12..=90; use a tall column to test the deep edge.
        let deep_h = 200;
        assert!(!can_place_ore(
            BlockId::Stone,
            deep_h - 11,
            deep_h,
            BlockId::IronOre
        ));
        assert!(can_place_ore(
            BlockId::Stone,
            deep_h - 12,
            deep_h,
            BlockId::IronOre
        ));
        assert!(can_place_ore(
            BlockId::Stone,
            deep_h - 90,
            deep_h,
            BlockId::IronOre
        ));
        assert!(!can_place_ore(
            BlockId::Stone,
            deep_h - 91,
            deep_h,
            BlockId::IronOre
        ));
        assert!(!can_place_ore(BlockId::Stone, h - 4, h, BlockId::IronOre));
        assert!(!can_place_ore(BlockId::Stone, h - 4, h, BlockId::Stone));
    }

    fn chunk_heights(pos: ChunkPos, seed: u32) -> [[i32; CHUNK_SIZE]; CHUNK_SIZE] {
        let base_wx = pos.x * CHUNK_SIZE as i32;
        let base_wz = pos.z * CHUNK_SIZE as i32;
        let mut heights = [[0i32; CHUNK_SIZE]; CHUNK_SIZE];
        for (lz, row) in heights.iter_mut().enumerate() {
            for (lx, h) in row.iter_mut().enumerate() {
                *h = terrain_height_at_seeded(base_wx + lx as i32, base_wz + lz as i32, seed);
            }
        }
        heights
    }

    /// Slice acceptance: generation is byte-for-byte deterministic, the sample
    /// is non-vacuous (carved cave Air and ore both present), and changing the
    /// seed changes the generated world.
    #[test]
    fn test_acceptance_generation_deterministic_and_nonvacuous() {
        let positions = [
            ChunkPos::new(0, -1, 0),
            ChunkPos::new(0, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(-1, 1, 2),
        ];
        let mut carved_air = 0usize;
        let mut ore_blocks = 0usize;

        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            for &pos in &positions {
                let first = generate_chunk_seeded(pos, seed);
                let second = generate_chunk_seeded(pos, seed);
                let heights = chunk_heights(pos, seed);
                let base_wy = pos.y * CHUNK_SIZE as i32;
                for (lz, row) in heights.iter().enumerate() {
                    for ly in 0..CHUNK_SIZE {
                        for (lx, &h) in row.iter().enumerate() {
                            let a = first.get(lx, ly, lz);
                            assert_eq!(
                                a,
                                second.get(lx, ly, lz),
                                "seed {seed} chunk {pos:?} differs at ({lx}, {ly}, {lz})"
                            );
                            let wy = base_wy + ly as i32;
                            if a == BlockId::Air && wy > -32 && wy < h - 3 {
                                carved_air += 1;
                            }
                            if matches!(a, BlockId::CoalOre | BlockId::IronOre) {
                                ore_blocks += 1;
                            }
                        }
                    }
                }
            }
        }

        let mut seed_changed_any_block = false;
        for &pos in &positions {
            let first = generate_chunk_seeded(pos, DEFAULT_WORLD_SEED);
            let second = generate_chunk_seeded(pos, 424_242);
            for lz in 0..CHUNK_SIZE {
                for ly in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        if first.get(lx, ly, lz) != second.get(lx, ly, lz) {
                            seed_changed_any_block = true;
                        }
                    }
                }
            }
        }

        assert!(
            carved_air > 0,
            "acceptance sample contains no carved cave Air below h-3"
        );
        assert!(ore_blocks > 0, "acceptance sample contains no ore blocks");
        assert!(
            seed_changed_any_block,
            "seeds 1337 and 424242 produced identical blocks across the acceptance sample"
        );
    }

    /// Slice acceptance: cave surfaces produce interior mesh quads (a uniform
    /// stone chunk only meshes boundary quads), and exposed ore faces are
    /// emitted into the correct face buckets with texture layers 14/15.
    #[test]
    fn test_acceptance_caves_and_ores_are_visible_in_mesh() {
        use crate::mesh::greedy::{mesh_chunk, PaddedChunk};

        let seed = DEFAULT_WORLD_SEED;
        let max_local = CHUNK_SIZE as i32 - 1;
        let mut cave_chunks = 0usize;
        let mut interior_quads = 0usize;
        let mut coal_faces = 0usize;
        let mut iron_faces = 0usize;

        for cx in -4..=4 {
            for cz in -4..=4 {
                for cy in -1..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, seed);
                    let heights = chunk_heights(pos, seed);
                    let base_wy = pos.y * CHUNK_SIZE as i32;
                    let mut has_carved_air = false;
                    let mut exposed_ores: Vec<(BlockId, u8)> = Vec::new();

                    for lz in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let h = heights[lz][lx];
                            for ly in 0..CHUNK_SIZE {
                                let wy = base_wy + ly as i32;
                                let block = chunk.get(lx, ly, lz);
                                if block == BlockId::Air && wy > -32 && wy < h - 3 {
                                    has_carved_air = true;
                                }
                                if !matches!(block, BlockId::CoalOre | BlockId::IronOre) {
                                    continue;
                                }
                                for (face_dir, &(dx, dy, dz)) in NEIGHBOR_OFFSETS.iter().enumerate()
                                {
                                    let nlx = lx as i32 + dx;
                                    let nly = ly as i32 + dy;
                                    let nlz = lz as i32 + dz;
                                    if !(0..=max_local).contains(&nlx)
                                        || !(0..=max_local).contains(&nly)
                                        || !(0..=max_local).contains(&nlz)
                                    {
                                        continue;
                                    }
                                    let (nlx, nly, nlz) =
                                        (nlx as usize, nly as usize, nlz as usize);
                                    if chunk.get(nlx, nly, nlz) != BlockId::Air {
                                        continue;
                                    }
                                    let nwy = base_wy + nly as i32;
                                    if nwy < heights[nlz][nlx] - 3 {
                                        exposed_ores.push((block, face_dir as u8));
                                    }
                                }
                            }
                        }
                    }

                    if !has_carved_air && exposed_ores.is_empty() {
                        continue;
                    }

                    let mesh = mesh_chunk(&PaddedChunk::from_center_only(&chunk));
                    if has_carved_air {
                        cave_chunks += 1;
                        for bucket in &mesh.buckets {
                            for quad in bucket {
                                let (x, y, z, _, _, face_dir, _, _, _) = quad.unpack();
                                let axis_d = match face_dir / 2 {
                                    0 => x,
                                    1 => y,
                                    _ => z,
                                };
                                if axis_d > 0 && axis_d < CHUNK_SIZE as u32 - 1 {
                                    interior_quads += 1;
                                }
                            }
                        }
                    }

                    for (ore, face_dir) in exposed_ores {
                        let layer = ore.face_tex_layer(face_dir);
                        if mesh.buckets[face_dir as usize]
                            .iter()
                            .any(|q| q.unpack().6 == layer)
                        {
                            match ore {
                                BlockId::CoalOre => coal_faces += 1,
                                BlockId::IronOre => iron_faces += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        assert!(
            cave_chunks > 0,
            "no chunk with carved cave Air in the mesh scan region"
        );
        assert!(
            interior_quads > 0,
            "cave chunk meshing produced no interior-axis quads; cave surfaces were not meshed"
        );
        assert!(
            coal_faces > 0,
            "no coal ore face meshed with texture layer 14"
        );
        assert!(
            iron_faces > 0,
            "no iron ore face meshed with texture layer 15"
        );
    }

    /// Slice acceptance: seed 1337 has deterministic, human-reachable dig
    /// targets. Coordinates are printed for the closeout UAT (`-- --nocapture`).
    #[test]
    fn test_acceptance_uat_dig_targets_near_origin() {
        let seed = DEFAULT_WORLD_SEED;
        let mut shallowest_coal: Option<(i32, i32, i32)> = None;
        let mut shallowest_iron: Option<(i32, i32, i32)> = None;

        for cx in -4..=4 {
            for cz in -4..=4 {
                for cy in -1..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, seed);
                    let base_wx = cx * CHUNK_SIZE as i32;
                    let base_wy = cy * CHUNK_SIZE as i32;
                    let base_wz = cz * CHUNK_SIZE as i32;
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let block = chunk.get(lx, ly, lz);
                                if !matches!(block, BlockId::CoalOre | BlockId::IronOre)
                                    || !has_subsurface_air_neighbor(&chunk, pos, lx, ly, lz, seed)
                                {
                                    continue;
                                }
                                let coord = (
                                    base_wx + lx as i32,
                                    base_wy + ly as i32,
                                    base_wz + lz as i32,
                                );
                                let slot = if block == BlockId::CoalOre {
                                    &mut shallowest_coal
                                } else {
                                    &mut shallowest_iron
                                };
                                if slot.is_none_or(|(_, best_y, _)| coord.1 > best_y) {
                                    *slot = Some(coord);
                                }
                            }
                        }
                    }
                }
            }
        }

        let coal = shallowest_coal.expect("seed 1337 has no reachable coal ore near origin");
        let iron = shallowest_iron.expect("seed 1337 has no reachable iron ore near origin");
        eprintln!(
            "UAT seed {seed}: visible coal ore at world ({}, {}, {})",
            coal.0, coal.1, coal.2
        );
        eprintln!(
            "UAT seed {seed}: visible iron ore at world ({}, {}, {})",
            iron.0, iron.1, iron.2
        );
    }

    /// Slice acceptance invariants over a broad region: ores stay strictly
    /// below the h-3 roof and inside their depth bands, the bedrock floor is
    /// intact, the surface skin (h, h-1..h-3) is never carved, and Stone
    /// mountain tops (h >= 58) are never punched through.
    #[test]
    fn test_acceptance_invariants_over_broad_region() {
        let seed = DEFAULT_WORLD_SEED;
        let mut ore_count = 0usize;
        let mut mountain_surface_checks = 0usize;

        for cx in -8..=8 {
            for cz in -8..=8 {
                let heights = chunk_heights(ChunkPos::new(cx, 0, cz), seed);
                let base_wx = cx * CHUNK_SIZE as i32;
                let base_wz = cz * CHUNK_SIZE as i32;
                for cy in -1..=2 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, seed);
                    let base_wy = cy * CHUNK_SIZE as i32;
                    let top_wy = base_wy + CHUNK_SIZE as i32 - 1;
                    for (lz, row) in heights.iter().enumerate() {
                        for (lx, &h) in row.iter().enumerate() {
                            let wx = base_wx + lx as i32;
                            let wz = base_wz + lz as i32;
                            for ly in 0..CHUNK_SIZE {
                                let wy = base_wy + ly as i32;
                                let block = chunk.get(lx, ly, lz);
                                if wy == -32 {
                                    assert_eq!(
                                        block,
                                        BlockId::Bedrock,
                                        "wy == -32 is not Bedrock at ({wx}, {wy}, {wz})"
                                    );
                                }
                                if let Some((min_depth, max_depth)) = ore_depth_band(block) {
                                    ore_count += 1;
                                    assert!(
                                        wy < h - 3,
                                        "{block:?} at ({wx}, {wy}, {wz}) is not below h-3 (h = {h})"
                                    );
                                    assert!(
                                        wy > -32,
                                        "{block:?} at ({wx}, {wy}, {wz}) intrudes into bedrock"
                                    );
                                    let depth = h - wy;
                                    assert!(
                                        (min_depth..=max_depth).contains(&depth),
                                        "{block:?} depth {depth} at ({wx}, {wy}, {wz}) outside {min_depth}..={max_depth}"
                                    );
                                }
                            }
                            for wy in (h - 3)..=h {
                                if !(base_wy..=top_wy).contains(&wy) {
                                    continue;
                                }
                                let ly = (wy - base_wy) as usize;
                                assert_ne!(
                                    chunk.get(lx, ly, lz),
                                    BlockId::Air,
                                    "surface skin carved at world ({wx}, {wy}, {wz}) (h = {h})"
                                );
                            }
                            if h >= 58 && (base_wy..=top_wy).contains(&h) {
                                let ly = (h - base_wy) as usize;
                                let surface = chunk.get(lx, ly, lz);
                                assert_ne!(
                                    surface,
                                    BlockId::Air,
                                    "mountain-top surface carved at ({wx}, {h}, {wz})"
                                );
                                if h < 68 {
                                    assert_eq!(
                                        surface,
                                        BlockId::Stone,
                                        "expected Stone mountain top at ({wx}, {h}, {wz})"
                                    );
                                }
                                mountain_surface_checks += 1;
                            }
                        }
                    }
                }
            }
        }

        assert!(ore_count > 0, "broad invariant scan found no ore blocks");

        if mountain_surface_checks == 0 {
            // The near-origin region has no mountain; probe wider terrain so
            // the carve guard against surface holes is exercised non-vacuously.
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
        } else {
            eprintln!(
                "acceptance invariants: checked {mountain_surface_checks} mountain-top surface cells"
            );
        }
    }
}
