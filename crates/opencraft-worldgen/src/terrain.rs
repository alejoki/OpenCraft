//! The complete terrain-generation body over a neutral [`ChunkBuffer`].
//!
//! Moved verbatim from the game generator so the two byte-identical passes can
//! be compared before any game call site changes. The crate speaks raw `u16`
//! block ids and the neutral [`Season`] vocabulary; the game maps its own
//! `BlockId`/`Season` at the adapter boundary.
//!
//! Ordering is part of the contract and must never be reordered:
//!
//! 1. built-in terrain, caves and trees
//! 2. built-in coal/iron veins (own stream, always byte-identical)
//! 3. pack ore veins (one independent stream per rule)
//! 4. built-in surface ruins
//! 5. pack surface patches (one independent stream per rule)
//! 6. village assembly (the true last pass)
//!
//! Both early returns (high sky and all-above-terrain uniform Air) and the
//! closing `try_compress` stay exactly as they are, so an empty rule slice and
//! an absent village catalog reproduce the pre-M010/M015 generator byte for
//! byte.

use opencraft_content::catalog::{BiomeTag, WorldgenRule};
use opencraft_core::chunk::{linear_index, local_coords_from_index, ChunkPos, CHUNK_SIZE};

use crate::biome::{biome_for_height, forest_noise, Biome, BEACH_BAND, SEA_LEVEL};
use crate::block::{
    BLOCK_AIR, BLOCK_BEDROCK, BLOCK_BRICK, BLOCK_COAL_ORE, BLOCK_COBBLESTONE, BLOCK_DIRT,
    BLOCK_GRASS, BLOCK_IRON_ORE, BLOCK_LOOT_CHEST, BLOCK_OAK_LEAVES, BLOCK_OAK_LOG, BLOCK_SAND,
    BLOCK_SNOW, BLOCK_STONE, BLOCK_WATER,
};
use crate::buffer::ChunkBuffer;
use crate::height::{terrain_height_at_seeded, MAX_TERRAIN_HEIGHT};
use crate::prefab::PREFAB_CAPS;
use crate::ruin::{ruin_at, ruin_loot_chest_pos, RuinSpec, RUIN_FOOTPRINT_HALF};
use crate::season::Season;
use crate::tree::{oak_tree_at, TREE_CANOPY_HEADROOM, TREE_MAX_HEIGHT_OVER_SURFACE};
use crate::village::{place_village_cells, VillageCatalog};

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

pub const DEFAULT_WORLD_SEED: u32 = 1337;

/// Snow caps appear only above this mountain surface height.
const SNOW_CAP_HEIGHT: i32 = 96;

