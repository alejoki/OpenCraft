//! Real-binary acceptance matrix for `opencraft --demo-village` (M015/S02-T06).
//!
//! Each leg spawns the actual compiled binary in an isolated temporary working
//! directory with the graphical environment removed, so the pinned digest is
//! proven at the process boundary: exact stdout, exit codes, fail-soft
//! diagnostics and — via a full tree snapshot — zero writes.
//!
//! `--demo-village` is a release-facing launch switch (M015/S02-T05), so
//! nothing here is gated on `debug_assertions`: the matrix runs under both
//! `cargo test` and `cargo test --release`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in `worldgen/` tree.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The exact clean digest for the checked-in reference tree.
///
/// Measured from a real `cargo run -- --demo-village` and independently pinned
/// in `src/main.rs`'s `village_demo_pins_the_checked_in_reference_village_line_set`.
/// Never derive these from the code under test.
const CLEAN_LINES: [&str; 11] = [
    "village_demo: leg=catalog prefabs=3 villages=1 diagnostics=0 fingerprint=0x5c2b550240d34cfb",
    "village_demo: leg=context rules=0 villages=1",
    "village_demo: leg=site cell=6,-6 anchor=1654,-1383 definition=plains prefabs=13 socket_links=12 roads=48 unmatched=22",
    "village_demo: leg=counts in_range=true min=10 max=20",
    "village_demo: leg=layout digest=0x79663be18a213bab",
    "village_demo: leg=order identical=true",
    "village_demo: leg=single_chunk identical=true",
    "village_demo: leg=empty_catalog identical=true",
    "village_demo: leg=outside identical=true",
    "village_demo: leg=fit foundations=true no_overlap=true no_orphan_road=true sockets_linked=true",
    "village_demo: ok=true",
];

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

/// Copy the checked-in `worldgen/` tree byte-for-byte into `dest/worldgen`.
fn copy_worldgen(dest: &Path) -> PathBuf {
    let from = Path::new(MANIFEST_DIR).join("worldgen");
    let to = dest.join("worldgen");
    copy_dir_all(&from, &to);
    to
}

/// Snapshot every file under `root` as relative path -> bytes, sorted, so a
/// test can prove the digest wrote nothing.
fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read_dir {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("path under snapshot root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                out.insert(relative, bytes);
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
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

/// Replace exactly one occurrence, proving the fixture edit actually landed.
fn replace_once(path: &Path, from: &str, to: &str) {
    let contents = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert_eq!(
        contents.matches(from).count(),
        1,
        "{from:?} must appear exactly once in {}",
        path.display()
    );
    std::fs::write(path, contents.replacen(from, to, 1))
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

/// Assert the demo created no startup state in the isolated CWD.
fn assert_no_startup_state(dir: &Path) {
    for name in ["settings.toml", "saves", "packs"] {
        assert!(
            !dir.join(name).exists(),
            "{name} must not be created by --demo-village"
        );
    }
}

/// All `village_demo: warning` lines, in print order.
fn warning_lines(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|line| line.starts_with("village_demo: warning "))
        .collect()
}

/// The single `leg=catalog` line.
fn catalog_line(lines: &[String]) -> &String {
    let matches: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("village_demo: leg=catalog "))
        .collect();
    assert_eq!(matches.len(), 1, "exactly one catalog line expected");
    matches[0]
}

#[test]
fn clean_tree_prints_the_pinned_digest_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    let before = snapshot_tree(&worldgen);

    let output = run_in(dir.path(), &["--demo-village"]);

    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    assert_eq!(stdout_lines(&output), CLEAN_LINES, "{}", report(&output));
    assert_no_startup_state(dir.path());
    assert_eq!(
        snapshot_tree(&worldgen),
        before,
        "--demo-village must not modify worldgen/\n{}",
        report(&output)
    );
}

#[test]
fn broken_village_definition_warns_once_and_fails_site() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    replace_once(
        &worldgen.join("villages/plains.toml"),
        "spacing = 4",
        "spacing = 99",
    );
    let before = snapshot_tree(&worldgen);

    let output = run_in(dir.path(), &["--demo-village"]);

    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    let lines = stdout_lines(&output);
    assert_eq!(
        warning_lines(&lines),
        vec![
            "village_demo: warning worldgen/villages/plains.toml: spacing must be an integer in 1..=16"
        ],
        "{}",
        report(&output)
    );
    assert!(
        catalog_line(&lines).contains("villages=0"),
        "the rejected definition must leave an empty catalog\n{}",
        report(&output)
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("village_demo: ok=false failed=site"),
        "{}",
        report(&output)
    );
    assert_no_startup_state(dir.path());
    assert_eq!(snapshot_tree(&worldgen), before, "{}", report(&output));
}

#[test]
fn missing_worldgen_tree_fails_site_without_creating_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(snapshot_tree(dir.path()).is_empty());

    let output = run_in(dir.path(), &["--demo-village"]);

    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    let lines = stdout_lines(&output);
    assert!(
        warning_lines(&lines).is_empty(),
        "a missing tree is valid empty content, not a warning\n{}",
        report(&output)
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("village_demo: ok=false failed=site"),
        "{}",
        report(&output)
    );
    assert_no_startup_state(dir.path());
    assert!(
        snapshot_tree(dir.path()).is_empty(),
        "--demo-village must write nothing\n{}",
        report(&output)
    );
}

#[test]
fn broken_hub_prefab_warns_and_fails_site() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    replace_once(
        &worldgen.join("prefabs/plains_hub.toml"),
        "format = 1",
        "format = [",
    );
    let before = snapshot_tree(&worldgen);

    let output = run_in(dir.path(), &["--demo-village"]);

    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    let lines = stdout_lines(&output);
    assert_eq!(
        warning_lines(&lines),
        vec![
            "village_demo: warning worldgen/prefabs/plains_hub.toml: invalid TOML",
            "village_demo: warning worldgen/villages/plains.toml: hub prefab 'plains_hub' is not in the catalog",
        ],
        "{}",
        report(&output)
    );
    let catalog = catalog_line(&lines);
    assert!(
        catalog.contains("prefabs=2")
            && catalog.contains("villages=0")
            && catalog.contains("diagnostics=2"),
        "the broken hub must leave two prefabs and no definitions\n{}",
        report(&output)
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("village_demo: ok=false failed=site"),
        "{}",
        report(&output)
    );
    assert_no_startup_state(dir.path());
    assert_eq!(snapshot_tree(&worldgen), before, "{}", report(&output));
}

#[test]
fn following_save_switch_is_not_consumed_and_no_save_is_written() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    let before = snapshot_tree(&worldgen);

    let output = run_in(dir.path(), &["--demo-village", "--save", "custom.mcvx"]);

    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        CLEAN_LINES,
        "--save must not change the digest\n{}",
        report(&output)
    );
    assert!(
        !dir.path().join("custom.mcvx").exists(),
        "--demo-village must not write a save"
    );
    assert_no_startup_state(dir.path());
    assert_eq!(snapshot_tree(&worldgen), before, "{}", report(&output));
}
