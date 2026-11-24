//! Game adapter over the neutral terrain generator in `opencraft-worldgen`.
//!
//! Every generation pass — terrain/height, caves, built-in and pack ores,
//! ruins, oak trees, pack surface patches, the seasonal surface/leaf policy
//! and the final village assembly — lives in
//! [`opencraft_worldgen::terrain`]. This module keeps the game-facing public
//! API byte-compatible: it re-exports the neutral biome/height/ruin/spawn/tree
//! vocabulary, converts the two game-only inputs ([`WorldgenRule`] and
//! [`Season`]) at the boundary, and moves the neutral raw-`u16`
//! [`ChunkBuffer`] into a game [`Chunk`] without copying a dense cell array.

use std::borrow::Cow;

use opencraft_content::catalog::WorldgenRule as NeutralWorldgenRule;
use opencraft_worldgen::buffer::ChunkBuffer;

use crate::content::registry::worldgen_to_catalog;
use crate::content::WorldgenRule;
use crate::world::block::BlockId;
use crate::world::chunk::{Chunk, ChunkPos, ChunkStorage};
use crate::world::season::Season;
use crate::world::village::VillageCatalog;

use opencraft_worldgen::structure::StructureCatalog;

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
pub use opencraft_worldgen::terrain::DEFAULT_WORLD_SEED;
pub use opencraft_worldgen::tree::{
    oak_tree_at, tree_trunk_height, OakTree, TREE_CANOPY_HEADROOM, TREE_CANOPY_TOP_OVER_TRUNK,
    TREE_MAX_HEIGHT, TREE_MAX_HEIGHT_OVER_SURFACE, TREE_MAX_TRUNK_HEIGHT, TREE_MIN_HEIGHT,
};

/// Everything generation depends on besides `(pos, seed, season)`.
///
/// `rules` is the pack worldgen stack (empty for vanilla worlds), `villages`
/// is the optional accepted village catalog (empty/`None` reproduces the
/// pre-M015 generator byte-for-byte), `spawn_column` lets a caller that
/// already knows the spawn skip the bounded [`find_spawn_column`] search when
/// the village planner needs it, and `structures` is the optional accepted
/// structure catalog: `None` reproduces the compile-time builtin master
/// exactly (every headless call and every pre-M019 world), while `Some`
/// overlays the runtime `worldgen/structures` content by name.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorldGenContext<'a> {
    pub rules: &'a [WorldgenRule],
    pub villages: Option<&'a VillageCatalog>,
    pub spawn_column: Option<(i32, i32)>,
    pub structures: Option<&'a StructureCatalog>,
}

/// Maps the game calendar season onto the neutral crate season.
///
/// Exhaustive by construction: adding a game season without a neutral mirror
/// fails to compile here instead of silently generating the wrong blanket.
#[inline]
fn neutral_season(season: Season) -> opencraft_worldgen::season::Season {
    match season {
        Season::Spring => opencraft_worldgen::season::Season::Spring,
        Season::Summer => opencraft_worldgen::season::Season::Summer,
        Season::Autumn => opencraft_worldgen::season::Season::Autumn,
        Season::Winter => opencraft_worldgen::season::Season::Winter,
    }
}

/// Converts authored game rules into neutral catalog rules, preserving
/// authored order and vector order.
///
/// The empty slice is borrowed, never allocated: a world with no pack rules
/// keeps the adapter work-free, and the crate's own empty-rule path keeps
/// generation byte-identical to the pre-M010 generator.
#[inline]
fn neutral_rules(rules: &[WorldgenRule]) -> Cow<'_, [NeutralWorldgenRule]> {
    if rules.is_empty() {
        Cow::Borrowed(&[])
    } else {
        Cow::Owned(rules.iter().map(worldgen_to_catalog).collect())
    }
}

/// Moves a neutral raw-`u16` buffer into a game chunk.
///
/// A uniform buffer stays uniform; a dense buffer's `Box<[u16; CHUNK_VOLUME]>`
/// is moved straight into [`ChunkStorage::Dense`], so no 64 KiB cell array is
/// ever copied at the boundary. Ids are interpreted through
/// [`BlockId::from_raw`], never rewritten, which keeps the pinned digests
/// byte-identical whether the caller reads the crate buffer or the game chunk.
pub(crate) fn chunk_from_buffer(buffer: ChunkBuffer) -> Chunk {
    match buffer {
        ChunkBuffer::Uniform(raw) => Chunk::new_uniform(BlockId::from_raw(raw)),
        ChunkBuffer::Dense(cells) => Chunk {
            storage: ChunkStorage::Dense(cells),
        },
    }
}