/// World-aligned cave lattice spacing in blocks. Sampling the density on a
/// 4-block grid (9 corners per 32-block chunk axis) keeps cave generation
/// linear in chunk volume while interpolation stays shared across chunk
/// borders because chunk bases are multiples of 32.
const CAVE_LATTICE: usize = 4;
const CAVE_THRESHOLD: f32 = 0.66;

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
/// Local to the worker that generates the chunk: no shared state.
struct CaveField {
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
fn is_cave_carvable(block: u16, wy: i32, surface: i32) -> bool {
    block == BLOCK_STONE && is_inside_stone_band(wy, surface)
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

/// Built-in ore-stream salt. Pack rules must never reuse this value: a pack
/// stream seeded with it would consume the same sequence as coal/iron and
/// could no longer be proven byte-identical to an empty-rule generation.
const BUILTIN_ORE_SALT: u32 = 0x000F_E51D;

#[inline]
fn ore_depth_band(ore: u16) -> Option<(i32, i32)> {
    match ore {
        BLOCK_COAL_ORE => Some(COAL_DEPTH_BAND),
        BLOCK_IRON_ORE => Some(IRON_DEPTH_BAND),
        _ => None,
    }
}

/// Every ore replacement must satisfy all of these guards: the target is
/// still Stone, it is strictly inside the stone band, and its depth from the
/// column surface is inside the ore's band. Re-checking per growth cell keeps
/// veins from climbing into the surface skin or drifting out of their band.
#[inline]
fn can_place_ore(block: u16, wy: i32, surface: i32, ore: u16) -> bool {
    if block != BLOCK_STONE || !is_inside_stone_band(wy, surface) {
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

/// One FNV-1a step over a little-endian `u32`.
#[inline]
fn fnv1a_u32(mut hash: u32, value: u32) -> u32 {
    for byte in value.to_le_bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Stable discriminant for the biome list inside a patch rule.
///
/// Identical to the pre-move `Biome` match: `plains = 0`, `forest = 1`,
/// `mountains = 2`, `ocean = 3`.
#[inline]
fn biome_discriminant(biome: BiomeTag) -> u32 {
    biome.index() as u32
}

/// Deterministic per-rule stream salt.
///
/// Hand-rolled FNV-1a over the rule's discriminant, authored block ids and
/// every numeric/biome field. Never `DefaultHasher`, never a clock, and never
/// the built-in ore salt: that independence is what lets the built-in coal and
/// iron streams stay byte-identical no matter which pack rules are installed.
fn pack_rule_salt(index: usize, rule: &WorldgenRule) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    hash = fnv1a_u32(hash, index as u32);
    match rule {
        WorldgenRule::Ore {
            block,
            replace,
            min_y,
            max_y,
            vein,
            attempts,
        } => {
            hash = fnv1a_u32(hash, 0x0E1A_0001);
            hash = fnv1a_u32(hash, *block as u32);
            hash = fnv1a_u32(hash, *replace as u32);
            hash = fnv1a_u32(hash, *min_y as u32);
            hash = fnv1a_u32(hash, *max_y as u32);
            hash = fnv1a_u32(hash, *vein as u32);
            hash = fnv1a_u32(hash, *attempts as u32);
        }
        WorldgenRule::Patch {
            block,
            replace,
            biomes,
            min_y,
            max_y,
            chance,
        } => {
            hash = fnv1a_u32(hash, 0x0A7C_0002);
            hash = fnv1a_u32(hash, *block as u32);
            hash = fnv1a_u32(hash, *replace as u32);
            hash = fnv1a_u32(hash, *min_y as u32);
            hash = fnv1a_u32(hash, *max_y as u32);
            hash = fnv1a_u32(hash, chance.to_bits());
            for biome in biomes {
                hash = fnv1a_u32(hash, biome_discriminant(*biome));
            }
        }
    }
    if hash == BUILTIN_ORE_SALT {
        hash ^= 1;
    }
    hash
}

/// Pack ore guards: authored host, inside the stone band, and inside the
/// authored world-Y band. Checked for the selected start and re-checked for
/// every grown cell, exactly like [`can_place_ore`].
#[inline]
fn can_place_pack_ore(
    block: u16,
    wy: i32,
    surface: i32,
    replace: u16,
    min_y: i32,
    max_y: i32,
) -> bool {
    block == replace && is_inside_stone_band(wy, surface) && (min_y..=max_y).contains(&wy)
}

/// Stone cells with at least one in-chunk Air neighbor that is itself below
/// the neighbor column's surface skin. These are cave walls and underground
/// cliffs, not open sky; veins started from this list are visible when a
/// player digs into a cave.
fn exposed_stone_cells(
    chunk: &ChunkBuffer,
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
                if chunk.get(lx, ly, lz) != BLOCK_STONE
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
                    if chunk.get(nlx, nly, nlz) != BLOCK_AIR {
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
    chunk: &mut ChunkBuffer,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    pos: ChunkPos,
    seed: u32,
) {
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let mut rng = Xorshift32::new(hash3d(pos.x, pos.y, pos.z, seed ^ BUILTIN_ORE_SALT));
    let exposed = exposed_stone_cells(chunk, heights, pos);
    let max_local = CHUNK_SIZE as i32 - 1;

    for (ore, attempts) in [(BLOCK_COAL_ORE, 14u32), (BLOCK_IRON_ORE, 10u32)] {
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

/// Place authored pack ore veins.
///
/// Runs after the built-in coal/iron pass on a fresh, independent xorshift
/// stream per rule, so the built-in ore cells stay byte-identical to an
/// empty-rule generation. Candidate starts come from one linear scan that
/// honours the authored host block (`rule.replace`) and Y band; this avoids
/// `exposed_stone_cells`' hardcoded Stone assumption and gives a non-Stone
/// host a chance to spawn. Growth mirrors [`place_ore_veins`]: chunk-local
/// neighbour steps with `clamp(0, 31)`, re-checking every guard per cell.
fn place_pack_ore_veins(
    chunk: &mut ChunkBuffer,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    pos: ChunkPos,
    seed: u32,
    rules: &[WorldgenRule],
) {
    if rules.is_empty() {
        return;
    }

    let base_wy = pos.y * CHUNK_SIZE as i32;
    let max_local = CHUNK_SIZE as i32 - 1;

    for (rule_index, rule) in rules.iter().enumerate() {
        let (block, replace, min_y, max_y, vein, attempts) = match rule {
            WorldgenRule::Ore {
                block,
                replace,
                min_y,
                max_y,
                vein,
                attempts,
            } => (*block, *replace, *min_y, *max_y, *vein, *attempts),
            WorldgenRule::Patch { .. } => continue,
        };

        // One scan per rule: every cell that currently satisfies the authored
        // host, stone-band and Y-band guards. Built earlier rules may already
        // have rewritten some cells, which those rules' own bands decide.
        let mut candidates: Vec<u16> = Vec::new();
        for (lz, row) in heights.iter().enumerate() {
            for ly in 0..CHUNK_SIZE {
                let wy = base_wy + ly as i32;
                for (lx, &surface) in row.iter().enumerate() {
                    if can_place_pack_ore(chunk.get(lx, ly, lz), wy, surface, replace, min_y, max_y)
                    {
                        candidates.push(linear_index(lx, ly, lz) as u16);
                    }
                }
            }
        }
        if candidates.is_empty() {
            continue;
        }

        let mut rng = Xorshift32::new(hash3d(
            pos.x,
            pos.y,
            pos.z,
            seed ^ pack_rule_salt(rule_index, rule),
        ));

        for _ in 0..attempts {
            let start = candidates[(rng.next() as usize) % candidates.len()];
            let (mut lx, mut ly, mut lz) = local_coords_from_index(start);
            let start_wy = base_wy + ly as i32;
            if !can_place_pack_ore(
                chunk.get(lx, ly, lz),
                start_wy,
                heights[lz][lx],
                replace,
                min_y,
                max_y,
            ) {
                continue;
            }

            for step in 0..vein as usize {
                let wy = base_wy + ly as i32;
                if can_place_pack_ore(
                    chunk.get(lx, ly, lz),
                    wy,
                    heights[lz][lx],
                    replace,
                    min_y,
                    max_y,
                ) {
                    chunk.set(lx, ly, lz, block);
                }
                if step + 1 == vein as usize {
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

/// Whether any oak log — a trunk cell of a tree rooted in the footprint or a
/// one-block branch of a tree rooted next to it — falls inside the vertical
/// window a ruin could occupy (`surface + 1..=surface + TREE_CANOPY_HEADROOM`).
///
/// `place_ruin_at`'s in-chunk OakLog scan can only see the chunk layer that owns
/// the anchor surface, while a D088 tree near a vertical chunk edge keeps cells
/// in the layer above. This pure model check closes that gap, so the
/// tree-overlap policy is layer-independent: a ruin never clips a trunk or a
/// branch, even when the tree's upper cells live in the next chunk layer.
fn tree_logs_intersect_ruin_window(spec: &RuinSpec, seed: u32) -> bool {
    let min_wy = spec.surface + 1;
    let max_wy = spec.surface + TREE_CANOPY_HEADROOM;
    let min_x = spec.anchor_x - RUIN_FOOTPRINT_HALF;
    let max_x = spec.anchor_x + RUIN_FOOTPRINT_HALF;
    let min_z = spec.anchor_z - RUIN_FOOTPRINT_HALF;
    let max_z = spec.anchor_z + RUIN_FOOTPRINT_HALF;

    // A branch reaches one block out from its root, so every tree that could
    // write a log into the footprint is rooted in the footprint expanded by
    // one. One bounded 7x7 scan covers all of them.
    for rz in (min_z - 1)..=(max_z + 1) {
        for rx in (min_x - 1)..=(max_x + 1) {
            let h = terrain_height_at_seeded(rx, rz, seed);
            let biome = crate::biome::biome_at_seeded(rx, rz, seed);
            let Some(tree) = oak_tree_at(rx, rz, h, biome, seed) else {
                continue;
            };

            // Trunk cells stay on the root column, so only trees rooted inside
            // the footprint can intersect it.
            if (min_x..=max_x).contains(&rx)
                && (min_z..=max_z).contains(&rz)
                && h < max_wy
                && h + tree.trunk_height >= min_wy
            {
                return true;
            }

            // Keep only branch cells that land in the footprint and the
            // window. The terrain check mirrors the emission pass: a branch is
            // never written into a ground cell. Branch cells are well above
            // sea level, so Water cannot occupy them.
            for (bx, by, bz) in tree.branch_cells(rx, rz, h) {
                if (min_x..=max_x).contains(&bx)
                    && (min_z..=max_z).contains(&bz)
                    && (min_wy..=max_wy).contains(&by)
                    && terrain_height_at_seeded(bx, bz, seed) < by
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Place the deterministic surface ruins whose anchors fall inside this chunk.
///
/// Modeled on `place_ore_veins`: one linear scan over the chunk's 32x32
/// columns, then a tiny bounded write per ruin. Placement is confined to the
/// chunk that owns the anchor (see `RUIN_ANCHOR_MARGIN`), so the pass never
/// writes into a neighbouring chunk and needs no neighbour heights.
///
/// The surface-block gate is the authoritative one, exactly like the tree
/// pass: the column must still hold a land skin after the terrain, cave, tree
/// and ore passes. `Grass` is the warm-season skin; the seasonal Winter
/// blanket turns the same Plains/Forest land into `Snow`, which is equally
/// valid. That single check is what keeps ruins off Sand beaches, seabed and
/// mountain Stone. The altitude Snow cap can never reach here: `ruin_at`
/// already rejects every column outside the Plains/Forest height band.
///
/// Ruins are therefore season-independent at the anchor level: the same
/// ruins and loot chests exist in every season. A winter shell can gain a cell
/// where a warm-season leaf occupied the only-into-Air write; it never loses
/// one.
fn place_surface_ruins(
    chunk: &mut ChunkBuffer,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    biomes: &[[Biome; CHUNK_SIZE]; CHUNK_SIZE],
    pos: ChunkPos,
    seed: u32,
) {
    let base_wx = pos.x * CHUNK_SIZE as i32;
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let base_wz = pos.z * CHUNK_SIZE as i32;

    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let h = heights[lz][lx];
            let surface_ly = h - base_wy;
            if !(0..CHUNK_SIZE as i32).contains(&surface_ly) {
                continue;
            }
            if !matches!(
                chunk.get(lx, surface_ly as usize, lz),
                BLOCK_GRASS | BLOCK_SNOW
            ) {
                continue;
            }
            let wx = base_wx + lx as i32;
            let wz = base_wz + lz as i32;
            let biome = biomes[lz][lx];
            let Some(spec) = ruin_at(wx, wz, h, biome, seed) else {
                continue;
            };
            // The in-chunk log scan inside `place_ruin_at` can only see the
            // current chunk layer. A D088 tree can straddle a vertical chunk
            // edge, so also reject anchors whose window intersects any trunk or
            // branch in the pure tree model (see `tree_logs_intersect_ruin_window`).
            if tree_logs_intersect_ruin_window(&spec, seed) {
                continue;
            }
            place_ruin_at(chunk, heights, base_wy, spec);
        }
    }
}

/// Place authored pack surface patches.
///
/// Runs last, after built-in ruins, and only ever rewrites the column's own
/// surface cell (`wy == h`). It never writes above the surface, so trees,
/// leaves, ruin shells and loot chests are structurally untouchable no matter
/// what a pack authors. Each rule has its own xorshift stream and rolls at most
/// once per eligible column in `(lz, lx)` order.
fn place_pack_surface_patches(
    chunk: &mut ChunkBuffer,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    biomes: &[[Biome; CHUNK_SIZE]; CHUNK_SIZE],
    pos: ChunkPos,
    seed: u32,
    rules: &[WorldgenRule],
) {
    if rules.is_empty() {
        return;
    }

    let base_wy = pos.y * CHUNK_SIZE as i32;

    for (rule_index, rule) in rules.iter().enumerate() {
        let (block, replace, rule_biomes, min_y, max_y, chance) = match rule {
            WorldgenRule::Patch {
                block,
                replace,
                biomes,
                min_y,
                max_y,
                chance,
            } => (*block, *replace, biomes, *min_y, *max_y, *chance),
            WorldgenRule::Ore { .. } => continue,
        };
        if chance <= 0.0 {
            continue;
        }

        let mut rng = Xorshift32::new(hash3d(
            pos.x,
            pos.y,
            pos.z,
            seed ^ pack_rule_salt(rule_index, rule),
        ));

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let h = heights[lz][lx];
                let surface_ly = h - base_wy;
                if !(0..CHUNK_SIZE as i32).contains(&surface_ly) {
                    continue;
                }
                if !rule_biomes.contains(&biomes[lz][lx].tag()) {
                    continue;
                }
                if !(min_y..=max_y).contains(&h) {
                    continue;
                }
                let surface_ly = surface_ly as usize;
                if chunk.get(lx, surface_ly, lz) != replace {
                    continue;
                }
                // Uniform [0, 1): the denominator is one past u32::MAX, so a
                // chance of 1.0 always passes instead of flaking on u32::MAX.
                let roll = rng.next() as f64 / 4_294_967_296.0;
                if roll < chance as f64 {
                    chunk.set(lx, surface_ly, lz, block);
                }
            }
        }
    }
}

/// Write one ruin shell into `chunk`.
///
/// `ruin_at`'s chunk-interior anchor margin guarantees the whole footprint lies
/// inside this chunk, so every `lx`/`lz` below is a plain offset (no wrapping).
/// `ruin_at`'s footprint-aware vertical-fit gate additionally guarantees every
/// shell cell — including the tallest corner pillar on the highest footprint
/// column — stays in the chunk layer that owns the anchor surface, so the
/// in-range skips remain defensive only.
///
/// Three policies are pinned here, all deterministic and independent of
/// iteration order:
///
/// 1. Tree-overlap: if any footprint column already holds `OakLog`, the anchor
///    is skipped entirely — a ruin never carves through a tree.
/// 2. Only-into-Air: each shell cell is written only where the chunk currently
///    holds `BLOCK_AIR`, so trees, leaves, the Grass surface skin, Snow and
///    Water are never overwritten. Footprint columns whose own surface is not
///    Grass are skipped too, so a shell cannot stand on a Sand/Stone neighbour.
/// 3. One loot chest: after the shell loop, a single `LootChest` is written at
///    [`ruin_loot_chest_pos`] (the anchor column, one above the surface) and
///    only when that cell is currently `Air`. An interlocking shell, leaves,
///    Water or any other occupant wins, and the anchor then carries no chest;
///    this pass never writes a second chest.
fn place_ruin_at(
    chunk: &mut ChunkBuffer,
    heights: &[[i32; CHUNK_SIZE]; CHUNK_SIZE],
    base_wy: i32,
    spec: RuinSpec,
) {
    let anchor_lx = spec.anchor_x.rem_euclid(CHUNK_SIZE as i32);
    let anchor_lz = spec.anchor_z.rem_euclid(CHUNK_SIZE as i32);

    for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
        for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            let lx = (anchor_lx + dx) as usize;
            let lz = (anchor_lz + dz) as usize;
            if (0..CHUNK_SIZE).any(|ly| chunk.get(lx, ly, lz) == BLOCK_OAK_LOG) {
                return;
            }
        }
    }

    for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
        for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            // Shell only: the footprint interior stays open Air.
            if dx.abs() != RUIN_FOOTPRINT_HALF && dz.abs() != RUIN_FOOTPRINT_HALF {
                continue;
            }
            let lx = (anchor_lx + dx) as usize;
            let lz = (anchor_lz + dz) as usize;
            let h = heights[lz][lx];
            let surface_ly = h - base_wy;
            if !(0..CHUNK_SIZE as i32).contains(&surface_ly) {
                continue;
            }
            // Per-footprint-column surface gate: the shell stands on the
            // seasonal land skin (Grass, or Snow in Winter) only, so a
            // footprint straddling a beach or a mountain stone column simply
            // loses those cells.
            if !matches!(
                chunk.get(lx, surface_ly as usize, lz),
                BLOCK_GRASS | BLOCK_SNOW
            ) {
                continue;
            }
            let block = ruin_wall_block(dx, dz);
            let bottom = ruin_cell_bottom(dx, dz);
            let top = ruin_cell_top(dx, dz, spec.wall_height);
            for level in bottom..=top {
                let ly = h + level - base_wy;
                if !(0..CHUNK_SIZE as i32).contains(&ly) {
                    continue;
                }
                let ly = ly as usize;
                if chunk.get(lx, ly, lz) == BLOCK_AIR {
                    chunk.set(lx, ly, lz, block);
                }
            }
        }
    }

    // One loot chest at the interior centre. The only-into-Air rule applies to
    // the chest as well: a neighbouring shell, leaves, Water or anything else
    // already occupying the cell wins, and this anchor carries no chest rather
    // than overwriting it. No second chest is ever written.
    let chest = ruin_loot_chest_pos(&spec);
    let chest_lx = chest.x.rem_euclid(CHUNK_SIZE as i32);
    let chest_lz = chest.z.rem_euclid(CHUNK_SIZE as i32);
    let chest_ly = chest.y - base_wy;
    if (0..CHUNK_SIZE as i32).contains(&chest_lx)
        && (0..CHUNK_SIZE as i32).contains(&chest_ly)
        && (0..CHUNK_SIZE as i32).contains(&chest_lz)
    {
        let (lx, ly, lz) = (chest_lx as usize, chest_ly as usize, chest_lz as usize);
        if chunk.get(lx, ly, lz) == BLOCK_AIR {
            chunk.set(lx, ly, lz, BLOCK_LOOT_CHEST);
        }
    }
}

/// Wall material for a footprint offset: Brick on the four corner pillars,
/// Cobblestone on the wall runs between them. Both are already complete,
/// legal-edit opaque blocks, so ruins add no block ids and no registry rows.
#[inline]
fn ruin_wall_block(dx: i32, dz: i32) -> u16 {
    if dx.abs() == RUIN_FOOTPRINT_HALF && dz.abs() == RUIN_FOOTPRINT_HALF {
        BLOCK_BRICK
    } else {
        BLOCK_COBBLESTONE
    }
}

/// Lowest wall level (above the column surface) at which this footprint cell
/// starts. The centre of the +z wall is a doorway: it is left open for the
/// whole wall height so the ruin reads as an entrance, not a sealed box.
#[inline]
fn ruin_cell_bottom(dx: i32, dz: i32) -> i32 {
    if dx == 0 && dz == RUIN_FOOTPRINT_HALF {
        2
    } else {
        1
    }
}

/// Highest wall level of this footprint cell: corner pillars stand one block
/// above the wall runs between them.
#[inline]
fn ruin_cell_top(dx: i32, dz: i32, wall_height: i32) -> i32 {
    let corner = dx.abs() == RUIN_FOOTPRINT_HALF && dz.abs() == RUIN_FOOTPRINT_HALF;
    wall_height + i32::from(corner)
}

/// Generates `pos` with the default world seed and [`Season::Summer`].
///
/// Season-independent callers (tooling, small unit tests, previews) use this
/// wrapper: Summer is the blanket-absent baseline, so a caller that never
/// thinks about the calendar still sees the pre-seasonal surface.
pub fn generate_chunk(pos: ChunkPos) -> ChunkBuffer {
    generate_chunk_seeded(pos, DEFAULT_WORLD_SEED, Season::Summer)
}

/// Surface block of a non-beach plains/forest column in `season`.
///
/// Winter paints the seasonal snow blanket; every other season keeps Grass.
/// Beach Sand, Ocean Sand and the Mountains altitude rule never call this, so
/// the seasonal blanket and the season-blind altitude snow cap stay
/// distinguishable by construction.
#[inline]
fn seasonal_land_surface(season: Season) -> u16 {
    match season {
        Season::Winter => BLOCK_SNOW,
        Season::Spring | Season::Summer | Season::Autumn => BLOCK_GRASS,
    }
}

/// Generates `pos` for `seed` under the given calendar `season`.
///
/// `season` is a plain value parameter — no manager state, no clock reads — so
/// generation stays a pure function of `(pos, seed, season)`. It changes
/// exactly two things:
///
/// 1. the surface skin of non-beach plains/forest land (`Snow` in Winter,
///    `Grass` otherwise), and
/// 2. the oak leaf canopy (Winter sheds it while keeping the trunks; every
///    other season keeps the full canopy).
///
/// Heights, biomes, subsurface skin, caves, ores, bedrock, water fill and the
/// season-blind Mountain altitude snow cap are untouched, so a seasonal
/// blanket can never be confused with the altitude cap.
pub fn generate_chunk_seeded(pos: ChunkPos, seed: u32, season: Season) -> ChunkBuffer {
    generate_chunk_seeded_with(pos, seed, season, &[])
}

/// Generates `pos` for `seed` under `calendar season` and the given pack
/// worldgen `rules`.
///
/// This is the single generation body; [`generate_chunk_seeded`] is the
/// empty-rule wrapper, so a world with no pack rules reproduces the pre-M010
/// generator byte-for-byte by construction. Rules run in authored order after
/// the built-in passes:
///
/// 1. built-in terrain, caves and trees
/// 2. built-in coal/iron veins (own stream, always byte-identical)
/// 3. pack ore veins (one independent stream per rule)
/// 4. built-in surface ruins
/// 5. pack surface patches (one independent stream per rule)
///
/// Both early returns (high sky and all-above-terrain uniform Air) and
/// `try_compress` stay exactly as they were, so an empty rule slice is not
/// merely equivalent but work-free.
pub fn generate_chunk_seeded_with(
    pos: ChunkPos,
    seed: u32,
    season: Season,
    rules: &[WorldgenRule],
) -> ChunkBuffer {
    generate_chunk_with_context(
        pos,
        seed,
        season,
        &WorldGenContext {
            rules,
            villages: None,
            spawn_column: None,
        },
    )
}

/// Everything generation depends on besides `(pos, seed, season)`.
///
/// `rules` is the pack worldgen stack (empty for vanilla worlds), `villages`
/// is the optional accepted village catalog (empty/`None` reproduces the
/// pre-M015 generator byte-for-byte) and `spawn_column` lets a caller that
/// already knows the spawn skip the bounded [`crate::spawn::find_spawn_column`]
/// search when the village planner needs it.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorldGenContext<'a> {
    pub rules: &'a [WorldgenRule],
    pub villages: Option<&'a VillageCatalog>,
    pub spawn_column: Option<(i32, i32)>,
}

/// Whether a chunk whose base starts at `base_wy` is entirely above every
/// possible cell: terrain, trees and (when the chunk may hold a village) the
/// full prefab height cap above the highest legal village surface.
///
/// Extracted so the tests can pin the exact formulas instead of re-deriving
/// them. With `village_candidate = false` this is exactly the pre-M015 sky
/// shortcut, so empty-catalog identity and the pinned `--demo-worldgen`
/// digests hold by construction.
#[inline]
fn sky_is_air(base_wy: i32, village_candidate: bool) -> bool {
    let headroom = if village_candidate {
        PREFAB_CAPS.max_y as i32
    } else {
        TREE_CANOPY_HEADROOM
    };
    base_wy > MAX_TERRAIN_HEIGHT + headroom
}

/// Whether a chunk is entirely above its own locally sampled terrain plus tree
/// headroom.
///
/// Never applies to a village-candidate chunk: a village footprint may span
/// chunks and `base_y` is the maximum over the whole footprint, so a chunk's
/// local `max_h` can sit below a village cell that lives in this chunk (the
/// prefab's base is the footprint maximum, not this chunk's maximum). Skipping
/// the shortcut there is what prevents silent clipping.
#[inline]
fn above_local_terrain_is_air(base_wy: i32, max_h: i32, village_candidate: bool) -> bool {
    !village_candidate && base_wy > max_h + TREE_CANOPY_HEADROOM
}

/// The single generation body: terrain, caves, trees, ores, ruins, pack
/// patches and — when a catalog is supplied — the last-pass village assembly.
///
/// [`generate_chunk_seeded_with`] is the no-village wrapper, so an empty rule
/// slice and an absent catalog stay byte-identical to the pre-M015 generator.
pub fn generate_chunk_with_context(
    pos: ChunkPos,
    seed: u32,
    season: Season,
    ctx: &WorldGenContext<'_>,
) -> ChunkBuffer {
    let base_wx = pos.x * CHUNK_SIZE as i32;
    let base_wy = pos.y * CHUNK_SIZE as i32;
    let base_wz = pos.z * CHUNK_SIZE as i32;

    // Conservative gate computed before either early return: a chunk can only
    // skip the block loop when it provably holds no village cell either.
    let village_candidate = ctx
        .villages
        .is_some_and(|catalog| !catalog.is_empty() && catalog.chunk_may_hold_village(pos, seed));

    // Fast path for high sky chunks. Derived from MAX_TERRAIN_HEIGHT so the
    // envelope and this shortcut cannot silently desync: everything above all
    // possible terrain plus tree headroom is Air. Village candidates raise the
    // bound to the prefab height cap because a village can stand on the
    // highest legal surface height.
    if sky_is_air(base_wy, village_candidate) {
        return ChunkBuffer::uniform(BLOCK_AIR);
    }

    // Precompute column heights and biomes in 32x32. The biome is derived from
    // the same height (plus the forest mask), so one pass serves both.
    let mut heights = [[0i32; CHUNK_SIZE]; CHUNK_SIZE];
    let mut biomes = [[Biome::Plains; CHUNK_SIZE]; CHUNK_SIZE];
    let mut max_h = i32::MIN;
    for (lz, row) in heights.iter_mut().enumerate() {
        for (lx, height) in row.iter_mut().enumerate() {
            let wx = base_wx + lx as i32;
            let wz = base_wz + lz as i32;
            let h = terrain_height_at_seeded(wx, wz, seed);
            *height = h;
            max_h = max_h.max(h);
            biomes[lz][lx] = biome_for_height(h, forest_noise(wx, wz, seed));
        }
    }

    // If the entire chunk is well above max terrain height + tree headroom.
    // Skipped for village candidates: a footprint can span chunks and `base_y`
    // is the maximum over the whole footprint, so this chunk's local `max_h`
    // can be below a village cell that belongs here.
    if above_local_terrain_is_air(base_wy, max_h, village_candidate) {
        return ChunkBuffer::uniform(BLOCK_AIR);
    }
    // Deep chunks must still flow through the block loop so caves (and,
    // later, ores) can carve them; no uniform-stone fast path here.
    let cave_field = CaveField::new(pos, seed);

    let mut chunk = ChunkBuffer::uniform(BLOCK_AIR);
    for (lz, row) in heights.iter().enumerate() {
        for (lx, &h) in row.iter().enumerate() {
            let wx = base_wx + lx as i32;
            let wz = base_wz + lz as i32;
            let biome = biomes[lz][lx];
            let beach = h <= SEA_LEVEL + BEACH_BAND;

            for ly in 0..CHUNK_SIZE {
                let wy = base_wy + ly as i32;
                let block = if wy <= -32 {
                    BLOCK_BEDROCK
                } else if wy < h - 3 {
                    BLOCK_STONE
                } else if wy < h {
                    // Subsurface skin, three blocks deep. It is never carved
                    // (caves/ores stop at h - 4), so it is also the seal that
                    // keeps future water columns from draining into caves.
                    match biome {
                        Biome::Mountains => BLOCK_STONE,
                        Biome::Ocean => BLOCK_SAND,
                        Biome::Plains | Biome::Forest => {
                            if beach {
                                BLOCK_SAND
                            } else {
                                BLOCK_DIRT
                            }
                        }
                    }
                } else if wy == h {
                    match biome {
                        Biome::Mountains => {
                            if h >= SNOW_CAP_HEIGHT {
                                BLOCK_SNOW
                            } else {
                                BLOCK_STONE
                            }
                        }
                        Biome::Ocean => BLOCK_SAND,
                        Biome::Plains | Biome::Forest => {
                            if beach {
                                // Beaches stay Sand in every season: the
                                // seasonal blanket is land-only.
                                BLOCK_SAND
                            } else {
                                seasonal_land_surface(season)
                            }
                        }
                    }
                } else if h < SEA_LEVEL && wy <= SEA_LEVEL {
                    // Ocean basin: static worldgen water from just above the
                    // seabed skin (wy > h) up to the named sea level. The
                    // three-block skin below stays Sand and cave carving stops
                    // at h - 4, so the seabed is a real seal.
                    BLOCK_WATER
                } else {
                    BLOCK_AIR
                };

                if is_cave_carvable(block, wy, h)
                    && cave_field.may_carve()
                    && cave_field.is_carved(lx, ly, lz)
                {
                    continue;
                }

                if block != BLOCK_AIR {
                    chunk.set(lx, ly, lz, block);
                }
            }

            // Deterministic oak trees inside the safe interior (lx, lz in
            // 3..29) so canopies never cross a chunk border. The gate is the
            // pre-season terrain class (`Plains`/`Forest` + the Grass height
            // band in `oak_tree_at`), never the post-season surface block, so
            // Winter keeps its trunks and branches and only the leaves are
            // seasonal (D088). Beaches, seabeds and mountain stone stay out of
            // the band by construction, exactly as before.
            //
            // Every chunk layer whose vertical span intersects the tree emits
            // the cells it owns. A tree near the shore can top the first
            // chunk-Y boundary (wy 32); gating on the surface layer alone used
            // to clip exactly those trees halfway. `tree_top` is derived from
            // the same model constants as the emitted cells, so this
            // intersection test can never skip a branch or canopy top.
            let tree_top = h + TREE_MAX_HEIGHT_OVER_SURFACE;
            if tree_top >= base_wy && h + 1 < base_wy + CHUNK_SIZE as i32 {
                if let Some(tree) = oak_tree_at(wx, wz, h, biome, seed) {
                    for ty in (h + 1)..=(h + tree.trunk_height) {
                        let ly_tree = ty - base_wy;
                        if (0..CHUNK_SIZE as i32).contains(&ly_tree) {
                            chunk.set(lx, ly_tree as usize, lz, BLOCK_OAK_LOG);
                        }
                    }

                    // Deterministic branches on the upper trunk. A branch may
                    // replace an existing leaf but never terrain, water, Snow or
                    // another structure cell. Admitting leaves keeps the log
                    // skeleton identical in every season regardless of the
                    // column iteration order (Winter has no leaves to collide
                    // with, so an Air-only rule would make winter trees grow
                    // extra branches).
                    for (bx, by, bz) in tree.branch_cells(wx, wz, h) {
                        let ly_b = by - base_wy;
                        if !(0..CHUNK_SIZE as i32).contains(&ly_b) {
                            continue;
                        }
                        let tlx = bx.rem_euclid(CHUNK_SIZE as i32) as usize;
                        let tlz = bz.rem_euclid(CHUNK_SIZE as i32) as usize;
                        if matches!(
                            chunk.get(tlx, ly_b as usize, tlz),
                            BLOCK_AIR | BLOCK_OAK_LEAVES
                        ) {
                            chunk.set(tlx, ly_b as usize, tlz, BLOCK_OAK_LOG);
                        }
                    }

                    // Only leaves are seasonal: Spring, Summer and Autumn keep
                    // the full canopy, Winter stays bare. The trunk and branches
                    // above survive every season.
                    if season != Season::Winter {
                        let trunk_h = tree.trunk_height;
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
                                    if chunk.get(tlx, ly_c as usize, tlz) == BLOCK_AIR {
                                        chunk.set(tlx, ly_c as usize, tlz, BLOCK_OAK_LEAVES);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    place_ore_veins(&mut chunk, &heights, pos, seed);
    // Pack ore runs after built-in coal/iron and before ruins, each rule on
    // its own stream; the built-in ore cells are therefore untouched.
    place_pack_ore_veins(&mut chunk, &heights, pos, seed, ctx.rules);
    // Third structure pass: after ores, so a ruin can never be carved by the
    // ore pass, and after trees, so the tree-overlap policy can see the actual
    // OakLog cells it refuses to overwrite.
    place_surface_ruins(&mut chunk, &heights, &biomes, pos, seed);
    // Pack patches run last and only touch `wy == h`, so no structure or
    // canopy cell can ever be rewritten.
    place_pack_surface_patches(&mut chunk, &heights, &biomes, pos, seed, ctx.rules);
    // Village assembly is the true last pass: it overwrites pack patches,
    // ruin shells, trees and the raw terrain surface with the plan's cells, so
    // a village is never clipped by an earlier pass or the uniform-Air
    // shortcuts above. Season-independent by construction.
    if let Some(catalog) = ctx.villages {
        place_village_cells(&mut chunk, pos, seed, catalog, ctx.spawn_column);
    }
    chunk.try_compress();
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::{biome_at_seeded, MOUNTAIN_HEIGHT_THRESHOLD};
    use crate::block::{BLOCK_AIR, BLOCK_CRAFTING_TABLE};
    use crate::height::MIN_TERRAIN_HEIGHT;
    use crate::prefab::{Prefab, PrefabBlock};
    use crate::ruin::{
        ruin_at, ruin_at_seeded, ruin_covers, ruin_loot, ruin_loot_chest_at, ruin_loot_chest_pos,
        RuinSpec, RUIN_MAX_HEIGHT, RUIN_MIN_HEIGHT, RUIN_WALL_MAX_HEIGHT,
    };
    use crate::spawn::{find_spawn_column, is_spawn_safe, SPAWN_TREE_CLEARANCE};
    use crate::tree::{
        tree_trunk_height, OakTree, TREE_CANOPY_TOP_OVER_TRUNK, TREE_MAX_HEIGHT, TREE_MIN_HEIGHT,
    };
    use crate::village::{
        village_cell_at, village_plan_with_spawn, VillageCatalog, VillageCell, VillageDefinition,
        VillagePlan,
    };
    use glam::IVec3;
    use std::collections::HashSet;
    use std::path::Path;

    /// FNV-1a 64 over every cell in `linear_index` order (x fastest, then z,
    /// then y), hashing each raw block id as a little-endian `u16`.
    fn fnv1a64_buffer(buffer: &ChunkBuffer) -> (Option<u16>, usize, u64) {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut non_air = 0usize;
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    debug_assert_eq!(linear_index(x, y, z), y * 1024 + z * 32 + x);
                    let block = buffer.get(x, y, z);
                    if block != BLOCK_AIR {
                        non_air += 1;
                    }
                    for byte in block.to_le_bytes() {
                        hash ^= byte as u64;
                        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                }
            }
        }
        (buffer.is_uniform(), non_air, hash)
    }

    #[test]
    fn pinned_terrain_generation_matches_pre_m010_baseline() {
        let cases = [
            (ChunkPos::new(0, -1, 0), DEFAULT_WORLD_SEED, Season::Summer),
            (ChunkPos::new(0, 1, 0), 1337, Season::Winter),
            (ChunkPos::new(-1, 1, 2), 424_242, Season::Summer),
        ];
        // (uniform, non_air_cells, FNV-1a 64 digest) captured from the
        // unmodified pre-M010 generator. These are the behavior-preservation
        // gate for the crate move: any terrain drift must fail here, not
        // merely differ from a second run of the same code.
        let expected: [(Option<u16>, usize, u64); 3] = [
            (None, 26_913, 5_429_992_561_724_371_152),
            (None, 20_878, 8_773_073_591_056_755_479),
            (Some(BLOCK_AIR), 0, 16_934_947_671_703_429_925),
        ];

        for ((pos, seed, season), expected) in cases.into_iter().zip(expected) {
            let chunk = generate_chunk_seeded(pos, seed, season);
            let actual = fnv1a64_buffer(&chunk);
            assert_eq!(
                actual, expected,
                "pre-M010 terrain baseline drifted for pos {pos:?}, seed {seed}, {season:?}"
            );
        }
    }

    #[test]
    fn test_cave_generation_deterministic_full_chunks() {
        for seed in [DEFAULT_WORLD_SEED, 424_242] {
            for cy in -1..=1 {
                let pos = ChunkPos::new(0, cy, 0);
                let first = generate_chunk_seeded(pos, seed, Season::Summer);
                let second = generate_chunk_seeded(pos, seed, Season::Summer);
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
                    let chunk =
                        generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
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
                                if wy > -32 && wy < h - 3 && chunk.get(lx, ly, lz) == BLOCK_AIR {
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
            h >= MOUNTAIN_HEIGHT_THRESHOLD,
            "height search did not reach mountain terrain (max h = {h}, threshold = {MOUNTAIN_HEIGHT_THRESHOLD})"
        );

        let pos = ChunkPos::new(
            wx.div_euclid(CHUNK_SIZE as i32),
            h.div_euclid(CHUNK_SIZE as i32),
            wz.div_euclid(CHUNK_SIZE as i32),
        );
        let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
        let lx = wx.rem_euclid(CHUNK_SIZE as i32) as usize;
        let ly = h.rem_euclid(CHUNK_SIZE as i32) as usize;
        let lz = wz.rem_euclid(CHUNK_SIZE as i32) as usize;
        assert_ne!(
            chunk.get(lx, ly, lz),
            BLOCK_AIR,
            "cave punched a hole in the mountain surface at ({wx}, {h}, {wz})"
        );
    }

    #[test]
    fn test_bedrock_floor_preserved_in_deep_chunk() {
        let chunk =
            generate_chunk_seeded(ChunkPos::new(0, -1, 0), DEFAULT_WORLD_SEED, Season::Summer);
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                assert_eq!(
                    chunk.get(lx, 0, lz),
                    BLOCK_BEDROCK,
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
                    let chunk =
                        generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
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
                                    BLOCK_AIR,
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
        assert!(!is_cave_carvable(BLOCK_STONE, h, h));
        assert!(!is_cave_carvable(BLOCK_STONE, h - 1, h));
        assert!(!is_cave_carvable(BLOCK_STONE, h - 2, h));
        assert!(!is_cave_carvable(BLOCK_STONE, h - 3, h));
        assert!(is_cave_carvable(BLOCK_STONE, h - 4, h));
        assert!(is_cave_carvable(BLOCK_STONE, 0, h));
        assert!(!is_cave_carvable(BLOCK_STONE, -32, h));
        assert!(!is_cave_carvable(BLOCK_BEDROCK, h - 4, h));
        assert!(!is_cave_carvable(BLOCK_AIR, h - 4, h));
        assert!(!is_cave_carvable(BLOCK_DIRT, h - 4, h));
    }

    #[test]
    fn test_terrain_generation_deterministic_and_layered() {
        let c1 = generate_chunk(ChunkPos::new(0, 0, 0));
        let c2 = generate_chunk(ChunkPos::new(0, 0, 0));
        assert_eq!(c1.get(10, 5, 10), c2.get(10, 5, 10));

        let bottom = generate_chunk(ChunkPos::new(0, -1, 0));
        assert_eq!(bottom.get(0, 0, 0), BLOCK_BEDROCK);

        let sky = generate_chunk(ChunkPos::new(0, 4, 0));
        assert_eq!(sky.is_uniform(), Some(BLOCK_AIR));
        // The top chunk layer is only provably air while the envelope plus
        // tree headroom stays below its base wy; this pins the derivation.
        const {
            assert!(
                MAX_TERRAIN_HEIGHT + TREE_CANOPY_HEADROOM < 4 * CHUNK_SIZE as i32,
                "MAX_TERRAIN_HEIGHT drifted above the uniform-Air fast-path bound"
            );
        }
    }

    fn has_subsurface_air_neighbor(
        chunk: &ChunkBuffer,
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
            if chunk.get(nlx, nly, nlz) != BLOCK_AIR {
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
                        let first = generate_chunk_seeded(pos, seed, Season::Summer);
                        let second = generate_chunk_seeded(pos, seed, Season::Summer);
                        for lz in 0..CHUNK_SIZE {
                            for ly in 0..CHUNK_SIZE {
                                for lx in 0..CHUNK_SIZE {
                                    let a = first.get(lx, ly, lz);
                                    assert_eq!(
                                        a,
                                        second.get(lx, ly, lz),
                                        "seed {seed} chunk {pos:?} differs at ({lx}, {ly}, {lz})"
                                    );
                                    if matches!(a, BLOCK_COAL_ORE | BLOCK_IRON_ORE) {
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
                    let chunk =
                        generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                match chunk.get(lx, ly, lz) {
                                    BLOCK_COAL_ORE => coal += 1,
                                    BLOCK_IRON_ORE => iron += 1,
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
                    let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
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
                    let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                match chunk.get(lx, ly, lz) {
                                    BLOCK_COAL_ORE
                                        if has_subsurface_air_neighbor(
                                            &chunk, pos, lx, ly, lz, seed,
                                        ) =>
                                    {
                                        coal_exposed += 1;
                                    }
                                    BLOCK_IRON_ORE
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
                        let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
                        for lz in 0..CHUNK_SIZE {
                            for ly in 0..CHUNK_SIZE {
                                for lx in 0..CHUNK_SIZE {
                                    let block = chunk.get(lx, ly, lz);
                                    if matches!(block, BLOCK_COAL_ORE | BLOCK_IRON_ORE) {
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
        assert!(can_place_ore(BLOCK_STONE, h - 4, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_STONE, h - 3, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_STONE, h, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_AIR, h - 4, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_DIRT, h - 4, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_SAND, h - 4, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_BEDROCK, h - 4, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_OAK_LOG, h - 4, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_STONE, -32, h, BLOCK_COAL_ORE));

        // Coal depth band is 4..=36.
        assert!(can_place_ore(BLOCK_STONE, h - 4, h, BLOCK_COAL_ORE));
        assert!(can_place_ore(BLOCK_STONE, h - 36, h, BLOCK_COAL_ORE));
        assert!(!can_place_ore(BLOCK_STONE, h - 37, h, BLOCK_COAL_ORE));

        // Iron depth band is 12..=90; use a tall column to test the deep edge.
        let deep_h = 200;
        assert!(!can_place_ore(
            BLOCK_STONE,
            deep_h - 11,
            deep_h,
            BLOCK_IRON_ORE
        ));
        assert!(can_place_ore(
            BLOCK_STONE,
            deep_h - 12,
            deep_h,
            BLOCK_IRON_ORE
        ));
        assert!(can_place_ore(
            BLOCK_STONE,
            deep_h - 90,
            deep_h,
            BLOCK_IRON_ORE
        ));
        assert!(!can_place_ore(
            BLOCK_STONE,
            deep_h - 91,
            deep_h,
            BLOCK_IRON_ORE
        ));
        assert!(!can_place_ore(BLOCK_STONE, h - 4, h, BLOCK_IRON_ORE));
        assert!(!can_place_ore(BLOCK_STONE, h - 4, h, BLOCK_STONE));
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
                let first = generate_chunk_seeded(pos, seed, Season::Summer);
                let second = generate_chunk_seeded(pos, seed, Season::Summer);
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
                            if a == BLOCK_AIR && wy > -32 && wy < h - 3 {
                                carved_air += 1;
                            }
                            if matches!(a, BLOCK_COAL_ORE | BLOCK_IRON_ORE) {
                                ore_blocks += 1;
                            }
                        }
                    }
                }
            }
        }

        let mut seed_changed_any_block = false;
        for &pos in &positions {
            let first = generate_chunk_seeded(pos, DEFAULT_WORLD_SEED, Season::Summer);
            let second = generate_chunk_seeded(pos, 424_242, Season::Summer);
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
                    let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
                    let base_wx = cx * CHUNK_SIZE as i32;
                    let base_wy = cy * CHUNK_SIZE as i32;
                    let base_wz = cz * CHUNK_SIZE as i32;
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let block = chunk.get(lx, ly, lz);
                                if !matches!(block, BLOCK_COAL_ORE | BLOCK_IRON_ORE)
                                    || !has_subsurface_air_neighbor(&chunk, pos, lx, ly, lz, seed)
                                {
                                    continue;
                                }
                                let coord = (
                                    base_wx + lx as i32,
                                    base_wy + ly as i32,
                                    base_wz + lz as i32,
                                );
                                let slot = if block == BLOCK_COAL_ORE {
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
    /// intact, the surface skin (h, h-1..h-3) is never carved, and mountain
    /// stone tops (h >= MOUNTAIN_HEIGHT_THRESHOLD) are never punched through.
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
                    let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
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
                                        block, BLOCK_BEDROCK,
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
                                    BLOCK_AIR,
                                    "surface skin carved at world ({wx}, {wy}, {wz}) (h = {h})"
                                );
                            }
                            if h >= MOUNTAIN_HEIGHT_THRESHOLD && (base_wy..=top_wy).contains(&h) {
                                let ly = (h - base_wy) as usize;
                                let surface = chunk.get(lx, ly, lz);
                                assert_ne!(
                                    surface, BLOCK_AIR,
                                    "mountain-top surface carved at ({wx}, {h}, {wz})"
                                );
                                if h < SNOW_CAP_HEIGHT {
                                    assert_eq!(
                                        surface, BLOCK_STONE,
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
                h >= MOUNTAIN_HEIGHT_THRESHOLD,
                "height search did not reach mountain terrain (max h = {h}, threshold = {MOUNTAIN_HEIGHT_THRESHOLD})"
            );
            let pos = ChunkPos::new(
                wx.div_euclid(CHUNK_SIZE as i32),
                h.div_euclid(CHUNK_SIZE as i32),
                wz.div_euclid(CHUNK_SIZE as i32),
            );
            let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
            let lx = wx.rem_euclid(CHUNK_SIZE as i32) as usize;
            let ly = h.rem_euclid(CHUNK_SIZE as i32) as usize;
            let lz = wz.rem_euclid(CHUNK_SIZE as i32) as usize;
            assert_ne!(
                chunk.get(lx, ly, lz),
                BLOCK_AIR,
                "cave punched a hole in the mountain surface at ({wx}, {h}, {wz})"
            );
        } else {
            eprintln!(
                "acceptance invariants: checked {mountain_surface_checks} mountain-top surface cells"
            );
        }
    }

    // ---- Biome sampling helpers ---------------------------------------------

    /// Deterministic index for per-biome bookkeeping.
    fn biome_index(biome: Biome) -> usize {
        match biome {
            Biome::Plains => 0,
            Biome::Forest => 1,
            Biome::Mountains => 2,
            Biome::Ocean => 3,
        }
    }

    /// First `(wx, wz, height)` column of each biome inside the bounded,
    /// seed-pinned acceptance region.
    fn first_column_per_biome(seed: u32) -> [Option<(i32, i32, i32)>; 4] {
        let mut found = [None; 4];
        for wz in (-512..=512).step_by(4) {
            for wx in (-512..=512).step_by(4) {
                let idx = biome_index(biome_at_seeded(wx, wz, seed));
                if found[idx].is_none() {
                    found[idx] = Some((wx, wz, terrain_height_at_seeded(wx, wz, seed)));
                }
            }
        }
        found
    }

    /// Block at a world position for an explicit `season`; the chunk is
    /// regenerated per call, so callers sample a handful of blocks at most.
    fn generated_block_at(wx: i32, wy: i32, wz: i32, seed: u32, season: Season) -> u16 {
        let pos = ChunkPos::new(
            wx.div_euclid(CHUNK_SIZE as i32),
            wy.div_euclid(CHUNK_SIZE as i32),
            wz.div_euclid(CHUNK_SIZE as i32),
        );
        let chunk = generate_chunk_seeded(pos, seed, season);
        chunk.get(
            wx.rem_euclid(CHUNK_SIZE as i32) as usize,
            wy.rem_euclid(CHUNK_SIZE as i32) as usize,
            wz.rem_euclid(CHUNK_SIZE as i32) as usize,
        )
    }

    /// Integration: the block the player's feet occupy (surface + 1..=+4) is
    /// Air in the actually generated chunk, so no trunk, leaf, or water block
    /// sits inside the spawn body.
    #[test]
    fn test_spawn_column_body_is_air_in_the_generated_chunk() {
        let ch = CHUNK_SIZE as i32;
        for seed in [DEFAULT_WORLD_SEED, 12345, 4, 0x5EED_4A11] {
            let (wx, wz) = find_spawn_column(seed);
            let h = terrain_height_at_seeded(wx, wz, seed);
            for dy in 1..=4 {
                let wy = h + dy;
                let pos = ChunkPos::new(wx.div_euclid(ch), wy.div_euclid(ch), wz.div_euclid(ch));
                let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
                let block = chunk.get(
                    wx.rem_euclid(ch) as usize,
                    wy.rem_euclid(ch) as usize,
                    wz.rem_euclid(ch) as usize,
                );
                assert_eq!(
                    block, BLOCK_AIR,
                    "seed {seed} spawn body block at ({wx}, {wy}, {wz}) is {block:?}, not Air"
                );
            }
        }
    }

    /// Surface skin follows the biome: sand seabed and beaches, grass/dirt on
    /// plains and forest, stone (or snow high up) on mountains. The three-block
    /// subsurface band stays solid so caves never open the surface.
    #[test]
    fn test_surface_skin_matches_biome() {
        let seed = DEFAULT_WORLD_SEED;
        let found = first_column_per_biome(seed);
        for (idx, column) in found.iter().enumerate() {
            let (wx, wz, h) = column.unwrap_or_else(|| panic!("missing biome index {idx}"));
            let biome = biome_at_seeded(wx, wz, seed);
            let surface = generated_block_at(wx, h, wz, seed, Season::Summer);
            let expected = match biome {
                Biome::Ocean => BLOCK_SAND,
                Biome::Mountains if h >= SNOW_CAP_HEIGHT => BLOCK_SNOW,
                Biome::Mountains => BLOCK_STONE,
                Biome::Plains | Biome::Forest if h <= SEA_LEVEL + BEACH_BAND => BLOCK_SAND,
                Biome::Plains | Biome::Forest => BLOCK_GRASS,
            };
            assert_eq!(
                surface, expected,
                "{biome:?} column ({wx}, {h}, {wz}) has surface {surface:?}, expected {expected:?}"
            );
            for wy in (h - 3)..h {
                assert_ne!(
                    generated_block_at(wx, wy, wz, seed, Season::Summer),
                    BLOCK_AIR,
                    "subsurface skin at ({wx}, {wy}, {wz}) is Air"
                );
            }
        }
    }

    /// Water fill invariant: every ocean column is filled with static Water
    /// from just above the seabed up to SEA_LEVEL, no land column ever holds
    /// water, and water never appears below the seabed or above sea level.
    ///
    /// Seabeds can now sit below wy 0 after the ocean deepening, so the scan
    /// covers both chunks of the 64-block vertical band that can hold water.
    #[test]
    fn test_ocean_basins_are_filled_with_water_up_to_sea_level() {
        const BOTTOM_LAYER: i32 = -1;
        let seed = DEFAULT_WORLD_SEED;
        let mut water_blocks = 0usize;
        let mut ocean_columns = 0usize;
        let mut land_columns = 0usize;

        for cx in -2..=2 {
            for cz in -2..=2 {
                let chunks: [ChunkBuffer; 2] = [BOTTOM_LAYER, 0].map(|cy| {
                    generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer)
                });
                let base_wx = cx * CHUNK_SIZE as i32;
                let base_wz = cz * CHUNK_SIZE as i32;
                let block_at = |wy: i32, lx: usize, lz: usize| {
                    let cy = wy.div_euclid(CHUNK_SIZE as i32);
                    let ly = wy.rem_euclid(CHUNK_SIZE as i32) as usize;
                    let layer = (cy - BOTTOM_LAYER) as usize;
                    chunks[layer].get(lx, ly, lz)
                };

                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let wx = base_wx + lx as i32;
                        let wz = base_wz + lz as i32;
                        let h = terrain_height_at_seeded(wx, wz, seed);
                        let is_ocean = h < SEA_LEVEL;
                        if is_ocean {
                            ocean_columns += 1;
                        } else {
                            land_columns += 1;
                        }

                        // The whole water band is Water, block for block.
                        for wy in (h + 1)..=SEA_LEVEL {
                            assert_eq!(
                                block_at(wy, lx, lz),
                                BLOCK_WATER,
                                "ocean column ({wx}, {wz}) is not filled at wy {wy} (h = {h})"
                            );
                            water_blocks += 1;
                        }
                        if is_ocean {
                            assert_eq!(
                                block_at(SEA_LEVEL + 1, lx, lz),
                                BLOCK_AIR,
                                "ocean column ({wx}, {wz}) has a block above SEA_LEVEL"
                            );
                        }

                        // Nothing outside the band may be Water: not below the
                        // seabed, not inside land, and not above sea level.
                        for (layer_idx, chunk) in chunks.iter().enumerate() {
                            let base_wy = (layer_idx as i32 + BOTTOM_LAYER) * CHUNK_SIZE as i32;
                            for ly in 0..CHUNK_SIZE {
                                let wy = base_wy + ly as i32;
                                if chunk.get(lx, ly, lz) != BLOCK_WATER {
                                    continue;
                                }
                                assert!(
                                    is_ocean,
                                    "water in land column ({wx}, {wy}, {wz}) with h = {h}"
                                );
                                assert!(
                                    (h + 1..=SEA_LEVEL).contains(&wy),
                                    "water at ({wx}, {wy}, {wz}) outside seabed..sea band \
                                     (h = {h}, SEA_LEVEL = {SEA_LEVEL})"
                                );
                            }
                        }
                    }
                }
            }
        }

        assert!(ocean_columns > 0, "scan found no ocean columns to fill");
        assert!(land_columns > 0, "scan found no land columns");
        assert!(water_blocks > 0, "scan found no Water blocks");
        eprintln!(
            "water fill scan: {ocean_columns} ocean columns, {land_columns} land columns, \
             {water_blocks} Water blocks"
        );
    }

    /// The seabed seal under a water column: the three-block subsurface skin
    /// and the surface stay Sand. Caves only carve strictly below `h - 3`, so
    /// they can never puncture the skin and drain a basin into a cave.
    ///
    /// Deepened basins can have their seabed below wy 0, so the scan checks
    /// both chunk layers that can own the skin.
    #[test]
    fn test_water_columns_keep_seabed_skin_and_caves_below_it() {
        const BOTTOM_LAYER: i32 = -1;
        let seed = DEFAULT_WORLD_SEED;
        let mut checked = 0usize;
        for cx in -2..=2 {
            for cz in -2..=2 {
                let chunks: [ChunkBuffer; 2] = [BOTTOM_LAYER, 0].map(|cy| {
                    generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer)
                });
                let base_wx = cx * CHUNK_SIZE as i32;
                let base_wz = cz * CHUNK_SIZE as i32;
                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let wx = base_wx + lx as i32;
                        let wz = base_wz + lz as i32;
                        let h = terrain_height_at_seeded(wx, wz, seed);
                        if h >= SEA_LEVEL {
                            continue;
                        }
                        checked += 1;
                        assert!(
                            (MIN_TERRAIN_HEIGHT..SEA_LEVEL).contains(&h),
                            "ocean surface h {h} outside the water envelope"
                        );
                        for wy in (h - 3)..=h {
                            let cy = wy.div_euclid(CHUNK_SIZE as i32);
                            let ly = wy.rem_euclid(CHUNK_SIZE as i32) as usize;
                            let layer = (cy - BOTTOM_LAYER) as usize;
                            assert_eq!(
                                chunks[layer].get(lx, ly, lz),
                                BLOCK_SAND,
                                "seabed skin at ({wx}, {wy}, {wz}) is not Sand (h = {h})"
                            );
                            assert!(
                                !is_cave_carvable(BLOCK_SAND, wy, h),
                                "a cave must never carve the seabed skin at wy {wy}"
                            );
                        }
                    }
                }
            }
        }
        assert!(checked > 0, "scan found no water columns to guard");
        eprintln!("seabed skin scan: {checked} water columns kept their three-block seal");
    }

    /// Deepened basins can put the seabed below wy 0 while the water column
    /// reaches up to sea level. This is the layer-boundary case: the surface
    /// and skin live in chunk layer -1, the top of the water in layer 0, and
    /// the fill must be continuous across the wy 0 seam.
    #[test]
    fn test_deep_ocean_column_crosses_the_zero_layer_seam() {
        const BOTTOM_LAYER: i32 = -1;
        let seed = DEFAULT_WORLD_SEED;

        // Find a column whose water starts below wy 0, so the seam is really
        // exercised (the deepening guarantees the deepest basins reach -2).
        let mut deepest = (0, 0, i32::MAX);
        for wz in (-1024..=1024).step_by(8) {
            for wx in (-1024..=1024).step_by(8) {
                let h = terrain_height_at_seeded(wx, wz, seed);
                if h < deepest.2 {
                    deepest = (wx, wz, h);
                }
            }
        }
        let (wx, wz, h) = deepest;
        assert!(
            h + 1 < 0,
            "seed {seed} has no ocean column whose water starts below wy 0 (deepest h {h})"
        );
        assert!(
            (MIN_TERRAIN_HEIGHT..SEA_LEVEL).contains(&h),
            "deepest ocean h {h} outside the water envelope"
        );

        let chunks: [ChunkBuffer; 2] = [BOTTOM_LAYER, 0].map(|cy| {
            generate_chunk_seeded(
                ChunkPos::new(
                    wx.div_euclid(CHUNK_SIZE as i32),
                    cy,
                    wz.div_euclid(CHUNK_SIZE as i32),
                ),
                seed,
                Season::Summer,
            )
        });
        let lx = wx.rem_euclid(CHUNK_SIZE as i32) as usize;
        let lz = wz.rem_euclid(CHUNK_SIZE as i32) as usize;
        let block_at = |wy: i32| {
            let cy = wy.div_euclid(CHUNK_SIZE as i32);
            let ly = wy.rem_euclid(CHUNK_SIZE as i32) as usize;
            chunks[(cy - BOTTOM_LAYER) as usize].get(lx, ly, lz)
        };

        for wy in (h - 3)..=h {
            assert_eq!(
                block_at(wy),
                BLOCK_SAND,
                "deep seabed skin at ({wx}, {wy}, {wz}) is not Sand (h = {h})"
            );
        }
        for wy in (h + 1)..=SEA_LEVEL {
            assert_eq!(
                block_at(wy),
                BLOCK_WATER,
                "deep ocean column ({wx}, {wz}) is not filled at wy {wy} (h = {h})"
            );
        }
        assert_eq!(
            block_at(SEA_LEVEL + 1),
            BLOCK_AIR,
            "deep ocean column has a block above SEA_LEVEL"
        );
        eprintln!(
            "deep ocean seam scan: column ({wx}, {wz}) h {h} fills {} water blocks across the wy 0 seam",
            SEA_LEVEL - h
        );
    }

    /// Water generation is a pure function of position + seed: two repeats of
    /// the same ocean chunk are byte-identical, including the water volume.
    #[test]
    fn test_water_generation_is_byte_identical_across_repeats() {
        let seed = DEFAULT_WORLD_SEED;
        let mut found_water = false;
        for cx in -2..=2 {
            for cz in -2..=2 {
                let pos = ChunkPos::new(cx, 0, cz);
                let first = generate_chunk_seeded(pos, seed, Season::Summer);
                let second = generate_chunk_seeded(pos, seed, Season::Summer);
                for lz in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let a = first.get(lx, ly, lz);
                            assert_eq!(
                                a,
                                second.get(lx, ly, lz),
                                "seed {seed} chunk {pos:?} differs at ({lx}, {ly}, {lz})"
                            );
                            found_water |= a == BLOCK_WATER;
                        }
                    }
                }
            }
        }
        assert!(
            found_water,
            "repeat scan never encountered the water volume"
        );
    }

    /// First column of `biome` whose surface is high enough for trees (above
    /// the beach band), so the density comparison is not sampled on a coast.
    fn first_tree_column(seed: u32, biome: Biome) -> (i32, i32, i32) {
        for wz in (-512..=512).step_by(4) {
            for wx in (-512..=512).step_by(4) {
                if biome_at_seeded(wx, wz, seed) != biome {
                    continue;
                }
                let h = terrain_height_at_seeded(wx, wz, seed);
                if (TREE_MIN_HEIGHT..=TREE_MAX_HEIGHT).contains(&h) {
                    return (wx, wz, h);
                }
            }
        }
        panic!("no {biome:?} column inside the tree height band");
    }

    /// Count oak logs around a chunk and assert the per-column placement rule.
    ///
    /// Every log column must be either a valid trunk column (the pure
    /// `oak_tree_at` gate holds, its lowest log sits exactly on `h + 1`, and its
    /// Summer surface is Grass) or a one-block branch of an adjacent valid tree
    /// (D088). No free-floating OakLog is allowed, and no log may grow on a
    /// column outside the Plains/Forest tree band.
    fn scan_region_for_log_rule(cx: i32, cz: i32, radius: i32, seed: u32) -> usize {
        use std::collections::HashMap;
        let mut logs = 0usize;
        // (wx, wz) -> lowest log wy seen in that column.
        let mut log_columns: HashMap<(i32, i32), i32> = HashMap::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                for cy in 0..=1 {
                    let chunk = generate_chunk_seeded(
                        ChunkPos::new(cx + dx, cy, cz + dz),
                        seed,
                        Season::Summer,
                    );
                    let base_wy = cy * CHUNK_SIZE as i32;
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                if chunk.get(lx, ly, lz) != BLOCK_OAK_LOG {
                                    continue;
                                }
                                logs += 1;
                                let wx = (cx + dx) * CHUNK_SIZE as i32 + lx as i32;
                                let wz = (cz + dz) * CHUNK_SIZE as i32 + lz as i32;
                                let wy = base_wy + ly as i32;
                                let slot = log_columns.entry((wx, wz)).or_insert(wy);
                                *slot = (*slot).min(wy);
                            }
                        }
                    }
                }
            }
        }

        let mut trunk_columns_checked = 0usize;
        for (&(wx, wz), &lowest_log) in &log_columns {
            let biome = biome_at_seeded(wx, wz, seed);
            let h = terrain_height_at_seeded(wx, wz, seed);
            if oak_tree_at(wx, wz, h, biome, seed).is_some() {
                assert_eq!(
                    lowest_log,
                    h + 1,
                    "lowest oak log at ({wx}, {lowest_log}, {wz}) is not the trunk base h+1 (h = {h})"
                );
                // Bound the surface-block regeneration sample: the pure gate
                // above already proves the height/biome rule; this re-checks
                // the actual post-season surface block.
                if trunk_columns_checked < 40 {
                    assert_eq!(
                        generated_block_at(wx, h, wz, seed, Season::Summer),
                        BLOCK_GRASS,
                        "oak log grows out of a non-Grass surface at ({wx}, {h}, {wz})"
                    );
                    trunk_columns_checked += 1;
                }
                continue;
            }

            // Not a trunk column: the only other legal log is a branch of a
            // tree rooted in a neighbouring column (branches are one block
            // out, so dx/dz are in -1..=1).
            let mut explained = false;
            'neighbours: for ndz in -1..=1 {
                for ndx in -1..=1 {
                    if ndx == 0 && ndz == 0 {
                        continue;
                    }
                    let (sx, sz) = (wx + ndx, wz + ndz);
                    let sh = terrain_height_at_seeded(sx, sz, seed);
                    let sb = biome_at_seeded(sx, sz, seed);
                    let Some(source) = oak_tree_at(sx, sz, sh, sb, seed) else {
                        continue;
                    };
                    if source
                        .branch_cells(sx, sz, sh)
                        .any(|(bx, by, bz)| (bx, by, bz) == (wx, lowest_log, wz))
                    {
                        explained = true;
                        break 'neighbours;
                    }
                }
            }
            assert!(
                explained,
                "oak log at ({wx}, {lowest_log}, {wz}) is neither a trunk base nor a branch of an adjacent tree (h {h}, biome {biome:?})"
            );
        }
        if !log_columns.is_empty() {
            assert!(
                trunk_columns_checked > 0,
                "logs were found but no trunk column was sampled, so the log rule was never exercised"
            );
        }
        logs
    }

    /// Trees follow biome density: forest is dense, plains sparse, and no trunk
    /// ever appears on sand beaches/seabeds or mountain stone. Branches only
    /// ever extend one block from a valid trunk (D088).
    #[test]
    fn test_tree_density_follows_biome_and_surface_block() {
        let seed = DEFAULT_WORLD_SEED;
        let forest = first_tree_column(seed, Biome::Forest);
        let plains = first_tree_column(seed, Biome::Plains);
        let found = first_column_per_biome(seed);
        let ocean = found[biome_index(Biome::Ocean)].expect("seed must have ocean");
        let mountains = found[biome_index(Biome::Mountains)].expect("seed must have mountains");

        let chunk_of = |(wx, wz, _h): (i32, i32, i32)| {
            (
                wx.div_euclid(CHUNK_SIZE as i32),
                wz.div_euclid(CHUNK_SIZE as i32),
            )
        };
        let (fcx, fcz) = chunk_of(forest);
        let (pcx, pcz) = chunk_of(plains);
        let (ocx, ocz) = chunk_of(ocean);
        let (mcx, mcz) = chunk_of(mountains);

        let forest_logs = scan_region_for_log_rule(fcx, fcz, 1, seed);
        let plains_logs = scan_region_for_log_rule(pcx, pcz, 1, seed);
        assert!(
            forest_logs > 0,
            "forest region produced no oak logs (dense forest is empty)"
        );
        assert!(
            forest_logs > plains_logs,
            "forest density {forest_logs} not above plains density {plains_logs}"
        );

        // Scan ocean and mountain regions too; the helper asserts the per-column
        // gate for every log it finds (these regions may contain land chunks).
        let _ = scan_region_for_log_rule(ocx, ocz, 1, seed);
        let _ = scan_region_for_log_rule(mcx, mcz, 1, seed);

        // The selected ocean and mountain columns themselves are never log
        // columns: their surface block is Sand / Stone, never Grass.
        for (wx, h, wz, biome) in [
            (ocean.0, ocean.2, ocean.1, Biome::Ocean),
            (mountains.0, mountains.2, mountains.1, Biome::Mountains),
        ] {
            let surface = generated_block_at(wx, h, wz, seed, Season::Summer);
            assert_ne!(
                surface, BLOCK_GRASS,
                "{biome:?} column ({wx}, {h}, {wz}) has a Grass surface"
            );
        }
    }

    // ---- Surface ruins (S07) ------------------------------------------------

    /// ChunkBuffer radius of the ruin acceptance region (7x7 chunks, three vertical
    /// layers so every ruin cell up to `surface + 4` is covered). Sized from
    /// the 1/512 per-anchor gate: ~784 interior anchors per chunk, so this
    /// region yields dozens of ruins and the non-zero bounds below are not
    /// flaky.
    const RUIN_SCAN_RADIUS: i32 = 3;

    /// Lower bound on the distinct ruins the loot-seam acceptance scan must
    /// find before "every ruin returns no loot" means anything.
    ///
    /// Derived from the same envelope: `RUIN_SCAN_RADIUS` covers 49 chunks, ~784
    /// interior anchors each, so the 1/512 gate passes ~75 anchors and roughly
    /// half of those sit in the Plains/Forest band — tens of candidates before
    /// the footprint-aware vertical-fit gate, tree overlaps and interlocking
    /// shells. Measured after the footprint gate on DEFAULT_WORLD_SEED: 23
    /// distinct ruins place at least one cell. 8 is comfortably below the
    /// observed count yet far above vacuous.
    const RUIN_LOOT_MIN_RUINS: usize = 8;

    /// Every ruin anchor the placement rule yields inside a chunk region,
    /// derived from `ruin_at` exactly as `place_surface_ruins` does.
    fn ruin_anchors_in_region(radius: i32, seed: u32) -> Vec<RuinSpec> {
        let mut anchors = Vec::new();
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let wx = cx * CHUNK_SIZE as i32 + lx as i32;
                        let wz = cz * CHUNK_SIZE as i32 + lz as i32;
                        let h = terrain_height_at_seeded(wx, wz, seed);
                        let biome = biome_for_height(h, forest_noise(wx, wz, seed));
                        if let Some(spec) = ruin_at(wx, wz, h, biome, seed) {
                            anchors.push(spec);
                        }
                    }
                }
            }
        }
        anchors
    }

    /// Every Cobblestone/Brick cell in a chunk region (`cy 0..=2`, enough for
    /// `surface + 4` up to the top of the ruin band). Worldgen paints exactly
    /// those two blocks for ruins — ores are CoalOre/IronOre, trees are
    /// OakLog/OakLeaves — so this set is the placed ruin shells.
    fn ruin_blocks_in_region(radius: i32, seed: u32, season: Season) -> Vec<(i32, i32, i32, u8)> {
        let mut blocks = Vec::new();
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for cy in 0..=2 {
                    let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, season);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let block = chunk.get(lx, ly, lz);
                                if matches!(block, BLOCK_COBBLESTONE | BLOCK_BRICK) {
                                    blocks.push((
                                        cx * CHUNK_SIZE as i32 + lx as i32,
                                        cy * CHUNK_SIZE as i32 + ly as i32,
                                        cz * CHUNK_SIZE as i32 + lz as i32,
                                        block as u8,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        blocks
    }

    /// Every `LootChest` cell in a chunk region (`cy 0..=2`). Worldgen paints
    /// LootChest only as the ruin chest, so this set is the generated chests.
    fn loot_chests_in_region(radius: i32, seed: u32, season: Season) -> Vec<(i32, i32, i32)> {
        let mut chests = Vec::new();
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for cy in 0..=2 {
                    let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, season);
                    for lz in 0..CHUNK_SIZE {
                        for ly in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                if chunk.get(lx, ly, lz) == BLOCK_LOOT_CHEST {
                                    chests.push((
                                        cx * CHUNK_SIZE as i32 + lx as i32,
                                        cy * CHUNK_SIZE as i32 + ly as i32,
                                        cz * CHUNK_SIZE as i32 + lz as i32,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        chests
    }

    #[test]
    fn test_surface_ruins_stand_on_plains_forest_land_in_every_season() {
        let seed = DEFAULT_WORLD_SEED;
        let anchors = ruin_anchors_in_region(RUIN_SCAN_RADIUS, seed);
        assert!(!anchors.is_empty(), "region has no ruin anchors");
        let blocks = ruin_blocks_in_region(RUIN_SCAN_RADIUS, seed, Season::Summer);
        assert!(
            blocks.len() >= 16,
            "region produced {} ruin cells — expected several 5x5 shells",
            blocks.len()
        );
        let brick_cells = blocks.iter().filter(|b| b.3 == BLOCK_BRICK as u8).count();
        assert!(
            brick_cells >= 4,
            "region produced {brick_cells} Brick cells, expected corner accents"
        );

        let mut surface_checked = 0usize;
        for &(wx, wy, wz, id) in &blocks {
            let h = terrain_height_at_seeded(wx, wz, seed);
            let biome = biome_at_seeded(wx, wz, seed);
            assert!(
                matches!(biome, Biome::Plains | Biome::Forest),
                "ruin cell at ({wx}, {wy}, {wz}) stands in {biome:?}"
            );
            assert!(
                (RUIN_MIN_HEIGHT..=RUIN_MAX_HEIGHT).contains(&h),
                "ruin column ({wx}, {wz}) has h {h} outside the ruin band"
            );
            assert!(
                (h + 1..=h + RUIN_WALL_MAX_HEIGHT + 1).contains(&wy),
                "ruin cell at ({wx}, {wy}, {wz}) outside surface+1..=surface+{} (h = {h})",
                RUIN_WALL_MAX_HEIGHT + 1
            );
            // Attribute the cell to the anchors whose footprint contains it.
            // Overlapping ruins are legal (two anchors may sit within 4
            // blocks), so a cell only has to be a shell cell of *some*
            // covering anchor, with that anchor's wall material.
            let covering: Vec<&RuinSpec> = anchors
                .iter()
                .filter(|spec| {
                    (spec.anchor_x - wx).abs() <= RUIN_FOOTPRINT_HALF
                        && (spec.anchor_z - wz).abs() <= RUIN_FOOTPRINT_HALF
                })
                .collect();
            assert!(
                !covering.is_empty(),
                "ruin cell at ({wx}, {wy}, {wz}) belongs to no anchor footprint"
            );
            let on_shell = covering.iter().any(|spec| {
                let dx = wx - spec.anchor_x;
                let dz = wz - spec.anchor_z;
                dx.abs().max(dz.abs()) == RUIN_FOOTPRINT_HALF && ruin_wall_block(dx, dz) as u8 == id
            });
            assert!(
                on_shell,
                "ruin cell at ({wx}, {wy}, {wz}) is not a shell cell of any covering anchor"
            );
            // Bound the per-column regeneration sample: the biome/band checks
            // above already pin the rule, this re-checks the generated block.
            if surface_checked < 40 {
                assert_eq!(
                    generated_block_at(wx, h, wz, seed, Season::Summer),
                    BLOCK_GRASS,
                    "ruin cell column ({wx}, {h}, {wz}) has a non-Grass surface"
                );
                surface_checked += 1;
            }
        }

        // The seasonal Winter blanket turns Plains/Forest land into Snow. The
        // surface gate accepts that skin too, so the same chunks must carry the
        // same shells and the same loot chests in Winter: ruins are
        // season-independent by construction.
        let mut ruin_chunks: Vec<(i32, i32)> = blocks
            .iter()
            .map(|&(wx, _, wz, _)| {
                (
                    wx.div_euclid(CHUNK_SIZE as i32),
                    wz.div_euclid(CHUNK_SIZE as i32),
                )
            })
            .collect();
        ruin_chunks.sort_unstable();
        ruin_chunks.dedup();
        assert!(!ruin_chunks.is_empty(), "no ruin chunks to re-check");
        for &(cx, cz) in ruin_chunks.iter().take(8) {
            for cy in 0..=2 {
                let winter = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Winter);
                let summer = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
                for lz in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let summer_block = summer.get(lx, ly, lz);
                            if matches!(
                                summer_block,
                                BLOCK_COBBLESTONE | BLOCK_BRICK | BLOCK_LOOT_CHEST
                            ) {
                                assert_eq!(
                                    winter.get(lx, ly, lz),
                                    summer_block,
                                    "winter removed ruin cell ({lx}, {ly}, {lz}) in chunk ({cx}, {cy}, {cz})"
                                );
                            }
                        }
                    }
                }
            }
        }

        // The whole region keeps every Summer ruin shell cell in Winter. Winter
        // may gain shell cells where Summer leaves occupied the only-into-Air
        // write (bare winter canopies expose more Air), but it must never lose
        // one. Loot chests are identical because their interior column is never
        // a canopy cell.
        let winter_blocks = ruin_blocks_in_region(RUIN_SCAN_RADIUS, seed, Season::Winter);
        let winter_cells: std::collections::HashSet<_> = winter_blocks.iter().copied().collect();
        for cell in &blocks {
            assert!(
                winter_cells.contains(cell),
                "winter removed ruin cell {cell:?}"
            );
        }
        assert_eq!(
            loot_chests_in_region(RUIN_SCAN_RADIUS, seed, Season::Winter),
            loot_chests_in_region(RUIN_SCAN_RADIUS, seed, Season::Summer),
            "winter must keep every ruin loot chest"
        );
        assert!(
            !loot_chests_in_region(RUIN_SCAN_RADIUS, seed, Season::Winter).is_empty(),
            "Winter ruins must still carry loot chests"
        );

        // Ocean and mountain chunks carry no ruin at all: Sand and Stone
        // surfaces still fail the land-skin gate.
        let found = first_column_per_biome(seed);
        for biome in [Biome::Ocean, Biome::Mountains] {
            let (wx, _, wz) = found[biome_index(biome)].expect("seed must have both biomes");
            let (cx, cz) = (
                wx.div_euclid(CHUNK_SIZE as i32),
                wz.div_euclid(CHUNK_SIZE as i32),
            );
            for cy in 0..=2 {
                let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
                for lz in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let block = chunk.get(lx, ly, lz);
                            assert!(
                                !matches!(
                                    block,
                                    BLOCK_COBBLESTONE | BLOCK_BRICK | BLOCK_LOOT_CHEST
                                ),
                                "{biome:?} chunk ({cx}, {cy}, {cz}) has ruin block {block:?} at ({lx}, {ly}, {lz})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Determinism and seed sensitivity: the same seed regenerates the same ruin
    /// set, distinct seeds move it. Mirrors `test_different_seeds_change_ore_layout`.
    #[test]
    fn test_surface_ruins_are_deterministic_and_seed_dependent() {
        // Radius 3 matches RUIN_SCAN_RADIUS: the D088 branch-aware overlap
        // policy rejects more forest anchors, so the smaller radius-2 sample
        // can legitimately contain zero surviving ruins for a given seed.
        let radius = RUIN_SCAN_RADIUS;
        let first = ruin_blocks_in_region(radius, DEFAULT_WORLD_SEED, Season::Summer);
        let repeat = ruin_blocks_in_region(radius, DEFAULT_WORLD_SEED, Season::Summer);
        let other = ruin_blocks_in_region(radius, 424_242, Season::Summer);
        assert!(!first.is_empty(), "seed 1337 produced no ruins");
        assert!(!other.is_empty(), "seed 424242 produced no ruins");
        assert_eq!(first, repeat, "same seed produced different ruin layouts");
        assert_ne!(
            first, other,
            "different seeds produced identical ruin layouts"
        );
    }

    /// Only-into-Air pinned directly on the pass: a shell cell that already
    /// holds a non-Air block (OakLeaves from a neighbouring canopy, Water, the
    /// Grass surface skin) is left untouched, and an anchor whose footprint
    /// holds an OakLog is skipped entirely. Inline fixture chunk, real
    /// `place_ruin_at` — no mocks.
    #[test]
    fn test_place_ruin_only_writes_into_air() {
        let heights = [[40i32; CHUNK_SIZE]; CHUNK_SIZE];
        let base_wy = 32;
        let spec = RuinSpec {
            anchor_x: 16,
            anchor_z: 16,
            surface: 40,
            wall_height: 2,
        };

        let mut chunk = ChunkBuffer::uniform(BLOCK_AIR);
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                chunk.set(lx, 8, lz, BLOCK_GRASS);
            }
        }
        // (18, 16) is a +x wall-run cell at level 1 and (14, 14) a corner at
        // level 2: non-Air blocks there must survive the pass.
        chunk.set(18, 9, 16, BLOCK_OAK_LEAVES);
        chunk.set(14, 10, 14, BLOCK_WATER);

        place_ruin_at(&mut chunk, &heights, base_wy, spec);

        assert_eq!(
            chunk.get(18, 9, 16),
            BLOCK_OAK_LEAVES,
            "ruin overwrote OakLeaves"
        );
        assert_eq!(chunk.get(14, 10, 14), BLOCK_WATER, "ruin overwrote Water");
        // Free shell cells did get their wall material (level 1 Cobblestone on
        // the wall run, level 2 Brick on the corner pillar).
        assert_eq!(chunk.get(18, 9, 17), BLOCK_COBBLESTONE);
        assert_eq!(chunk.get(18, 10, 18), BLOCK_BRICK);
        // The doorway and the footprint interior stay open; the only interior
        // cell the pass writes is the single loot chest at the anchor column.
        assert_eq!(chunk.get(16, 9, 18), BLOCK_AIR, "doorway was walled up");
        assert_eq!(
            chunk.get(16, 9, 16),
            BLOCK_LOOT_CHEST,
            "the ruin interior must carry exactly one loot chest"
        );
        // The Grass surface skin itself is never a shell target.
        assert_eq!(chunk.get(18, 8, 16), BLOCK_GRASS);

        // Tree-overlap policy: an OakLog anywhere in the footprint skips the
        // whole anchor, even where the shell cell itself is still Air.
        let mut tree_chunk = ChunkBuffer::uniform(BLOCK_AIR);
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                tree_chunk.set(lx, 8, lz, BLOCK_GRASS);
            }
        }
        tree_chunk.set(17, 9, 17, BLOCK_OAK_LOG);
        place_ruin_at(&mut tree_chunk, &heights, base_wy, spec);
        for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                for ly in 0..CHUNK_SIZE {
                    let block = tree_chunk.get((16 + dx) as usize, ly, (16 + dz) as usize);
                    assert!(
                        !matches!(block, BLOCK_COBBLESTONE | BLOCK_BRICK),
                        "anchor overlapping an OakLog still placed {block:?}"
                    );
                }
            }
        }
        assert_eq!(tree_chunk.get(17, 9, 17), BLOCK_OAK_LOG);
        assert_eq!(
            tree_chunk.get(16, 9, 16),
            BLOCK_AIR,
            "a tree-skipped anchor must not leave a loot chest behind"
        );
    }

    /// Synthetic complete shell: on a flat Grass fixture the real
    /// `place_ruin_at` paints every shell cell and exactly one LootChest at the
    /// interior centre, while a non-Air chest cell and a tree overlap both
    /// suppress the chest without overwriting anything.
    #[test]
    fn test_complete_surface_ruin_has_one_loot_chest() {
        let heights = [[10i32; CHUNK_SIZE]; CHUNK_SIZE];
        let base_wy = 0;
        let spec = RuinSpec {
            anchor_x: 16,
            anchor_z: 16,
            surface: 10,
            wall_height: 2,
        };

        let grass_flat = || {
            let mut chunk = ChunkBuffer::uniform(BLOCK_AIR);
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    chunk.set(lx, 10, lz, BLOCK_GRASS);
                }
            }
            chunk
        };

        let mut chunk = grass_flat();
        place_ruin_at(&mut chunk, &heights, base_wy, spec);

        // Every shell cell is painted, including both levels of the wall runs
        // and the taller corner pillars; the doorway stays open.
        for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                if dx.abs() != RUIN_FOOTPRINT_HALF && dz.abs() != RUIN_FOOTPRINT_HALF {
                    continue;
                }
                let lx = (spec.anchor_x + dx) as usize;
                let lz = (spec.anchor_z + dz) as usize;
                for level in ruin_cell_bottom(dx, dz)..=ruin_cell_top(dx, dz, spec.wall_height) {
                    assert_eq!(
                        chunk.get(lx, (spec.surface + level) as usize, lz),
                        ruin_wall_block(dx, dz),
                        "shell cell ({dx}, {level}, {dz}) was not painted"
                    );
                }
            }
        }

