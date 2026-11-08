//! Save/reload continuity for generated villages (M015/S03-T04).
//!
//! Villages are generated content and are never persisted: a save carries only
//! `chunk_edits`. These tests drive the real production funnel — the session's
//! `to_save_data` projection, the v9 codec, and a reloaded `WorldSession` — and
//! prove that a mined village block wins over regeneration while every
//! unedited village cell regenerates identically, including for a world saved
//! before M015 existed.
//!
//! The pinned seed-14 site is the same one `tests/village_session.rs` pins:
//! cell `(-1, 0)`, spawn column `(0, 0)`, six non-empty chunks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opencraft::content::ContentRegistry;
use opencraft::player::survival::GameMode;
use opencraft::world::block::BlockId;
use opencraft::world::chunk::{linear_index, world_to_chunk_and_local, ChunkPos, CHUNK_SIZE};
use opencraft::world::persistence::{
    load_world_from_bytes, load_world_from_path, save_world_to_bytes, save_world_to_path,
    SAVE_VERSION,
};
use opencraft::world::season::{Season, DEFAULT_SEASON_LENGTH_DAYS};
use opencraft::world::terrain::find_spawn_column;
use opencraft::world::village::{
    village_plan_with_spawn, VillageCatalog, VillageCell, VillagePlan,
};
use opencraft::WorldSession;

/// Serialises the tests that drive the shared global rayon streaming pool.
///
/// Same guard as `tests/village_session.rs`: concurrent rings on one pool can
/// starve generation and hold a target chunk past the pump deadline.
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

/// Render distance used for the streamed sessions. The generation spiral is
/// that distance plus two, which covers the whole village cell.
const SESSION_RENDER_DISTANCE: i32 = 6;

/// Wall-clock bound on one streamed pump loop, matching the streaming tests:
/// the process-wide rayon pool may still be draining a previous test's jobs.
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

/// Every chunk the plan writes into, in a stable `(z, y, x)` order.
fn plan_chunks(plan: &VillagePlan<'_>) -> Vec<ChunkPos> {
    let size = CHUNK_SIZE as i32;
    let mut chunks = Vec::new();
    for cz in plan.bounds.min_z.div_euclid(size)..=plan.bounds.max_z.div_euclid(size) {
        for cy in plan.bounds.min_y.div_euclid(size)..=plan.bounds.max_y.div_euclid(size) {
            for cx in plan.bounds.min_x.div_euclid(size)..=plan.bounds.max_x.div_euclid(size) {
                let pos = ChunkPos::new(cx, cy, cz);
                if !plan.cells_for_chunk(pos).is_empty() {
                    chunks.push(pos);
                }
            }
        }
    }
    chunks
}

/// Centre the stream on the village's own chunk AABB so the target chunks are
/// dispatched on the first ring (same rationale as `tests/village_session.rs`).
fn stream_center(chunks: &[ChunkPos]) -> ChunkPos {
    let min_x = chunks.iter().map(|pos| pos.x).min().unwrap_or(0);
    let max_x = chunks.iter().map(|pos| pos.x).max().unwrap_or(0);
    let min_z = chunks.iter().map(|pos| pos.z).min().unwrap_or(0);
    let max_z = chunks.iter().map(|pos| pos.z).max().unwrap_or(0);
    ChunkPos::new((min_x + max_x) / 2, 0, (min_z + max_z) / 2)
}

