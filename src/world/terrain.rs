use crate::content::WorldgenRule;
use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, local_coords_from_index, Chunk, ChunkPos, CHUNK_SIZE};
use crate::world::prefab::PREFAB_CAPS;
use crate::world::season::Season;
use crate::world::village::{place_village_cells, VillageCatalog};

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

pub use opencraft_worldgen::biome::{
    biome_at_seeded, biome_for_height, forest_noise, Biome, BEACH_BAND, MOUNTAIN_HEIGHT_THRESHOLD,
    SEA_LEVEL,
};
pub use opencraft_worldgen::height::{
    terrain_height_at_seeded, MAX_TERRAIN_HEIGHT, MIN_TERRAIN_HEIGHT,
};
pub use opencraft_worldgen::ruin::{
    ruin_at, ruin_at_seeded, ruin_covers, ruin_loot, ruin_loot_chest_at, ruin_loot_chest_pos,
    RuinSpec, RUIN_FOOTPRINT_HALF,
};
pub use opencraft_worldgen::spawn::{
    find_spawn_column, is_spawn_safe, SPAWN_MIN_HEIGHT, SPAWN_SEARCH_RADIUS, SPAWN_TREE_CLEARANCE,
};
pub use opencraft_worldgen::tree::{
    oak_tree_at, tree_trunk_height, OakTree, TREE_CANOPY_HEADROOM, TREE_CANOPY_TOP_OVER_TRUNK,
    TREE_MAX_HEIGHT, TREE_MAX_HEIGHT_OVER_SURFACE, TREE_MAX_TRUNK_HEIGHT, TREE_MIN_HEIGHT,
};

pub const DEFAULT_WORLD_SEED: u32 = 1337;

/// Snow caps appear only above this mountain surface height.
pub const SNOW_CAP_HEIGHT: i32 = 96;

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

