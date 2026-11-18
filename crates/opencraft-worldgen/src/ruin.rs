//! Pure `(seed, x, z)` surface-ruin placement and loot queries.
//!
//! A ruin is a deterministic 5x5 Cobblestone shell with Brick corner pillars
//! and a doorway, standing on one Plains/Forest land column. Placement is a
//! pure function of the column and the world seed: the chunk-interior margin,
//! the land height band, the biome gate, the sparse per-anchor hash and the
//! footprint-aware vertical-fit gate all live here, so the generator, the
//! spawn search and the village planner cannot disagree about where a ruin
//! stands or which column a chest owns.
//!
//! Nothing in this module reads a chunk, a clock, an RNG or a season: it is
//! chunk-free and device-free by construction, and it speaks only raw ids and
//! neutral tags. The game keeps the emission side (`place_ruin_at`), which
//! paints the shell into a `Chunk`.

use glam::IVec3;
use opencraft_core::chunk::CHUNK_SIZE;

use crate::biome::{biome_at_seeded, biome_for_height, forest_noise, Biome};
use crate::height::terrain_height_at_seeded;
use crate::loot::LootEntry;
use crate::noise::hash2d;

/// Horizontal half-extent of a surface ruin's square footprint, in blocks.
///
/// A 5x5 shell (`2 * RUIN_FOOTPRINT_HALF + 1`) is small enough to read as one
/// landmark and still fits inside a 32-block chunk once the anchor stays
/// `RUIN_ANCHOR_MARGIN` away from each edge.
pub const RUIN_FOOTPRINT_HALF: i32 = 2;

/// Anchor margin from each chunk edge, derived from the footprint: an anchor at
/// `RUIN_ANCHOR_MARGIN` reaches exactly chunk column 0 (and `CHUNK_SIZE - 1` on
/// the far side), so every ruin cell is written by the chunk that generated it.
/// That chunk confinement is what lets the loot seam (T02) stay registry- and
/// persistence-free: one chunk owns each ruin, exactly like one chunk owns a
/// tree.
const RUIN_ANCHOR_MARGIN: i32 = RUIN_FOOTPRINT_HALF;

/// Wall height band above the anchor column's surface, in blocks. Corner
/// pillars are one taller, so the tallest ruin cell sits `surface +
/// RUIN_WALL_MAX_HEIGHT + 1` = `surface + 4`, which is exactly the top block of
/// the `surface + 1..=surface + 4` spawn body. `is_spawn_safe` therefore
/// rejects every column inside a footprint rather than only wall cells.
const RUIN_WALL_MIN_HEIGHT: i32 = 1;
pub const RUIN_WALL_MAX_HEIGHT: i32 = 3;

/// Surface-height band a ruin may stand on: exactly the land band above the
/// Sand beach shelf and below Mountain stone (the same window trees use, so
/// both structures agree on what counts as buildable land).
pub const RUIN_MIN_HEIGHT: i32 = crate::height::SEA_LEVEL + crate::height::BEACH_BAND + 1;
pub const RUIN_MAX_HEIGHT: i32 = crate::height::MOUNTAIN_HEIGHT_THRESHOLD - 1;

/// Hash salt for the ruin gate, kept distinct from the tree (`777`) and ore
/// salts so ruin placement cannot correlate with those passes.
const RUIN_SALT: u32 = 0x5EED_9A11;

/// Sparse per-anchor gate: `1 / (RUIN_GATE_MASK + 1)` = 1/512 of eligible
/// interior anchors carry a ruin. Sized so a 32x32 chunk (~28x28 interior
/// anchors, roughly half of them Plains/Forest land) yields a ruin now and then
/// instead of in every chunk, while a 5x5 chunk headless scan still guarantees
/// several (the acceptance scan asserts a non-zero lower bound).
const RUIN_GATE_MASK: u32 = 0x1FF;

/// A deterministic surface ruin: a small Cobblestone shell with Brick corner
/// pillars and a doorway, standing on one Plains/Forest Grass column.
///
/// Produced by [`ruin_at`], which is the single placement rule shared by the
/// generator, the spawn search and the tests. The struct carries the anchor
/// column plus the surface it was derived from, so consumers never have to
/// re-derive the footprint or re-sample the terrain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuinSpec {
    /// World x of the footprint centre column. The shell spans
    /// `anchor_x - RUIN_FOOTPRINT_HALF..=anchor_x + RUIN_FOOTPRINT_HALF`.
    pub anchor_x: i32,
    /// World z of the footprint centre column.
    pub anchor_z: i32,
    /// Anchor column surface height; the shell starts at `surface + 1`.
    pub surface: i32,
    /// Wall height above `surface`; corner pillars are one block taller.
    pub wall_height: i32,
}

