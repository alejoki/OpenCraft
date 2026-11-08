//! Real-binary village-enabled frame-budget harness (M015/S03-T05).
//!
//! The library-level `streaming_budget_villages` harness measures the
//! main-thread dispatch cost in isolation. This test measures the other half
//! of the slice claim: a real `opencraft` process with villages enabled must
//! hold frame p95 at or under one 60 Hz frame (`16.6 ms`) over a bounded
//! accelerated run.
//!
//! The run spawns the compiled binary in an isolated temporary working
//! directory and copies the checked-in `worldgen/` tree into it, so the
//! production startup path loads the accepted village catalog and streams the
//! pinned seed-14 village while the frame sampler runs.
//!
//! The temp CWD is mandatory, not tidiness:
//! - the repository-root `settings.toml` sets `vsync = true`, which paces
//!   frames to the refresh interval and makes the p95 claim meaningless;
//! - it guarantees the run writes nothing into the repository.
//!
//! Release-only and ignored: the measured binary must be the release binary.
//! Run it (twice or three times) on an idle desktop session:
//!
//! ```text
//! cargo test --locked --release --test village_frame_budget -- --ignored --nocapture
//! ```

use std::path::Path;
use std::process::Command;

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in `worldgen/` tree.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The operational MEM229 target: one 60 Hz frame at p95.
const FRAME_P95_BUDGET_MS: f64 = 16.6;
/// A bounded one-cycle run at 120x still renders far more than this; the floor
/// only rejects a run that sampled almost nothing (e.g. a menu-only launch).
const MIN_SAMPLES: usize = 120;

/// Recursively copy `from` into `to`, creating `to`.
fn copy_dir_all(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|error| panic!("create {}: {error}", to.display()));
    for entry in std::fs::read_dir(from)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", from.display()))
    {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap_or_else(|error| {
                panic!("copy {} -> {}: {error}", path.display(), target.display())
            });
        }
    }
}

/// Parse one numeric token of the `frame_budget:` digest, e.g.
/// `samples=4200` or `p95=4.123ms`.
fn digest_value(line: &str, key: &str, suffix: &str) -> f64 {
    let token = line
        .split_whitespace()
        .find(|token| token.starts_with(key))
        .unwrap_or_else(|| panic!("frame budget digest lost `{key}` in {line:?}"));
    let raw = token
        .strip_prefix(key)
        .and_then(|rest| rest.strip_suffix(suffix))
        .unwrap_or_else(|| panic!("malformed `{key}` token {token:?} in {line:?}"));
    raw.parse::<f64>()
        .unwrap_or_else(|error| panic!("parse `{key}` from {token:?}: {error}"))
}

#[test]
#[ignore = "release-only real-binary village frame harness: cargo test --locked --release --test village_frame_budget -- --ignored --nocapture"]
fn village_enabled_release_frame_budget() {
    if cfg!(debug_assertions) {
        panic!(
            "village_frame_budget measures release frame times: rerun with \
             `cargo test --locked --release --test village_frame_budget -- --ignored --nocapture`"
        );
    }
    // `#[ignore]` keeps normal CI away from this; a developer running it in a
    // headless shell gets the precondition instead of an opaque window error.
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        panic!(
            "village_frame_budget needs a desktop session: neither DISPLAY nor WAYLAND_DISPLAY is \
             set, so the real binary cannot create its window. Run it from a graphical session."
        );
    }

    let temp = tempfile::tempdir().expect("temp dir for the bounded run");
    copy_dir_all(
        &Path::new(MANIFEST_DIR).join("worldgen"),
        &temp.path().join("worldgen"),
    );
    let save = temp.path().join("v14.mcvx");
    let save_arg = save.to_str().expect("utf8 save path");

    let output = Command::new(BIN)
        .args([
            "--new",
            "--seed",
            "14",
            "--save",
            save_arg,
            "--time-scale",
            "120",
            "--demo-days",
            "1",
        ])
        .current_dir(temp.path())
        .output()
        .expect("spawn opencraft");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "bounded village run failed with {}: stdout={stdout:?} stderr={stderr:?}",
        output.status
    );

    let line = stdout
        .lines()
        .find(|line| line.starts_with("frame_budget:"))
        .unwrap_or_else(|| {
            panic!(
                "the bounded run never printed a frame_budget digest, so no budget was measured: \
                 stdout={stdout:?} stderr={stderr:?}"
            )
        });
    eprintln!("village_frame_budget: {line}");

    let samples = digest_value(line, "samples=", "") as usize;
    let p95 = digest_value(line, "p95=", "ms");
    assert!(
        samples >= MIN_SAMPLES,
        "only {samples} Playing frames sampled (< {MIN_SAMPLES}): the run did not exercise a real \
         village session"
    );
    assert!(
        p95 <= FRAME_P95_BUDGET_MS,
        "village-enabled release frame p95 {p95:.3}ms exceeds the {FRAME_P95_BUDGET_MS:.1}ms MEM229 \
         target: {line}"
    );

    // The bounded run completed through its own clean shutdown, which saves
    // the session inside the temp dir and nowhere else.
    let bytes =
        std::fs::read(&save).unwrap_or_else(|error| panic!("read {}: {error}", save.display()));
    assert!(
        bytes.starts_with(b"MCVX"),
        "the bounded run's save does not start with the MCVX magic: {:?}",
        &bytes[..bytes.len().min(8)]
    );
}