/// Built-in ore-stream salt. Pack rules must never reuse this value: a pack
/// stream seeded with it would consume the same sequence as coal/iron and
/// could no longer be proven byte-identical to an empty-rule generation.
const BUILTIN_ORE_SALT: u32 = 0x000F_E51D;

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
#[inline]
fn biome_discriminant(biome: Biome) -> u32 {
    match biome {
        Biome::Plains => 0,
        Biome::Forest => 1,
        Biome::Mountains => 2,
        Biome::Ocean => 3,
    }
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
            hash = fnv1a_u32(hash, block.to_raw() as u32);
            hash = fnv1a_u32(hash, replace.to_raw() as u32);
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
            hash = fnv1a_u32(hash, block.to_raw() as u32);
            hash = fnv1a_u32(hash, replace.to_raw() as u32);
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
    block: BlockId,
    wy: i32,
    surface: i32,
    replace: BlockId,
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
    let mut rng = Xorshift32::new(hash3d(pos.x, pos.y, pos.z, seed ^ BUILTIN_ORE_SALT));
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
    chunk: &mut Chunk,
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
            let biome = biome_at_seeded(rx, rz, seed);
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
    chunk: &mut Chunk,
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
                BlockId::Grass | BlockId::Snow
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
    chunk: &mut Chunk,
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
                if !rule_biomes.contains(&biomes[lz][lx]) {
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
///    holds `BlockId::Air`, so trees, leaves, the Grass surface skin, Snow and
///    Water are never overwritten. Footprint columns whose own surface is not
///    Grass are skipped too, so a shell cannot stand on a Sand/Stone neighbour.
/// 3. One loot chest: after the shell loop, a single `LootChest` is written at
///    [`ruin_loot_chest_pos`] (the anchor column, one above the surface) and
///    only when that cell is currently `Air`. An interlocking shell, leaves,
///    Water or any other occupant wins, and the anchor then carries no chest;
///    this pass never writes a second chest.
fn place_ruin_at(
    chunk: &mut Chunk,
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
            if (0..CHUNK_SIZE).any(|ly| chunk.get(lx, ly, lz) == BlockId::OakLog) {
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
                BlockId::Grass | BlockId::Snow
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
                if chunk.get(lx, ly, lz) == BlockId::Air {
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
        if chunk.get(lx, ly, lz) == BlockId::Air {
            chunk.set(lx, ly, lz, BlockId::LootChest);
        }
    }
}

/// Wall material for a footprint offset: Brick on the four corner pillars,
/// Cobblestone on the wall runs between them. Both are already complete,
/// legal-edit opaque blocks, so ruins add no block ids and no registry rows.
#[inline]
fn ruin_wall_block(dx: i32, dz: i32) -> BlockId {
    if dx.abs() == RUIN_FOOTPRINT_HALF && dz.abs() == RUIN_FOOTPRINT_HALF {
        BlockId::Brick
    } else {
        BlockId::Cobblestone
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
pub fn generate_chunk(pos: ChunkPos) -> Chunk {
    generate_chunk_seeded(pos, DEFAULT_WORLD_SEED, Season::Summer)
}

/// Surface block of a non-beach plains/forest column in `season`.
///
/// Winter paints the seasonal snow blanket; every other season keeps Grass.
/// Beach Sand, Ocean Sand and the Mountains altitude rule never call this, so
/// the seasonal blanket and the season-blind altitude snow cap stay
/// distinguishable by construction.
#[inline]
fn seasonal_land_surface(season: Season) -> BlockId {
    match season {
        Season::Winter => BlockId::Snow,
        Season::Spring | Season::Summer | Season::Autumn => BlockId::Grass,
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
pub fn generate_chunk_seeded(pos: ChunkPos, seed: u32, season: Season) -> Chunk {
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
) -> Chunk {
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
/// is the optional accepted village catalog (empty/N`one` reproduces the
/// pre-M015 generator byte-for-byte) and `spawn_column` lets a caller that
/// already knows the spawn skip the bounded [`find_spawn_column`] search when
/// the village planner needs it.
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
) -> Chunk {
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
        return Chunk::new_uniform(BlockId::Air);
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
            let biome = biomes[lz][lx];
            let beach = h <= SEA_LEVEL + BEACH_BAND;

            for ly in 0..CHUNK_SIZE {
                let wy = base_wy + ly as i32;
                let block = if wy <= -32 {
                    BlockId::Bedrock
                } else if wy < h - 3 {
                    BlockId::Stone
                } else if wy < h {
                    // Subsurface skin, three blocks deep. It is never carved
                    // (caves/ores stop at h - 4), so it is also the seal that
                    // keeps future water columns from draining into caves.
                    match biome {
                        Biome::Mountains => BlockId::Stone,
                        Biome::Ocean => BlockId::Sand,
                        Biome::Plains | Biome::Forest => {
                            if beach {
                                BlockId::Sand
                            } else {
                                BlockId::Dirt
                            }
                        }
                    }
                } else if wy == h {
                    match biome {
                        Biome::Mountains => {
                            if h >= SNOW_CAP_HEIGHT {
                                BlockId::Snow
                            } else {
                                BlockId::Stone
                            }
                        }
                        Biome::Ocean => BlockId::Sand,
                        Biome::Plains | Biome::Forest => {
                            if beach {
                                // Beaches stay Sand in every season: the
                                // seasonal blanket is land-only.
                                BlockId::Sand
                            } else {
                                seasonal_land_surface(season)
                            }
                        }
                    }
                } else if h < SEA_LEVEL && wy <= SEA_LEVEL {
                    // Ocean basin: static worldgen water from just above the
                    // seabed skin (wy > h) up to the named sea level. The
                    // three-block skin below stays Sand and cave carving stops
                    // at h - 4, so the seabed is a real seal. MEM230: no flow,
                    // never a persisted edit (see T02's Water rejection paths).
                    BlockId::Water
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
                            chunk.set(lx, ly_tree as usize, lz, BlockId::OakLog);
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
                            BlockId::Air | BlockId::OakLeaves
                        ) {
                            chunk.set(tlx, ly_b as usize, tlz, BlockId::OakLog);
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
    }

    place_ore_veins(&mut chunk, &heights, pos, seed);
    // Pack ore runs after built-in coal/iron and before ruins, each rule on
    // its own stream; the built-in ore cells are therefore untouched.
    place_pack_ore_veins(&mut chunk, &heights, pos, seed, ctx.rules);
    // Third structure pass (S07): after ores, so a ruin can never be carved by
    // the ore pass, and after trees, so the tree-overlap policy can see the
    // actual OakLog cells it refuses to overwrite.
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
    use crate::world::block::BlockId;

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
                    let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
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

    /// FNV-1a 64 over every cell in `linear_index` order (x fastest, then z,
    /// then y), hashing each block id as a little-endian `u16`.
    ///
    /// Deliberately local to this test module: the identity refactor swapped
    /// the `block as u8` expression for `to_raw()` without moving the pinned
    /// literals.
    fn fnv1a64_chunk_baseline(chunk: &Chunk) -> (Option<BlockId>, usize, u64) {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut non_air = 0usize;
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    debug_assert_eq!(linear_index(x, y, z), y * 1024 + z * 32 + x);
                    let block = chunk.get(x, y, z);
                    if block != BlockId::Air {
                        non_air += 1;
                    }
                    for byte in block.to_raw().to_le_bytes() {
                        hash ^= byte as u64;
                        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                }
            }
        }
        (chunk.is_uniform(), non_air, hash)
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
        // gate for S01: any terrain drift must fail here, not merely differ
        // from a second run of the same code.
        let expected: [(Option<BlockId>, usize, u64); 3] = [
            (None, 26_913, 5_429_992_561_724_371_152),
            (None, 20_878, 8_773_073_591_056_755_479),
            (Some(BlockId::Air), 0, 16_934_947_671_703_429_925),
        ];

        for ((pos, seed, season), expected) in cases.into_iter().zip(expected) {
            let chunk = generate_chunk_seeded(pos, seed, season);
            let actual = fnv1a64_chunk_baseline(&chunk);
            assert_eq!(
                actual, expected,
                "pre-M010 terrain baseline drifted for pos {pos:?}, seed {seed}, {season:?}"
            );
        }
    }
}
