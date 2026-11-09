//! Library-level proof that a production `WorldSession` streams the pinned
//! seed-14 village through `WorldManager::update_streaming` — the same
//! generation and read seams a playing client uses (M015/S03-T03).
//!
//! The pinned site was measured from a real run (the `--demo-village` session
//! leg prints the same numbers). Expectations are pinned constants, never
//! derived from the code under test. Every session writes its save path only
//! in name: no save is ever issued, so a test-local directory must stay empty.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opencraft::content::ContentRegistry;
use opencraft::player::survival::GameMode;
use opencraft::world::chunk::{ChunkPos, CHUNK_SIZE};
use opencraft::world::season::{Season, DEFAULT_SEASON_LENGTH_DAYS};
use opencraft::world::terrain::{find_spawn_column, generate_chunk_seeded_with};
use opencraft::world::village::{
    village_plan_with_spawn, VillageCatalog, VillageCell, VillagePlan,
};
use opencraft::WorldSession;

/// Serialises the tests that drive the shared global rayon streaming pool.
///
/// Each test streams its own complete ring; running three rings concurrently
/// on one pool starves generation and can hold a target chunk past the pump
/// deadline. This is the same guard `WorldManager`'s streaming tests use.
static STREAMING_POOL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_streaming_pool() -> std::sync::MutexGuard<'static, ()> {
    STREAMING_POOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The pinned production seed whose village cell `(-1, 0)` lies inside the
/// default spawn ring.
const PINNED_SEED: u32 = 14;

/// The village cell the pinned seed plans.
const PINNED_CELL: VillageCell = VillageCell { x: -1, z: 0 };

/// Measured spawn column of the pinned seed.
const PINNED_SPAWN: (i32, i32) = (0, 0);

/// Measured non-empty village chunk count.
const PINNED_CHUNKS: usize = 6;

/// Measured total village blocks across the pinned chunks.
const PINNED_CELLS: usize = 2962;

/// Measured placement count of the pinned plan.
const PINNED_PLACEMENTS: usize = 14;

/// Render distance used for the streamed sessions. The generation spiral is
/// that distance plus two, which covers the whole village cell.
const SESSION_RENDER_DISTANCE: i32 = 6;

/// Wall-clock bound on one streamed pump loop.
///
/// A clean run needs well under 100 pumps even in a debug build, but every
/// test in this binary shares the process-wide rayon pool: a previous test's
/// already-dispatched generation and light jobs keep running after it stops
/// pumping. The generous deadline keeps that cross-test contention from
/// turning a healthy stream into a flaky failure, while still bounding a
/// genuinely stalled stream.
const PUMP_TIMEOUT: Duration = Duration::from_secs(60);

fn worldgen_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("worldgen")
}

/// Load the checked-in tree, asserting the reference counts/diagnostics.
fn load_catalog() -> VillageCatalog {
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&worldgen_root());
    assert!(
        diagnostics.is_empty(),
        "checked-in worldgen tree must load without diagnostics: {diagnostics:?}"
    );
    assert_eq!(catalog.prefabs().len(), 3, "three reference prefabs");
    assert_eq!(catalog.len(), 1, "one reference village definition");
    catalog
}

/// The pinned plan, built against the measured spawn column.
fn pinned_plan(catalog: &VillageCatalog) -> VillagePlan<'_> {
    village_plan_with_spawn(PINNED_CELL, PINNED_SEED, catalog, PINNED_SPAWN)
        .expect("the pinned seed must plan its pinned village cell")
}