/// A fresh production session that saves only where the test asks it to.
fn new_session(save_path: &Path) -> WorldSession {
    WorldSession::new_session_with_mode_and_registry(
        PINNED_SEED,
        save_path.to_path_buf(),
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

fn session_block(session: &WorldSession, world: [i32; 3]) -> BlockId {
    session.world.get_block(world[0], world[1], world[2])
}

/// Stream the pinned village into a fresh session that has the catalog
/// installed, asserting every plan chunk arrived.
fn streamed_village_session(
    catalog: &VillageCatalog,
    save_path: &Path,
) -> (WorldSession, Vec<ChunkPos>) {
    let plan = pinned_plan(catalog);
    let chunks = plan_chunks(&plan);
    let mut session = new_session(save_path);
    session.set_village_catalog(Arc::new(catalog.clone()));
    session.world.set_render_distance(SESSION_RENDER_DISTANCE);
    let pumped = pump_until_resident(&mut session, &chunks);
    for &pos in &chunks {
        assert!(
            session.world.is_resident(pos),
            "chunk {pos:?} never became resident after {pumped} pumps"
        );
    }
    (session, chunks)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn mined_village_block_survives_reload_and_unedited_cells_match() {
    let _heavy = lock_streaming_pool();
    let catalog = load_catalog();
    assert_eq!(
        find_spawn_column(PINNED_SEED),
        PINNED_SPAWN,
        "the pinned spawn column must stay the measured one"
    );
    let plan = pinned_plan(&catalog);
    let chunks = plan_chunks(&plan);

    let dir = tempfile::tempdir().expect("tempdir");
    let save_path = dir.path().join("continuity.mcvx");
    let (mut session, _) = streamed_village_session(&catalog, &save_path);

    // Snapshot every plan cell. A freshly streamed village must reproduce the
    // plan everywhere, so the snapshot is also an independent generation check.
    let mut cells: Vec<_> = Vec::new();
    for &pos in &chunks {
        cells.extend(plan.cells_for_chunk(pos));
    }
    assert!(!cells.is_empty(), "the pinned plan must emit village cells");
    let snapshot: Vec<([i32; 3], BlockId)> = cells
        .iter()
        .map(|cell| {
            let block = session_block(&session, cell.world);
            assert_eq!(
                block, cell.block,
                "the pre-save stream must reproduce the plan at {:?}",
                cell.world
            );
            (cell.world, block)
        })
        .collect();

    // Mine the first non-Air village cell through the player-edit path, which
    // is the only world state a save carries.
    let mined = cells
        .iter()
        .find(|cell| cell.block != BlockId::Air)
        .expect("the pinned plan must place at least one non-Air block")
        .world;
    session
        .world
        .set_block_immediate(mined[0], mined[1], mined[2], BlockId::Air);
    assert_eq!(session_block(&session, mined), BlockId::Air);
    let (mined_chunk, (mined_lx, mined_ly, mined_lz)) =
        world_to_chunk_and_local(mined[0], mined[1], mined[2]);
    let mined_index = linear_index(mined_lx, mined_ly, mined_lz) as u16;
    assert_eq!(
        session
            .world
            .chunk_edits()
            .get(&mined_chunk)
            .and_then(|edits| edits.get(&mined_index)),
        Some(&BlockId::Air),
        "the mined block must be recorded as a chunk edit"
    );

    let data = session.to_save_data();
    save_world_to_path(&save_path, &data).expect("write the session save");

    let loaded = load_world_from_path(&save_path).expect("reload the written save");
    let mut reloaded = WorldSession::session_from_save(loaded, save_path.clone());
    reloaded.set_village_catalog(Arc::new(catalog.clone()));
    reloaded.world.set_render_distance(SESSION_RENDER_DISTANCE);
    let pumped = pump_until_resident(&mut reloaded, &chunks);
    for &pos in &chunks {
        assert!(
            reloaded.world.is_resident(pos),
            "reloaded chunk {pos:?} never became resident after {pumped} pumps"
        );
    }

    // The mined coordinate is the player's edit, applied over regeneration.
    assert_eq!(
        session_block(&reloaded, mined),
        BlockId::Air,
        "the mined village block must stay mined after reload"
    );
    // Every unedited plan cell regenerates identically.
    for (world, expected) in &snapshot {
        if *world == mined {
            continue;
        }
        assert_eq!(
            session_block(&reloaded, *world),
            *expected,
            "unedited village cell {world:?} must regenerate identically"
        );
    }
    // The recorded edit itself survives the codec.
    assert_eq!(
        reloaded
            .world
            .chunk_edits()
            .get(&mined_chunk)
            .and_then(|edits| edits.get(&mined_index)),
        Some(&BlockId::Air),
        "the reloaded session must carry the mined chunk edit"
    );
}

#[test]
fn pre_m015_world_gains_villages_and_keeps_its_edit() {
    let _heavy = lock_streaming_pool();
    let catalog = load_catalog();
    let plan = pinned_plan(&catalog);
    let chunks = plan_chunks(&plan);

    let dir = tempfile::tempdir().expect("tempdir");
    let save_path = dir.path().join("pre-m015.mcvx");

    // A world saved before M015: no catalog, plain terrain.
    let mut legacy = new_session(&save_path);
    legacy.world.set_render_distance(SESSION_RENDER_DISTANCE);
    let pumped = pump_until_resident(&mut legacy, &chunks);
    for &pos in &chunks {
        assert!(
            legacy.world.is_resident(pos),
            "legacy chunk {pos:?} never became resident after {pumped} pumps"
        );
    }

    // Pick a chunk where the village will visibly change terrain, the cell the
    // reload must regenerate as authored (`other`), and a second plan cell in
    // the same chunk whose coordinate the player will edit (`edit`).
    let (edit, other) = chunks
        .iter()
        .find_map(|&pos| {
            let cells = plan.cells_for_chunk(pos);
            let other = cells.iter().find(|cell| {
                cell.block != BlockId::Air && session_block(&legacy, cell.world) != cell.block
            })?;
            let edit = cells.iter().find(|cell| cell.world != other.world)?;
            Some((*edit, *other))
        })
        .expect("the pinned village must overwrite terrain in a multi-cell chunk");

    legacy
        .world
        .set_block_immediate(edit.world[0], edit.world[1], edit.world[2], BlockId::Brick);
    assert_eq!(
        session_block(&legacy, edit.world),
        BlockId::Brick,
        "the pre-M015 edit must land in a resident chunk"
    );
    let data = legacy.to_save_data();
    assert!(
        !data.chunk_edits.is_empty(),
        "the pre-M015 save must carry the recorded edit"
    );
    save_world_to_path(&save_path, &data).expect("write the pre-M015 save");

    // Reload WITH the catalog: the village regenerates around the edit.
    let loaded = load_world_from_path(&save_path).expect("reload the pre-M015 save");
    let mut reloaded = WorldSession::session_from_save(loaded, save_path.clone());
    reloaded.set_village_catalog(Arc::new(catalog.clone()));
    reloaded.world.set_render_distance(SESSION_RENDER_DISTANCE);
    let pumped = pump_until_resident(&mut reloaded, &chunks);
    for &pos in &chunks {
        assert!(
            reloaded.world.is_resident(pos),
            "reloaded chunk {pos:?} never became resident after {pumped} pumps"
        );
    }

    // The player's edit wins over the village's authored block.
    assert_eq!(
        session_block(&reloaded, edit.world),
        BlockId::Brick,
        "a recorded pre-M015 edit must survive village generation"
    );
    // The rest of the same chunk gains the village it never had.
    assert_eq!(
        session_block(&reloaded, other.world),
        other.block,
        "the village must be applied around the edit at {:?}",
        other.world
    );
    assert_ne!(
        session_block(&reloaded, other.world),
        BlockId::Air,
        "the authored village cell must be a placed block"
    );
}

#[test]
fn save_is_v9_and_carries_no_prefab_name() {
    let _heavy = lock_streaming_pool();
    let catalog = load_catalog();
    let dir = tempfile::tempdir().expect("tempdir");
    let save_path = dir.path().join("v9.mcvx");
    let (mut session, _) = streamed_village_session(&catalog, &save_path);

    // One real edit so the save carries a non-trivial edit table.
    let mined = {
        let plan = pinned_plan(&catalog);
        let chunks = plan_chunks(&plan);
        chunks
            .iter()
            .flat_map(|&pos| plan.cells_for_chunk(pos))
            .find(|cell| cell.block != BlockId::Air)
            .map(|cell| cell.world)
            .expect("the pinned plan must place at least one non-Air block")
    };
    session
        .world
        .set_block_immediate(mined[0], mined[1], mined[2], BlockId::Air);

    let data = session.to_save_data();
    assert_eq!(SAVE_VERSION, 9, "the save format must stay v9");
    save_world_to_path(&save_path, &data).expect("write the v9 save");
    let bytes = std::fs::read(&save_path).expect("read the written save bytes");
    assert_eq!(
        &bytes[4..8],
        &SAVE_VERSION.to_le_bytes(),
        "the file header must name version 9"
    );

    // The file decodes through the same production codec.
    let decoded = load_world_from_bytes(&bytes).expect("decode the written save");
    assert_eq!(
        decoded.chunk_edits, data.chunk_edits,
        "decoded chunk edits must match the session's"
    );

    // No prefab, village, or worldgen name may leak into the save.
    for needle in [
        b"plains_hub".as_slice(),
        b"small_house".as_slice(),
        b"wheat_farm".as_slice(),
        b"worldgen".as_slice(),
        b"prefab".as_slice(),
    ] {
        assert!(
            !contains_bytes(&bytes, needle),
            "save bytes must not contain {:?}",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn to_save_data_matches_the_production_codec_round_trip() {
    let _heavy = lock_streaming_pool();
    let dir = tempfile::tempdir().expect("tempdir");
    let save_path = dir.path().join("round-trip.mcvx");
    let mut session = new_session(&save_path);
    session.world.set_render_distance(4);

    // Stream one chunk column so a real edit can be recorded.
    let target = ChunkPos::new(0, 0, 0);
    let pumped = pump_until_resident(&mut session, &[target]);
    assert!(
        session.world.is_resident(target),
        "chunk {target:?} never became resident after {pumped} pumps"
    );

    // Find a non-Air block in the resident chunk and edit it.
    let chunk_base = |axis: i32| axis * CHUNK_SIZE as i32;
    let mut edited = None;
    'search: for ly in (0..CHUNK_SIZE as i32).rev() {
        for lz in 0..CHUNK_SIZE as i32 {
            for lx in 0..CHUNK_SIZE as i32 {
                let world = [
                    chunk_base(target.x) + lx,
                    chunk_base(target.y) + ly,
                    chunk_base(target.z) + lz,
                ];
                if session_block(&session, world) != BlockId::Air {
                    session
                        .world
                        .set_block_immediate(world[0], world[1], world[2], BlockId::Air);
                    edited = Some(world);
                    break 'search;
                }
            }
        }
    }
    let edited = edited.expect("the streamed chunk must contain a non-Air block");

    let data = session.to_save_data();
    assert_eq!(data.seed, PINNED_SEED);
    assert!(!data.chunk_edits.is_empty(), "the edit must be recorded");

    let mut bytes = Vec::new();
    save_world_to_bytes(&mut bytes, &data).expect("encode the save payload");
    let decoded = load_world_from_bytes(&bytes).expect("decode the save payload");

    assert_eq!(decoded.seed, data.seed, "seed must round-trip");
    assert_eq!(
        decoded.time_of_day, data.time_of_day,
        "clock time must round-trip"
    );
    assert_eq!(
        decoded.day_index, data.day_index,
        "day index must round-trip"
    );
    assert_eq!(
        decoded.season_length_days, data.season_length_days,
        "season length must round-trip"
    );
    assert_eq!(decoded.mode, data.mode, "mode must round-trip");
    assert_eq!(
        decoded.player.position, data.player.position,
        "player position must round-trip"
    );
    assert_eq!(
        decoded.chunk_edits, data.chunk_edits,
        "chunk edits must round-trip"
    );
    assert_eq!(
        session_block(&session, edited),
        BlockId::Air,
        "the edit must be live in the session before saving"
    );
}
