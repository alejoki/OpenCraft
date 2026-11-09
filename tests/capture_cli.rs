//! Real-binary acceptance matrix for `opencraft --demo-capture <dir>`
//! (M015/S05-T05).
//!
//! The demo is the headless capture -> validate leg: it builds an isolated
//! in-memory world, drives the real `PrefabCapture` state machine, writes one
//! fixed house through `capture_to_file`, re-parses the bytes, and runs the
//! real worldgen validator. Each leg spawns the compiled binary in an isolated
//! temporary working directory with the graphical environment removed, so the
//! pinned digest is proven at the process boundary: exact stdout, exit codes,
//! fail-closed errors and — via a full tree snapshot — zero stray writes.
//!
//! `--demo-capture` is a release-facing launch switch, so nothing here is
//! gated on `debug_assertions`: the matrix runs under both `cargo test` and
//! `cargo test --release`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use opencraft::world::block::BlockId;
use opencraft::world::prefab::{Direction, PrefabSocket};

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");

/// The exact clean digest for a fresh demo directory.
///
/// Measured from a real `cargo run -- --demo-capture <fresh-dir>` and
/// independently pinned in `src/main.rs`'s
/// `capture_demo_pins_the_fixed_house_line_set`. Never derive these from the
/// code under test: the region, byte count and round-trip/validate/socket/
/// catalog/village/road values below are pinned independently.
const CLEAN_LINES: [&str; 10] = [
    "capture_demo: leg=region min=0,40,0 max=4,43,4 size=5x4x5 cells=100",
    "capture_demo: leg=write name=capture path=prefabs/capture.toml bytes=423",
    "capture_demo: leg=roundtrip identical=true",
    "capture_demo: leg=validate ok=true prefabs=1 errors=0",
    "capture_demo: leg=socket position=0,1,-2 direction=north sockets=1 markers=0",
    "capture_demo: leg=ambiguous pos=2,41,0 wrote=false reason=socket marker at [2, 41, 0] is ambiguous",
    "capture_demo: leg=catalog captured=true definition=true diagnostics=0",
    "capture_demo: leg=village definition=capture_demo biome=plains cell=6,-6 anchor=1654,-1383 prefabs=2 socket_links=1 roads=2 prefab=capture",
    "capture_demo: leg=road cobblestone=2/2 socket_clear=true prefab_block=true roads=2",
    "capture_demo: ok=true",
];

/// The exact `--validate-worldgen` report for a clean demo root.
const VALIDATE_LINES: [&str; 2] = [
    "ok: prefab capture",
    "validate-worldgen: 1 prefabs, 0 errors, 0 warnings",
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

/// Snapshot every file under `root` as relative path -> bytes, sorted, so a
/// test can prove the demo wrote exactly one file and nothing else.
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

/// The single `leg=write` line.
fn write_line(lines: &[String]) -> &String {
    let matches: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("capture_demo: leg=write "))
        .collect();
    assert_eq!(matches.len(), 1, "exactly one write line expected");
    matches[0]
}

/// Extract one `key=value` field from a digest line.
fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(key))
        .unwrap_or_else(|| panic!("{key} missing from {line:?}"))
}

/// Assert the demo created no startup state in the isolated CWD.
fn assert_no_startup_state(dir: &Path) {
    for name in ["settings.toml", "saves", "packs"] {
        assert!(
            !dir.join(name).exists(),
            "{name} must not be created by --demo-capture"
        );
    }
}

