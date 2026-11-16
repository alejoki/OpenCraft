//! Release-only v9 10M-edit cold-load baseline harness (M018/S01-T02).
//!
//! D316/D320: the harness lives in a `#[cfg(test)]` lib module (not an
//! integration test) so S02's frozen `#[cfg(test)]` v9 witness encoder can be
//! used in the same run. It reuses the deterministic fixture shape from
//! `tests/autosave_budget.rs`: 500 `BlockId::Stone` edits in each of 20,000
//! chunks, written by the live v9 writer and then cold-loaded five times after
//! one warm-up.
//!
//! Measured phases:
//! - `storage_load_ms`: open + header/content table/index/section decode;
//! - `session_setup_ms`: `WorldSession::from_loaded_world`;
//! - `storage_session_ms`: one outer timer around both (the target phase);
//! - `full_verify_ms`: materialize every edit and mix a deterministic,
//!   order-independent checksum;
//! - one-off `crop_scan_ms`: `CropRuntime::from_edits` attribution;
//! - one-off `first_area_ms`: fixed 5x5 y=0 resident/meshed drain guardrail.
//!
//! Run:
//!
//! ```text
//! cargo test --locked --release --lib -- --ignored cold_load_release_baseline --nocapture
//! ```
//!
//! First run with `RECORDED_V9_STORAGE_SESSION_P50_MS = 0.0` prints the
//! distribution and panics with "record the measured baseline first": paste
//! the printed `storage_session_p50_ms` into the constant and rerun. Never
//! re-pin `TARGET_RATIO` after seeing v10.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::content::ContentRegistry;
use crate::world::block::BlockId;
use crate::world::chunk::ChunkPos;
use crate::world::farming::CropRuntime;
use crate::world::persistence::{
    load_world_from_path_with, save_world_to_path, ChunkEditMap, SAVE_MAGIC, SAVE_VERSION,
};
use crate::world::season::{Season, DEFAULT_SEASON_LENGTH_DAYS};
use crate::WorldSession;

/// Distinct local indices written per chunk, copied from
/// `tests/autosave_budget.rs`. A chunk has 32^3 = 32,768 blocks.
const EDITS_PER_CHUNK: usize = 500;
/// The 10M-edit stress fixture size.
const EDIT_TARGET: usize = 10_000_000;
/// Exactly `EDIT_TARGET / EDITS_PER_CHUNK`.
const CHUNK_COUNT: usize = EDIT_TARGET / EDITS_PER_CHUNK;
/// Measured samples after one warm-up.
const SAMPLES: usize = 5;
/// Same deterministic fixture seed as the autosave harness.
const WORLD_SEED: u32 = 4242;
/// Fixed first-area target: a 5x5 y=0 block of chunks around the origin.
const FIRST_AREA_RADIUS: i32 = 2;
/// A hung streaming drain must fail the harness, not hang CI.
const FIRST_AREA_DEADLINE: Duration = Duration::from_secs(120);
const FIRST_AREA_POLL: Duration = Duration::from_millis(2);

/// Frozen `storage_session_p50_ms` measured on this host by the first release
/// run of this harness. S02/S03 compare v10 against this exact target; it is
/// never re-pinned after seeing v10 (D316).
const RECORDED_V9_STORAGE_SESSION_P50_MS: f64 = 319.630;
/// D316: the v10 storage+session p50 must be at most half the recorded v9 p50.
const TARGET_RATIO: f64 = 0.50;

/// Build a deterministic edit map with exactly `edit_target` edits spread over
/// `ceil(edit_target / EDITS_PER_CHUNK)` chunks.
///
/// Copied exactly from `tests/autosave_budget.rs`: chunk positions follow
/// `ChunkPos::new(cx, 0, cz)` with `cx = i % 50` and `cz = i / 50`; local
/// indices are the first `n` values in each chunk; every value is Stone.
fn big_edits(edit_target: usize) -> (ChunkEditMap, usize) {
    let chunk_count = edit_target.div_ceil(EDITS_PER_CHUNK);
    let mut edits: ChunkEditMap =
        ChunkEditMap::with_capacity_and_hasher(chunk_count, Default::default());
    let mut remaining = edit_target;
    for i in 0..chunk_count {
        let in_chunk = remaining.min(EDITS_PER_CHUNK);
        remaining -= in_chunk;
        let cx = (i % 50) as i32;
        let cz = (i / 50) as i32;
        let mut chunk =
            rustc_hash::FxHashMap::with_capacity_and_hasher(in_chunk, Default::default());
        for local in 0..in_chunk {
            chunk.insert(local as u16, BlockId::Stone);
        }
        edits.insert(ChunkPos::new(cx, 0, cz), chunk);
    }
    debug_assert_eq!(remaining, 0);
    assert_eq!(
        edit_totals(&edits).1,
        edit_target,
        "generator must produce exactly the requested edit count"
    );
    (edits, chunk_count)
}

