//! Real-binary integration proof for `opencraft --validate-worldgen` (M015/S01).
//!
//! These tests spawn the actual compiled binary in isolated temporary working
//! directories, so the slice acceptance claim is pinned at the process
//! boundary: exact exit codes, exact stdout lines, logical
//! `worldgen/prefabs/<file>` labels independent of the process CWD, and the
//! absence of any startup state (settings file, saves, packs, GPU/window).
//!
//! `--validate-worldgen` is release-facing (D245), so nothing here is gated on
//! `debug_assertions`: the whole matrix runs under both `cargo test` and
//! `cargo test --release`. The codec and catalog unit tests live in
//! `src/world/prefab.rs`; `tests/worldgen_refs.rs` pins the checked-in content.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use opencraft::world::prefab::MAX_PREFAB_FILE_BYTES;

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in `worldgen/` tree.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The exact clean report for the checked-in reference tree.
const CLEAN_LINES: [&str; 4] = [
    "ok: prefab plains_hub",
    "ok: prefab small_house",
    "ok: prefab wheat_farm",
    "validate-worldgen: 3 prefabs, 0 errors, 0 warnings",
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

/// Copy the checked-in `worldgen/` tree byte-for-byte into `dest/worldgen`
/// and return that root.
fn copy_worldgen(dest: &Path) -> PathBuf {
    let from = Path::new(MANIFEST_DIR).join("worldgen");
    let to = dest.join("worldgen");
    copy_dir_all(&from, &to);
    to
}

/// Snapshot every file under `root` as relative path -> bytes, sorted, so a
/// test can prove the validator wrote nothing.
fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
        {
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

/// Assert the validator created no startup state in the isolated CWD.
fn assert_no_startup_state(dir: &Path) {
    for name in ["settings.toml", "saves", "packs"] {
        assert!(
            !dir.join(name).exists(),
            "{name} must not be created by --validate-worldgen"
        );
    }
}

/// Assert exit 0 and the exact clean report.
fn assert_clean(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{}", report(output));
    assert_eq!(stdout_lines(output), CLEAN_LINES, "{}", report(output));
}

/// Assert exit 1, exactly one `error:` line, and that it equals `expected`.
fn assert_single_error(output: &Output, expected: &str) {
    assert_eq!(output.status.code(), Some(1), "{}", report(output));
    let lines = stdout_lines(output);
    let errors: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("error:"))
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "exactly one error line expected\n{}",
        report(output)
    );
    assert_eq!(errors[0], expected, "{}", report(output));
}

#[test]
fn clean_worldgen_validates_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    let before = snapshot_tree(&worldgen);

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_clean(&output);
    assert_no_startup_state(dir.path());
    assert_eq!(
        snapshot_tree(&worldgen),
        before,
        "validator must not modify worldgen/\n{}",
        report(&output)
    );
}

#[test]
fn broken_toml_reports_one_error_and_exits_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    replace_once(
        &worldgen.join("prefabs/small_house.toml"),
        "format = 1",
        "format = [",
    );

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_single_error(
        &output,
        "error: worldgen/prefabs/small_house.toml: invalid TOML",
    );
    assert_eq!(
        stdout_lines(&output),
        vec![
            "ok: prefab plains_hub",
            "error: worldgen/prefabs/small_house.toml: invalid TOML",
            "ok: prefab wheat_farm",
            "validate-worldgen: 2 prefabs, 1 errors, 0 warnings",
        ],
        "{}",
        report(&output)
    );
    assert_no_startup_state(dir.path());
}

#[test]
fn oversize_axis_reports_the_cap_and_exits_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    replace_once(
        &worldgen.join("prefabs/small_house.toml"),
        "size = [5, 4, 5]",
        "size = [33, 4, 5]",
    );

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_single_error(
        &output,
        "error: worldgen/prefabs/small_house.toml: size 33x4x5 exceeds cap 32x32x32",
    );
    assert_eq!(
        stdout_lines(&output),
        vec![
            "ok: prefab plains_hub",
            "error: worldgen/prefabs/small_house.toml: size 33x4x5 exceeds cap 32x32x32",
            "ok: prefab wheat_farm",
            "validate-worldgen: 2 prefabs, 1 errors, 0 warnings",
        ],
        "{}",
        report(&output)
    );
}

#[test]
fn oversize_file_reports_the_byte_cap_and_exits_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    std::fs::write(
        worldgen.join("prefabs/huge.toml"),
        vec![b' '; MAX_PREFAB_FILE_BYTES as usize + 1],
    )
    .expect("write huge.toml");

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_single_error(
        &output,
        "error: worldgen/prefabs/huge.toml: file is larger than 1 MiB",
    );
    assert_eq!(
        stdout_lines(&output),
        vec![
            "error: worldgen/prefabs/huge.toml: file is larger than 1 MiB",
            "ok: prefab plains_hub",
            "ok: prefab small_house",
            "ok: prefab wheat_farm",
            "validate-worldgen: 3 prefabs, 1 errors, 0 warnings",
        ],
        "{}",
        report(&output)
    );
}