/// Position-only half of the ruin gate: the chunk-interior anchor margin plus
/// the sparse per-anchor hash.
///
/// Split out so callers that only need "could a ruin stand here at all" (the
/// spawn search) skip the terrain samples for the 511/512 anchors the hash
/// rejects. [`ruin_at`] remains the authoritative rule; this is its cheap
/// prefix, so the gate cannot drift between the two.
fn ruin_anchor_hash(wx: i32, wz: i32, seed: u32) -> Option<u32> {
    let half = CHUNK_SIZE as i32 - 1 - RUIN_ANCHOR_MARGIN;
    let lx = wx.rem_euclid(CHUNK_SIZE as i32);
    let lz = wz.rem_euclid(CHUNK_SIZE as i32);
    if !(RUIN_ANCHOR_MARGIN..=half).contains(&lx) || !(RUIN_ANCHOR_MARGIN..=half).contains(&lz) {
        return None;
    }
    let ruin_hash = hash2d(wx, wz, RUIN_SALT ^ seed);
    ((ruin_hash & RUIN_GATE_MASK) == 0).then_some(ruin_hash)
}

/// Wall height a ruin hash selects, inside the
/// `RUIN_WALL_MIN_HEIGHT..=RUIN_WALL_MAX_HEIGHT` band.
///
/// Split out so the footprint-fit regression can re-derive the height the gate
/// must account for instead of duplicating the mixer's bit slice.
fn ruin_wall_height(ruin_hash: u32) -> i32 {
    let span = (RUIN_WALL_MAX_HEIGHT - RUIN_WALL_MIN_HEIGHT + 1) as u32;
    RUIN_WALL_MIN_HEIGHT + ((ruin_hash >> 9) % span) as i32
}

/// Whether a ruin at `surface` with `wall_height` keeps its own corner pillar
/// in the chunk layer that owns the anchor surface.
///
/// Anchor-column half of the L085 bound, kept as the cheap prefix in
/// [`ruin_at`]. The authoritative gate is
/// [`ruin_footprint_fits_chunk_layer`]: `place_ruin_at` paints every shell
/// column from that column's own surface, so a footprint can cross a layer
/// boundary even when the anchor column does not. This check still rejects the
/// common anchor-boundary case without sampling the 25 footprint columns. The
/// topmost cell is the corner pillar at `surface + wall_height + 1`.
fn ruin_fits_chunk_layer(surface: i32, wall_height: i32) -> bool {
    let surface_ly = surface.rem_euclid(CHUNK_SIZE as i32);
    surface_ly + wall_height < CHUNK_SIZE as i32 - 1
}

/// Whether a ruin anchored at `(anchor_x, anchor_z)` keeps its **whole**
/// footprint in the chunk layer that owns the anchor surface.
///
/// L085: the generator only writes the layer that owns the anchor surface
/// (`place_surface_ruins` runs there), while `place_ruin_at` paints each shell
/// column from that column's own `terrain_height_at_seeded` and keeps defensive
/// in-range skips. The anchor-column check alone therefore misses two
/// boundary-incomplete classes:
///
/// - an uphill footprint column whose higher surface pushes its corner pillar
///   into the layer above — the highest corner cell, not the anchor cell, is
///   the top bound; and
/// - a downhill footprint column whose surface falls into the layer below,
///   where the in-range guard skips the whole column and the shell loses cells
///   entirely.
///
/// Sampling all 25 footprint columns is only paid by candidates that already
/// passed the cheap hash, height-band and biome gates.
fn ruin_footprint_fits_chunk_layer(
    anchor_x: i32,
    anchor_z: i32,
    surface: i32,
    wall_height: i32,
    seed: u32,
) -> bool {
    let base_wy = surface.div_euclid(CHUNK_SIZE as i32) * CHUNK_SIZE as i32;
    let mut min_h = surface;
    let mut max_h = surface;
    for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
        for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            let h = terrain_height_at_seeded(anchor_x + dx, anchor_z + dz, seed);
            min_h = min_h.min(h);
            max_h = max_h.max(h);
        }
    }
    // Every shell column needs its own surface inside the anchor layer (a lower
    // column would be skipped entirely), and the tallest footprint corner
    // (surface + wall_height + 1) must stay below the layer's last index.
    min_h >= base_wy && max_h + wall_height < base_wy + CHUNK_SIZE as i32 - 1
}