/// `(chunks, edits)` totals for an edit map.
fn edit_totals(edits: &ChunkEditMap) -> (usize, usize) {
    (
        edits.len(),
        edits.values().map(|chunk| chunk.len()).sum::<usize>(),
    )
}

/// Write the deterministic 10M-edit v9 fixture and return its byte length.
///
/// The edit map is moved into the save payload and dropped explicitly after
/// the write so the five measured loads do not coexist with the generator's
/// copy. The header is read back from disk to prove the live writer emitted
/// v9 (`SAVE_VERSION == 9` in S01).
fn write_fixture(path: &Path) -> u64 {
    let mut data =
        WorldSession::new_session(WORLD_SEED, path.to_path_buf(), DEFAULT_SEASON_LENGTH_DAYS)
            .to_save_data();
    let (edits, chunks) = big_edits(EDIT_TARGET);
    assert_eq!(chunks, CHUNK_COUNT, "fixture must span 20,000 chunks");
    data.chunk_edits = edits;
    save_world_to_path(path, &data)
        .unwrap_or_else(|error| panic!("write fixture {}: {error:?}", path.display()));
    drop(data);

    let mut header = [0u8; 8];
    {
        use std::io::Read;
        let mut file = std::fs::File::open(path)
            .unwrap_or_else(|error| panic!("open fixture {}: {error}", path.display()));
        file.read_exact(&mut header)
            .unwrap_or_else(|error| panic!("read fixture header {}: {error}", path.display()));
    }
    assert_eq!(
        &header[..4],
        &SAVE_MAGIC[..],
        "fixture must carry the live save magic"
    );
    let version = u32::from_le_bytes(header[4..8].try_into().expect("4-byte version word"));
    assert_eq!(version, 9, "S01 fixture is a v9 save");
    assert_eq!(
        SAVE_VERSION, 9,
        "S01 freezes the v9 baseline; the version bump belongs to S02"
    );

    std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat fixture {}: {error}", path.display()))
        .len()
}

/// One order-independent mix round.
fn mix(acc: u64, value: u64) -> u64 {
    acc.wrapping_add(value.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .rotate_left(29)
        .wrapping_mul(0x94D0_49BB_1331_11EB)
}

/// Materialize every persisted edit and return `(chunks, edits, checksum)`.
///
/// The checksum adds one independent hash per edit, so it is commutative over
/// `FxHashMap` iteration order and must agree across every sample.
fn verify_edits(edits: &ChunkEditMap) -> (usize, usize, u64) {
    let mut chunks = 0usize;
    let mut total = 0usize;
    let mut checksum = 0u64;
    for (pos, blocks) in edits {
        chunks += 1;
        for (&local, &block) in blocks {
            total += 1;
            let mut hash = 0xCBF2_9CE4_8422_2325;
            hash = mix(hash, u32::from_ne_bytes(pos.x.to_ne_bytes()) as u64);
            hash = mix(hash, u32::from_ne_bytes(pos.y.to_ne_bytes()) as u64);
            hash = mix(hash, u32::from_ne_bytes(pos.z.to_ne_bytes()) as u64);
            hash = mix(hash, u64::from(local));
            hash = mix(hash, u64::from(block.to_raw()));
            checksum = checksum.wrapping_add(hash);
        }
    }
    (chunks, total, checksum)
}

/// Nearest-rank percentile over an already-sorted slice.
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (((sorted.len() as f64 - 1.0) * fraction).round() as usize).min(sorted.len() - 1);
    sorted[index]
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut out = values.to_vec();
    out.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    out
}

/// One measured cold-load sample.
struct Sample {
    storage_ms: f64,
    session_ms: f64,
    storage_session_ms: f64,
    full_verify_ms: f64,
}

