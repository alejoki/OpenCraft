//! Device-free village-planning proof for `opencraft-worldgen`.
//!
//! This is the outside-crate proof for the neutral world-generation crate: a
//! synthetic prefab vocabulary and one plains/forest village definition are
//! embedded as TOML, parsed through the crate's public codec, assembled into a
//! `VillageCatalog` and planned for a fixed seed. The proof asserts that the
//! plan has at least two placements, a socket link and a road, that chunk-local
//! emission is pure, and that a second full run reproduces the same
//! fingerprint, bounds and cells. Everything runs from memory: no path is
//! opened, no clock is read, no thread is spawned and no GPU object is created,
//! so the proof is byte-deterministic on a headless CI runner.

use opencraft_core::chunk::{ChunkPos, CHUNK_SIZE};
use opencraft_worldgen::prefab::{parse_prefab, Prefab, PrefabCatalog, PrefabError};
use opencraft_worldgen::spawn::find_spawn_column;
use opencraft_worldgen::village::{
    parse_village_definition, village_plan_with_spawn, VillageCatalog, VillageCell, VillagePlan,
};

/// Fixed proof seed; a literal, never derived from the code under test.
const SEED: u32 = 424_242;

/// Fixed cell window scanned in row-major order for the first two-placement
/// plan. Measured to contain one; widening it is the only sanctioned change if
/// the crate's deterministic terrain ever moves.
const SCAN_RADIUS: i32 = 8;

/// A 3x3 stone hub with one north socket.
const HUB: &str = r#"format = 1
name = "test_hub"
origin = [0, 0, 0]
size = [3, 3, 3]

layers = [
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
]

[palette]
"x" = "stone"

[[sockets]]
position = [1, 1, 0]
direction = "north"
"#;

/// A 2x2 oak-plank house with one north socket.
const HOUSE: &str = r#"format = 1
name = "test_house"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]

[palette]
"x" = "oak_planks"

[[sockets]]
position = [0, 1, 0]
direction = "north"
"#;

/// A 2x2 farmland patch with one north socket.
const FARM: &str = r#"format = 1
name = "test_farm"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]

[palette]
"x" = "farmland"

[[sockets]]
position = [0, 1, 0]
direction = "north"
"#;

/// One plains/forest village whose hub and junction are the tested hub prefab.
const VILLAGE: &str = r#"format = 1
name = "plains"
biomes = ["plains", "forest"]
hub = "test_hub"
spacing = 4
min_prefabs = 10
max_prefabs = 20
max_slope = 6

[pool]
house = ["test_house"]
farm = ["test_farm"]
well = []
junction = ["test_hub"]
"#;

/// A prefab document missing its `[palette]` table.
///
/// Embedded so the external contract can prove the same decoder the proof
/// relies on fails closed with a typed [`PrefabError`] instead of panicking.
pub const MALFORMED_PREFAB: &str = r#"format = 1
name = "broken"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]
"#;

/// The observable result of one device-free worldgen proof run.
#[derive(Clone, Copy, Debug)]
pub struct WorldgenProof {
    /// Planned village cell coordinates.
    pub village_cell: (i32, i32),
    /// Accepted prefab placements in the plan.
    pub prefabs: usize,
    /// Matched socket-to-socket connections.
    pub links: usize,
    /// Deduped road surface cells.
    pub roads: usize,
}

/// Parse the embedded malformed prefab fixture.
///
/// Not called by [`run`]: it exists so the external worldgen contract can show
/// that a malformed embedded document surfaces a typed error rather than
/// panicking.
pub fn parse_malformed_prefab() -> Result<Prefab, PrefabError> {
    parse_prefab(MALFORMED_PREFAB)
}