/// Every chunk the plan writes into, plus the total cell count, in a stable
/// `(z, y, x)` order.
fn plan_chunks(plan: &VillagePlan<'_>) -> (Vec<ChunkPos>, usize) {
    let size = CHUNK_SIZE as i32;
    let mut chunks = Vec::new();
    let mut cells = 0;
    for cz in plan.bounds.min_z.div_euclid(size)..=plan.bounds.max_z.div_euclid(size) {
        for cy in plan.bounds.min_y.div_euclid(size)..=plan.bounds.max_y.div_euclid(size) {
            for cx in plan.bounds.min_x.div_euclid(size)..=plan.bounds.max_x.div_euclid(size) {
                let pos = ChunkPos::new(cx, cy, cz);
                let count = plan.cells_for_chunk(pos).len();
                if count > 0 {
                    cells += count;
                    chunks.push(pos);
                }
            }
        }
    }
    (chunks, cells)
}

/// Centre the stream on the village's own chunk AABB.
///
/// The plan itself is anchored to the world spawn column; this centre only
/// controls which ring the generation spiral visits first, so the target
/// chunks arrive immediately instead of after every inner ring.
fn stream_center(chunks: &[ChunkPos]) -> ChunkPos {
    let min_x = chunks.iter().map(|pos| pos.x).min().unwrap_or(0);
    let max_x = chunks.iter().map(|pos| pos.x).max().unwrap_or(0);
    let min_z = chunks.iter().map(|pos| pos.z).min().unwrap_or(0);
    let max_z = chunks.iter().map(|pos| pos.z).max().unwrap_or(0);
    ChunkPos::new((min_x + max_x) / 2, 0, (min_z + max_z) / 2)
}

/// A fresh production session that never writes: its save path lives inside
/// `dir` and no save is ever requested, so only the caller can prove the
/// directory stayed empty.
fn new_session(dir: &Path) -> WorldSession {
    WorldSession::new_session_with_mode_and_registry(
        PINNED_SEED,
        dir.join("village-session.mcvx"),
        DEFAULT_SEASON_LENGTH_DAYS,
        GameMode::Creative,
        ContentRegistry::builtin_arc(),
    )
}