#[test]
fn out_of_range_socket_reports_the_pinned_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    replace_once(
        &worldgen.join("prefabs/small_house.toml"),
        "position = [0, 1, -2]",
        "position = [99, 1, -2]",
    );

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_single_error(
        &output,
        "error: worldgen/prefabs/small_house.toml: socket position [99, 1, -2] is outside the prefab",
    );
    assert_eq!(
        stdout_lines(&output),
        vec![
            "ok: prefab plains_hub",
            "error: worldgen/prefabs/small_house.toml: socket position [99, 1, -2] is outside the prefab",
            "ok: prefab wheat_farm",
            "validate-worldgen: 2 prefabs, 1 errors, 0 warnings",
        ],
        "{}",
        report(&output)
    );
}

#[test]
fn unknown_palette_character_reports_the_pinned_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    // plains_hub's palette defines '#' and '=' only, so '$' is undefined.
    // The whole first layer row is unique, so exactly one '#' becomes '$'.
    replace_once(
        &worldgen.join("prefabs/plains_hub.toml"),
        "  [ \"#######\", \"#######\", \"#######\", \"#######\", \"#######\", \"#######\", \"#######\" ],",
        "  [ \"$######\", \"#######\", \"#######\", \"#######\", \"#######\", \"#######\", \"#######\" ],",
    );

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_single_error(
        &output,
        "error: worldgen/prefabs/plains_hub.toml: palette character '$' is not defined",
    );
    assert_eq!(
        stdout_lines(&output),
        vec![
            "error: worldgen/prefabs/plains_hub.toml: palette character '$' is not defined",
            "ok: prefab small_house",
            "ok: prefab wheat_farm",
            "validate-worldgen: 2 prefabs, 1 errors, 0 warnings",
        ],
        "{}",
        report(&output)
    );
}

#[test]
fn duplicate_name_rejects_the_later_sorted_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    std::fs::copy(
        worldgen.join("prefabs/small_house.toml"),
        worldgen.join("prefabs/zzz_duplicate.toml"),
    )
    .expect("copy duplicate prefab");

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_single_error(
        &output,
        "error: worldgen/prefabs/zzz_duplicate.toml: duplicate prefab name 'small_house'",
    );
    assert_eq!(
        stdout_lines(&output),
        vec![
            "ok: prefab plains_hub",
            "ok: prefab small_house",
            "ok: prefab wheat_farm",
            "error: worldgen/prefabs/zzz_duplicate.toml: duplicate prefab name 'small_house'",
            "validate-worldgen: 3 prefabs, 1 errors, 0 warnings",
        ],
        "{}",
        report(&output)
    );
}

#[test]
fn missing_tree_is_valid_empty_content() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["validate-worldgen: 0 prefabs, 0 errors, 0 warnings"],
        "{}",
        report(&output)
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("read tempdir").count(),
        0,
        "a missing tree must not create any file\n{}",
        report(&output)
    );
}

#[test]
fn non_toml_files_and_subdirectories_are_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worldgen = copy_worldgen(dir.path());
    std::fs::write(worldgen.join("prefabs/notes.txt"), "not a prefab\n").expect("write notes.txt");
    std::fs::create_dir(worldgen.join("prefabs/subdir")).expect("create subdir");

    let output = run_in(dir.path(), &["--validate-worldgen"]);

    assert_clean(&output);
    assert_no_startup_state(dir.path());
}

#[test]
fn custom_root_keeps_logical_labels_and_clean_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = Path::new(MANIFEST_DIR).join("worldgen");
    copy_dir_all(&source, &dir.path().join("custom_worldgen"));

    let output = run_in(dir.path(), &["--validate-worldgen", "custom_worldgen"]);

    assert_clean(&output);
    assert_no_startup_state(dir.path());
    assert!(
        !dir.path().join("worldgen").exists(),
        "the custom root must be the only tree read\n{}",
        report(&output)
    );
}

#[test]
fn following_switch_is_not_consumed_as_the_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    copy_worldgen(dir.path());

    let output = run_in(
        dir.path(),
        &["--validate-worldgen", "--save", "ignored.mcvx"],
    );

    assert_clean(&output);
    assert_no_startup_state(dir.path());
    assert!(
        !dir.path().join("ignored.mcvx").exists(),
        "the validator must return before any save write\n{}",
        report(&output)
    );
}