/// Whether a surface ruin is anchored at this world column, and which one.
///
/// Single source of truth shared by `generate_chunk_seeded`, `find_spawn_column`
/// and the tests, mirroring the `tree_trunk_height` shape: the chunk-interior
/// anchor margin, the land height band, the Plains/Forest gate, the sparse hash
/// gate and the footprint-aware vertical-fit gate all live here, so no caller
/// can disagree with the generator about where a ruin stands. The generator
/// additionally checks the actual generated surface block (`Grass`, or `Snow`
/// under the seasonal Winter blanket), which is the authoritative gate for
/// beaches, seabed and mountain stone.
///
/// Deliberately season-blind: ruins are a function of `(pos, seed)` only, so
/// the same ruins exist in every season. The generator's surface gate accepts
/// both the warm-season `Grass` skin and the seasonal Winter `Snow` blanket;
/// the biome/height gate above already excludes Sand, Stone and the altitude
/// Snow cap. Trees share the same season-blind placement after D088.
pub fn ruin_at(wx: i32, wz: i32, h: i32, biome: Biome, seed: u32) -> Option<RuinSpec> {
    let ruin_hash = ruin_anchor_hash(wx, wz, seed)?;
    // The band is the land window: above the Sand beach shelf and below the
    // Mountain stone threshold. Combined with the Plains/Forest gate below,
    // a ruin can never stand on Sand, Stone or a water column. The surface
    // may be Grass or the seasonal Winter Snow blanket.
    if !(RUIN_MIN_HEIGHT..=RUIN_MAX_HEIGHT).contains(&h) {
        return None;
    }
    if !matches!(biome, Biome::Plains | Biome::Forest) {
        return None;
    }
    let wall_height = ruin_wall_height(ruin_hash);
    // L085: reject anchors whose own tallest corner pillar would cross into
    // the chunk layer above the one that owns the anchor surface. This cheap
    // prefix is subsumed by the footprint check below, but it skips the 25
    // terrain samples for the common anchor-boundary case.
    if !ruin_fits_chunk_layer(h, wall_height) {
        return None;
    }
    // L085 closeout: each shell column is painted from its own surface, so the
    // whole footprint — highest corner and lowest column — must stay in the
    // anchor layer.
    if !ruin_footprint_fits_chunk_layer(wx, wz, h, wall_height, seed) {
        return None;
    }
    Some(RuinSpec {
        anchor_x: wx,
        anchor_z: wz,
        surface: h,
        wall_height,
    })
}

/// The ruin whose placement rule yields an anchor at this world column,
/// derived from `seed` alone.
///
/// Public wrapper for tests, demos and S08: samples the terrain height and
/// biome exactly as the generator does and forwards to [`ruin_at`], so no
/// caller can disagree with worldgen about *placement*.
///
/// It cannot observe the chunk-only suppressions the generator applies: an
/// `OakLog` anywhere in the footprint skips the anchor, a footprint column
/// whose generated surface is not `Grass` loses its shell cells, and an
/// occupied chest cell suppresses the `LootChest`. Callers that need to know
/// whether a ruin — or its chest — actually exists must inspect the live
/// chunk; [`ruin_loot_chest_at`] in particular only proves a cell is a *legal*
/// ruin chest cell, matching [`ruin_covering`]'s conservative contract.
pub fn ruin_at_seeded(wx: i32, wz: i32, seed: u32) -> Option<RuinSpec> {
    let h = terrain_height_at_seeded(wx, wz, seed);
    let biome = biome_at_seeded(wx, wz, seed);
    ruin_at(wx, wz, h, biome, seed)
}

