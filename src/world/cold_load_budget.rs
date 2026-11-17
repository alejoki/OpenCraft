//! Release-only v9/v10 10M-edit cold-load harness (M018/S01-T02, extended to
//! the same-run v9/v10 comparison by M018/S03-T01).
//!
//! D316/D320: the harness lives in a `#[cfg(test)]` lib module (not an
//! integration test) so S02's frozen `#[cfg(test)]` v9 witness encoder can be
//! used in the same run. It reuses the deterministic fixture shape from
//! `tests/autosave_budget.rs`: 500 `BlockId::Stone` edits in each of 20,000
//! chunks, written by the frozen v9 witness writer *and* by the live v10 writer
//! from the same `WorldSaveData`, then cold-loaded five times each after one
//! warm-up.
//!
//! Measured phases, per fixture:
//! - `storage_load_ms`: open + header/content table/index/section decode;
//! - `session_setup_ms`: `WorldSession::from_loaded_world`;
//! - `storage_session_ms`: one outer timer around both (the target phase);
//! - `full_verify_ms`: materialize every edit and mix a deterministic,
//!   order-independent checksum;
//! - one-off `crop_scan_ms`: `CropRuntime::from_edits` attribution;
//! - one-off `first_area_ms`: fixed 5x5 y=0 resident/meshed drain guardrail.
//!
//! Printed lines:
//! - `cold_load_v9:` / `cold_load_v10:` — the per-fixture distributions;
//! - `cold_load_compare:` — same-run v9/v10 p50, speedup, the frozen target
//!   value (report-only here), both first-area/full-verify phases and both
//!   checksums;
//! - `cold_load_target:` — the frozen D316/D321 target line, unchanged from
//!   S01 (S08 pins its fields);
//! - `cold_load_1m_compare:` — the 1M-edit report-only pair (D316/D321);
//! - `cold_load_migration:` — v9 -> session -> v10 -> reload proof.
//!
//! T01 deliberately does **not** assert the 159.815 ms target: today's eager
//! v10 decoder is expected to miss it (its per-edit map build is the cost S03
//! removes), and T03 owns enabling the gate.
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
    load_world_from_path_with, save_v9_world_to_path, save_world_to_path, ChunkEditMap, SAVE_MAGIC,
    SAVE_VERSION,
};
use crate::world::season::{Season, DEFAULT_SEASON_LENGTH_DAYS};
use crate::WorldSession;

/// Distinct local indices written per chunk, copied from
/// `tests/autosave_budget.rs`. A chunk has 32^3 = 32,768 blocks.
const EDITS_PER_CHUNK: usize = 500;
/// The 10M-edit stress fixture size.
const EDIT_TARGET: usize = 10_000_000;
/// The 1M-edit autosave fixture size, reported for the two-way cold-load
/// comparison but never gated here (D316/D321).
const ONE_MILLION_EDITS: usize = 1_000_000;
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

/// Frozen byte-level pin for the checked 10M-edit v9 witness fixture.
///
/// S01 measured these values on the S01 writer; S03's store work (T02/T03)
/// must not move a single v9 byte.
#[derive(Clone, Copy)]
struct FixturePin {
    file_bytes: u64,
    edit_checksum: u64,
}

const PINNED_V9_10M: FixturePin = FixturePin {
    file_bytes: 40_320_186,
    edit_checksum: 0xd3ef_88bd_c7ae_0ebc,
};

/// Build a deterministic edit map with exactly `edit_target` edits spread over
/// `ceil(edit_target / EDITS_PER_CHUNK)` chunks.
///
/// Copied exactly from `tests/autosave_budget.rs`: chunk positions follow
/// `ChunkPos::new(cx, 0, cz)` with `cx = i % 50` and `cz = i / 50`; local
/// indices are the first `n` values in each chunk; every value is Stone.
fn big_edits(edit_target: usize) -> (ChunkEditMap, usize) {
    let chunk_count = edit_target.div_ceil(EDITS_PER_CHUNK);
    let mut edits: ChunkEditMap = ChunkEditMap::with_capacity(chunk_count);
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

/// File length of `path`, with a readable panic on a missing fixture.
fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat fixture {}: {error}", path.display()))
        .len()
}