/// The fixed 5x5 y=0 first-area target set around the origin.
fn first_area_targets() -> Vec<ChunkPos> {
    let mut targets = Vec::with_capacity(25);
    for cz in -FIRST_AREA_RADIUS..=FIRST_AREA_RADIUS {
        for cx in -FIRST_AREA_RADIUS..=FIRST_AREA_RADIUS {
            targets.push(ChunkPos::new(cx, 0, cz));
        }
    }
    debug_assert_eq!(targets.len(), 25);
    targets
}

/// Drain streaming until all 25 fixed first-area chunks are resident and at
/// least one mesh has completed; return `(drain_ms, meshes)`.
///
/// The timer covers only the post-load drain (D316's resident/meshed terrain
/// guardrail), not the fresh load/session setup measured separately above.
fn measure_first_area(path: &Path, targets: &[ChunkPos]) -> (f64, usize) {
    let loaded = load_world_from_path_with(path, ContentRegistry::builtin())
        .unwrap_or_else(|error| panic!("first_area load {}: {error:?}", path.display()));
    let mut session = WorldSession::from_loaded_world(loaded, path.to_path_buf());
    let center = ChunkPos::new(0, 0, 0);

    let start = Instant::now();
    let deadline = start + FIRST_AREA_DEADLINE;
    let mut meshes = 0usize;
    loop {
        let (completed, _unloaded) = session.world.update_streaming(center, Season::Summer);
        meshes += completed.len();
        let resident = targets
            .iter()
            .filter(|pos| session.world.is_resident(**pos))
            .count();
        if resident == targets.len() && meshes > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "first_area: {resident}/{} target chunks resident, {meshes} meshes after {FIRST_AREA_DEADLINE:?}",
            targets.len()
        );
        std::thread::sleep(FIRST_AREA_POLL);
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;

    let resident = targets
        .iter()
        .filter(|pos| session.world.is_resident(**pos))
        .count();
    assert_eq!(
        resident,
        targets.len(),
        "all fixed first-area chunks must be resident"
    );
    assert!(meshes > 0, "first_area must complete at least one mesh");
    (elapsed_ms, meshes)
}

