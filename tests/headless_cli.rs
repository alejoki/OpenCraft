//! Real-binary process coverage for the six headless seams that until now only
//! had in-process digest tests (M016/S03-T02): `--demo-ui`, `--validate-pack`,
//! `--demo-presentation`, `--demo-crafting`, `--demo-worldgen` and
//! `--demo-palette`.
//!
//! Each test spawns the actual compiled binary in its own temporary working
//! directory with the graphical environment removed, so `cargo test --locked`
//! proves the real process can run every seam from a clean CWD on Linux and
//! Windows without a display, GPU or network. The full digests stay pinned in
//! `src/main.rs` unit tests; here we assert exit status and the latch lines.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in fixture pack.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

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

/// Stage the checked-in reference pack plus a settings stack that selects it,
/// exactly as a creator would after dropping the pack into `packs/`.
fn install_reference_pack(dir: &Path) {
    let source = Path::new(MANIFEST_DIR).join("tests/fixtures/packs/mymod-ruby");
    copy_dir_all(&source, &dir.join("packs/mymod-ruby"));
    std::fs::write(
        dir.join("settings.toml"),
        "[packs]\nstack = [\"mymod-ruby\"]\n",
    )
    .expect("write settings.toml");
}

/// Run the real binary with `args` from `dir` as the process CWD, with any
/// graphical environment removed so a regression that reaches startup fails
/// here instead of opening a window.
fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("spawn opencraft")
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn last_line(lines: &[String]) -> &str {
    lines.last().map(String::as_str).unwrap_or("")
}

/// Render both streams for assertion failure messages.
fn report(output: &Output) -> String {
    format!(
        "status={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Assert the written save starts with the MCVX magic and version 9.
fn assert_mcvx_v9(path: &Path) {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert!(
        bytes.len() >= 8,
        "{} is too short: {bytes:?}",
        path.display()
    );
    assert_eq!(
        &bytes[..4],
        b"MCVX",
        "{} lacks the MCVX magic",
        path.display()
    );
    assert_eq!(
        &bytes[4..8],
        &9u32.to_le_bytes(),
        "{} is not version 9",
        path.display()
    );
}

/// The staged-tree entries ending in `.mcvx`, sorted so failures are stable.
fn mcvx_files(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "mcvx")
        })
        .collect();
    found.sort();
    found
}

#[test]
fn demo_ui_prints_three_ok_lines_and_writes_nothing() {
    let temp = tempfile::tempdir().expect("temp dir");
    let output = run_in(temp.path(), &["--demo-ui"]);
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(lines.len(), 3, "{}", report(&output));
    for line in &lines {
        assert!(line.starts_with("ui_demo: "), "{}", report(&output));
        assert!(line.ends_with("ok=true"), "{}", report(&output));
    }

    let entries: Vec<_> = std::fs::read_dir(temp.path())
        .expect("read_dir temp")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "`--demo-ui` must write nothing, found {entries:?}"
    );
}

#[test]
fn validate_pack_accepts_the_reference_pack() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_reference_pack(temp.path());

    let output = run_in(temp.path(), &["--validate-pack", "mymod-ruby"]);
    assert!(output.status.success(), "{}", report(&output));
    let lines = stdout_lines(&output);
    assert_eq!(
        last_line(&lines),
        "validate-pack: mymod-ruby: 12 entries, 0 errors, 0 warnings",
        "{}",
        report(&output)
    );
}

#[test]
fn demo_presentation_digest_runs_headlessly() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_reference_pack(temp.path());

    let output = run_in(
        temp.path(),
        &["--demo-presentation", "--save", "presentation.mcvx"],
    );
    assert!(output.status.success(), "{}", report(&output));
    assert_eq!(
        last_line(&stdout_lines(&output)),
        "presentation_demo: ok=true",
        "{}",
        report(&output)
    );
    assert_mcvx_v9(&temp.path().join("presentation.mcvx"));
}

#[test]
fn demo_crafting_digest_runs_headlessly() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_reference_pack(temp.path());

    let output = run_in(temp.path(), &["--demo-crafting", "--save", "crafting.mcvx"]);
    assert!(output.status.success(), "{}", report(&output));
    assert_eq!(
        last_line(&stdout_lines(&output)),
        "crafting: ok",
        "{}",
        report(&output)
    );
    assert_mcvx_v9(&temp.path().join("crafting.mcvx"));
}

#[test]
fn demo_worldgen_digest_runs_headlessly() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_reference_pack(temp.path());

    let output = run_in(temp.path(), &["--demo-worldgen"]);
    assert!(output.status.success(), "{}", report(&output));
    assert_eq!(
        last_line(&stdout_lines(&output)),
        "worldgen_demo: ok=true",
        "{}",
        report(&output)
    );
    let written = mcvx_files(temp.path());
    assert!(
        written.is_empty(),
        "`--demo-worldgen` must write no save, found {written:?}"
    );
}

#[test]
fn demo_palette_digest_runs_headlessly() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_reference_pack(temp.path());

    let output = run_in(temp.path(), &["--demo-palette"]);
    assert!(output.status.success(), "{}", report(&output));
    assert_eq!(
        last_line(&stdout_lines(&output)),
        "palette: ok",
        "{}",
        report(&output)
    );
}