/// Read the 8-byte magic + version header and return the version word.
///
/// The magic is asserted here so every fixture check also proves the file was
/// written by this build's save writer rather than some other container.
fn header_version(path: &Path) -> u32 {
    use std::io::Read;
    let mut header = [0u8; 8];
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("open fixture {}: {error}", path.display()));
    file.read_exact(&mut header)
        .unwrap_or_else(|error| panic!("read fixture header {}: {error}", path.display()));
    assert_eq!(
        &header[..4],
        &SAVE_MAGIC[..],
        "{}: fixture must carry the live save magic",
        path.display()
    );
    u32::from_le_bytes(header[4..8].try_into().expect("4-byte version word"))
}

/// Paths and byte lengths of one paired v9/v10 fixture.
struct Fixtures {
    v9: PathBuf,
    v10: PathBuf,
    v9_bytes: u64,
    v10_bytes: u64,
}

/// Write the paired v9/v10 fixtures for `edit_target` edits and return their
/// paths and byte lengths.
///
/// The `WorldSaveData` is built once, written twice (frozen v9 witness +
/// production v10 writer) and dropped before the caller measures, so the five
/// measured loads never coexist with the generator's copy. `pin` names the
/// frozen byte length / edit checksum the v9 witness must keep; only the
/// checked 10M fixture carries one.
fn write_fixtures(dir: &Path, stem: &str, edit_target: usize, pin: Option<FixturePin>) -> Fixtures {
    let v9_path = dir.join(format!("{stem}_v9.mcvx"));
    let v10_path = dir.join(format!("{stem}_v10.mcvx"));
    let expected_chunks = edit_target.div_ceil(EDITS_PER_CHUNK);

    let mut data = WorldSession::new_session(
        WORLD_SEED,
        dir.join(format!("{stem}_session")),
        DEFAULT_SEASON_LENGTH_DAYS,
    )
    .to_save_data();
    let (edits, chunks) = big_edits(edit_target);
    assert_eq!(
        chunks, expected_chunks,
        "{stem}: fixture must span the expected chunk count"
    );
    let (_, total, checksum) = verify_edits(&edits);
    assert_eq!(total, edit_target, "{stem}: fixture edit total");
    data.chunk_edits = edits;

    save_v9_world_to_path(&v9_path, &data, ContentRegistry::builtin())
        .unwrap_or_else(|error| panic!("write v9 fixture {}: {error:?}", v9_path.display()));
    save_world_to_path(&v10_path, &data)
        .unwrap_or_else(|error| panic!("write v10 fixture {}: {error:?}", v10_path.display()));
    drop(data);

    assert_eq!(
        header_version(&v9_path),
        9,
        "{stem}: the frozen witness fixture is a v9 save"
    );
    assert_eq!(
        SAVE_VERSION, 10,
        "the live writer is v10; the frozen witness fixture stays v9"
    );
    assert_eq!(
        header_version(&v10_path),
        SAVE_VERSION,
        "{stem}: the paired fixture must carry the live v10 header"
    );

    let v9_bytes = file_len(&v9_path);
    let v10_bytes = file_len(&v10_path);
    if let Some(pin) = pin {
        assert_eq!(
            v9_bytes, pin.file_bytes,
            "the S01 10M-edit v9 fixture must keep its measured byte length"
        );
        assert_eq!(
            checksum, pin.edit_checksum,
            "the S01 10M-edit v9 fixture must keep its measured edit checksum"
        );
    }

    Fixtures {
        v9: v9_path,
        v10: v10_path,
        v9_bytes,
        v10_bytes,
    }
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

/// One fixture's measured distribution plus its correctness facts.
struct Distribution {
    edits: usize,
    chunks: usize,
    samples: Vec<Sample>,
    file_bytes: u64,
    checksum: u64,
    crop_scan_ms: f64,
    first_area_ms: f64,
}

impl Distribution {
    fn values(&self, field: fn(&Sample) -> f64) -> Vec<f64> {
        self.samples.iter().map(field).collect()
    }

    fn p50(&self, field: fn(&Sample) -> f64) -> f64 {
        percentile(&sorted(&self.values(field)), 0.50)
    }

    fn p95(&self, field: fn(&Sample) -> f64) -> f64 {
        percentile(&sorted(&self.values(field)), 0.95)
    }

    fn max(&self, field: fn(&Sample) -> f64) -> f64 {
        sorted(&self.values(field)).last().copied().unwrap_or(0.0)
    }
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

/// Cold-load `path` `samples` times after one warm-up and return the measured
/// distribution plus the correctedness facts for `expected_chunks` /
/// `expected_edits`.
///
/// Every sample must decode exactly the expected chunk/edit totals and the
/// same order-independent checksum; a mismatch fails the sample rather than
/// reporting a timing for the wrong world.
fn measure(
    path: &Path,
    samples: usize,
    expected_chunks: usize,
    expected_edits: usize,
    targets: &[ChunkPos],
) -> Distribution {
    // One warm-up load+session: page cache and lazily-built registry/static
    // state must not land in the measured distribution.
    {
        let loaded = load_world_from_path_with(path, ContentRegistry::builtin())
            .unwrap_or_else(|error| panic!("warm-up load {}: {error:?}", path.display()));
        let session = WorldSession::from_loaded_world(loaded, path.to_path_buf());
        assert_eq!(
            session.world.chunk_edits().len(),
            expected_chunks,
            "warm-up load must carry every edited chunk"
        );
    }

    let mut measured = Vec::with_capacity(samples);
    let mut checksums = Vec::with_capacity(samples);
    for sample in 0..samples {
        let outer = Instant::now();
        let storage_start = Instant::now();
        let loaded = load_world_from_path_with(path, ContentRegistry::builtin())
            .unwrap_or_else(|error| panic!("sample {sample}: load {}: {error:?}", path.display()));
        let storage_ms = storage_start.elapsed().as_secs_f64() * 1_000.0;

        let session_start = Instant::now();
        let session = WorldSession::from_loaded_world(loaded, path.to_path_buf());
        let session_ms = session_start.elapsed().as_secs_f64() * 1_000.0;
        let storage_session_ms = outer.elapsed().as_secs_f64() * 1_000.0;

        let verify_start = Instant::now();
        let (chunks, edits, checksum) = verify_edits(session.world.chunk_edits());
        let full_verify_ms = verify_start.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(
            chunks, expected_chunks,
            "sample {sample}: fixture must decode the expected edited chunks"
        );
        assert_eq!(
            edits, expected_edits,
            "sample {sample}: fixture must decode the expected edit total"
        );
        checksums.push(checksum);
        measured.push(Sample {
            storage_ms,
            session_ms,
            storage_session_ms,
            full_verify_ms,
        });

        // Release the heavy edit map before the next sample: five live copies
        // would OOM on a modest host.
        drop(session);
    }

    let checksum = checksums[0];
    assert!(
        checksums.iter().all(|candidate| *candidate == checksum),
        "samples disagree on the edit checksum: {checksums:?}"
    );

    // One-off attribution: crop scanning is report-only; session setup includes
    // its own scan.
    let crop_scan_ms = {
        let loaded = load_world_from_path_with(path, ContentRegistry::builtin())
            .unwrap_or_else(|error| panic!("crop_scan load {}: {error:?}", path.display()));
        let start = Instant::now();
        let crops = CropRuntime::from_edits(&loaded.data.chunk_edits);
        std::hint::black_box(&crops);
        start.elapsed().as_secs_f64() * 1_000.0
    };

    let (first_area_ms, _meshes) = measure_first_area(path, targets);

    Distribution {
        edits: expected_edits,
        chunks: expected_chunks,
        samples: measured,
        file_bytes: file_len(path),
        checksum,
        crop_scan_ms,
        first_area_ms,
    }
}

/// Print one fixture's distribution line.
///
/// The `cold_load_v9:` field names and order are frozen: S08's operational
/// gate pins them.
fn print_distribution(label: &str, d: &Distribution) {
    println!(
        "{label}: edits={} chunks={} samples={} file_bytes={} storage_p50_ms={:.3} storage_p95_ms={:.3} storage_max_ms={:.3} session_p50_ms={:.3} session_p95_ms={:.3} storage_session_p50_ms={:.3} storage_session_p95_ms={:.3} crop_scan_ms={:.3} first_area_ms={:.3} full_verify_p50_ms={:.3} checksum=0x{:016x}",
        d.edits,
        d.chunks,
        d.samples.len(),
        d.file_bytes,
        d.p50(|sample| sample.storage_ms),
        d.p95(|sample| sample.storage_ms),
        d.max(|sample| sample.storage_ms),
        d.p50(|sample| sample.session_ms),
        d.p95(|sample| sample.session_ms),
        d.p50(|sample| sample.storage_session_ms),
        d.p95(|sample| sample.storage_session_ms),
        d.crop_scan_ms,
        d.first_area_ms,
        d.p50(|sample| sample.full_verify_ms),
        d.checksum,
    );
}

/// Release-only 10M-edit v9/v10 cold-load baseline, comparison and migration.
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

    let dir = tempfile::tempdir().expect("tempdir for cold-load fixtures");
    let targets = first_area_targets();

    // The frozen 10M-edit v9 witness and the production v10 fixture, written
    // from the same `WorldSaveData`.
    let fixtures = write_fixtures(dir.path(), "cold_load", EDIT_TARGET, Some(PINNED_V9_10M));

    let v9 = measure(&fixtures.v9, SAMPLES, CHUNK_COUNT, EDIT_TARGET, &targets);
    print_distribution("cold_load_v9", &v9);
    let v10 = measure(&fixtures.v10, SAMPLES, CHUNK_COUNT, EDIT_TARGET, &targets);
    print_distribution("cold_load_v10", &v10);

    let same_edits = v9.edits == v10.edits && v9.chunks == v10.chunks;
    let same_checksum = v9.checksum == v10.checksum;
    assert!(
        same_edits,
        "v9 and v10 fixtures must decode the same 20,000 chunks / 10,000,000 edits"
    );
    assert!(
        same_checksum,
        "v9 and v10 fixtures must decode the same edit checksum: 0x{:016x} vs 0x{:016x}",
        v9.checksum, v10.checksum
    );

    let v9_storage_session_p50 = v9.p50(|sample| sample.storage_session_ms);
    let v10_storage_session_p50 = v10.p50(|sample| sample.storage_session_ms);
    let speedup = v9_storage_session_p50 / v10_storage_session_p50;
    let target_p50 = RECORDED_V9_STORAGE_SESSION_P50_MS * TARGET_RATIO;
    // Report-only in T01: today's eager v10 decoder is expected to miss the
    // target, and T03 owns the assertion once the lazy palette store lands.
    let target_met = v10_storage_session_p50 <= target_p50;

    println!(
        "cold_load_compare: v9_storage_session_p50_ms={v9_storage_session_p50:.3} v10_storage_session_p50_ms={v10_storage_session_p50:.3} speedup={speedup:.3} target_p50_ms={target_p50:.3} target_met={target_met} v9_first_area_ms={:.3} v10_first_area_ms={:.3} v9_full_verify_p50_ms={:.3} v10_full_verify_p50_ms={:.3} v9_checksum=0x{:016x} v10_checksum=0x{:016x} same_edits={same_edits} same_checksum={same_checksum}",
        v9.first_area_ms,
        v10.first_area_ms,
        v9.p50(|sample| sample.full_verify_ms),
        v10.p50(|sample| sample.full_verify_ms),
        v9.checksum,
        v10.checksum,
    );

    // Two-run freeze (D316): the first run prints the distribution above and
    // stops at the positivity check. Bind the constants to locals so the debug
    // `--no-run` build stays clean of `clippy::assertions_on_constants`.
    let recorded = RECORDED_V9_STORAGE_SESSION_P50_MS;
    let recorded_is_positive = recorded > 0.0;
    assert!(
        recorded_is_positive,
        "record the measured baseline first: set RECORDED_V9_STORAGE_SESSION_P50_MS to the printed storage_session_p50_ms ({v9_storage_session_p50:.3})"
    );

    println!(
        "cold_load_target: fixture=10m_edits phase=storage+session recorded_v9_p50_ms={recorded:.3} measured_v9_p50_ms={v9_storage_session_p50:.3} target_p50_ms={target_p50:.3} target_ratio={TARGET_RATIO:.2} rule=storage_plus_session_p50_at_most_half_recorded_v9"
    );

    // 1M two-way report: the autosave fixture shape, report-only per
    // D316/D321 (S03's gate is the 10M cold load; this row records whether the
    // smaller world moves in the same direction).
    let one_million_chunks = ONE_MILLION_EDITS.div_ceil(EDITS_PER_CHUNK);
    let one_million = write_fixtures(dir.path(), "cold_load_1m", ONE_MILLION_EDITS, None);
    let v9_1m = measure(
        &one_million.v9,
        SAMPLES,
        one_million_chunks,
        ONE_MILLION_EDITS,
        &targets,
    );
    let v10_1m = measure(
        &one_million.v10,
        SAMPLES,
        one_million_chunks,
        ONE_MILLION_EDITS,
        &targets,
    );
    assert_eq!(
        v9_1m.checksum, v10_1m.checksum,
        "the 1M v9 and v10 fixtures must decode the same edits"
    );
    let v9_1m_storage_session_p50 = v9_1m.p50(|sample| sample.storage_session_ms);
    let v10_1m_storage_session_p50 = v10_1m.p50(|sample| sample.storage_session_ms);
    let improvement = v9_1m_storage_session_p50 / v10_1m_storage_session_p50;
    println!(
        "cold_load_1m_compare: edits={ONE_MILLION_EDITS} chunks={one_million_chunks} v9_storage_session_p50_ms={v9_1m_storage_session_p50:.3} v10_storage_session_p50_ms={v10_1m_storage_session_p50:.3} improvement={improvement:.3}x checksums=0x{:016x}/0x{:016x}",
        v9_1m.checksum, v10_1m.checksum,
    );

    // Migration leg: a user's v9 save must load, project through the session,
    // save as v10, reload and keep every persisted value. Peak memory stays
    // bounded: the session is dropped before the reload, and only one
    // comparison copy of the 10M map survives.
    let migrated_path = dir.path().join("cold_load_migrated.mcvx");
    let v9_loaded = load_world_from_path_with(&fixtures.v9, ContentRegistry::builtin())
        .unwrap_or_else(|error| panic!("migration: load v9 {}: {error:?}", fixtures.v9.display()));
    let v9_unresolved = v9_loaded.unresolved;
    assert_eq!(
        v9_unresolved, 0,
        "the builtin 10M fixture must resolve every saved content reference"
    );
    let origin_seed = v9_loaded.data.seed;
    let origin_time_of_day = v9_loaded.data.time_of_day;
    let origin_day_index = v9_loaded.data.day_index;
    let origin_season_length_days = v9_loaded.data.season_length_days;
    let origin_player = v9_loaded.data.player.clone();
    let origin_chests = v9_loaded.data.chests.clone();
    let origin_mobs = v9_loaded.data.mobs.clone();
    let (origin_chunks, origin_edits, origin_checksum) = verify_edits(&v9_loaded.data.chunk_edits);
    assert_eq!(
        origin_chunks, CHUNK_COUNT,
        "migration: the v9 fixture must decode 20,000 edited chunks"
    );
    assert_eq!(
        origin_edits, EDIT_TARGET,
        "migration: the v9 fixture must decode 10,000,000 edits"
    );
    assert_eq!(
        origin_checksum, PINNED_V9_10M.edit_checksum,
        "migration: the v9 fixture must keep its frozen edit checksum"
    );

    let session = WorldSession::from_loaded_world(v9_loaded, fixtures.v9.clone());
    let migrated = session.to_save_data();
    drop(session);
    assert_eq!(
        edit_totals(&migrated.chunk_edits),
        (CHUNK_COUNT, EDIT_TARGET),
        "migration: the session projection must carry every edit"
    );
    assert_eq!(
        verify_edits(&migrated.chunk_edits).2,
        PINNED_V9_10M.edit_checksum,
        "migration: the session projection must preserve the edit checksum"
    );
    save_world_to_path(&migrated_path, &migrated)
        .unwrap_or_else(|error| panic!("migration: write {}: {error:?}", migrated_path.display()));
    let migrated_file_bytes = file_len(&migrated_path);

    {
        let reloaded = load_world_from_path_with(&migrated_path, ContentRegistry::builtin())
            .unwrap_or_else(|error| {
                panic!("migration: reload {}: {error:?}", migrated_path.display())
            });
        assert_eq!(
            reloaded.unresolved, 0,
            "the migrated v10 save must resolve every content reference"
        );
        let (reloaded_chunks, reloaded_edits, reloaded_checksum) =
            verify_edits(&reloaded.data.chunk_edits);
        assert_eq!(
            (reloaded_chunks, reloaded_edits),
            (CHUNK_COUNT, EDIT_TARGET),
            "the reloaded v10 save must carry every edit"
        );
        assert_eq!(
            reloaded_checksum, PINNED_V9_10M.edit_checksum,
            "the reloaded v10 save must preserve the edit checksum"
        );
        assert_eq!(
            reloaded.data, migrated,
            "the v9 -> session -> v10 -> reload round-trip must preserve every persisted field"
        );
        assert_eq!(
            reloaded.data.seed, origin_seed,
            "seed must survive v9 -> v10"
        );
        assert_eq!(
            reloaded.data.time_of_day, origin_time_of_day,
            "time of day must survive v9 -> v10"
        );
        assert_eq!(
            reloaded.data.day_index, origin_day_index,
            "day index must survive v9 -> v10"
        );
        assert_eq!(
            reloaded.data.season_length_days, origin_season_length_days,
            "season length must survive v9 -> v10"
        );
        assert_eq!(
            reloaded.data.player, origin_player,
            "player, inventory and crafting grids must survive v9 -> v10"
        );
        assert_eq!(
            reloaded.data.chests, origin_chests,
            "chests must survive v9 -> v10"
        );
        assert_eq!(
            reloaded.data.mobs, origin_mobs,
            "mobs must survive v9 -> v10"
        );
    }
    drop(migrated);

    // The migrated payload and a direct v10 save of the same world must be
    // byte-identical. A genuine difference means the projection or the writer
    // lost/normalised a persisted value: stop and report instead of weakening
    // the assertion.
    let direct_bytes = std::fs::read(&fixtures.v10)
        .unwrap_or_else(|error| panic!("migration: read {}: {error}", fixtures.v10.display()));
    let migrated_bytes = std::fs::read(&migrated_path)
        .unwrap_or_else(|error| panic!("migration: read {}: {error}", migrated_path.display()));
    assert_eq!(
        direct_bytes.len(),
        migrated_bytes.len(),
        "migration: direct-v10 and migrated-v10 must have the same byte length"
    );
    if direct_bytes != migrated_bytes {
        let offset = direct_bytes
            .iter()
            .zip(migrated_bytes.iter())
            .position(|(direct, migrated)| direct != migrated)
            .expect("equal lengths always have a differing byte when the slices differ");
        panic!(
            "migration: direct-v10 and migrated-v10 differ at byte {offset} of {}; investigate the session projection or writer instead of weakening this assertion",
            direct_bytes.len()
        );
    }

    println!(
        "cold_load_migration: v9_file_bytes={} v10_file_bytes={} migrated_file_bytes={migrated_file_bytes} edits={EDIT_TARGET} chunks={CHUNK_COUNT} checksum=0x{:016x} unresolved={v9_unresolved} byte_identical=true",
        fixtures.v9_bytes, fixtures.v10_bytes, PINNED_V9_10M.edit_checksum,
    );
}