/// The ruin whose footprint contains `(wx, wz)`, if any.
///
/// A footprint is `RUIN_FOOTPRINT_HALF` wide around its anchor, so the anchor
/// of any covering ruin must lie within that Chebyshev radius. Deriving the
/// answer from [`ruin_at`] keeps the footprint size, gate and height band in
/// one place: the spawn search (MEM242/MEM246 precedent) must not re-derive
/// them, or a `surface + 4` spawn could end up inside a ruin shell exactly
/// like an OakLog trunk.
///
/// Conservative by construction: it does not check the generated surface block
/// (that needs a chunk), so it may reject a column whose generated surface is
/// Sand or Stone even though no ruin could actually stand there. That
/// over-rejection is the same the tree clearance already accepts, and it keeps
/// the spawn purely a function of `seed`.
fn ruin_covering(wx: i32, wz: i32, seed: u32) -> Option<RuinSpec> {
    for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
        for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            let ax = wx + dx;
            let az = wz + dz;
            // Cheap position gate first: only hash-passing anchors pay for the
            // terrain + forest samples below.
            if ruin_anchor_hash(ax, az, seed).is_none() {
                continue;
            }
            let h = terrain_height_at_seeded(ax, az, seed);
            let biome = biome_for_height(h, forest_noise(ax, az, seed));
            if let Some(spec) = ruin_at(ax, az, h, biome, seed) {
                return Some(spec);
            }
        }
    }
    None
}

/// Whether any ruin footprint covers this column.
///
/// Public wrapper around the private [`ruin_covering`], added for the village
/// planner's last-pass fit gate: a village footprint must not overlap a ruin
/// shell for the same reason a spawn must not (the `surface + 4` wall top). It
/// is conservative for the same documented reasons as `ruin_covering` — it
/// cannot see the chunk-only Winter or surface suppressions — and performs no
/// generation work of its own.
pub fn ruin_covers(wx: i32, wz: i32, seed: u32) -> bool {
    ruin_covering(wx, wz, seed).is_some()
}

/// World position of a ruin's single loot chest: the anchor column, one block
/// above the surface.
///
/// That is the interior centre two blocks inside the +z doorway and on the
/// anchor column, so ownership is unambiguous even when two 5x5 shells
/// interlock: only one anchor can own this exact column, and `place_ruin_at`
/// still refuses to overwrite a cell another ruin already painted.
pub fn ruin_loot_chest_pos(spec: &RuinSpec) -> IVec3 {
    IVec3::new(spec.anchor_x, spec.surface + 1, spec.anchor_z)
}

/// Loot yielded by one ruin — the filled deterministic table.
///
/// Delegates to [`crate::loot::ruin_loot`] with the ruin's anchor column,
/// keeping the D084 signature (`seed`, `&RuinSpec`) and its purity contract: a
/// function of `(seed, ruin)` only. It reads no clock, RNG state, season/day
/// index or chunk, so loot can never make worldgen season-dependent (the
/// cross-slice season-purity invariant) and two callers asking about the same
/// ruin always get the same answer. The generator does not call it: loot is
/// materialized on demand from the chest position (see [`ruin_loot_chest_at`]).
pub fn ruin_loot(seed: u32, ruin: &RuinSpec) -> Vec<LootEntry> {
    crate::loot::ruin_loot(seed, ruin.anchor_x, ruin.anchor_z)
}

