//! Release save-budget harness for streaming autosave (M016/S02-T04).
//!
//! D267 gates frame-thread autosave work at p95 <= 16.6 ms (one 60 Hz frame)
//! on a ~1M-edit world, and reports the ~10M-edit case without gating it. This
//! harness drives the public session seam only: it marks the world dirty,
//! calls [`WorldSession::queue_autosave`] (the frame-thread dispatch), then
//! polls [`WorldSession::poll_autosave`] until the worker reaps. It measures
//! the two halves separately and proves the encode+write really happened
//! off-thread:
//!
//! - the frame distribution is the dispatch call plus every poll call;
//! - the worker distribution is dispatch-to-reap wall time;
//! - `worker_p50_ms > frame_p95_ms` on the 1M phase is the negative control
//!   showing encode+write dominates wall time on the worker, not the frame;
//! - `dispatches == samples` and `bytes == samples * expected.len()` are the
//!   counts/bytes controls (MEM723/MEM693) proving each save really ran and
//!   wrote the full v9 payload;
//! - the first sample compares the file byte-for-byte with the expected v9
//!   encoding, and each phase reloads the file and checks the edit/chunk
//!   totals (round-trip control).
//!
//! This is intentionally a release-only, `#[ignore]`d harness: a debug build
//! would measure unoptimized encode/clone work and make the p95 gate
//! meaningless. Run it with:
//!
//! ```text
//! cargo test --locked --release --test autosave_budget -- --ignored --nocapture
//! ```
//!
//! The 10M phase builds ~20,000 chunks x 500 edits and is memory-heavy
//! (~150-300 MB for the edit maps plus the expected byte buffer, and a second
//! copy while the snapshot is in flight). That is deliberate: it is a stress
//! report, not a gate, and it is never run by ordinary `cargo test`.

use std::path::Path;
use std::time::{Duration, Instant};

use opencraft::world::block::BlockId;
use opencraft::world::chunk::ChunkPos;
use opencraft::world::persistence::{
    load_world_from_path, save_world_to_bytes_with_config, ChunkEditMap,
};
use opencraft::world::season::DEFAULT_SEASON_LENGTH_DAYS;
use opencraft::WorldSession;

/// Distinct local indices written per chunk. A chunk has 32^3 = 32,768
/// blocks, so 500 distinct indices fit comfortably.
const EDITS_PER_CHUNK: usize = 500;

/// One 60 Hz frame: the operational MEM229/D267 target.
const FRAME_P95_BUDGET_MS: f64 = 16.6;

/// Hard upper bound for one dispatch-to-reap cycle; a hung worker must fail
/// the harness rather than hang CI.
const REAP_DEADLINE: Duration = Duration::from_secs(30);

/// Build a deterministic edit map with exactly `edit_target` edits spread over
/// `ceil(edit_target / EDITS_PER_CHUNK)` chunks.
///
/// Chunk positions follow `ChunkPos::new(cx, 0, cz)` with `cx = i % 50` and
/// `cz = i / 50`; local indices are the first `n` values in each chunk. Maps
/// are pre-sized so construction stays linear.
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

/// Build a session carrying `edit_target` edits without generating chunks.
///
/// `session_from_save` installs the edit map directly, and is also the load
/// path under test. The expected v9 bytes are encoded from the session's own
/// `to_save_data()` because constructing from save normalizes fields (fly
/// mode, selection) that would otherwise differ from the pre-session struct.
fn big_session(edit_target: usize, dir: &Path) -> (WorldSession, Vec<u8>) {
    let path = dir.join("world.mcvx");
    let mut data =
        WorldSession::new_session(4242, path.clone(), DEFAULT_SEASON_LENGTH_DAYS).to_save_data();
    let (edits, _chunks) = big_edits(edit_target);
    data.chunk_edits = edits;
    let session = WorldSession::session_from_save(data, path);

    let mut expected = Vec::new();
    save_world_to_bytes_with_config(
        &mut expected,
        &session.to_save_data(),
        session.registry.as_ref(),
        session.save_config(),
    )
    .expect("expected v9 bytes encode");
    (session, expected)
}

/// Percentile over an already-sorted slice (nearest-rank).
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (((sorted.len() as f64 - 1.0) * fraction).round() as usize).min(sorted.len() - 1);
    sorted[index]
}