/// Release-only 10M-edit v9 cold-load baseline and target freeze.
///
/// A debug build must never be able to "pass" this harness: it would measure
/// unoptimized decode/clone work and make the target meaningless.
#[test]
#[ignore = "release-only cold-load harness: cargo test --locked --release --lib -- --ignored cold_load_release_baseline --nocapture"]
fn cold_load_release_baseline() {
    // Bound to a local first: `assert!(cfg!(...))` trips
    // `clippy::assertions_on_constants` in the debug `--no-run` build.
    let release_build = !cfg!(debug_assertions);
    assert!(
        release_build,
        "cold_load_release_baseline is release-only: run cargo test --locked --release --lib -- --ignored cold_load_release_baseline --nocapture"
    );

    let dir = tempfile::tempdir().expect("tempdir for 10M cold-load fixture");
    let path: PathBuf = dir.path().join("cold_load_v9.mcvx");
    let file_bytes = write_fixture(&path);

    // One warm-up load+session: page cache and lazily-built registry/static
    // state must not land in the measured distribution.
    {
        let loaded = load_world_from_path_with(&path, ContentRegistry::builtin())
            .unwrap_or_else(|error| panic!("warm-up load {}: {error:?}", path.display()));
        let session = WorldSession::from_loaded_world(loaded, path.clone());
        assert_eq!(
            session.world.chunk_edits().len(),
            CHUNK_COUNT,
            "warm-up load must carry every edited chunk"
        );
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    let mut checksums = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let outer = Instant::now();
        let storage_start = Instant::now();
        let loaded = load_world_from_path_with(&path, ContentRegistry::builtin())
            .unwrap_or_else(|error| panic!("sample {sample}: load {}: {error:?}", path.display()));
        let storage_ms = storage_start.elapsed().as_secs_f64() * 1_000.0;

        let session_start = Instant::now();
        let session = WorldSession::from_loaded_world(loaded, path.clone());
        let session_ms = session_start.elapsed().as_secs_f64() * 1_000.0;
        let storage_session_ms = outer.elapsed().as_secs_f64() * 1_000.0;

        let verify_start = Instant::now();
        let (chunks, edits, checksum) = verify_edits(session.world.chunk_edits());
        let full_verify_ms = verify_start.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(
            chunks, CHUNK_COUNT,
            "sample {sample}: fixture must decode 20,000 edited chunks"
        );
        assert_eq!(
            edits, EDIT_TARGET,
            "sample {sample}: fixture must decode 10,000,000 edits"
        );
        checksums.push(checksum);
        samples.push(Sample {
            storage_ms,
            session_ms,
            storage_session_ms,
            full_verify_ms,
        });

        // Release the 10M map before the next sample: five live copies would
        // OOM on a modest host.
        drop(session);
    }

    let checksum = checksums[0];
    assert!(
        checksums.iter().all(|candidate| *candidate == checksum),
        "samples disagree on the edit checksum: {checksums:?}"
    );

    let storage: Vec<f64> = samples.iter().map(|s| s.storage_ms).collect();
    let session: Vec<f64> = samples.iter().map(|s| s.session_ms).collect();
    let storage_session: Vec<f64> = samples.iter().map(|s| s.storage_session_ms).collect();
    let full_verify: Vec<f64> = samples.iter().map(|s| s.full_verify_ms).collect();

    let storage_sorted = sorted(&storage);
    let session_sorted = sorted(&session);
    let storage_session_sorted = sorted(&storage_session);
    let full_verify_sorted = sorted(&full_verify);

    let storage_p50 = percentile(&storage_sorted, 0.50);
    let storage_p95 = percentile(&storage_sorted, 0.95);
    let storage_max = *storage_sorted.last().expect("at least one sample");
    let session_p50 = percentile(&session_sorted, 0.50);
    let session_p95 = percentile(&session_sorted, 0.95);
    let storage_session_p50 = percentile(&storage_session_sorted, 0.50);
    let storage_session_p95 = percentile(&storage_session_sorted, 0.95);
    let full_verify_p50 = percentile(&full_verify_sorted, 0.50);

    // One-off attribution: crop scanning is report-only; session setup includes
    // its own scan.
    let crop_scan_ms = {
        let loaded = load_world_from_path_with(&path, ContentRegistry::builtin())
            .unwrap_or_else(|error| panic!("crop_scan load {}: {error:?}", path.display()));
        let start = Instant::now();
        let crops = CropRuntime::from_edits(&loaded.data.chunk_edits);
        std::hint::black_box(&crops);
        start.elapsed().as_secs_f64() * 1_000.0
    };

    let targets = first_area_targets();
    let (first_area_ms, _meshes) = measure_first_area(&path, &targets);

    println!(
        "cold_load_v9: edits={EDIT_TARGET} chunks={CHUNK_COUNT} samples={SAMPLES} file_bytes={file_bytes} storage_p50_ms={storage_p50:.3} storage_p95_ms={storage_p95:.3} storage_max_ms={storage_max:.3} session_p50_ms={session_p50:.3} session_p95_ms={session_p95:.3} storage_session_p50_ms={storage_session_p50:.3} storage_session_p95_ms={storage_session_p95:.3} crop_scan_ms={crop_scan_ms:.3} first_area_ms={first_area_ms:.3} full_verify_p50_ms={full_verify_p50:.3} checksum=0x{checksum:016x}"
    );

    // Two-run freeze (D316): the first run prints the distribution above and
    // stops here. Bind the constants to locals so the debug `--no-run` build
    // stays clean of `clippy::assertions_on_constants`.
    let recorded = RECORDED_V9_STORAGE_SESSION_P50_MS;
    let recorded_is_positive = recorded > 0.0;
    assert!(
        recorded_is_positive,
        "record the measured baseline first: set RECORDED_V9_STORAGE_SESSION_P50_MS to the printed storage_session_p50_ms ({storage_session_p50:.3})"
    );
    let target_p50 = recorded * TARGET_RATIO;

    println!(
        "cold_load_target: fixture=10m_edits phase=storage+session recorded_v9_p50_ms={recorded:.3} measured_v9_p50_ms={storage_session_p50:.3} target_p50_ms={target_p50:.3} target_ratio={TARGET_RATIO:.2} rule=storage_plus_session_p50_at_most_half_recorded_v9"
    );
}