/// The loot of the ruin whose exact anchor-column chest cell is `pos`.
///
/// `Some` means `pos` is exactly [`ruin_loot_chest_pos`] for a legal ruin
/// anchored at `(pos.x, pos.z)`; the contents are that ruin's deterministic
/// table. `None` means "not a generated ruin chest" — the caller must open
/// empty rather than guess a neighbouring anchor's table, so an interlocked
/// ruin can never supply loot for a cell it does not own.
///
/// Pure and chunk-free like [`ruin_loot`]: it validates the position against
/// [`ruin_at_seeded`] and never re-derives placement or height rules. `Some`
/// means "legal ruin chest cell", not "the live world holds a `LootChest`
/// here": the generator can skip the anchor (footprint `OakLog` overlap), drop
/// shell cells on a non-Grass footprint column, or suppress the chest when its
/// cell is already occupied. It does not check the live block; the session
/// layer gates on the loot-chest block id, so a suppressed cell opens nothing
/// and a player-placed chest never derives ruin loot.
pub fn ruin_loot_chest_at(pos: IVec3, seed: u32) -> Option<Vec<LootEntry>> {
    let spec = ruin_at_seeded(pos.x, pos.z, seed)?;
    if ruin_loot_chest_pos(&spec) != pos {
        return None;
    }
    Some(ruin_loot(seed, &spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the game default seed; the crate never owns the game default.
    const DEFAULT_WORLD_SEED: u32 = 1337;

    /// Chunk radius of the ruin acceptance region (7x7 chunks). Sized from the
    /// 1/512 per-anchor gate: ~784 interior anchors per chunk, so this region
    /// yields dozens of ruins and the non-zero bounds below are not flaky.
    const RUIN_SCAN_RADIUS: i32 = 3;

    /// Every ruin anchor the placement rule yields inside a chunk region,
    /// derived from `ruin_at` exactly as the generator's scan does.
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

    /// Boundary pin for the anchor-column half of the L085 vertical-fit gate:
    /// the topmost corner pillar (`surface + wall_height + 1`) must stay in the
    /// anchor-surface layer.
    #[test]
    fn test_ruin_fits_chunk_layer() {
        // Local surface 26/27 leave room for every legal wall height.
        for wall_height in 1..=3 {
            assert!(ruin_fits_chunk_layer(26, wall_height));
            assert!(ruin_fits_chunk_layer(27, wall_height));
        }
        // 28 + wall 2 tops out at local 31; wall 3 crosses into the next layer.
        assert!(ruin_fits_chunk_layer(28, 2));
        assert!(!ruin_fits_chunk_layer(28, 3));
        // 29 only fits wall 1.
        assert!(ruin_fits_chunk_layer(29, 1));
        assert!(!ruin_fits_chunk_layer(29, 2));
        // Local 30/31 cannot fit even the shortest ruin.
        assert!(!ruin_fits_chunk_layer(30, 1));
        assert!(!ruin_fits_chunk_layer(31, 1));
        // The next chunk layer repeats the same local boundaries.
        assert!(ruin_fits_chunk_layer(60, 2));
        assert!(!ruin_fits_chunk_layer(60, 3));
        assert!(ruin_fits_chunk_layer(61, 1));
        assert!(!ruin_fits_chunk_layer(61, 2));
        assert!(!ruin_fits_chunk_layer(62, 1));
        assert!(!ruin_fits_chunk_layer(63, 1));

        // Exhaustive expectation over the full ruin height band.
        for surface in RUIN_MIN_HEIGHT..=RUIN_MAX_HEIGHT {
            let local = surface.rem_euclid(CHUNK_SIZE as i32);
            for wall_height in RUIN_WALL_MIN_HEIGHT..=RUIN_WALL_MAX_HEIGHT {
                assert_eq!(
                    ruin_fits_chunk_layer(surface, wall_height),
                    local + wall_height < CHUNK_SIZE as i32 - 1,
                    "surface {surface} (local {local}) with wall {wall_height}"
                );
            }
        }
    }

    /// L085 closeout regression: a ruin must keep its **whole footprint** in
    /// the chunk layer that owns the anchor surface, because `place_ruin_at`
    /// paints each shell column from that column's own surface. The anchor-only
    /// gate accepted both real sampled anchors below; the footprint-aware gate
    /// must reject them.
    #[test]
    fn test_ruin_footprint_vertical_fit_rejects_boundary_incomplete_anchors() {
        let seed = DEFAULT_WORLD_SEED;

        // Uphill footprint: the anchor surface 29 and wall 1 pass the
        // anchor-only bound, but footprint column (45, 121) sits at surface 31,
        // so its corner pillar would need (45, 33, 121) — one layer above the
        // chunk that generates the anchor.
        assert_eq!(terrain_height_at_seeded(47, 123, seed), 29);
        assert_eq!(terrain_height_at_seeded(45, 121, seed), 31);
        let hash = ruin_anchor_hash(47, 123, seed).expect("anchor passes the cheap gate");
        let wall_height = ruin_wall_height(hash);
        assert!(
            ruin_fits_chunk_layer(29, wall_height),
            "the anchor-only bound must accept (47, 123) so the regression is real"
        );
        assert!(!ruin_footprint_fits_chunk_layer(
            47,
            123,
            29,
            wall_height,
            seed
        ));
        assert_eq!(ruin_at_seeded(47, 123, seed), None);

        // Downhill footprint: the anchor surface 32 (local 0) and wall 1 pass
        // the anchor-only bound, but footprint column (-87, -111) sits at
        // surface 31 in the layer below, where `place_ruin_at` skips the whole
        // column and the shell loses cells.
        assert_eq!(terrain_height_at_seeded(-85, -109, seed), 32);
        assert_eq!(terrain_height_at_seeded(-87, -111, seed), 31);
        let hash = ruin_anchor_hash(-85, -109, seed).expect("anchor passes the cheap gate");
        let wall_height = ruin_wall_height(hash);
        assert!(
            ruin_fits_chunk_layer(32, wall_height),
            "the anchor-only bound must accept (-85, -109) so the regression is real"
        );
        assert!(!ruin_footprint_fits_chunk_layer(
            -85,
            -109,
            32,
            wall_height,
            seed
        ));
        assert_eq!(ruin_at_seeded(-85, -109, seed), None);
    }

    /// Every anchor the placement rule reports fits the chunk layer that owns
    /// its surface, so `ruin_at` never advertises a shell the generator cannot
    /// paint completely (L085).
    #[test]
    fn test_ruin_anchors_fit_their_chunk_layer() {
        let seed = DEFAULT_WORLD_SEED;
        let anchors = ruin_anchors_in_region(RUIN_SCAN_RADIUS, seed);
        assert!(!anchors.is_empty(), "region has no ruin anchors");
        let mut surface_ly_counts = [0usize; CHUNK_SIZE];
        for spec in &anchors {
            assert!(
                ruin_fits_chunk_layer(spec.surface, spec.wall_height),
                "anchor ({}, {}) surface {} wall {} does not fit its chunk layer",
                spec.anchor_x,
                spec.anchor_z,
                spec.surface,
                spec.wall_height
            );
            assert!(
                ruin_footprint_fits_chunk_layer(
                    spec.anchor_x,
                    spec.anchor_z,
                    spec.surface,
                    spec.wall_height,
                    seed
                ),
                "anchor ({}, {}) surface {} wall {} has a footprint outside its chunk layer",
                spec.anchor_x,
                spec.anchor_z,
                spec.surface,
                spec.wall_height
            );
            surface_ly_counts[spec.surface.rem_euclid(CHUNK_SIZE as i32) as usize] += 1;
        }
        // Measured after the footprint-aware gate on DEFAULT_WORLD_SEED: 65
        // surviving anchors in the 7x7-chunk region (anchor-only gate: 68). The
        // footprint gate also rejects anchors at other local surface heights
        // when a footprint column crosses a layer boundary, so the distribution
        // below is informational. 8 is a conservative non-vacuity floor far
        // below the measured count.
        assert!(
            anchors.len() >= 8,
            "measured {} ruin anchors in the {RUIN_SCAN_RADIUS}-chunk region (need >= 8)",
            anchors.len()
        );
        // The gate is not vacuous: anchors survive and every surviving anchor
        // (and its whole footprint) fits its chunk layer. The distribution is
        // kept visible for future measurements.
        eprintln!(
            "ruin anchors by local surface ly (seed {seed}): {:?}",
            surface_ly_counts
        );
    }

    /// `ruin_covers` is exactly the public footprint-coverage query: true for
    /// every column inside a scanned anchor's shell and false for a nearby-ish
    /// ruin-free column.
    #[test]
    fn ruin_covers_matches_anchor_coverage() {
        let seed = DEFAULT_WORLD_SEED;
        let anchors = ruin_anchors_in_region(2, seed);
        let spec = anchors
            .first()
            .expect("scan region must contain a ruin anchor");

        for dz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
            for dx in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                assert!(
                    ruin_covers(spec.anchor_x + dx, spec.anchor_z + dz, seed),
                    "ruin_covers missed footprint cell ({dx}, {dz}) of ({}, {})",
                    spec.anchor_x,
                    spec.anchor_z
                );
            }
        }

        // Pick a far column whose own 5x5 anchor window is ruin-free, so the
        // negative assertion cannot flake on a legal far-away ruin.
        let mut far = None;
        'search: for dz in 0..128 {
            for dx in 0..128 {
                let wx = spec.anchor_x + 4096 + dx;
                let wz = spec.anchor_z + 4096 + dz;
                let mut covered = false;
                for oz in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                    for ox in -RUIN_FOOTPRINT_HALF..=RUIN_FOOTPRINT_HALF {
                        if ruin_at_seeded(wx + ox, wz + oz, seed).is_some() {
                            covered = true;
                            break;
                        }
                    }
                    if covered {
                        break;
                    }
                }
                if !covered {
                    far = Some((wx, wz));
                    break 'search;
                }
            }
        }
        let (far_x, far_z) = far.expect("no ruin-free far column in the search window");
        assert!(!ruin_covers(far_x, far_z, seed));
    }
}