/// Build the catalog from the embedded TOML fixtures, entirely in memory.
fn fixture_catalog() -> VillageCatalog {
    let prefabs: Vec<Prefab> = [HUB, HOUSE, FARM]
        .into_iter()
        .map(|text| parse_prefab(text).expect("the embedded proof prefab must parse"))
        .collect();
    let prefab_catalog = PrefabCatalog::from_prefabs(prefabs.clone());
    let (definition, diagnostics) = parse_village_definition(VILLAGE, &prefab_catalog)
        .expect("the embedded proof village must parse");
    assert!(
        diagnostics.is_empty(),
        "the embedded proof village must parse without diagnostics: {diagnostics:?}"
    );
    VillageCatalog::from_content(prefabs, vec![definition])
}

/// Scan the fixed cell window row-major for the first two-placement plan.
fn plan_village(catalog: &VillageCatalog) -> VillagePlan<'_> {
    let spawn = find_spawn_column(SEED);
    for cz in -SCAN_RADIUS..=SCAN_RADIUS {
        for cx in -SCAN_RADIUS..=SCAN_RADIUS {
            let cell = VillageCell { x: cx, z: cz };
            if let Some(plan) = village_plan_with_spawn(cell, SEED, catalog, spawn) {
                if plan.placement_count() >= 2 {
                    return plan;
                }
            }
        }
    }
    panic!("no two-placement village plan in the fixed {SCAN_RADIUS}-cell window");
}

/// The first chunk covering the plan bounds that actually emits cells.
fn first_nonempty_chunk(plan: &VillagePlan<'_>) -> ChunkPos {
    let cs = CHUNK_SIZE as i32;
    for cy in plan.bounds.min_y.div_euclid(cs)..=plan.bounds.max_y.div_euclid(cs) {
        for cz in plan.bounds.min_z.div_euclid(cs)..=plan.bounds.max_z.div_euclid(cs) {
            for cx in plan.bounds.min_x.div_euclid(cs)..=plan.bounds.max_x.div_euclid(cs) {
                let pos = ChunkPos::new(cx, cy, cz);
                if !plan.cells_for_chunk(pos).is_empty() {
                    return pos;
                }
            }
        }
    }
    panic!("the plan bounds contain no chunk-local cells");
}

/// Run the village-planning proof twice and assert the runs agree.
///
/// Both runs parse the fixtures from memory, plan the same cell for the same
/// seed and compare fingerprint, shape and emitted cells. A mismatch means the
/// crate's plan is no longer a pure function of `(cell, seed, catalog)`.
pub fn run() -> WorldgenProof {
    let catalog = fixture_catalog();
    let first = plan_village(&catalog);
    let chunk = first_nonempty_chunk(&first);

    assert!(
        first.socket_link_count() >= 1,
        "the proof plan must connect at least one socket"
    );
    assert!(
        first.road_cell_count() >= 1,
        "the proof plan must emit at least one road cell"
    );
    let cells = first.cells_for_chunk(chunk);
    assert!(!cells.is_empty(), "the chosen bounds chunk must emit cells");
    assert_eq!(
        cells,
        first.cells_for_chunk(chunk),
        "chunk emission must be pure"
    );

    let second = plan_village(&catalog);
    assert_eq!(
        first.catalog_fingerprint, second.catalog_fingerprint,
        "two runs must keep one catalog fingerprint"
    );
    assert_eq!(first.cell, second.cell, "two runs must plan one cell");
    assert_eq!(first.anchor, second.anchor, "two runs must share an anchor");
    assert_eq!(first.bounds, second.bounds, "two runs must share bounds");
    assert_eq!(
        first.placement_count(),
        second.placement_count(),
        "two runs must place the same prefabs"
    );
    assert_eq!(
        first.socket_link_count(),
        second.socket_link_count(),
        "two runs must link the same sockets"
    );
    assert_eq!(
        first.road_cell_count(),
        second.road_cell_count(),
        "two runs must emit the same roads"
    );
    assert_eq!(
        cells,
        second.cells_for_chunk(chunk),
        "two runs must emit identical chunk cells"
    );

    WorldgenProof {
        village_cell: (first.cell.x, first.cell.z),
        prefabs: first.placement_count(),
        links: first.socket_link_count(),
        roads: first.road_cell_count(),
    }
}
