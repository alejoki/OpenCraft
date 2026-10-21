//! Pure sky-light propagation over a region of chunk columns.
//!
//! D038 fixes the architecture: a per-chunk nibble sky-light grid (16 KB per
//! chunk, matching the 4-bit per-vertex light scale), a vertical column shaft
//! pass followed by a lateral BFS, -1 light per step, 15 levels, and light is
//! never persisted. This module owns only that algorithm. The streaming
//! manager owns when it runs, where the grids live, and how they reach the
//! mesher.
//!
//! S01 opacity rule: every non-air `BlockId` is opaque (`BlockId::is_solid`).
//! There is no water or glass yet; when transparency lands in a later
//! milestone this is the single place that needs an opacity predicate.

use std::collections::VecDeque;

use rustc_hash::FxHashMap;

use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, ChunkPos, CHUNK_SIZE};
use crate::world::manager::{MAX_CHUNK_Y, MIN_CHUNK_Y};

/// Maximum sky-light value (4-bit scale).
pub const MAX_SKY_LIGHT: u8 = 15;

const CELLS_PER_CHUNK: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;
const BYTES_PER_CHUNK: usize = CELLS_PER_CHUNK / 2;

/// The six axis-aligned neighbours used by the lateral flood fill.
const NEIGHBORS: [(i32, i32, i32); 6] = [
    (-1, 0, 0),
    (1, 0, 0),
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
];

/// Nibble-packed per-chunk sky light.
///
/// Two horizontally adjacent cells (consecutive `linear_index` values) share a
/// byte: the even index in the low nibble, the odd index in the high nibble.
/// That is exactly 16 KB per chunk, and `PartialEq`/`Eq` compare the packed
/// bytes, which is the fast dirty-on-light-diff check the manager needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightGrid {
    bytes: Box<[u8; BYTES_PER_CHUNK]>,
}

impl LightGrid {
    /// All-zero grid (fully dark).
    pub fn new() -> Self {
        Self {
            bytes: Box::new([0u8; BYTES_PER_CHUNK]),
        }
    }

    /// All-15 grid (fully sky-lit). Used by tests and by the T05 uniform-light
    /// comparison baseline.
    pub fn full() -> Self {
        Self {
            bytes: Box::new([0xFFu8; BYTES_PER_CHUNK]),
        }
    }

    /// Read the light of a local chunk cell (0-15).
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        let idx = linear_index(x, y, z);
        let byte = self.bytes[idx >> 1];
        if idx & 1 == 0 {
            byte & 0x0F
        } else {
            byte >> 4
        }
    }

    /// Write the light of a local chunk cell (0-15).
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, light: u8) {
        debug_assert!(light <= MAX_SKY_LIGHT, "sky light must fit in 4 bits");
        let idx = linear_index(x, y, z);
        let byte = &mut self.bytes[idx >> 1];
        if idx & 1 == 0 {
            *byte = (*byte & 0xF0) | (light & 0x0F);
        } else {
            *byte = (*byte & 0x0F) | ((light & 0x0F) << 4);
        }
    }
}

impl Default for LightGrid {
    fn default() -> Self {
        Self::new()
    }
}

/// Byte-diff helper for dirty-on-light-diff remeshing.
#[inline]
pub fn grids_differ(a: &LightGrid, b: &LightGrid) -> bool {
    a != b
}

/// All-15 grid, the uniform-light baseline.
pub fn full_light_grid() -> LightGrid {
    LightGrid::full()
}