/// Pump `update_streaming` until every target chunk is resident, returning the
/// pump count. Tests assert residency themselves, so a stalled stream fails
/// with the missing chunk rather than a timeout panic.
fn pump_until_resident(session: &mut WorldSession, chunks: &[ChunkPos]) -> usize {
    let center = stream_center(chunks);
    let deadline = Instant::now() + PUMP_TIMEOUT;
    let mut pumped = 0;
    while Instant::now() < deadline {
        session.world.update_streaming(center, Season::Summer);
        pumped += 1;
        if chunks.iter().all(|&pos| session.world.is_resident(pos)) {
            return pumped;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    pumped
}

fn session_block(session: &WorldSession, world: [i32; 3]) -> opencraft::world::block::BlockId {
    session.world.get_block(world[0], world[1], world[2])
}

#[test]
fn real_session_streams_the_seed_14_village() {
    let _heavy = lock_streaming_pool();
    let catalog = load_catalog();
    assert_eq!(
        find_spawn_column(PINNED_SEED),
        PINNED_SPAWN,
        "the pinned spawn column must stay the measured one"
    );
    let plan = pinned_plan(&catalog);
    assert_eq!(plan.placement_count(), PINNED_PLACEMENTS);

    let (chunks, cells) = plan_chunks(&plan);
    assert_eq!(chunks.len(), PINNED_CHUNKS, "measured non-empty chunks");
    assert_eq!(cells, PINNED_CELLS, "measured village cells");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut session = new_session(dir.path());
    session.set_village_catalog(Arc::new(catalog.clone()));
    session.world.set_render_distance(SESSION_RENDER_DISTANCE);
    let pumped = pump_until_resident(&mut session, &chunks);

    for &pos in &chunks {
        assert!(
            session.world.is_resident(pos),
            "chunk {pos:?} never became resident after {pumped} pumps"
        );
    }
    for &pos in &chunks {
        for cell in plan.cells_for_chunk(pos) {
            assert_eq!(
                session_block(&session, cell.world),
                cell.block,
                "streamed block at {:?} in {pos:?} must match the plan",
                cell.world
            );
        }
    }

    // The stream never saves: the session's save path must not exist and the
    // test directory must stay empty.
    assert!(
        !session.save_path.exists(),
        "streaming must not write the session save path"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("read tempdir").count(),
        0,
        "a streaming session must write nothing"
    );
}

#[test]
fn two_independent_sessions_stream_identical_village_cells() {
    let _heavy = lock_streaming_pool();
    let catalog = load_catalog();
    let plan = pinned_plan(&catalog);
    let (chunks, _) = plan_chunks(&plan);

    let first_dir = tempfile::tempdir().expect("tempdir");
    let second_dir = tempfile::tempdir().expect("tempdir");

    let mut first = new_session(first_dir.path());
    first.set_village_catalog(Arc::new(catalog.clone()));
    first.world.set_render_distance(SESSION_RENDER_DISTANCE);
    pump_until_resident(&mut first, &chunks);

    let mut second = new_session(second_dir.path());
    second.set_village_catalog(Arc::new(catalog.clone()));
    second.world.set_render_distance(SESSION_RENDER_DISTANCE);
    pump_until_resident(&mut second, &chunks);

    for &pos in &chunks {
        assert!(
            first.world.is_resident(pos) && second.world.is_resident(pos),
            "both independent sessions must stream {pos:?}"
        );
        for cell in plan.cells_for_chunk(pos) {
            let left = session_block(&first, cell.world);
            let right = session_block(&second, cell.world);
            assert_eq!(
                left, right,
                "independent sessions must agree at {:?} ({pos:?})",
                cell.world
            );
            assert_eq!(
                left, cell.block,
                "both sessions must reproduce the plan at {:?}",
                cell.world
            );
        }
    }
}

#[test]
fn a_village_catalog_installed_after_construction_is_used_by_the_first_stream() {
    let _heavy = lock_streaming_pool();
    let catalog = load_catalog();
    let plan = pinned_plan(&catalog);
    let (chunks, _) = plan_chunks(&plan);
    let first_chunk = chunks[0];
    let first_cells = plan.cells_for_chunk(first_chunk);

    let dir = tempfile::tempdir().expect("tempdir");
    let mut session = new_session(dir.path());
    // Install strictly after construction and after the render distance, and
    // strictly before the first `update_streaming` call: this is the ordering
    // every session swap relies on.
    session.world.set_render_distance(SESSION_RENDER_DISTANCE);
    session.set_village_catalog(Arc::new(catalog.clone()));
    assert!(
        session.world.village_catalog().is_some(),
        "the post-construction install must land on the manager"
    );
    let pumped = pump_until_resident(&mut session, &chunks);

    for &pos in &chunks {
        assert!(
            session.world.is_resident(pos),
            "chunk {pos:?} never became resident after {pumped} pumps"
        );
        for cell in plan.cells_for_chunk(pos) {
            assert_eq!(
                session_block(&session, cell.world),
                cell.block,
                "the first stream after the install must use the catalog at {:?}",
                cell.world
            );
        }
    }

    // Prove the install actually changed generation rather than the terrain
    // happening to match: a catalog-less generation of the same chunk differs
    // from the streamed chunk at at least one planned village cell.
    let legacy = generate_chunk_seeded_with(first_chunk, PINNED_SEED, Season::Summer, &[]);
    let chunk_min_x = first_chunk.x * CHUNK_SIZE as i32;
    let chunk_min_y = first_chunk.y * CHUNK_SIZE as i32;
    let chunk_min_z = first_chunk.z * CHUNK_SIZE as i32;
    let differs = first_cells.iter().any(|cell| {
        let local = (
            cell.world[0] - chunk_min_x,
            cell.world[1] - chunk_min_y,
            cell.world[2] - chunk_min_z,
        );
        legacy.get(local.0 as usize, local.1 as usize, local.2 as usize) != cell.block
    });
    assert!(
        differs,
        "at least one planned village cell must differ from legacy generation"
    );
}