/// One phase's measurements.
struct Stats {
    frame_p50_ms: f64,
    frame_p95_ms: f64,
    frame_max_ms: f64,
    worker_p50_ms: f64,
    worker_p95_ms: f64,
    worker_max_ms: f64,
    dispatches: usize,
    bytes: u64,
}

/// Measure `samples` dispatch-to-reap cycles on `session`.
///
/// Each cycle marks the world dirty, times `queue_autosave` as frame work,
/// then times each `poll_autosave` as frame work until the worker reaps. The
/// dispatch-to-reap wall time is recorded as worker work. Every reap must be
/// `Ok` and must have written exactly `expected.len()` bytes; the first cycle
/// also compares the file bytes with `expected` exactly.
fn measure(session: &mut WorldSession, samples: usize, path: &Path, expected: &[u8]) -> Stats {
    let mut frame = Vec::with_capacity(samples);
    let mut worker = Vec::with_capacity(samples);
    let mut dispatches = 0usize;
    let mut bytes = 0u64;

    for sample in 0..samples {
        session.world.mark_unsaved();

        let worker_start = Instant::now();
        let dispatch_start = Instant::now();
        let dispatched = session.queue_autosave(&[ChunkPos::new(0, 0, 0)]);
        let dispatch_elapsed = dispatch_start.elapsed().as_secs_f64() * 1_000.0;
        assert!(dispatched, "sample {sample}: queue_autosave must dispatch");
        dispatches += 1;

        let poll_deadline = Instant::now();
        let mut poll_elapsed_ms = 0.0f64;
        loop {
            let poll_start = Instant::now();
            let reaped = session.poll_autosave();
            poll_elapsed_ms += poll_start.elapsed().as_secs_f64() * 1_000.0;
            if let Some(result) = reaped {
                result.unwrap_or_else(|error| {
                    panic!("sample {sample}: worker save failed: {error:?}")
                });
                break;
            }
            assert!(
                poll_deadline.elapsed() < REAP_DEADLINE,
                "sample {sample}: worker did not reap within {REAP_DEADLINE:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        frame.push(dispatch_elapsed + poll_elapsed_ms);
        worker.push(worker_start.elapsed().as_secs_f64() * 1_000.0);

        let written = std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("sample {sample}: stat {}: {error}", path.display()))
            .len();
        assert_eq!(
            written,
            expected.len() as u64,
            "sample {sample}: save must write the full v9 payload"
        );
        bytes += written;

        if sample == 0 {
            let actual = std::fs::read(path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            assert_eq!(actual, expected, "first save must be byte-identical v9");
        }
    }

    frame.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    worker.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));

    Stats {
        frame_p50_ms: percentile(&frame, 0.50),
        frame_p95_ms: percentile(&frame, 0.95),
        frame_max_ms: *frame.last().expect("at least one sample"),
        worker_p50_ms: percentile(&worker, 0.50),
        worker_p95_ms: percentile(&worker, 0.95),
        worker_max_ms: *worker.last().expect("at least one sample"),
        dispatches,
        bytes,
    }
}

/// Reload the saved file and assert the edit/chunk totals survived the
/// round-trip. Returns the load duration in milliseconds.
fn reload_and_check(path: &Path, expected_chunks: usize, expected_edits: usize) -> f64 {
    let start = Instant::now();
    let loaded = load_world_from_path(path)
        .unwrap_or_else(|error| panic!("reload {}: {error:?}", path.display()));
    let load_ms = start.elapsed().as_secs_f64() * 1_000.0;
    let (chunks, edits) = edit_totals(&loaded.chunk_edits);
    assert_eq!(
        edits, expected_edits,
        "reloaded save must contain every edit"
    );
    assert_eq!(
        chunks, expected_chunks,
        "reloaded save must contain every edited chunk"
    );
    load_ms
}

/// Cheap non-ignored coverage for the edit-map generator.
#[test]
fn big_edits_hits_the_requested_edit_count() {
    let (edits, chunks) = big_edits(10_000);
    let (actual_chunks, total) = edit_totals(&edits);
    assert_eq!(total, 10_000);
    assert_eq!(actual_chunks, 20, "10,000 edits / 500 per chunk");
    assert_eq!(chunks, actual_chunks);
    assert!(edits
        .values()
        .all(|chunk| chunk.values().all(|block| *block == BlockId::Stone)));

    // Boundary: a non-multiple target leaves a partial final chunk instead of
    // rounding up to a full chunk of edits.
    let (partial, partial_chunks) = big_edits(10_001);
    assert_eq!(partial_chunks, 21);
    assert_eq!(edit_totals(&partial), (21, 10_001));
    assert_eq!(
        partial[&ChunkPos::new(20, 0, 0)].len(),
        1,
        "the 21st chunk holds the single leftover edit"
    );
}