/// Compute sky light for every chunk column in the inclusive chunk region
/// `cx0..=cx1`, `cz0..=cz1`, across the full loaded Y range
/// (`MIN_CHUNK_Y..=MAX_CHUNK_Y`, 192 blocks).
///
/// `get_block` answers world-coordinate queries. The caller owns out-of-range
/// semantics: above the top loaded chunk should read `Air` (open sky) and below
/// `MIN_CHUNK_Y` should read `Bedrock`, matching `WorldManager::get_block`.
///
/// The result contains a grid for every `(cx, cy, cz)` in the region, including
/// chunks whose entire volume is solid or air.
///
/// Algorithm:
/// 1. Vertical shaft pass per world column: 15 while descending through
///    non-solid cells; at and below the first solid cell the shaft value is 0.
/// 2. Lateral BFS seeded from every shaft-lit cell, -1 per 6-neighbour step,
///    never entering solid cells, floor 0. Downward propagation is handled by
///    the shaft (`15` through open sky), so BFS steps attenuate in every
///    direction including down, exactly as D038 specifies.
pub fn compute_region_light<F>(
    get_block: F,
    cx0: i32,
    cx1: i32,
    cz0: i32,
    cz1: i32,
) -> FxHashMap<ChunkPos, LightGrid>
where
    F: Fn(i32, i32, i32) -> BlockId,
{
    let mut grids: FxHashMap<ChunkPos, LightGrid> = FxHashMap::default();
    if cx1 < cx0 || cz1 < cz0 {
        return grids;
    }

    let cs = CHUNK_SIZE as i32;
    let min_wx = cx0 * cs;
    let max_wx = (cx1 + 1) * cs; // exclusive
    let min_wz = cz0 * cs;
    let max_wz = (cz1 + 1) * cs; // exclusive
    let min_wy = MIN_CHUNK_Y * cs;
    let max_wy = (MAX_CHUNK_Y + 1) * cs; // exclusive

    let width = (max_wx - min_wx) as usize;
    let depth = (max_wz - min_wz) as usize;
    let height = (max_wy - min_wy) as usize;
    let layer = width * depth;

    let index = |wx: i32, wy: i32, wz: i32| -> usize {
        ((wy - min_wy) as usize) * layer
            + ((wz - min_wz) as usize) * width
            + ((wx - min_wx) as usize)
    };

    let mut solid = vec![false; layer * height];
    let mut light = vec![0u8; layer * height];

    // Pass 1: vertical shafts. A column is fully lit above its first solid
    // cell and fully dark at and below it.
    for wz in min_wz..max_wz {
        for wx in min_wx..max_wx {
            let mut level = MAX_SKY_LIGHT;
            for wy in (min_wy..max_wy).rev() {
                let idx = index(wx, wy, wz);
                if get_block(wx, wy, wz).is_solid() {
                    solid[idx] = true;
                    level = 0;
                } else {
                    light[idx] = level;
                }
            }
        }
    }

    // Pass 2: lateral flood fill. Seed only cells that can actually raise a
    // neighbour, which keeps an open-sky region (everything already 15) from
    // filling the queue with no-op work.
    let mut queue: VecDeque<usize> = VecDeque::new();
    for wy in min_wy..max_wy {
        for wz in min_wz..max_wz {
            for wx in min_wx..max_wx {
                let idx = index(wx, wy, wz);
                let level = light[idx];
                if level <= 1 {
                    continue;
                }
                let next = level - 1;
                let mut needs_flood = false;
                for (dx, dy, dz) in NEIGHBORS {
                    let nx = wx + dx;
                    let ny = wy + dy;
                    let nz = wz + dz;
                    if nx < min_wx
                        || nx >= max_wx
                        || ny < min_wy
                        || ny >= max_wy
                        || nz < min_wz
                        || nz >= max_wz
                    {
                        continue;
                    }
                    let nidx = index(nx, ny, nz);
                    if !solid[nidx] && light[nidx] < next {
                        needs_flood = true;
                        break;
                    }
                }
                if needs_flood {
                    queue.push_back(idx);
                }
            }
        }
    }

    while let Some(idx) = queue.pop_front() {
        let level = light[idx];
        if level <= 1 {
            continue;
        }
        let next = level - 1;

        let wx = (idx % width) as i32 + min_wx;
        let wz = ((idx / width) % depth) as i32 + min_wz;
        let wy = (idx / layer) as i32 + min_wy;

        for (dx, dy, dz) in NEIGHBORS {
            let nx = wx + dx;
            let ny = wy + dy;
            let nz = wz + dz;
            if nx < min_wx
                || nx >= max_wx
                || ny < min_wy
                || ny >= max_wy
                || nz < min_wz
                || nz >= max_wz
            {
                continue;
            }
            let nidx = index(nx, ny, nz);
            if solid[nidx] {
                continue;
            }
            if light[nidx] < next {
                light[nidx] = next;
                queue.push_back(nidx);
            }
        }
    }

    // Pack the working grid into per-chunk nibble grids.
    for cx in cx0..=cx1 {
        for cz in cz0..=cz1 {
            let base_wx = cx * cs;
            let base_wz = cz * cs;
            for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                let base_wy = cy * cs;
                let mut grid = LightGrid::new();
                for ly in 0..CHUNK_SIZE {
                    for lz in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let value = light[index(
                                base_wx + lx as i32,
                                base_wy + ly as i32,
                                base_wz + lz as i32,
                            )];
                            grid.set(lx, ly, lz, value);
                        }
                    }
                }
                grids.insert(ChunkPos::new(cx, cy, cz), grid);
            }
        }
    }

    grids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `get_block` closure from a solidity predicate.
    fn world<F>(solid: F) -> impl Fn(i32, i32, i32) -> BlockId
    where
        F: Fn(i32, i32, i32) -> bool,
    {
        move |x, y, z| {
            if solid(x, y, z) {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        }
    }

    fn chunk_grid(grids: &FxHashMap<ChunkPos, LightGrid>, cx: i32, cy: i32, cz: i32) -> &LightGrid {
        grids
            .get(&ChunkPos::new(cx, cy, cz))
            .expect("region must contain every chunk of every column")
    }

    #[test]
    fn open_sky_cells_are_full_light() {
        // Ground plane at y <= 0, open sky above.
        let get_block = world(|_, y, _| y <= 0);
        let grids = compute_region_light(&get_block, 0, 0, 0, 0);
        let grid = chunk_grid(&grids, 0, 0, 0);

        assert_eq!(grid.get(5, 1, 5), MAX_SKY_LIGHT);
        assert_eq!(grid.get(0, 31, 0), MAX_SKY_LIGHT);
        // Solid cells receive no light.
        assert_eq!(grid.get(5, 0, 5), 0);
    }

    #[test]
    fn cell_under_overhang_is_dim_but_not_dark() {
        // Ground at y <= 0, a roof at y == 4 covering x 0..=15.
        let get_block = world(|x, y, _| y <= 0 || (y == 4 && x <= 15));
        let grids = compute_region_light(&get_block, 0, 0, 0, 0);
        let grid = chunk_grid(&grids, 0, 0, 0);

        // Light bleeds 8 lateral steps in from the open column at x = 16.
        assert_eq!(grid.get(8, 3, 8), 15 - 8);
        assert!(grid.get(8, 3, 8) > 0);
        assert!(grid.get(8, 3, 8) < MAX_SKY_LIGHT);
        // The roof itself stays opaque.
        assert_eq!(grid.get(8, 4, 8), 0);
    }

    #[test]
    fn sealed_cave_is_completely_dark() {
        // Ground, plus a 5x5x5 solid cube holding one enclosed air cell.
        let get_block = world(|x, y, z| {
            y <= 0
                || ((1..=5).contains(&x)
                    && (1..=5).contains(&y)
                    && (1..=5).contains(&z)
                    && !(x == 3 && y == 3 && z == 3))
        });
        let grids = compute_region_light(&get_block, 0, 0, 0, 0);
        let grid = chunk_grid(&grids, 0, 0, 0);

        assert_eq!(grid.get(3, 3, 3), 0);
    }

    #[test]
    fn lateral_light_attenuates_by_one_per_step() {
        // Roof at y == 4 covering x 0..=20; open sky at x >= 21.
        let get_block = world(|x, y, _| y <= 0 || (y == 4 && x <= 20));
        let grids = compute_region_light(&get_block, 0, 0, 0, 0);
        let grid = chunk_grid(&grids, 0, 0, 0);

        assert_eq!(grid.get(21, 3, 8), MAX_SKY_LIGHT);
        for steps in 1..=10u8 {
            let x = 21 - steps as usize;
            assert_eq!(
                grid.get(x, 3, 8),
                MAX_SKY_LIGHT - steps,
                "cell {steps} lateral steps from the shaft-lit column"
            );
        }
    }

    #[test]
    fn chunk_border_light_is_region_size_independent() {
        // Roof over x < 32 (chunk column 0), open sky in chunk column 1+, and
        // the same cross-section for every z (including the extra ring), so a
        // wider region can only add paths the minimal region already had.
        let get_block = world(|x, y, _| y <= 0 || (y == 4 && x < 32));
        let minimal = compute_region_light(&get_block, 0, 1, 0, 0);
        let wider = compute_region_light(&get_block, -1, 2, -1, 1);

        for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
            for cx in 0..=1 {
                let minimal_grid = chunk_grid(&minimal, cx, cy, 0);
                let wider_grid = chunk_grid(&wider, cx, cy, 0);
                assert_eq!(
                    minimal_grid, wider_grid,
                    "chunk ({cx},{cy},0) light must not depend on region size"
                );
            }
        }

        // Non-vacuous seam check: light crosses the chunk border into the
        // overhang, and the value agrees with the wider-region computation.
        let border_minimal = chunk_grid(&minimal, 0, 0, 0).get(31, 3, 8);
        let border_wider = chunk_grid(&wider, 0, 0, 0).get(31, 3, 8);
        assert_eq!(border_minimal, 14);
        assert_eq!(border_wider, 14);
    }

    #[test]
    fn nibble_storage_round_trips_all_values() {
        let mut grid = LightGrid::new();

        for value in 0..=MAX_SKY_LIGHT {
            grid.set(5, 5, 5, value);
            assert_eq!(grid.get(5, 5, 5), value);
        }

        // Adjacent cells share a byte; writing one must not disturb the other.
        grid.set(0, 0, 0, 3);
        grid.set(1, 0, 0, 12);
        assert_eq!(grid.get(0, 0, 0), 3);
        assert_eq!(grid.get(1, 0, 0), 12);
        // Chunk corners and volume boundaries.
        grid.set(31, 31, 31, 15);
        assert_eq!(grid.get(31, 31, 31), 15);

        assert_eq!(grid.clone(), grid);
        assert_ne!(grid, LightGrid::new());
        assert!(grids_differ(&grid, &LightGrid::new()));
        assert!(!grids_differ(&grid, &grid.clone()));

        let full = full_light_grid();
        assert_eq!(full.get(0, 0, 0), MAX_SKY_LIGHT);
        assert_eq!(full.get(31, 31, 31), MAX_SKY_LIGHT);
        assert!(grids_differ(&full, &LightGrid::new()));
        assert_eq!(full, full_light_grid());
    }

    #[test]
    fn empty_or_inverted_region_returns_no_grids() {
        let get_block = world(|_, y, _| y <= 0);
        assert!(compute_region_light(&get_block, 0, -1, 0, 0).is_empty());
        assert!(compute_region_light(&get_block, 0, 0, 0, -1).is_empty());
    }

    #[test]
    #[should_panic]
    fn out_of_range_light_grid_index_panics() {
        // A y of 32 is outside the chunk; debug builds trip linear_index's
        // debug assertion and release builds panic on the slice bound.
        let grid = LightGrid::new();
        let _ = grid.get(0, CHUNK_SIZE, 0);
    }
}