        let chest = ruin_loot_chest_pos(&spec);
        assert_eq!((chest.x, chest.y, chest.z), (16, 11, 16));
        assert_eq!(chunk.get(16, 11, 16), BLOCK_LOOT_CHEST);
        assert_eq!(chunk.get(16, 10, 16), BLOCK_GRASS);
        let mut chests = 0usize;
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if chunk.get(lx, ly, lz) == BLOCK_LOOT_CHEST {
                        chests += 1;
                    }
                }
            }
        }
        assert_eq!(chests, 1, "a complete ruin carries exactly one loot chest");

        // Negative: a non-Air chest cell (Stone here) is never overwritten and
        // no LootChest is created anywhere.
        let mut blocked = grass_flat();
        blocked.set(16, 11, 16, BLOCK_STONE);
        place_ruin_at(&mut blocked, &heights, base_wy, spec);
        assert_eq!(blocked.get(16, 11, 16), BLOCK_STONE);
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    assert_ne!(
                        blocked.get(lx, ly, lz),
                        BLOCK_LOOT_CHEST,
                        "chest cell occupant must suppress the LootChest"
                    );
                }
            }
        }

        // Negative: an OakLog in any footprint column skips the whole anchor,
        // chest included.
        let mut tree = grass_flat();
        tree.set(17, 11, 17, BLOCK_OAK_LOG);
        place_ruin_at(&mut tree, &heights, base_wy, spec);
        assert_eq!(tree.get(16, 11, 16), BLOCK_AIR);
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let block = tree.get(lx, ly, lz);
                    assert!(
                        !matches!(block, BLOCK_COBBLESTONE | BLOCK_BRICK),
                        "tree-skipped anchor still painted {block:?}"
                    );
                }
            }
        }
    }

    /// Generated acceptance: real chunks in the pinned region contain at least
    /// one LootChest; every chest belongs to the anchor at its own column, sits
    /// at `ruin_loot_chest_pos`, and keeps its whole footprint in the anchor
    /// chunk layer. At least one isolated, chest-bearing anchor must show the
    /// complete shell, so "a chest exists somewhere" is never enough.
    #[test]
    fn test_generated_surface_ruin_has_one_loot_chest() {
        use std::collections::BTreeSet;

        let seed = DEFAULT_WORLD_SEED;
        // Widen deterministically only if the default region yields no complete
        // ruin at all. Measured after the footprint-aware gate on the default
        // seed: the 7x7-chunk region still contains 23 chest-bearing ruins (the
        // two boundary-incomplete chests are gone), so this loop does not run.
        let mut radius = RUIN_SCAN_RADIUS;
        let mut chests = loot_chests_in_region(radius, seed, Season::Summer);
        while chests.is_empty() && radius < 5 {
            radius += 1;
            chests = loot_chests_in_region(radius, seed, Season::Summer);
        }
        assert!(
            !chests.is_empty(),
            "no generated LootChest in the {radius}-chunk region — the chest write never fired"
        );
        eprintln!(
            "generated ruin loot chests (seed {seed}, radius {radius}): {}",
            chests.len()
        );

        let anchors = ruin_anchors_in_region(radius, seed);
        let mut chest_columns = BTreeSet::new();
        for &(wx, wy, wz) in &chests {
            let spec = ruin_at_seeded(wx, wz, seed).unwrap_or_else(|| {
                panic!("generated LootChest at ({wx}, {wy}, {wz}) has no ruin anchor")
            });
            let expected = ruin_loot_chest_pos(&spec);
            assert_eq!(
                (expected.x, expected.y, expected.z),
                (wx, wy, wz),
                "chest at ({wx}, {wy}, {wz}) is not at its own ruin's anchor column"
            );
            assert_eq!(wy, spec.surface + 1, "chest must sit one above the surface");
            assert!(
                anchors.contains(&spec),
                "chest anchor ({}, {}) is missing from the placement scan",
                spec.anchor_x,
                spec.anchor_z
            );
            chest_columns.insert((wx, wz));

            // L085 closeout: every footprint column's surface and every shell
            // cell must stay in the chunk layer that owns this anchor, for
            // *every* generated chest, not only the isolated one below.
            let base_wy = spec.surface.div_euclid(CHUNK_SIZE as i32) * CHUNK_SIZE as i32;
            let layer_top = base_wy + CHUNK_SIZE as i32 - 1;
            for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                    let fx = spec.anchor_x + dx;
                    let fz = spec.anchor_z + dz;
                    let column_surface = terrain_height_at_seeded(fx, fz, seed);
                    assert!(
                        (base_wy..=layer_top).contains(&column_surface),
                        "ruin at ({}, {}) footprint column ({fx}, {fz}) surface \
                         {column_surface} is outside its anchor layer {base_wy}..={layer_top}",
                        spec.anchor_x,
                        spec.anchor_z
                    );
                    for level in ruin_cell_bottom(dx, dz)..=ruin_cell_top(dx, dz, spec.wall_height)
                    {
                        let cell_wy = column_surface + level;
                        assert!(
                            (base_wy..=layer_top).contains(&cell_wy),
                            "ruin at ({}, {}) shell cell ({fx}, {cell_wy}, {fz}) is outside \
                             its anchor layer {base_wy}..={layer_top}",
                            spec.anchor_x,
                            spec.anchor_z
                        );
                    }
                }
            }

            // This anchor's own cell is present in the owning chunk layer. A
            // legally interlocking neighbouring anchor may own another chest
            // inside the footprint, so this is per anchor, not per footprint.
            let (cx, cy, cz) = (
                wx.div_euclid(CHUNK_SIZE as i32),
                wy.div_euclid(CHUNK_SIZE as i32),
                wz.div_euclid(CHUNK_SIZE as i32),
            );
            let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, Season::Summer);
            assert_eq!(
                chunk.get(
                    wx.rem_euclid(CHUNK_SIZE as i32) as usize,
                    wy.rem_euclid(CHUNK_SIZE as i32) as usize,
                    wz.rem_euclid(CHUNK_SIZE as i32) as usize,
                ),
                BLOCK_LOOT_CHEST,
                "ruin at ({}, {}) is missing its own chest cell ({wx}, {wy}, {wz})",
                spec.anchor_x,
                spec.anchor_z
            );

            // No other generated chest inside the footprint may resolve to
            // this anchor's table; an interlocking ruin keeps its own table.
            let own_table = ruin_loot(seed, &spec);
            for &(ox, oy, oz) in &chests {
                if (ox, oz) == (wx, wz) {
                    continue;
                }
                if (ox - spec.anchor_x).abs() <= RUIN_FOOTPRINT_HALF
                    && (oz - spec.anchor_z).abs() <= RUIN_FOOTPRINT_HALF
                {
                    assert_ne!(
                        ruin_loot_chest_at(IVec3::new(ox, oy, oz), seed),
                        Some(own_table.clone()),
                        "chest at ({ox}, {oy}, {oz}) inside ruin ({}, {}) resolves this \
                         anchor's table",
                        spec.anchor_x,
                        spec.anchor_z
                    );
                }
            }
        }

        // Isolated means no other anchor within Chebyshev 4 (2 * footprint
        // half), so no interlocking shell may legally paint inside this ruin.
        let isolated = anchors.iter().find(|spec| {
            chest_columns.contains(&(spec.anchor_x, spec.anchor_z))
                && !anchors.iter().any(|other| {
                    other != *spec
                        && (other.anchor_x - spec.anchor_x).abs() <= 2 * RUIN_FOOTPRINT_HALF
                        && (other.anchor_z - spec.anchor_z).abs() <= 2 * RUIN_FOOTPRINT_HALF
                })
        });
        let spec = isolated.expect("no isolated chest-bearing ruin in the scan region");

        for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                if dx.abs() != RUIN_FOOTPRINT_HALF && dz.abs() != RUIN_FOOTPRINT_HALF {
                    continue;
                }
                let wx = spec.anchor_x + dx;
                let wz = spec.anchor_z + dz;
                // Each footprint column paints relative to its own surface, so
                // a slope inside the footprint is part of the complete shell.
                let column_surface = terrain_height_at_seeded(wx, wz, seed);
                for level in ruin_cell_bottom(dx, dz)..=ruin_cell_top(dx, dz, spec.wall_height) {
                    let wy = column_surface + level;
                    assert_eq!(
                        generated_block_at(wx, wy, wz, seed, Season::Summer),
                        ruin_wall_block(dx, dz),
                        "isolated ruin at ({}, {}) is missing shell cell ({dx}, {level}, {dz})",
                        spec.anchor_x,
                        spec.anchor_z
                    );
                }
            }
        }
        let chest = ruin_loot_chest_pos(spec);
        assert_eq!(
            generated_block_at(chest.x, chest.y, chest.z, seed, Season::Summer),
            BLOCK_LOOT_CHEST,
            "isolated ruin at ({}, {}) is missing its loot chest",
            spec.anchor_x,
            spec.anchor_z
        );
    }

    /// Tree-overlap policy pinned: an anchor whose footprint already holds an
    /// OakLog is skipped entirely, so a ruin never carves through a tree.
    #[test]
    fn test_surface_ruins_skip_tree_overlaps() {
        use std::collections::HashMap;
        let seed = DEFAULT_WORLD_SEED;
        let anchors = ruin_anchors_in_region(RUIN_SCAN_RADIUS, seed);
        assert!(!anchors.is_empty(), "region has no ruin anchors to check");
        let ch = CHUNK_SIZE as i32;
        let mut cache: HashMap<(i32, i32, i32), ChunkBuffer> = HashMap::new();
        let mut checked = 0usize;

        for spec in &anchors {
            // Only unambiguous anchors: when another ruin anchor sits within 4
            // blocks its shell may legally paint inside this footprint, which
            // would blur the "this anchor was skipped" assertion.
            let overlapped_by_ruin = anchors.iter().any(|other| {
                other != spec
                    && (other.anchor_x - spec.anchor_x).abs() <= 2 * RUIN_FOOTPRINT_HALF
                    && (other.anchor_z - spec.anchor_z).abs() <= 2 * RUIN_FOOTPRINT_HALF
            });
            if overlapped_by_ruin {
                continue;
            }

            let mut has_log = false;
            for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                    let wx = spec.anchor_x + dx;
                    let wz = spec.anchor_z + dz;
                    for wy in (spec.surface + 1)..=(spec.surface + TREE_CANOPY_HEADROOM) {
                        let pos =
                            ChunkPos::new(wx.div_euclid(ch), wy.div_euclid(ch), wz.div_euclid(ch));
                        let chunk = cache
                            .entry((pos.x, pos.y, pos.z))
                            .or_insert_with(|| generate_chunk_seeded(pos, seed, Season::Summer));
                        let block = chunk.get(
                            wx.rem_euclid(ch) as usize,
                            wy.rem_euclid(ch) as usize,
                            wz.rem_euclid(ch) as usize,
                        );
                        if block == BLOCK_OAK_LOG {
                            has_log = true;
                        }
                    }
                }
            }
            if !has_log {
                continue;
            }
            checked += 1;
            for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                    if dx.abs() != RUIN_FOOTPRINT_HALF && dz.abs() != RUIN_FOOTPRINT_HALF {
                        continue;
                    }
                    let wx = spec.anchor_x + dx;
                    let wz = spec.anchor_z + dz;
                    let top = ruin_cell_top(dx, dz, spec.wall_height);
                    for level in ruin_cell_bottom(dx, dz)..=top {
                        let wy = spec.surface + level;
                        let pos =
                            ChunkPos::new(wx.div_euclid(ch), wy.div_euclid(ch), wz.div_euclid(ch));
                        let chunk = cache
                            .entry((pos.x, pos.y, pos.z))
                            .or_insert_with(|| generate_chunk_seeded(pos, seed, Season::Summer));
                        let block = chunk.get(
                            wx.rem_euclid(ch) as usize,
                            wy.rem_euclid(ch) as usize,
                            wz.rem_euclid(ch) as usize,
                        );
                        assert!(
                            !matches!(block, BLOCK_COBBLESTONE | BLOCK_BRICK),
                            "anchor ({}, {}) overlaps a tree but still placed {block:?} at ({wx}, {wy}, {wz})",
                            spec.anchor_x,
                            spec.anchor_z
                        );
                    }
                }
            }
        }
        assert!(
            checked >= 1,
            "no ruin anchor overlapped a tree, so the skip policy was never exercised"
        );
    }

    /// Spawn clearance: the row of ruin tests the spawn search shares the ruin
    /// rule, so no spawn column is inside a footprint, and the generated spawn
    /// body stays Air.
    #[test]
    fn test_find_spawn_column_avoids_ruin_footprints() {
        for seed in [DEFAULT_WORLD_SEED, 12345, 9, 4, 0x5EED_4A11] {
            let (wx, wz) = find_spawn_column(seed);
            assert!(
                !ruin_covers(wx, wz, seed),
                "seed {seed} spawn ({wx}, {wz}) is inside a ruin footprint"
            );
            let h = terrain_height_at_seeded(wx, wz, seed);
            let ch = CHUNK_SIZE as i32;
            let chunk = generate_chunk_seeded(
                ChunkPos::new(wx.div_euclid(ch), (h + 1).div_euclid(ch), wz.div_euclid(ch)),
                seed,
                Season::Summer,
            );
            for dy in 1..=4 {
                let wy = h + dy;
                let block = chunk.get(
                    wx.rem_euclid(ch) as usize,
                    wy.rem_euclid(ch) as usize,
                    wz.rem_euclid(ch) as usize,
                );
                assert!(
                    !matches!(block, BLOCK_COBBLESTONE | BLOCK_BRICK),
                    "seed {seed} spawn body at ({wx}, {wy}, {wz}) holds ruin block {block:?}"
                );
            }
        }

        // Non-vacuity: a Plains ruin column that is otherwise spawn-eligible
        // (dry land, no oak within the tree clearance) must still be rejected.
        let seed = DEFAULT_WORLD_SEED;
        let candidate = ruin_anchors_in_region(2, seed)
            .into_iter()
            .find(|spec| {
                let (wx, wz) = (spec.anchor_x, spec.anchor_z);
                if biome_at_seeded(wx, wz, seed) != Biome::Plains {
                    return false;
                }
                for dz in -SPAWN_TREE_CLEARANCE..=SPAWN_TREE_CLEARANCE {
                    for dx in -SPAWN_TREE_CLEARANCE..=SPAWN_TREE_CLEARANCE {
                        let nh = terrain_height_at_seeded(wx + dx, wz + dz, seed);
                        let nb = biome_for_height(nh, forest_noise(wx + dx, wz + dz, seed));
                        if tree_trunk_height(wx + dx, wz + dz, nh, nb, seed).is_some() {
                            return false;
                        }
                    }
                }
                true
            })
            .expect("no tree-free Plains ruin anchor in the scan region");
        assert!(
            !is_spawn_safe(candidate.anchor_x, candidate.anchor_z, seed),
            "is_spawn_safe accepted ruin anchor ({}, {})",
            candidate.anchor_x,
            candidate.anchor_z
        );
    }

    /// Acceptance: the filled loot table is deterministic and capacity-bounded
    /// at *every* ruin the generator actually places, not merely declared.
    ///
    /// The region is regenerated and its Cobblestone/Brick cells (worldgen
    /// paints those only for ruins) are attributed back to a covering anchor
    /// from `ruin_at`. Each distinct ruin found then gets two `ruin_loot` calls
    /// that must agree, stay within the 27-slot chest, keep every rolled count
    /// in `1..=64`, and always carry the Wheat + WheatSeeds pair S07 needs.
    /// Both non-vacuity bounds are explicit: the scan must find ruin cells at
    /// all *and* at least [`RUIN_LOOT_MIN_RUINS`] distinct ruins, plus every
    /// ruin cell must have a covering anchor (a generator/placement-rule
    /// disagreement fails loudly instead of silently shrinking the checked
    /// set).
    #[test]
    fn test_ruin_loot_is_deterministic_and_capacity_bounded_at_every_generated_ruin() {
        use crate::loot::{LootEntry, LootItem, RUIN_LOOT_MAX_SLOTS};
        use std::collections::BTreeSet;

        let seed = DEFAULT_WORLD_SEED;
        let anchors = ruin_anchors_in_region(RUIN_SCAN_RADIUS, seed);
        let blocks = ruin_blocks_in_region(RUIN_SCAN_RADIUS, seed, Season::Summer);
        assert!(
            !blocks.is_empty(),
            "acceptance scan found no ruin cells, so the loot assertion would be vacuous"
        );

        let mut ruins: BTreeSet<(i32, i32)> = BTreeSet::new();
        let mut loot_by_ruin: Vec<Vec<LootEntry>> = Vec::new();
        for (bx, by, bz, block) in &blocks {
            let Some(spec) = anchors.iter().find(|spec| {
                (bx - spec.anchor_x).abs() <= RUIN_FOOTPRINT_HALF
                    && (bz - spec.anchor_z).abs() <= RUIN_FOOTPRINT_HALF
            }) else {
                panic!(
                    "generated ruin block {block:?} at ({bx}, {by}, {bz}) has no covering anchor — \
                     the generator and ruin_at disagree"
                );
            };
            if !ruins.insert((spec.anchor_x, spec.anchor_z)) {
                continue;
            }
            let first = ruin_loot(seed, spec);
            let second = ruin_loot(seed, spec);
            assert_eq!(
                first, second,
                "ruin_loot must be deterministic at ruin ({}, {})",
                spec.anchor_x, spec.anchor_z
            );
            assert!(
                !first.is_empty(),
                "ruin_loot returned nothing for ruin ({}, {})",
                spec.anchor_x,
                spec.anchor_z
            );
            assert!(
                first.len() <= RUIN_LOOT_MAX_SLOTS,
                "ruin_loot returned {} entries for ruin ({}, {}) — a chest holds {RUIN_LOOT_MAX_SLOTS}",
                first.len(),
                spec.anchor_x,
                spec.anchor_z
            );
            let items: Vec<LootItem> = first.iter().map(|entry| entry.item).collect();
            assert!(
                items.contains(&LootItem::Wheat) && items.contains(&LootItem::WheatSeeds),
                "ruin ({}, {}) is missing the farm pair: {items:?}",
                spec.anchor_x,
                spec.anchor_z
            );
            for entry in &first {
                assert_eq!(
                    entry.min_count, entry.max_count,
                    "ruin ({}, {}) left a range unrolled: {entry:?}",
                    spec.anchor_x, spec.anchor_z
                );
                assert!(
                    (1..=64).contains(&entry.min_count),
                    "ruin ({}, {}) rolled count {} outside 1..=64",
                    spec.anchor_x,
                    spec.anchor_z,
                    entry.min_count
                );
            }
            loot_by_ruin.push(first);
        }

        assert!(
            ruins.len() >= RUIN_LOOT_MIN_RUINS,
            "acceptance scan found only {} distinct ruins (need >= {RUIN_LOOT_MIN_RUINS}) — \
             the loot assertion would be near-vacuous",
            ruins.len()
        );
        assert_eq!(loot_by_ruin.len(), ruins.len());

        // The table is not a constant: either two generated ruins disagree or
        // the same anchor under a different seed changes. Requiring every ruin
        // to differ would be flaky; requiring at least one difference proves
        // the mixer is live.
        let all_equal = loot_by_ruin.iter().all(|loot| loot == &loot_by_ruin[0]);
        let seed_rerolls = anchors
            .first()
            .is_some_and(|spec| ruin_loot(seed, spec) != ruin_loot(seed ^ 0x5555_5555, spec));
        assert!(
            !all_equal || seed_rerolls,
            "ruin loot is constant across every generated ruin and both seeds"
        );
    }

    /// Acceptance: `ruin_loot_chest_at` recovers a ruin's table only from the
    /// exact anchor-column chest cell that `place_ruin_at` writes.
    ///
    /// Generated chests are the oracle: each must resolve to its own anchor's
    /// table, a neighbouring column and the block below must resolve to
    /// `None`, and no neighbouring anchor may claim the cell. A non-ruin
    /// all-air coordinate must also resolve to `None`, so the session opens
    /// empty instead of guessing the nearest table.
    #[test]
    fn test_ruin_loot_chest_at_matches_exactly_one_generated_anchor_column() {
        let seed = DEFAULT_WORLD_SEED;
        let chests = loot_chests_in_region(RUIN_SCAN_RADIUS, seed, Season::Summer);
        assert!(
            !chests.is_empty(),
            "no generated LootChest in the {RUIN_SCAN_RADIUS}-chunk region"
        );
        eprintln!(
            "generated loot chests for exact-cell recovery: {}",
            chests.len()
        );

        for &(wx, wy, wz) in &chests {
            let pos = IVec3::new(wx, wy, wz);
            let spec = ruin_at_seeded(wx, wz, seed).unwrap_or_else(|| {
                panic!("generated LootChest at ({wx}, {wy}, {wz}) has no ruin anchor")
            });
            assert_eq!(
                ruin_loot_chest_pos(&spec),
                pos,
                "chest at ({wx}, {wy}, {wz}) is not its anchor column's chest cell"
            );
            let expected = ruin_loot(seed, &spec);
            assert_eq!(
                ruin_loot_chest_at(pos, seed),
                Some(expected.clone()),
                "chest at ({wx}, {wy}, {wz}) must recover its own anchor's table"
            );

            // Exact cell means exact. A +x neighbour may legally be another
            // interlocking ruin's own chest cell, so it must never resolve
            // *this* anchor's table (`None` and a neighbour's own table are
            // both fine).
            assert_ne!(
                ruin_loot_chest_at(pos + IVec3::new(1, 0, 0), seed),
                Some(expected.clone()),
                "chest at ({wx}, {wy}, {wz}) resolved its own table for the +x neighbour"
            );
            // The block below is never a chest cell: every chest sits exactly
            // at `surface + 1` on its own anchor column.
            assert_eq!(
                ruin_loot_chest_at(pos - IVec3::new(0, 1, 0), seed),
                None,
                "chest at ({wx}, {wy}, {wz}) resolved loot for the block below"
            );

            // No neighbouring anchor owns this cell: interlocking shells are
            // legal, but every chest cell stays on its own anchor column.
            for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    if let Some(other) = ruin_at_seeded(wx + dx, wz + dz, seed) {
                        assert_ne!(
                            ruin_loot_chest_pos(&other),
                            pos,
                            "neighbour anchor ({}, {}) claims chest ({wx}, {wy}, {wz})",
                            other.anchor_x,
                            other.anchor_z
                        );
                    }
                }
            }
        }

        // A non-ruin all-air coordinate must recover nothing: the session has
        // to open empty instead of guessing the nearest anchor.
        let sky = IVec3::new(0, 159, 0);
        assert_eq!(
            generated_block_at(sky.x, sky.y, sky.z, seed, Season::Summer),
            BLOCK_AIR,
            "the chosen negative coordinate must be ungenerated all-air"
        );
        assert_eq!(ruin_loot_chest_at(sky, seed), None);
    }

    /// Negative: a fixed set of chunk positions spanning all four biomes
    /// regenerates byte-for-byte identically.
    #[test]
    fn test_acceptance_byte_identical_repeat_generation() {
        let seed = DEFAULT_WORLD_SEED;
        let found = first_column_per_biome(seed);
        let mut positions = vec![
            ChunkPos::new(0, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(-3, 0, 7),
        ];
        for column in found.into_iter().flatten() {
            let (wx, _, wz) = column;
            let cx = wx.div_euclid(CHUNK_SIZE as i32);
            let cz = wz.div_euclid(CHUNK_SIZE as i32);
            for cy in 0..=1 {
                positions.push(ChunkPos::new(cx, cy, cz));
            }
        }

        for pos in positions {
            let first = generate_chunk_seeded(pos, seed, Season::Summer);
            let second = generate_chunk_seeded(pos, seed, Season::Summer);
            for lz in 0..CHUNK_SIZE {
                for ly in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        assert_eq!(
                            first.get(lx, ly, lz),
                            second.get(lx, ly, lz),
                            "chunk {pos:?} differs at ({lx}, {ly}, {lz})"
                        );
                    }
                }
            }
        }
    }

    /// ChunkBuffer positions spanning all four biomes for `seed` (plus the sky layer
    /// and a fixed off-origin column), mirroring the existing byte-identity
    /// precedent so the four-season acceptance is not sampled on one biome.
    fn biome_spanning_chunk_positions(seed: u32) -> Vec<ChunkPos> {
        let mut positions = vec![
            ChunkPos::new(0, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(-3, 0, 7),
        ];
        for (wx, wz, _) in first_column_per_biome(seed).into_iter().flatten() {
            let cx = wx.div_euclid(CHUNK_SIZE as i32);
            let cz = wz.div_euclid(CHUNK_SIZE as i32);
            for cy in 0..=1 {
                positions.push(ChunkPos::new(cx, cy, cz));
            }
        }
        positions
    }

    /// First `(wx, wz, h)` column of `biome` with `h` inside `min_h..=max_h`,
    /// used to pin seasonal surfaces away from the beach band.
    fn first_column_in_height_band(
        seed: u32,
        biome: Biome,
        min_h: i32,
        max_h: i32,
    ) -> (i32, i32, i32) {
        for wz in (-512..=512).step_by(4) {
            for wx in (-512..=512).step_by(4) {
                if biome_at_seeded(wx, wz, seed) != biome {
                    continue;
                }
                let h = terrain_height_at_seeded(wx, wz, seed);
                if (min_h..=max_h).contains(&h) {
                    return (wx, wz, h);
                }
            }
        }
        panic!("no {biome:?} column with h in {min_h}..={max_h} in the acceptance region");
    }

    /// First `(wx, wz, h)` Mountain column at or above the altitude snow cap.
    ///
    /// The cap only exists in the continental highlands, far outside the ±512
    /// four-biome acceptance region, so this scans a wider coarse grid (step
    /// 32). Cap regions span dozens of blocks, so a 32-block lattice cannot
    /// step over one.
    fn first_snow_cap_column(seed: u32) -> (i32, i32, i32) {
        for wz in (-8192..=8192).step_by(32) {
            for wx in (-8192..=8192).step_by(32) {
                if biome_at_seeded(wx, wz, seed) != Biome::Mountains {
                    continue;
                }
                let h = terrain_height_at_seeded(wx, wz, seed);
                if h >= SNOW_CAP_HEIGHT {
                    return (wx, wz, h);
                }
            }
        }
        panic!("no mountain column at or above the altitude snow cap in the wide region");
    }

    /// First `(wx, wz, h)` beach column: plains/forest land inside the Sand
    /// beach band (`SEA_LEVEL..=SEA_LEVEL + BEACH_BAND`).
    fn first_beach_column(seed: u32) -> (i32, i32, i32) {
        for wz in (-512..=512).step_by(4) {
            for wx in (-512..=512).step_by(4) {
                let biome = biome_at_seeded(wx, wz, seed);
                if !matches!(biome, Biome::Plains | Biome::Forest) {
                    continue;
                }
                let h = terrain_height_at_seeded(wx, wz, seed);
                if h <= SEA_LEVEL + BEACH_BAND {
                    return (wx, wz, h);
                }
            }
        }
        panic!("no beach column in the acceptance region");
    }

    /// First oak satisfying `predicate`, scanning the reachable region. The
    /// generator's pure gate is used to find candidates, so the test never has
    /// to duplicate the placement rule.
    fn find_tree(
        seed: u32,
        mut predicate: impl FnMut(i32, i32, i32, OakTree) -> bool,
    ) -> Option<(i32, i32, i32, OakTree)> {
        for wz in -512..=512 {
            for wx in -512..=512 {
                let h = terrain_height_at_seeded(wx, wz, seed);
                let biome = biome_at_seeded(wx, wz, seed);
                let Some(tree) = oak_tree_at(wx, wz, h, biome, seed) else {
                    continue;
                };
                if predicate(wx, wz, h, tree) {
                    return Some((wx, wz, h, tree));
                }
            }
        }
        None
    }

    /// First oak near the seed's beach whose model crosses the first chunk-Y
    /// boundary (wy 32) and whose branch cells are all above their neighbour
    /// columns. This is the D088 shoreline repro: before the fix the tree pass
    /// ran only in the surface chunk layer, so exactly these near-shore trees
    /// lost their upper trunk and canopy.
    fn shoreline_tree_crossing_a_chunk_y_boundary(seed: u32) -> (i32, i32, i32, OakTree) {
        let (beach_x, beach_z, _) = first_beach_column(seed);
        let search = 48;
        for wz in (beach_z - search)..=(beach_z + search) {
            for wx in (beach_x - search)..=(beach_x + search) {
                let h = terrain_height_at_seeded(wx, wz, seed);
                let biome = biome_at_seeded(wx, wz, seed);
                let Some(tree) = oak_tree_at(wx, wz, h, biome, seed) else {
                    continue;
                };
                let top = h + tree.trunk_height + TREE_CANOPY_TOP_OVER_TRUNK;
                if top < CHUNK_SIZE as i32 {
                    continue;
                }
                let clear_branches = tree
                    .branch_cells(wx, wz, h)
                    .all(|(bx, by, bz)| terrain_height_at_seeded(bx, bz, seed) < by);
                if clear_branches {
                    return (wx, wz, h, tree);
                }
            }
        }
        panic!("no shoreline oak crossing a chunk-Y boundary near ({beach_x}, {beach_z})");
    }

    /// Count `block` across every chunk layer of one chunk column. Trees stay
    /// inside a chunk's interior, so this captures a full oak (and every other
    /// oak in the same column).
    fn count_blocks_in_chunk_column(
        cx: i32,
        cz: i32,
        seed: u32,
        season: Season,
        block: u16,
    ) -> usize {
        let mut count = 0;
        for cy in -1..=2 {
            let chunk = generate_chunk_seeded(ChunkPos::new(cx, cy, cz), seed, season);
            for lz in 0..CHUNK_SIZE {
                for ly in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        if chunk.get(lx, ly, lz) == block {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    /// Acceptance: the same seed generated at the four boundary game-times (one
    /// per season) is byte-identical across repeat runs.
    #[test]
    fn test_acceptance_four_season_generation_is_byte_identical_across_repeats() {
        // SPRING_START_SEED precedent: seed 1 starts in Spring, so days 0..3
        // (the game's one-day seasons) land exactly on the four seasons.
        let seed = 1;
        let positions = biome_spanning_chunk_positions(seed);
        let seasons = [
            (0u32, Season::Spring),
            (1, Season::Summer),
            (2, Season::Autumn),
            (3, Season::Winter),
        ];

        for &pos in &positions {
            for (day, season) in seasons {
                let first = generate_chunk_seeded(pos, seed, season);
                let second = generate_chunk_seeded(pos, seed, season);
                for lz in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            assert_eq!(
                                first.get(lx, ly, lz),
                                second.get(lx, ly, lz),
                                "seed {seed} day {day} ({season:?}) chunk {pos:?} \
                                 differs at ({lx}, {ly}, {lz})"
                            );
                        }
                    }
                }
            }
        }

        // The four derived seasons must be distinct or the loop above was
        // vacuous.
        for (i, (_, a)) in seasons.iter().enumerate() {
            for (_, b) in &seasons[i + 1..] {
                assert_ne!(
                    a, b,
                    "season derivation collapsed two boundary days onto {a:?}"
                );
            }
        }

        // D088: the four-season pass must contain winter trees, not quietly
        // regenerate a treeless Winter. The trunk and branches survive every
        // season; only leaves disappear in Winter.
        let (twx, twz, th, tree) = find_tree(seed, |wx, wz, h, tree| {
            tree.branch_cells(wx, wz, h)
                .all(|(bx, by, bz)| terrain_height_at_seeded(bx, bz, seed) < by)
        })
        .expect("seed 1 must grow at least one oak with clear branch cells");
        for (day, season) in seasons {
            for ty in (th + 1)..=(th + tree.trunk_height) {
                assert_eq!(
                    generated_block_at(twx, ty, twz, seed, season),
                    BLOCK_OAK_LOG,
                    "day {day} ({season:?}) lost the trunk block at ({twx}, {ty}, {twz})"
                );
            }
            for (bx, by, bz) in tree.branch_cells(twx, twz, th) {
                assert_eq!(
                    generated_block_at(bx, by, bz, seed, season),
                    BLOCK_OAK_LOG,
                    "day {day} ({season:?}) lost a branch at ({bx}, {by}, {bz})"
                );
            }
        }
    }

    /// Acceptance: winter turns every non-beach plains/forest land surface into
    /// Snow, and summer carries none of it — the seasonal snow blanket is
    /// present in winter and absent in summer for the same seed.
    #[test]
    fn test_acceptance_winter_snow_blanket_present_and_summer_absent() {
        let seed = 1;
        let seasons = [
            (0u32, Season::Spring),
            (1, Season::Summer),
            (2, Season::Autumn),
            (3, Season::Winter),
        ];
        let winter = seasons[3].1;
        let summer = seasons[1].1;
        assert_eq!(winter, Season::Winter);
        assert_eq!(summer, Season::Summer);

        let mut winter_blanket = 0usize;
        let mut summer_blanket = 0usize;
        let mut summer_grass = 0usize;
        for cx in -4..=4 {
            for cz in -4..=4 {
                for cy in 0..=1 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let winter_chunk = generate_chunk_seeded(pos, seed, winter);
                    let summer_chunk = generate_chunk_seeded(pos, seed, summer);
                    let base_wx = cx * CHUNK_SIZE as i32;
                    let base_wy = cy * CHUNK_SIZE as i32;
                    let base_wz = cz * CHUNK_SIZE as i32;
                    for lz in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let wx = base_wx + lx as i32;
                            let wz = base_wz + lz as i32;
                            let biome = biome_at_seeded(wx, wz, seed);
                            if !matches!(biome, Biome::Plains | Biome::Forest) {
                                continue;
                            }
                            let h = terrain_height_at_seeded(wx, wz, seed);
                            if h <= SEA_LEVEL + BEACH_BAND {
                                // Beaches are season-blind Sand.
                                continue;
                            }
                            let ly = h - base_wy;
                            if !(0..CHUNK_SIZE as i32).contains(&ly) {
                                continue;
                            }
                            let ly = ly as usize;
                            assert_eq!(
                                winter_chunk.get(lx, ly, lz),
                                BLOCK_SNOW,
                                "winter land surface at ({wx}, {h}, {wz}) is not blanket Snow"
                            );
                            winter_blanket += 1;
                            match summer_chunk.get(lx, ly, lz) {
                                BLOCK_GRASS => summer_grass += 1,
                                BLOCK_SNOW => summer_blanket += 1,
                                other => panic!(
                                    "summer land surface at ({wx}, {h}, {wz}) is {other:?}, not Grass"
                                ),
                            }
                        }
                    }
                }
            }
        }
        assert!(winter_blanket > 0, "winter region produced no snow blanket");
        assert_eq!(
            summer_blanket, 0,
            "summer land surface still carried the seasonal blanket"
        );
        assert_eq!(
            winter_blanket, summer_grass,
            "winter blanket must cover exactly the summer grass land columns"
        );
    }

    /// Semantic pins: the blanket is land-only (beaches/ocean stay Sand), the
    /// Mountain altitude cap exists in every season, and plains/forest are
    /// Grass in the three non-winter seasons.
    #[test]
    fn test_seasonal_surfaces_are_pinned_per_biome() {
        let seed = DEFAULT_WORLD_SEED;
        let all = [
            Season::Spring,
            Season::Summer,
            Season::Autumn,
            Season::Winter,
        ];

        let non_beach = SEA_LEVEL + BEACH_BAND + 1;
        let plains = first_column_in_height_band(seed, Biome::Plains, non_beach, TREE_MAX_HEIGHT);
        let forest = first_column_in_height_band(seed, Biome::Forest, non_beach, TREE_MAX_HEIGHT);
        for (wx, wz, h) in [plains, forest] {
            for season in all {
                let expected = if season == Season::Winter {
                    BLOCK_SNOW
                } else {
                    BLOCK_GRASS
                };
                assert_eq!(
                    generated_block_at(wx, h, wz, seed, season),
                    expected,
                    "{season:?} surface at ({wx}, {h}, {wz}) must be {expected:?}"
                );
            }
        }

        // The altitude snow cap is season-blind: it must exist in summer.
        let mountain = first_snow_cap_column(seed);
        for season in all {
            assert_eq!(
                generated_block_at(mountain.0, mountain.2, mountain.1, seed, season),
                BLOCK_SNOW,
                "{season:?} lost the season-blind altitude cap at ({}, {}, {})",
                mountain.0,
                mountain.2,
                mountain.1
            );
        }

        // Beaches and the ocean seabed stay Sand in every season.
        let beach = first_beach_column(seed);
        let ocean =
            first_column_per_biome(seed)[biome_index(Biome::Ocean)].expect("seed must have ocean");
        for season in all {
            assert_eq!(
                generated_block_at(beach.0, beach.2, beach.1, seed, season),
                BLOCK_SAND,
                "{season:?} painted the beach at ({}, {}, {})",
                beach.0,
                beach.2,
                beach.1
            );
            assert_eq!(
                generated_block_at(ocean.0, ocean.2, ocean.1, seed, season),
                BLOCK_SAND,
                "{season:?} painted the ocean seabed at ({}, {}, {})",
                ocean.0,
                ocean.2,
                ocean.1
            );
        }
    }

    /// Spring, Summer and Autumn keep the full oak canopy and every trunk and
    /// branch; only Winter sheds the leaves. Deterministic, so the canopy reads
    /// consistently and the bare winter tree still has a silhouette.
    #[test]
    fn test_spring_summer_and_autumn_keep_oak_canopy_and_trunks() {
        let seed = DEFAULT_WORLD_SEED;
        let (wx, wz, h, tree) = find_tree(seed, |wx, wz, h, tree| {
            tree.branch_cells(wx, wz, h)
                .all(|(bx, by, bz)| terrain_height_at_seeded(bx, bz, seed) < by)
        })
        .expect("seed must grow at least one oak with clear branch cells");
        let trunk_h = tree.trunk_height;
        let (cx, cz) = (
            wx.div_euclid(CHUNK_SIZE as i32),
            wz.div_euclid(CHUNK_SIZE as i32),
        );
        // The canopy top centre is always a leaf in the non-winter seasons and
        // is too high to collide with a branch cell on this tree.
        let leaf_wy = h + trunk_h + 1;

        for season in [Season::Spring, Season::Summer, Season::Autumn] {
            assert_eq!(
                generated_block_at(wx, h + 1, wz, seed, season),
                BLOCK_OAK_LOG,
                "{season:?} lost the trunk at ({wx}, {}, {wz})",
                h + 1
            );
            assert_eq!(
                generated_block_at(wx, leaf_wy, wz, seed, season),
                BLOCK_OAK_LEAVES,
                "{season:?} has no canopy leaf at ({wx}, {leaf_wy}, {wz})"
            );
            for (bx, by, bz) in tree.branch_cells(wx, wz, h) {
                assert_eq!(
                    generated_block_at(bx, by, bz, seed, season),
                    BLOCK_OAK_LOG,
                    "{season:?} lost the branch at ({bx}, {by}, {bz})"
                );
            }
            assert!(
                count_blocks_in_chunk_column(cx, cz, seed, season, BLOCK_OAK_LEAVES) > 0,
                "{season:?} chunk column has no oak leaves at all"
            );
        }

        // Autumn keeps the canopy and the whole skeleton; Winter is the only
        // season that sheds leaves.
        assert_eq!(
            generated_block_at(wx, leaf_wy, wz, seed, Season::Autumn),
            BLOCK_OAK_LEAVES,
            "autumn dropped a canopy leaf at ({wx}, {leaf_wy}, {wz})"
        );
        assert!(
            count_blocks_in_chunk_column(cx, cz, seed, Season::Autumn, BLOCK_OAK_LEAVES) > 0,
            "autumn chunk column has no oak leaves"
        );
        for ty in (h + 1)..=(h + trunk_h) {
            assert_eq!(
                generated_block_at(wx, ty, wz, seed, Season::Autumn),
                BLOCK_OAK_LOG,
                "autumn removed the trunk block at ({wx}, {ty}, {wz})"
            );
        }
        for (bx, by, bz) in tree.branch_cells(wx, wz, h) {
            assert_eq!(
                generated_block_at(bx, by, bz, seed, Season::Autumn),
                BLOCK_OAK_LOG,
                "autumn removed the branch at ({bx}, {by}, {bz})"
            );
        }
        assert_eq!(
            generated_block_at(wx, leaf_wy, wz, seed, Season::Winter),
            BLOCK_AIR,
            "winter kept a canopy leaf at ({wx}, {leaf_wy}, {wz})"
        );
        assert_eq!(
            count_blocks_in_chunk_column(cx, cz, seed, Season::Winter, BLOCK_OAK_LEAVES),
            0,
            "winter chunk column still contains oak leaves"
        );
        assert!(
            count_blocks_in_chunk_column(cx, cz, seed, Season::Winter, BLOCK_OAK_LOG) > 0,
            "winter chunk column lost every oak trunk"
        );
    }

    /// D088: Winter turns the grass surface of plains and forest into Snow but
    /// keeps the whole tree skeleton — every trunk block and every deterministic
    /// branch — with zero leaves. Only leaves are seasonal.
    #[test]
    fn test_seasonal_winter_keeps_trunks_and_branches_without_leaves() {
        let seed = DEFAULT_WORLD_SEED;
        let (wx, wz, h, tree) = find_tree(seed, |wx, wz, h, tree| {
            tree.branch_cells(wx, wz, h)
                .all(|(bx, by, bz)| terrain_height_at_seeded(bx, bz, seed) < by)
        })
        .expect("seed must grow at least one oak with clear branch cells");
        let trunk_h = tree.trunk_height;
        let (cx, cz) = (
            wx.div_euclid(CHUNK_SIZE as i32),
            wz.div_euclid(CHUNK_SIZE as i32),
        );

        assert_eq!(
            generated_block_at(wx, h, wz, seed, Season::Winter),
            BLOCK_SNOW,
            "the same column must carry the winter blanket"
        );
        for ty in (h + 1)..=(h + trunk_h) {
            assert_eq!(
                generated_block_at(wx, ty, wz, seed, Season::Winter),
                BLOCK_OAK_LOG,
                "winter removed the trunk block at ({wx}, {ty}, {wz})"
            );
        }
        for (bx, by, bz) in tree.branch_cells(wx, wz, h) {
            assert_eq!(
                generated_block_at(bx, by, bz, seed, Season::Winter),
                BLOCK_OAK_LOG,
                "winter removed the branch at ({bx}, {by}, {bz})"
            );
        }

        // The same site keeps its full canopy in Spring: only the leaves are
        // seasonal, and the model itself is unchanged.
        assert_eq!(
            generated_block_at(wx, h + trunk_h + 1, wz, seed, Season::Spring),
            BLOCK_OAK_LEAVES,
            "spring lost the canopy top at ({wx}, {}, {wz})",
            h + trunk_h + 1
        );
        assert_eq!(
            generated_block_at(wx, h + trunk_h + 1, wz, seed, Season::Winter),
            BLOCK_AIR,
            "winter kept a canopy leaf at ({wx}, {}, {wz})",
            h + trunk_h + 1
        );
        assert_eq!(
            count_blocks_in_chunk_column(cx, cz, seed, Season::Winter, BLOCK_OAK_LEAVES),
            0,
            "winter chunk column still contains oak leaves"
        );
        assert!(
            count_blocks_in_chunk_column(cx, cz, seed, Season::Winter, BLOCK_OAK_LOG) > 0,
            "winter chunk column lost every oak trunk"
        );
    }

    /// Negative/structural: a season change alters only two things — the
    /// non-beach plains/forest surface block (Grass -> Snow) and oak leaves
    /// (present -> Air). Trunks and branches are season-persistent, so the tree
    /// skeleton must be byte-identical between Summer and Winter (D088).
    /// Heights, biomes, subsurface skin, caves, ores, bedrock, water and the
    /// Mountain cap are also byte-identical (R001/R005 survivors).
    #[test]
    fn test_season_changes_only_surface_skin_and_oak_canopy() {
        let seed = DEFAULT_WORLD_SEED;
        let positions = [
            ChunkPos::new(0, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(-3, 0, 7),
            ChunkPos::new(3, 0, -2),
        ];

        let mut surface_swaps = 0usize;
        let mut leaves_shed = 0usize;
        let mut summer_logs = 0usize;
        let mut winter_logs = 0usize;
        for pos in positions {
            let summer = generate_chunk_seeded(pos, seed, Season::Summer);
            let winter = generate_chunk_seeded(pos, seed, Season::Winter);
            let base_wx = pos.x * CHUNK_SIZE as i32;
            let base_wy = pos.y * CHUNK_SIZE as i32;
            let base_wz = pos.z * CHUNK_SIZE as i32;
            for lz in 0..CHUNK_SIZE {
                for ly in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let a = summer.get(lx, ly, lz);
                        let b = winter.get(lx, ly, lz);
                        if a == BLOCK_OAK_LOG {
                            summer_logs += 1;
                        }
                        if b == BLOCK_OAK_LOG {
                            winter_logs += 1;
                        }
                        if a == b {
                            continue;
                        }
                        let wx = base_wx + lx as i32;
                        let wy = base_wy + ly as i32;
                        let wz = base_wz + lz as i32;
                        let h = terrain_height_at_seeded(wx, wz, seed);
                        let biome = biome_at_seeded(wx, wz, seed);

                        let surface_swap = wy == h
                            && h > SEA_LEVEL + BEACH_BAND
                            && matches!(biome, Biome::Plains | Biome::Forest)
                            && a == BLOCK_GRASS
                            && b == BLOCK_SNOW;
                        let shed_leaf = a == BLOCK_OAK_LEAVES && b == BLOCK_AIR;
                        assert!(
                            surface_swap || shed_leaf,
                            "unexpected winter change at ({wx}, {wy}, {wz}) in {pos:?}: \
                             {a:?} -> {b:?} (h {h}, biome {biome:?}); trunks and \
                             branches must be season-persistent (D088)"
                        );
                        if surface_swap {
                            surface_swaps += 1;
                        } else {
                            leaves_shed += 1;
                        }
                    }
                }
            }
        }
        assert!(
            surface_swaps > 0,
            "the scan found no winter blanket surface swap (test is vacuous)"
        );
        assert!(
            leaves_shed > 0,
            "the scan found no leaves shed in winter (test is vacuous)"
        );
        assert_eq!(
            winter_logs, summer_logs,
            "winter changed the oak log skeleton ({summer_logs} summer logs vs \
             {winter_logs} winter logs); only leaves may be seasonal (D088)"
        );
    }

    /// D088 shoreline regression: a deterministic oak whose trunk/canopy top
    /// crosses the first chunk-Y boundary (exactly the near-shore band where the
    /// surface sits at wy 26..31) must render every trunk and branch cell in
    /// full. Before the fix, generation only ran the tree pass in the chunk
    /// layer holding the surface, so these trees were cut off at wy 32.
    #[test]
    fn test_shoreline_trees_are_not_clipped_at_the_chunk_y_boundary() {
        let seed = DEFAULT_WORLD_SEED;
        let (wx, wz, h, tree) = shoreline_tree_crossing_a_chunk_y_boundary(seed);
        let top = h + tree.trunk_height + TREE_CANOPY_TOP_OVER_TRUNK;

        // The repro premise: the tree genuinely straddles a vertical chunk
        // edge, so a surface-layer-only pass would have dropped its top cells.
        assert!(
            top >= CHUNK_SIZE as i32,
            "the chosen shoreline tree does not cross the first chunk-Y boundary"
        );
        assert_ne!(
            h.div_euclid(CHUNK_SIZE as i32),
            top.div_euclid(CHUNK_SIZE as i32),
            "the trunk/top must live in different vertical chunk layers"
        );

        for ty in (h + 1)..=(h + tree.trunk_height) {
            assert_eq!(
                generated_block_at(wx, ty, wz, seed, Season::Summer),
                BLOCK_OAK_LOG,
                "shoreline tree at ({wx}, {wz}) lost trunk block ({wx}, {ty}, {wz})"
            );
        }
        for (bx, by, bz) in tree.branch_cells(wx, wz, h) {
            assert_eq!(
                generated_block_at(bx, by, bz, seed, Season::Summer),
                BLOCK_OAK_LOG,
                "shoreline tree at ({wx}, {wz}) lost branch ({bx}, {by}, {bz})"
            );
        }
        assert_eq!(
            generated_block_at(wx, top, wz, seed, Season::Summer),
            BLOCK_OAK_LEAVES,
            "shoreline tree at ({wx}, {wz}) lost its canopy top at ({wx}, {top}, {wz})"
        );
    }

    fn assert_chunks_equal(actual: &ChunkBuffer, expected: &ChunkBuffer, context: &str) {
        assert_eq!(
            actual.is_uniform(),
            expected.is_uniform(),
            "{context}: uniform-ness differs"
        );
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    assert_eq!(
                        actual.get(lx, ly, lz),
                        expected.get(lx, ly, lz),
                        "{context}: cell ({lx}, {ly}, {lz}) differs"
                    );
                }
            }
        }
    }

    fn cells_of(chunk: &ChunkBuffer, block: u16) -> Vec<(usize, usize, usize)> {
        let mut cells = Vec::new();
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if chunk.get(lx, ly, lz) == block {
                        cells.push((lx, ly, lz));
                    }
                }
            }
        }
        cells
    }

    fn changed_cells(actual: &ChunkBuffer, legacy: &ChunkBuffer) -> Vec<(usize, usize, usize)> {
        let mut cells = Vec::new();
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if actual.get(lx, ly, lz) != legacy.get(lx, ly, lz) {
                        cells.push((lx, ly, lz));
                    }
                }
            }
        }
        cells
    }

    #[test]
    fn empty_rule_set_matches_the_legacy_generator() {
        let cases = [
            (ChunkPos::new(0, -1, 0), DEFAULT_WORLD_SEED, Season::Summer),
            (ChunkPos::new(0, 1, 0), 1337, Season::Winter),
            (ChunkPos::new(-1, 1, 2), 424_242, Season::Summer),
        ];
        for (pos, seed, season) in cases {
            let legacy = generate_chunk_seeded(pos, seed, season);
            let with_empty = generate_chunk_seeded_with(pos, seed, season, &[]);
            assert_chunks_equal(
                &with_empty,
                &legacy,
                &format!("empty rules for pos {pos:?}, seed {seed}, {season:?}"),
            );
        }
    }

    #[test]
    fn pack_ore_rule_places_authored_block_inside_its_band() {
        let pos = ChunkPos::new(2, 0, -2);
        let seed = 424_242;
        let season = Season::Summer;
        let rules = [WorldgenRule::Ore {
            block: BLOCK_BRICK,
            replace: BLOCK_STONE,
            min_y: 4,
            max_y: 20,
            vein: 6,
            attempts: 12,
        }];
        let legacy = generate_chunk_seeded(pos, seed, season);
        let generated = generate_chunk_seeded_with(pos, seed, season, &rules);
        let base_wy = pos.y * CHUNK_SIZE as i32;

        // Diff against the legacy chunk so pre-existing ruin Brick cells (which
        // stand above the surface) are not mistaken for authored ore.
        let mut placed = Vec::new();
        for (lx, ly, lz) in changed_cells(&generated, &legacy) {
            if generated.get(lx, ly, lz) == BLOCK_BRICK {
                let wx = pos.x * CHUNK_SIZE as i32 + lx as i32;
                let wz = pos.z * CHUNK_SIZE as i32 + lz as i32;
                let wy = base_wy + ly as i32;
                let surface = terrain_height_at_seeded(wx, wz, seed);
                assert!(
                    (4..=20).contains(&wy),
                    "authored ore escaped its Y band at ({wx}, {wy}, {wz})"
                );
                assert!(
                    wy > -32,
                    "authored ore reached the bedrock floor at ({wx}, {wy}, {wz})"
                );
                assert!(
                    wy < surface - 3,
                    "authored ore punched the surface skin at ({wx}, {wy}, {wz}): surface {surface}"
                );
                placed.push((wx, wy, wz));
            }
        }
        assert!(
            !placed.is_empty(),
            "the first-proof ore rule must place at least one Brick cell"
        );
    }

    #[test]
    fn pack_ore_rule_does_not_alter_coal_or_iron() {
        let cases = [
            (ChunkPos::new(0, -1, 0), DEFAULT_WORLD_SEED, Season::Summer),
            (ChunkPos::new(0, 1, 0), 1337, Season::Winter),
            (ChunkPos::new(-1, 1, 2), 424_242, Season::Summer),
        ];
        // Broad authored band so the rule actually runs in every non-uniform
        // pinned case; the comparison is still exact cell-set equality.
        let rules = [WorldgenRule::Ore {
            block: BLOCK_BRICK,
            replace: BLOCK_STONE,
            min_y: -64,
            max_y: 128,
            vein: 8,
            attempts: 32,
        }];
        for (pos, seed, season) in cases {
            let legacy = generate_chunk_seeded(pos, seed, season);
            let generated = generate_chunk_seeded_with(pos, seed, season, &rules);
            assert_eq!(
                cells_of(&generated, BLOCK_COAL_ORE),
                cells_of(&legacy, BLOCK_COAL_ORE),
                "pack ore moved coal cells for pos {pos:?}, seed {seed}, {season:?}"
            );
            assert_eq!(
                cells_of(&generated, BLOCK_IRON_ORE),
                cells_of(&legacy, BLOCK_IRON_ORE),
                "pack ore moved iron cells for pos {pos:?}, seed {seed}, {season:?}"
            );
        }
    }

    #[test]
    fn pack_ore_rules_are_independent() {
        let pos = ChunkPos::new(2, 0, -2);
        let seed = 424_242;
        let season = Season::Summer;
        // Different host blocks: neither rule's candidate scan can consume a
        // cell the other rule writes, so their placements are independent even
        // though both bands cover the whole chunk.
        let rule_a = WorldgenRule::Ore {
            block: BLOCK_BRICK,
            replace: BLOCK_STONE,
            min_y: -64,
            max_y: 128,
            vein: 6,
            attempts: 16,
        };
        let rule_b = WorldgenRule::Ore {
            block: BLOCK_CRAFTING_TABLE,
            replace: BLOCK_COAL_ORE,
            min_y: -64,
            max_y: 128,
            vein: 4,
            attempts: 16,
        };
        let legacy = generate_chunk_seeded(pos, seed, season);
        let only_a = generate_chunk_seeded_with(pos, seed, season, std::slice::from_ref(&rule_a));
        // The per-rule salt includes the authored slice index, so hold rule B
        // at index 1 with a no-op placeholder that neither pass acts on.
        let noop = WorldgenRule::Patch {
            block: BLOCK_BRICK,
            replace: BLOCK_GRASS,
            biomes: vec![Biome::Plains.tag()],
            min_y: 0,
            max_y: 0,
            chance: 0.0,
        };
        let only_b = generate_chunk_seeded_with(pos, seed, season, &[noop, rule_b.clone()]);
        let both = generate_chunk_seeded_with(pos, seed, season, &[rule_a.clone(), rule_b.clone()]);
        let both_again =
            generate_chunk_seeded_with(pos, seed, season, &[rule_a.clone(), rule_b.clone()]);

        assert!(
            !changed_cells(&only_a, &legacy).is_empty(),
            "the first independent rule must place cells"
        );
        assert!(
            !changed_cells(&only_b, &legacy).is_empty(),
            "the second independent rule must place cells"
        );
        assert_eq!(
            cells_of(&only_a, BLOCK_BRICK),
            cells_of(&both, BLOCK_BRICK),
            "rule A's cells changed when rule B was also installed"
        );
        assert_eq!(
            cells_of(&only_b, BLOCK_CRAFTING_TABLE),
            cells_of(&both, BLOCK_CRAFTING_TABLE),
            "rule B's cells changed when rule A was also installed"
        );
        assert_chunks_equal(&both, &both_again, "two rule runs must be identical");
    }

    #[test]
    fn pack_patch_rule_replaces_only_the_matching_surface_cell() {
        let pos = ChunkPos::new(2, 0, -2);
        let seed = 424_242;
        let season = Season::Summer;
        let legacy = generate_chunk_seeded(pos, seed, season);
        let rules = [WorldgenRule::Patch {
            block: BLOCK_BRICK,
            replace: BLOCK_GRASS,
            biomes: vec![Biome::Plains.tag(), Biome::Forest.tag()],
            min_y: -64,
            max_y: 128,
            chance: 1.0,
        }];
        let generated = generate_chunk_seeded_with(pos, seed, season, &rules);
        let base_wy = pos.y * CHUNK_SIZE as i32;

        // The pinned chunk must contain a structure so the surface-only proof
        // is not vacuous.
        assert!(
            !cells_of(&legacy, BLOCK_OAK_LOG).is_empty()
                || !cells_of(&legacy, BLOCK_LOOT_CHEST).is_empty(),
            "pinned chunk must contain a tree or ruin"
        );

        let mut changed_surfaces = 0usize;
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = pos.x * CHUNK_SIZE as i32 + lx as i32;
                let wz = pos.z * CHUNK_SIZE as i32 + lz as i32;
                let h = terrain_height_at_seeded(wx, wz, seed);
                let biome = biome_for_height(h, forest_noise(wx, wz, seed));
                let surface_ly = h - base_wy;
                if !(0..CHUNK_SIZE as i32).contains(&surface_ly) {
                    continue;
                }
                let surface_ly = surface_ly as usize;
                let expected = if legacy.get(lx, surface_ly, lz) == BLOCK_GRASS
                    && matches!(biome, Biome::Plains | Biome::Forest)
                {
                    changed_surfaces += 1;
                    BLOCK_BRICK
                } else {
                    legacy.get(lx, surface_ly, lz)
                };
                assert_eq!(
                    generated.get(lx, surface_ly, lz),
                    expected,
                    "surface cell ({lx}, {surface_ly}, {lz}) did not follow the patch rule"
                );
                for ly in 0..CHUNK_SIZE {
                    if ly == surface_ly {
                        continue;
                    }
                    assert_eq!(
                        generated.get(lx, ly, lz),
                        legacy.get(lx, ly, lz),
                        "patch touched a non-surface cell at ({lx}, {ly}, {lz})"
                    );
                }
            }
        }
        assert!(
            changed_surfaces > 0,
            "the chance-1.0 patch must replace at least one Grass surface"
        );
    }

    #[test]
    fn pack_patch_rule_respects_biome_and_y_bounds() {
        let pos = ChunkPos::new(2, 0, 2);
        let seed = 424_242;
        let season = Season::Summer;
        let legacy = generate_chunk_seeded(pos, seed, season);
        let base_wy = pos.y * CHUNK_SIZE as i32;

        let forest_only = [WorldgenRule::Patch {
            block: BLOCK_BRICK,
            replace: BLOCK_GRASS,
            biomes: vec![Biome::Forest.tag()],
            min_y: -64,
            max_y: 128,
            chance: 1.0,
        }];
        let generated = generate_chunk_seeded_with(pos, seed, season, &forest_only);
        let mut forest_surfaces = 0usize;
        for (lx, ly, lz) in changed_cells(&generated, &legacy) {
            let wx = pos.x * CHUNK_SIZE as i32 + lx as i32;
            let wz = pos.z * CHUNK_SIZE as i32 + lz as i32;
            let h = terrain_height_at_seeded(wx, wz, seed);
            let biome = biome_for_height(h, forest_noise(wx, wz, seed));
            assert_eq!(biome, Biome::Forest, "patch left its authored biome list");
            assert_eq!(base_wy + ly as i32, h, "patch wrote off the surface cell");
            assert_eq!(legacy.get(lx, ly, lz), BLOCK_GRASS);
            forest_surfaces += 1;
        }
        assert!(
            forest_surfaces > 0,
            "the pinned chunk must contain at least one Forest Grass surface"
        );

        // A rule whose authored Y band excludes every surface in the chunk
        // must be a full no-op.
        let out_of_band = [WorldgenRule::Patch {
            block: BLOCK_BRICK,
            replace: BLOCK_GRASS,
            biomes: vec![Biome::Plains.tag(), Biome::Forest.tag()],
            min_y: 200,
            max_y: 300,
            chance: 1.0,
        }];
        let excluded = generate_chunk_seeded_with(pos, seed, season, &out_of_band);
        assert_chunks_equal(&excluded, &legacy, "out-of-band patch must be a no-op");
    }

    #[test]
    fn pack_rules_allow_air_as_an_authored_block() {
        let pos = ChunkPos::new(2, 0, -2);
        let seed = 424_242;
        let season = Season::Summer;
        let legacy = generate_chunk_seeded(pos, seed, season);
        let base_wy = pos.y * CHUNK_SIZE as i32;

        // Air replaced by Air is a documented no-op, not a panic or a
        // validation error.
        let air_to_air = [WorldgenRule::Ore {
            block: BLOCK_AIR,
            replace: BLOCK_AIR,
            min_y: -64,
            max_y: 128,
            vein: 6,
            attempts: 12,
        }];
        let unchanged = generate_chunk_seeded_with(pos, seed, season, &air_to_air);
        assert_chunks_equal(&unchanged, &legacy, "Air -> Air ore must be a no-op");

        // Air authored as the replacement output clears only cells that pass
        // the same host/stone-band/Y-band guards as any other ore rule.
        let air_clears_stone = [WorldgenRule::Ore {
            block: BLOCK_AIR,
            replace: BLOCK_STONE,
            min_y: -64,
            max_y: 128,
            vein: 6,
            attempts: 12,
        }];
        let cleared = generate_chunk_seeded_with(pos, seed, season, &air_clears_stone);
        let mut cleared_cells = 0usize;
        for (lx, ly, lz) in changed_cells(&cleared, &legacy) {
            let wx = pos.x * CHUNK_SIZE as i32 + lx as i32;
            let wz = pos.z * CHUNK_SIZE as i32 + lz as i32;
            let wy = base_wy + ly as i32;
            let surface = terrain_height_at_seeded(wx, wz, seed);
            assert_eq!(legacy.get(lx, ly, lz), BLOCK_STONE);
            assert_eq!(cleared.get(lx, ly, lz), BLOCK_AIR);
            assert!(wy > -32, "Air rule reached the bedrock floor");
            assert!(wy < surface - 3, "Air rule punched the surface skin");
            cleared_cells += 1;
        }
        assert!(
            cleared_cells > 0,
            "Air-as-output ore rule must clear at least one Stone cell"
        );
    }

    const VILLAGE_TEST_SEED: u32 = 424_242;

    const VILLAGE_TEST_CELL: VillageCell = VillageCell { x: 3, z: 5 };

    fn checked_in_village_catalog() -> VillageCatalog {
        let (catalog, diagnostics) = VillageCatalog::load_sorted(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../worldgen"
        )));
        assert!(
            diagnostics.is_empty(),
            "village diagnostics: {diagnostics:?}"
        );
        assert!(!catalog.is_empty(), "checked-in village catalog must load");
        catalog
    }

    fn pinned_village_plan(catalog: &VillageCatalog) -> VillagePlan<'_> {
        let spawn = find_spawn_column(VILLAGE_TEST_SEED);
        village_plan_with_spawn(VILLAGE_TEST_CELL, VILLAGE_TEST_SEED, catalog, spawn)
            .expect("pinned village cell must plan")
    }

    fn village_context(catalog: &VillageCatalog) -> WorldGenContext<'_> {
        WorldGenContext {
            rules: &[],
            villages: Some(catalog),
            spawn_column: Some(find_spawn_column(VILLAGE_TEST_SEED)),
        }
    }

    fn local_cell(world: [i32; 3]) -> (usize, usize, usize) {
        let cs = CHUNK_SIZE as i32;
        (
            world[0].rem_euclid(cs) as usize,
            world[1].rem_euclid(cs) as usize,
            world[2].rem_euclid(cs) as usize,
        )
    }

    fn chunk_max_height(pos: ChunkPos, seed: u32) -> i32 {
        let base_wx = pos.x * CHUNK_SIZE as i32;
        let base_wz = pos.z * CHUNK_SIZE as i32;
        let mut max_h = i32::MIN;
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                max_h = max_h.max(terrain_height_at_seeded(
                    base_wx + lx as i32,
                    base_wz + lz as i32,
                    seed,
                ));
            }
        }
        max_h
    }

    /// A 32-tall hub whose cells reach `base_y + 31`, so a plan can place cells
    /// above `MAX_TERRAIN_HEIGHT`-free chunks and trip the legacy shortcuts.
    fn tall_hub_catalog() -> VillageCatalog {
        let mut blocks = Vec::new();
        for y in 0..32u32 {
            blocks.push(PrefabBlock {
                position: [2, y, 2],
                block: BLOCK_STONE,
            });
        }
        let prefab = Prefab {
            name: "tall_hub".to_owned(),
            origin: [2, 0, 2],
            size: [5, 32, 5],
            blocks,
            sockets: Vec::new(),
        };
        let definition = VillageDefinition {
            name: "tall".to_owned(),
            biomes: vec![Biome::Plains, Biome::Forest],
            hub: "tall_hub".to_owned(),
            spacing: 4,
            min_prefabs: 10,
            max_prefabs: 20,
            max_slope: 6,
            house: Vec::new(),
            farm: Vec::new(),
            well: Vec::new(),
            junction: Vec::new(),
        };
        VillageCatalog::from_content(vec![prefab], vec![definition])
    }

    /// The empty-catalog contract: `villages: None` and an empty catalog must
    /// both reproduce `generate_chunk_seeded_with(..., &[])` exactly, including
    /// uniform-Air layers and genuine village-candidate chunks.
    #[test]
    fn empty_catalog_identity() {
        let seed = VILLAGE_TEST_SEED;
        let empty = VillageCatalog::default();
        let catalog = checked_in_village_catalog();
        let mut positions = Vec::new();
        for cz in -1..=1 {
            for cx in -1..=1 {
                positions.push(ChunkPos::new(cx, 1, cz));
            }
        }
        // The uniform-Air sky layer must keep taking the legacy shortcut.
        positions.push(ChunkPos::new(0, 4, 0));

        // A real candidate chunk from the checked-in plan, so empty-catalog
        // identity is proven where a village *would* have been written.
        let plan = pinned_village_plan(&catalog);
        let cs = CHUNK_SIZE as i32;
        let mut candidates = 0usize;
        for cx in plan.bounds.min_x.div_euclid(cs)..=plan.bounds.max_x.div_euclid(cs) {
            for cz in plan.bounds.min_z.div_euclid(cs)..=plan.bounds.max_z.div_euclid(cs) {
                for cy in plan.bounds.min_y.div_euclid(cs)..=plan.bounds.max_y.div_euclid(cs) {
                    let pos = ChunkPos::new(cx, cy, cz);
                    if catalog.chunk_may_hold_village(pos, seed) {
                        positions.push(pos);
                        candidates += 1;
                    }
                }
            }
        }
        assert!(
            candidates > 0,
            "checked-in plan must expose candidate chunks"
        );

        for season in [Season::Spring, Season::Summer, Season::Winter] {
            for pos in &positions {
                let legacy = generate_chunk_seeded(*pos, seed, season);
                let baseline = generate_chunk_seeded_with(*pos, seed, season, &[]);
                assert_chunks_equal(
                    &baseline,
                    &legacy,
                    &format!("empty rule slice drifted at {pos:?}, {season:?}"),
                );
                let no_village =
                    generate_chunk_with_context(*pos, seed, season, &WorldGenContext::default());
                assert_chunks_equal(
                    &no_village,
                    &legacy,
                    &format!("villages: None drifted at {pos:?}, {season:?}"),
                );
                let empty_village = generate_chunk_with_context(
                    *pos,
                    seed,
                    season,
                    &WorldGenContext {
                        villages: Some(&empty),
                        ..WorldGenContext::default()
                    },
                );
                assert_chunks_equal(
                    &empty_village,
                    &legacy,
                    &format!("empty catalog drifted at {pos:?}, {season:?}"),
                );
            }
        }
    }

    /// The village pass may only write plan cells: every plan cell materializes
    /// verbatim, every difference from the legacy chunk is one of those cells,
    /// and chunks outside the plan bounds stay byte-identical.
    #[test]
    fn village_pass_writes_only_plan_cells() {
        let seed = VILLAGE_TEST_SEED;
        let catalog = checked_in_village_catalog();
        let plan = pinned_village_plan(&catalog);
        let context = village_context(&catalog);
        let season = Season::Summer;
        let cs = CHUNK_SIZE as i32;

        let mut changed_total = 0usize;
        let mut plan_cells_total = 0usize;
        let x_chunks = plan.bounds.min_x.div_euclid(cs) - 1..=plan.bounds.max_x.div_euclid(cs) + 1;
        let z_chunks = plan.bounds.min_z.div_euclid(cs) - 1..=plan.bounds.max_z.div_euclid(cs) + 1;
        let y_chunks = plan.bounds.min_y.div_euclid(cs) - 1..=plan.bounds.max_y.div_euclid(cs) + 1;

        for cy in y_chunks {
            for cx in x_chunks.clone() {
                for cz in z_chunks.clone() {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let legacy = generate_chunk_seeded(pos, seed, season);
                    let generated = generate_chunk_with_context(pos, seed, season, &context);
                    let plan_cells = plan.cells_for_chunk(pos);

                    if plan_cells.is_empty() {
                        // Only same-cell chunks have a provable identity here:
                        // a neighbouring cell may legitimately own its own
                        // village in the same checked-in catalog.
                        if village_cell_at(pos.x * cs, pos.z * cs) == plan.cell {
                            assert_chunks_equal(
                                &generated,
                                &legacy,
                                &format!("chunk {pos:?} is outside the plan but changed"),
                            );
                        }
                        continue;
                    }

                    let expected: HashSet<(usize, usize, usize)> = plan_cells
                        .iter()
                        .map(|cell| local_cell(cell.world))
                        .collect();
                    for cell in &plan_cells {
                        let local = local_cell(cell.world);
                        assert_eq!(
                            generated.get(local.0, local.1, local.2),
                            cell.block,
                            "village cell {:?} was not materialized in {pos:?}",
                            cell.world
                        );
                    }
                    let changed = changed_cells(&generated, &legacy);
                    for local in &changed {
                        assert!(
                            expected.contains(local),
                            "chunk {pos:?} changed non-plan cell {local:?}"
                        );
                    }
                    changed_total += changed.len();
                    plan_cells_total += plan_cells.len();
                }
            }
        }

        assert!(
            plan_cells_total > 0,
            "the pinned plan emitted no cells in its own bounds"
        );
        assert!(
            changed_total > 0,
            "the pinned plan changed nothing, so the pass was never exercised"
        );
    }

    /// The village pass is the true last pass: a chance-1.0 pack patch that
    /// would rewrite a village surface cell to Cobblestone must lose to the
    /// village block.
    #[test]
    fn pack_patch_cannot_overwrite_village() {
        let seed = VILLAGE_TEST_SEED;
        let catalog = checked_in_village_catalog();
        let plan = pinned_village_plan(&catalog);
        let spawn = find_spawn_column(seed);
        let season = Season::Summer;
        let cs = CHUNK_SIZE as i32;
        let rules = [WorldgenRule::Patch {
            block: BLOCK_COBBLESTONE,
            replace: BLOCK_GRASS,
            biomes: vec![Biome::Plains.tag(), Biome::Forest.tag()],
            min_y: 0,
            max_y: 200,
            chance: 1.0,
        }];
        let patch_only = WorldGenContext {
            rules: &rules,
            villages: None,
            spawn_column: Some(spawn),
        };
        let with_village = WorldGenContext {
            rules: &rules,
            villages: Some(&catalog),
            spawn_column: Some(spawn),
        };

        let mut found = false;
        'chunks: for cx in plan.bounds.min_x.div_euclid(cs)..=plan.bounds.max_x.div_euclid(cs) {
            for cz in plan.bounds.min_z.div_euclid(cs)..=plan.bounds.max_z.div_euclid(cs) {
                for cy in plan.bounds.min_y.div_euclid(cs)..=plan.bounds.max_y.div_euclid(cs) {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let legacy = generate_chunk_seeded(pos, seed, season);
                    for cell in plan.cells_for_chunk(pos) {
                        // Only a non-Cobblestone surface cell can prove the
                        // ordering: a Cobblestone village cell is
                        // indistinguishable from the patch block.
                        if cell.block == BLOCK_COBBLESTONE {
                            continue;
                        }
                        let [wx, wy, wz] = cell.world;
                        if wy != terrain_height_at_seeded(wx, wz, seed) {
                            continue;
                        }
                        let local = local_cell(cell.world);
                        if legacy.get(local.0, local.1, local.2) != BLOCK_GRASS {
                            continue;
                        }

                        let patched = generate_chunk_with_context(pos, seed, season, &patch_only);
                        let assembled =
                            generate_chunk_with_context(pos, seed, season, &with_village);
                        assert_eq!(
                            patched.get(local.0, local.1, local.2),
                            BLOCK_COBBLESTONE,
                            "patch rule did not rewrite the surface cell {:?}",
                            cell.world
                        );
                        assert_eq!(
                            assembled.get(local.0, local.1, local.2),
                            cell.block,
                            "pack patch beat the village pass at {:?}",
                            cell.world
                        );
                        found = true;
                        break 'chunks;
                    }
                }
            }
        }
        assert!(
            found,
            "no village surface cell was available to prove patch ordering"
        );
    }

    /// Season independence: the same plan cells materialize byte-for-byte in
    /// every season. Only the terrain surface skin and oak leaves are seasonal.
    #[test]
    fn village_pass_is_season_independent() {
        let seed = VILLAGE_TEST_SEED;
        let catalog = checked_in_village_catalog();
        let plan = pinned_village_plan(&catalog);
        let spawn = find_spawn_column(seed);
        let cs = CHUNK_SIZE as i32;
        let mut checked = 0usize;

        for cx in plan.bounds.min_x.div_euclid(cs)..=plan.bounds.max_x.div_euclid(cs) {
            for cz in plan.bounds.min_z.div_euclid(cs)..=plan.bounds.max_z.div_euclid(cs) {
                for cy in plan.bounds.min_y.div_euclid(cs)..=plan.bounds.max_y.div_euclid(cs) {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let cells = plan.cells_for_chunk(pos);
                    if cells.is_empty() {
                        continue;
                    }
                    for season in [Season::Spring, Season::Summer, Season::Winter] {
                        let chunk = generate_chunk_with_context(
                            pos,
                            seed,
                            season,
                            &WorldGenContext {
                                rules: &[],
                                villages: Some(&catalog),
                                spawn_column: Some(spawn),
                            },
                        );
                        for cell in &cells {
                            let local = local_cell(cell.world);
                            assert_eq!(
                                chunk.get(local.0, local.1, local.2),
                                cell.block,
                                "village cell {:?} changed in {season:?}",
                                cell.world
                            );
                        }
                    }
                    checked += cells.len();
                }
            }
        }
        assert!(checked > 0, "the pinned plan emitted no cells to check");
    }

    /// The corrected shortcuts must not clip a village whose cells sit above a
    /// chunk's local terrain bound (the old second return used `max_h`).
    #[test]
    fn fast_paths_do_not_clip_villages() {
        // Formula pins: the candidate bound is wider, and the local shortcut is
        // disabled only for candidates.
        assert!(!sky_is_air(128, true), "candidate sky chunk was clipped");
        assert!(sky_is_air(128, false), "legacy sky bound must not move");
        assert!(!sky_is_air(
            MAX_TERRAIN_HEIGHT + TREE_CANOPY_HEADROOM + 1,
            true
        ));
        assert!(sky_is_air(
            MAX_TERRAIN_HEIGHT + TREE_CANOPY_HEADROOM + 1,
            false
        ));
        assert!(!above_local_terrain_is_air(64, 40, true));
        assert!(above_local_terrain_is_air(64, 40, false));

        let seed = VILLAGE_TEST_SEED;
        let catalog = tall_hub_catalog();
        let spawn = find_spawn_column(seed);
        let cs = CHUNK_SIZE as i32;
        let mut found = false;

        'cells: for cz in -12..=12 {
            for cx in -12..=12 {
                let cell = VillageCell { x: cx, z: cz };
                let Some(plan) = village_plan_with_spawn(cell, seed, &catalog, spawn) else {
                    continue;
                };
                if plan.bounds.max_y < 64 {
                    continue;
                }
                for chunk_x in plan.bounds.min_x.div_euclid(cs)..=plan.bounds.max_x.div_euclid(cs) {
                    for chunk_z in
                        plan.bounds.min_z.div_euclid(cs)..=plan.bounds.max_z.div_euclid(cs)
                    {
                        for chunk_y in
                            plan.bounds.min_y.div_euclid(cs)..=plan.bounds.max_y.div_euclid(cs)
                        {
                            let pos = ChunkPos::new(chunk_x, chunk_y, chunk_z);
                            let cells = plan.cells_for_chunk(pos);
                            if cells.is_empty() {
                                continue;
                            }
                            let base_wy = chunk_y * cs;
                            let max_h = chunk_max_height(pos, seed);
                            if !above_local_terrain_is_air(base_wy, max_h, false) {
                                continue;
                            }
                            assert!(
                                catalog.chunk_may_hold_village(pos, seed),
                                "repro chunk {pos:?} must be a village candidate"
                            );

                            // The legacy shortcut returns uniform Air here and
                            // would silently drop every village cell.
                            let legacy = generate_chunk_seeded(pos, seed, Season::Summer);
                            assert_eq!(
                                legacy.is_uniform(),
                                Some(BLOCK_AIR),
                                "repro chunk {pos:?} does not trip the legacy fast path"
                            );

                            let generated = generate_chunk_with_context(
                                pos,
                                seed,
                                Season::Summer,
                                &WorldGenContext {
                                    rules: &[],
                                    villages: Some(&catalog),
                                    spawn_column: Some(spawn),
                                },
                            );
                            for cell in &cells {
                                let local = local_cell(cell.world);
                                assert_eq!(
                                    generated.get(local.0, local.1, local.2),
                                    cell.block,
                                    "fast path clipped village cell {:?} in {pos:?}",
                                    cell.world
                                );
                            }
                            found = true;
                            break 'cells;
                        }
                    }
                }
            }
        }
        assert!(
            found,
            "no tall-village chunk above the local terrain bound was found"
        );
    }
}