#[test]
fn clean_demo_writes_one_prefab_and_validates_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        snapshot_tree(dir.path()).is_empty(),
        "the demo directory must start empty"
    );

    let output = run_in(dir.path(), &["--demo-capture", "."]);

    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    assert_eq!(stdout_lines(&output), CLEAN_LINES, "{}", report(&output));
    assert_no_startup_state(dir.path());

    let tree = snapshot_tree(dir.path());
    assert_eq!(
        tree.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["prefabs/capture.toml", "villages/capture_demo.toml"],
        "exactly one prefab, one village definition and no stray files\n{}",
        report(&output)
    );

    // The written path and name come from the demo's own digest line.
    let lines = stdout_lines(&output);
    let write = write_line(&lines);
    let name = field(write, "name=");
    let path = field(write, "path=");
    assert_eq!(name, "capture", "a fresh directory prefills `capture`");
    assert_eq!(path, "prefabs/capture.toml");
    assert!(
        dir.path().join(path).is_file(),
        "the digest path must be the file that exists"
    );

    // The file is a real prefab with the pinned house shape.
    let text = std::fs::read_to_string(dir.path().join(path)).expect("read written prefab");
    let parsed = opencraft::world::prefab::parse_prefab(&text).expect("the written prefab parses");
    assert_eq!(parsed.name, name);
    assert_eq!(parsed.size, [5, 4, 5]);
    assert_eq!(parsed.blocks.len(), 100);

    // The doorway marker became one origin-relative socket, and the marker
    // block itself is gone: the doorway floor and header cells are air, the
    // threshold step is cobblestone, and no serialized cell is a marker.
    assert_eq!(
        parsed.sockets,
        vec![PrefabSocket {
            position: [0, 1, -2],
            direction: Direction::North,
        }],
        "the doorway marker must become exactly one north-facing socket"
    );
    assert!(
        parsed
            .blocks
            .iter()
            .all(|block| block.block != BlockId::SocketMarker),
        "no prefab cell may serialize a socket marker"
    );
    let block_at = |x: u32, y: u32, z: u32| {
        parsed
            .blocks
            .iter()
            .find(|block| block.position == [x, y, z])
            .map(|block| block.block)
    };
    assert_eq!(block_at(2, 1, 0), Some(BlockId::Air), "marker cell is air");
    assert_eq!(
        block_at(2, 2, 0),
        Some(BlockId::Air),
        "doorway header is air"
    );
    assert_eq!(
        block_at(2, 1, 1),
        Some(BlockId::Cobblestone),
        "threshold step survives capture"
    );
    assert!(
        !text.contains("socket_marker"),
        "the serialized prefab must not mention the marker block"
    );

    // The one demo definition names the captured prefab and loads through the
    // same production loader the binary uses: zero diagnostics, and both the
    // definition and the captured prefab are present.
    let definition_text = std::fs::read_to_string(dir.path().join("villages/capture_demo.toml"))
        .expect("read demo definition");
    assert!(
        definition_text.contains("hub = \"capture\""),
        "the demo definition must name the captured prefab: {definition_text}"
    );
    let (catalog, diagnostics) = opencraft::world::village::VillageCatalog::load_sorted(dir.path());
    assert_eq!(diagnostics, Vec::<String>::new());
    assert!(catalog.get("capture_demo").is_some());
    assert!(catalog.prefabs().get("capture").is_some());

    // The real validator accepts the demo root and reports the same name.
    let validate = run_in(dir.path(), &["--validate-worldgen", "."]);
    assert_eq!(validate.status.code(), Some(0), "{}", report(&validate));
    assert_eq!(
        stdout_lines(&validate),
        VALIDATE_LINES,
        "{}",
        report(&validate)
    );
    assert!(
        !dir.path().join("settings.toml").exists()
            && !dir.path().join("saves").exists()
            && !dir.path().join("packs").exists(),
        "--validate-worldgen must not create startup state"
    );
}

#[test]
fn missing_value_exits_2_with_exact_error_and_empty_tree() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = run_in(dir.path(), &["--demo-capture"]);

    assert_eq!(output.status.code(), Some(2), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["error: --demo-capture requires a directory"],
        "{}",
        report(&output)
    );
    assert!(
        snapshot_tree(dir.path()).is_empty(),
        "a missing directory must write nothing\n{}",
        report(&output)
    );
    assert_no_startup_state(dir.path());
}

#[test]
fn following_switch_is_not_consumed_and_no_save_is_written() {
    let dir = tempfile::tempdir().expect("tempdir");

    // The `peek` guard must keep `--save` independent, so the empty demo path
    // still fails closed and the save switch is never turned into a directory.
    let output = run_in(dir.path(), &["--demo-capture", "--save", "custom.mcvx"]);

    assert_eq!(output.status.code(), Some(2), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["error: --demo-capture requires a directory"],
        "{}",
        report(&output)
    );
    assert!(
        !dir.path().join("custom.mcvx").exists(),
        "--demo-capture must not write a save"
    );
    assert!(
        snapshot_tree(dir.path()).is_empty(),
        "the failed launch must write nothing\n{}",
        report(&output)
    );
}

#[test]
fn regular_file_root_exits_1_with_one_readable_error_and_no_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("not-a-directory");
    std::fs::write(&root, b"keep me").expect("write regular file");
    let before = snapshot_tree(dir.path());

    let output = run_in(dir.path(), &["--demo-capture", "not-a-directory"]);

    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    let lines = stdout_lines(&output);
    let errors: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("error="))
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "exactly one readable error line expected\n{}",
        report(&output)
    );
    assert!(
        errors[0].contains("not a directory"),
        "the error must name the filesystem reason\n{}",
        report(&output)
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("capture_demo: ok=false failed=write"),
        "{}",
        report(&output)
    );
    assert_eq!(
        snapshot_tree(dir.path()),
        before,
        "a failed write must leave the tree byte-identical\n{}",
        report(&output)
    );
}