/// Release-only save-budget gate and stress report.
///
/// Panics in a debug build so the gate can never be "passed" by an
/// unoptimized binary.
#[test]
#[ignore = "release-only save-budget harness: cargo test --locked --release --test autosave_budget -- --ignored --nocapture"]
fn autosave_release_frame_budget() {
    assert!(
        !cfg!(debug_assertions),
        "autosave_budget is release-only: run cargo test --locked --release --test autosave_budget -- --ignored --nocapture"
    );

    // ---- 1M phase: gated at p95 <= 16.6 ms. ----
    let dir = tempfile::tempdir().expect("tempdir for 1M phase");
    let (mut session, expected) = big_session(1_000_000, dir.path());
    let (expected_chunks, expected_edits) = edit_totals(session.world.chunk_edits());
    assert_eq!(expected_edits, 1_000_000);
    assert_eq!(expected_chunks, 2_000);
    let path = session.save_path.clone();

    let stats = measure(&mut session, 30, &path, &expected);
    assert!(
        stats.frame_p95_ms <= FRAME_P95_BUDGET_MS,
        "frame autosave work p95 {:.3} ms exceeds the {FRAME_P95_BUDGET_MS} ms budget",
        stats.frame_p95_ms
    );
    assert_eq!(stats.dispatches, 30, "one worker dispatch per sample");
    assert_eq!(
        stats.bytes,
        30 * expected.len() as u64,
        "every sample writes the full payload"
    );
    assert!(
        stats.worker_p50_ms > stats.frame_p95_ms,
        "encode+write must dominate wall time off-thread: worker p50 {:.3} ms vs frame p95 {:.3} ms",
        stats.worker_p50_ms,
        stats.frame_p95_ms
    );

    let load_ms = reload_and_check(&path, expected_chunks, expected_edits);
    println!(
        "autosave_budget: edits=1000000 chunks=2000 samples=30 frame_p50_ms={:.3} frame_p95_ms={:.3} frame_max_ms={:.3} worker_p50_ms={:.3} worker_p95_ms={:.3} worker_max_ms={:.3} load_ms={:.3} bytes={} dispatches={} gate_p95_ms=16.6",
        stats.frame_p50_ms,
        stats.frame_p95_ms,
        stats.frame_max_ms,
        stats.worker_p50_ms,
        stats.worker_p95_ms,
        stats.worker_max_ms,
        load_ms,
        stats.bytes,
        stats.dispatches,
    );

    // ---- 10M phase: stress report only, no frame-time gate. ----
    let stress_dir = tempfile::tempdir().expect("tempdir for 10M phase");
    let (mut stress_session, stress_expected) = big_session(10_000_000, stress_dir.path());
    let (stress_chunks, stress_edits) = edit_totals(stress_session.world.chunk_edits());
    assert_eq!(stress_edits, 10_000_000);
    assert_eq!(stress_chunks, 20_000);
    let stress_path = stress_session.save_path.clone();

    let stress = measure(&mut stress_session, 5, &stress_path, &stress_expected);
    assert_eq!(
        stress.dispatches, 5,
        "one worker dispatch per stress sample"
    );
    assert_eq!(
        stress.bytes,
        5 * stress_expected.len() as u64,
        "every stress sample writes the full payload"
    );

    let stress_load_ms = reload_and_check(&stress_path, stress_chunks, stress_edits);
    println!(
        "autosave_stress: edits=10000000 chunks=20000 samples=5 frame_p50_ms={:.3} frame_p95_ms={:.3} frame_max_ms={:.3} worker_p50_ms={:.3} worker_p95_ms={:.3} worker_max_ms={:.3} load_ms={:.3} bytes={} dispatches={} gate=none",
        stress.frame_p50_ms,
        stress.frame_p95_ms,
        stress.frame_max_ms,
        stress.worker_p50_ms,
        stress.worker_p95_ms,
        stress.worker_max_ms,
        stress_load_ms,
        stress.bytes,
        stress.dispatches,
    );
}