/// Generates `pos` with the default world seed and [`Season::Summer`].
///
/// Season-independent callers (tooling, small unit tests, previews) use this
/// wrapper: Summer is the blanket-absent baseline, so a caller that never
/// thinks about the calendar still sees the pre-seasonal surface.
pub fn generate_chunk(pos: ChunkPos) -> Chunk {
    generate_chunk_seeded(pos, DEFAULT_WORLD_SEED, Season::Summer)
}

/// Generates `pos` for `seed` under the given calendar `season`.
///
/// `season` is a plain value parameter — no manager state, no clock reads — so
/// generation stays a pure function of `(pos, seed, season)`. The neutral
/// crate changes exactly two things for it:
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
/// This is the empty-village wrapper over [`generate_chunk_with_context`], so
/// a world with no pack rules reproduces the pre-M010 generator byte-for-byte
/// by construction. Rules reach the crate in authored order and run after the
/// built-in passes:
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
            structures: None,
        },
    )
}

/// Generates `pos` for `seed`, `season` and the full [`WorldGenContext`] by
/// delegating to the neutral crate and moving its raw-`u16` buffer into a
/// game [`Chunk`].
///
/// The game and crate contexts differ only in their rule vocabulary: the pack
/// stack is converted field-for-field in authored order ([`worldgen_to_catalog`])
/// and the game season is mirrored one-to-one ([`neutral_season`]). Villages
/// and the spawn column are passed by reference because both sides share the
/// neutral [`VillageCatalog`], so no plan or cell list is ever rebuilt here.
///
/// An empty rule slice and an absent catalog stay byte-identical to the
/// pre-M015 generator.
pub fn generate_chunk_with_context(
    pos: ChunkPos,
    seed: u32,
    season: Season,
    ctx: &WorldGenContext<'_>,
) -> Chunk {
    let rules = neutral_rules(ctx.rules);
    let neutral_ctx = opencraft_worldgen::terrain::WorldGenContext {
        rules: rules.as_ref(),
        villages: ctx.villages,
        spawn_column: ctx.spawn_column,
        structures: ctx.structures,
    };
    chunk_from_buffer(opencraft_worldgen::terrain::generate_chunk_with_context(
        pos,
        seed,
        neutral_season(season),
        &neutral_ctx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;
    use crate::world::chunk::{linear_index, CHUNK_SIZE};

    const NEIGHBOR_OFFSETS: [(i32, i32, i32); 6] = [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];

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

    /// Cell-for-cell chunk equality, including the uniform/dense storage
    /// shape. Used by the adapter tests to pin byte-identical delegation
    /// without serializing a game chunk.
    fn chunks_equal(a: &Chunk, b: &Chunk) -> bool {
        a.is_uniform() == b.is_uniform()
            && (0..CHUNK_SIZE).all(|y| {
                (0..CHUNK_SIZE).all(|z| (0..CHUNK_SIZE).all(|x| a.get(x, y, z) == b.get(x, y, z)))
            })
    }

    /// Finds the first surface chunk in a small pinned window and returns its
    /// position. Bounded, deterministic and independent of iteration order:
    /// the first `Grass` cell pins the chunk, and the caller only needs the
    /// position.
    fn first_grass_chunk(seed: u32) -> ChunkPos {
        for cx in -1..=1 {
            for cz in -1..=1 {
                for cy in 0..=3 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let chunk = generate_chunk_seeded(pos, seed, Season::Summer);
                    let has_grass = (0..CHUNK_SIZE).any(|y| {
                        (0..CHUNK_SIZE)
                            .any(|z| (0..CHUNK_SIZE).any(|x| chunk.get(x, y, z) == BlockId::Grass))
                    });
                    if has_grass {
                        return pos;
                    }
                }
            }
        }
        panic!("no surface chunk with Grass in the pinned search window");
    }

    #[test]
    fn chunk_from_buffer_preserves_uniform_and_moves_dense() {
        let uniform = chunk_from_buffer(ChunkBuffer::uniform(BlockId::Air.to_raw()));
        assert_eq!(uniform.is_uniform(), Some(BlockId::Air));

        let mut buffer = ChunkBuffer::uniform(BlockId::Air.to_raw());
        buffer.set(0, 0, 0, BlockId::Grass.to_raw());
        buffer.set(5, 6, 7, BlockId::Cobblestone.to_raw());
        buffer.set(31, 31, 31, BlockId::IronOre.to_raw());

        // Snapshot every cell before the buffer is moved into the chunk.
        let mut expected = vec![0u16; crate::world::chunk::CHUNK_VOLUME];
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    expected[linear_index(x, y, z)] = buffer.get(x, y, z);
                }
            }
        }

        let chunk = chunk_from_buffer(buffer);
        assert!(
            matches!(&chunk.storage, ChunkStorage::Dense(_)),
            "a mixed buffer must move into dense storage, not flatten to uniform"
        );
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    assert_eq!(
                        chunk.get(x, y, z).to_raw(),
                        expected[linear_index(x, y, z)],
                        "cell ({x}, {y}, {z}) changed while moving the dense buffer"
                    );
                }
            }
        }
    }

    #[test]
    fn season_mirror_matches_the_game_semantics() {
        let seed = DEFAULT_WORLD_SEED;
        let pos = first_grass_chunk(seed);

        let summer = generate_chunk_seeded(pos, seed, Season::Summer);
        let winter = generate_chunk_seeded(pos, seed, Season::Winter);
        assert!(
            !chunks_equal(&summer, &winter),
            "surface chunk must differ in Winter"
        );
        assert!(chunks_equal(
            &summer,
            &generate_chunk_seeded(pos, seed, Season::Spring)
        ));
        assert!(chunks_equal(
            &summer,
            &generate_chunk_seeded(pos, seed, Season::Autumn)
        ));
    }

    #[test]
    fn default_wrapper_matches_the_seeded_summer_wrapper() {
        for pos in [
            ChunkPos::new(0, -1, 0),
            ChunkPos::new(-1, 1, 2),
            ChunkPos::new(3, 0, -2),
        ] {
            assert!(chunks_equal(
                &generate_chunk(pos),
                &generate_chunk_seeded(pos, DEFAULT_WORLD_SEED, Season::Summer)
            ));
        }
    }

    #[test]
    fn pack_rules_and_context_reach_the_crate_in_authored_order() {
        let seed = DEFAULT_WORLD_SEED;
        let pos = first_grass_chunk(seed);
        let rules = [
            WorldgenRule::Ore {
                block: BlockId::CoalOre,
                replace: BlockId::Stone,
                min_y: -64,
                max_y: 64,
                vein: 4,
                attempts: 8,
            },
            WorldgenRule::Patch {
                block: BlockId::Cobblestone,
                replace: BlockId::Grass,
                biomes: vec![Biome::Plains, Biome::Forest],
                min_y: -64,
                max_y: 200,
                chance: 1.0,
            },
        ];

        let actual = generate_chunk_with_context(
            pos,
            seed,
            Season::Summer,
            &WorldGenContext {
                rules: &rules,
                villages: None,
                spawn_column: None,
                structures: None,
            },
        );

        let neutral: Vec<NeutralWorldgenRule> = rules.iter().map(worldgen_to_catalog).collect();
        assert_eq!(neutral.len(), 2, "no authored rule may be dropped or added");
        let direct = chunk_from_buffer(opencraft_worldgen::terrain::generate_chunk_with_context(
            pos,
            seed,
            opencraft_worldgen::season::Season::Summer,
            &opencraft_worldgen::terrain::WorldGenContext {
                rules: &neutral,
                villages: None,
                spawn_column: None,
                structures: None,
            },
        ));
        assert!(
            chunks_equal(&actual, &direct),
            "the game context must reach the crate with the same rules in the same order"
        );

        let empty = generate_chunk_seeded(pos, seed, Season::Summer);
        assert!(
            !chunks_equal(&actual, &empty),
            "the two authored rules must change the chunk"
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
        // unmodified pre-M010 generator, except the Winter case re-pinned by
        // M019/S04 when oak output moved to authored prefab cells (D351). These
        // are the behavior-preservation gate for S01: any other terrain drift
        // must fail here, not merely differ from a second run of the same code.
        let expected: [(Option<BlockId>, usize, u64); 3] = [
            (None, 26_913, 5_429_992_561_724_371_152),
            (None, 20_889, 3_148_434_793_551_996_225),
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