#[test]
fn two_clean_runs_are_byte_identical() {
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");

    let first_output = run_in(first.path(), &["--demo-capture", "."]);
    let second_output = run_in(second.path(), &["--demo-capture", "."]);

    assert_eq!(
        first_output.status.code(),
        Some(0),
        "{}",
        report(&first_output)
    );
    assert_eq!(
        second_output.status.code(),
        Some(0),
        "{}",
        report(&second_output)
    );
    assert_eq!(
        stdout_lines(&first_output),
        stdout_lines(&second_output),
        "the digest must be a pure function of the fixture"
    );

    let first_bytes =
        std::fs::read(first.path().join("prefabs/capture.toml")).expect("first prefab");
    let second_bytes =
        std::fs::read(second.path().join("prefabs/capture.toml")).expect("second prefab");
    assert_eq!(
        first_bytes, second_bytes,
        "two clean runs must write byte-identical prefabs"
    );
    let first_definition =
        std::fs::read(first.path().join("villages/capture_demo.toml")).expect("first definition");
    let second_definition =
        std::fs::read(second.path().join("villages/capture_demo.toml")).expect("second definition");
    assert_eq!(
        first_definition, second_definition,
        "two clean runs must write byte-identical village definitions"
    );
}

#[test]
fn second_run_in_the_same_directory_prefills_capture_2_and_keeps_the_tree_valid() {
    let dir = tempfile::tempdir().expect("tempdir");

    let first = run_in(dir.path(), &["--demo-capture", "."]);
    assert_eq!(first.status.code(), Some(0), "{}", report(&first));
    assert_eq!(stdout_lines(&first), CLEAN_LINES, "{}", report(&first));
    let first_bytes = std::fs::read(dir.path().join("prefabs/capture.toml")).expect("first prefab");

    // A reused directory is idempotent at the tree level: the next free name
    // is `capture_2`, the original file stays byte-identical, and the leg
    // still passes because the tree has zero errors and accepts the captured
    // name. `prefabs` is therefore intentionally not pinned to one.
    let second = run_in(dir.path(), &["--demo-capture", "."]);
    assert_eq!(second.status.code(), Some(0), "{}", report(&second));
    let lines = stdout_lines(&second);
    let write = write_line(&lines);
    assert_eq!(field(write, "name="), "capture_2");
    assert_eq!(field(write, "path="), "prefabs/capture_2.toml");
    assert!(
        lines
            .iter()
            .any(|line| line == "capture_demo: leg=validate ok=true prefabs=2 errors=0"),
        "the second run must validate both prefabs: {}",
        report(&second)
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("capture_demo: ok=true")
    );

    let second_path = dir.path().join("prefabs/capture_2.toml");
    assert_eq!(
        field(write, "bytes="),
        std::fs::read(&second_path)
            .expect("second prefab")
            .len()
            .to_string(),
        "the digest byte count must describe the file that was written"
    );
    assert_eq!(
        std::fs::read(dir.path().join("prefabs/capture.toml")).expect("first prefab"),
        first_bytes,
        "a reused directory must never rewrite the first prefab"
    );
    assert_eq!(
        snapshot_tree(dir.path())
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "prefabs/capture.toml",
            "prefabs/capture_2.toml",
            "villages/capture_demo.toml",
        ],
        "the reused directory holds both prefabs and the fixed definition\n{}",
        report(&second)
    );
    // The fixed definition file is overwritten in place to name the prefab the
    // second run actually captured, so the catalog never points at a name that
    // does not exist.
    let definition_text = std::fs::read_to_string(dir.path().join("villages/capture_demo.toml"))
        .expect("read demo definition");
    assert!(
        definition_text.contains("hub = \"capture_2\""),
        "the second run must rewrite the definition: {definition_text}"
    );
    assert!(
        !definition_text.contains("hub = \"capture\"\n"),
        "the stale first-run hub must be gone: {definition_text}"
    );

    let parsed = opencraft::world::prefab::parse_prefab(
        &std::fs::read_to_string(&second_path).expect("read second prefab"),
    )
    .expect("capture_2 parses");
    assert_eq!(parsed.name, "capture_2");
    assert_eq!(parsed.size, [5, 4, 5]);

    let validate = run_in(dir.path(), &["--validate-worldgen", "."]);
    assert_eq!(validate.status.code(), Some(0), "{}", report(&validate));
    assert_eq!(
        stdout_lines(&validate),
        vec![
            "ok: prefab capture".to_owned(),
            "ok: prefab capture_2".to_owned(),
            "validate-worldgen: 2 prefabs, 0 errors, 0 warnings".to_owned(),
        ],
        "{}",
        report(&validate)
    );
}

/// Guard against an accidentally relative binary path in future refactors.
#[allow(dead_code)]
fn _binary_is_absolute() {
    assert!(PathBuf::from(BIN).is_absolute());
}
