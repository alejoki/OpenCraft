//! Real-binary integration proof for `opencraft --demo-save-v9` (M014/S04-T05).
//!
//! These tests spawn the actual compiled binary in isolated temporary working
//! directories, so the v8-to-v9 migration is pinned at the process boundary:
//! a frozen v8 fixture loads without a config notice, the derived v9 output
//! carries the defaults schema + digest after the unchanged v8 tail, a changed
//! config produces exactly one non-blocking load notice, a matching-config
//! reload is silent, and a missing input fails closed. Codec unit tests live in
//! `src/world/persistence.rs`; this file covers the real launch path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in `config/` tree and fixture.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The six curated config files.
const CONFIG_FILES: [&str; 6] = [
    "player.toml",
    "survival.toml",
    "combat.toml",
    "world.toml",
    "mining.toml",
    "render.toml",
];

/// Registry schema and digest of the checked-in default config.
const DEFAULTS_SCHEMA: u16 = 1;
const DEFAULTS_DIGEST: u64 = 0x2d99_ee0c_5e96_d8bd;

/// Copy the six checked-in config files byte-for-byte into `dest/config`.
fn copy_config(dest: &Path) -> PathBuf {
    let source = Path::new(MANIFEST_DIR).join("config");
    let config = dest.join("config");
    std::fs::create_dir_all(&config).expect("create dest/config");
    for name in CONFIG_FILES {
        let from = source.join(name);
        std::fs::copy(&from, config.join(name))
            .unwrap_or_else(|error| panic!("copy {}: {error}", from.display()));
    }
    config
}

/// Copy the frozen v8 fixture into `dest` under `name`.
fn copy_fixture(dest: &Path, name: &str) -> Vec<u8> {
    let from = Path::new(MANIFEST_DIR).join("tests/fixtures/saves/v8-baseline.mcvx");
    let bytes = std::fs::read(&from)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", from.display()));
    std::fs::write(dest.join(name), &bytes)
        .unwrap_or_else(|error| panic!("write fixture copy {name}: {error}"));
    bytes
}

/// Run the real binary with `--demo-save-v9 --save <name>` from `dir`.
fn run_demo(dir: &Path, save: &str) -> Output {
    Command::new(BIN)
        .arg("--demo-save-v9")
        .arg("--save")
        .arg(save)
        .current_dir(dir)
        .output()
        .expect("spawn opencraft --demo-save-v9")
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

fn read_u32_at(bytes: &[u8], offset: usize) -> u32 {
    let raw = bytes
        .get(offset..offset + 4)
        .unwrap_or_else(|| panic!("save must carry a version word at byte {offset}"));
    u32::from_le_bytes(raw.try_into().expect("4-byte version slice"))
}

fn assert_v9_tail(bytes: &[u8], digest: u64) {
    let tail = bytes
        .get(bytes.len().saturating_sub(10)..)
        .unwrap_or_else(|| panic!("v9 save must carry the 10-byte config tail"));
    let mut expected = Vec::with_capacity(10);
    expected.extend_from_slice(&DEFAULTS_SCHEMA.to_le_bytes());
    expected.extend_from_slice(&digest.to_le_bytes());
    assert_eq!(tail, expected.as_slice(), "v9 config tail mismatch");
}

/// Replace exactly one occurrence, proving the fixture edit actually landed.
fn replace_once(path: &Path, from: &str, to: &str) {
    let contents = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let matches = contents.matches(from).count();
    assert_eq!(
        matches,
        1,
        "{from:?} must appear exactly once in {}",
        path.display()
    );
    std::fs::write(path, contents.replacen(from, to, 1))
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn sorted_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Parses `save_v9: leg=resave version=9 config_schema=1 config_digest=0x<16 hex> bytes=<n>`.
fn parse_resave(line: &str) -> (u64, usize) {
    let rest = line
        .strip_prefix("save_v9: leg=resave version=9 config_schema=1 config_digest=0x")
        .unwrap_or_else(|| panic!("resave line has the pinned prefix: {line:?}"));
    let (digest, bytes) = rest
        .split_once(" bytes=")
        .unwrap_or_else(|| panic!("resave line has a bytes token: {line:?}"));
    assert_eq!(digest.len(), 16, "digest must be 16 hex digits: {line:?}");
    let digest = u64::from_str_radix(digest, 16).expect("digest must be lowercase hex");
    let bytes = bytes.parse::<usize>().expect("bytes must be a number");
    (digest, bytes)
}

#[test]
fn v8_fixture_migrates_to_v9_and_reloads_silently() {
    let temp = tempfile::tempdir().expect("temp dir");
    copy_config(temp.path());
    let original = copy_fixture(temp.path(), "v8.mcvx");

    let output = run_demo(temp.path(), "v8.mcvx");
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("save_v9: leg=load version=8 packs=0 saved_config=none notice=none playable=true"),
        "{}",
        report(&output)
    );
    let resave = lines.get(1).expect("resave line").as_str();
    let (digest, bytes) = parse_resave(resave);
    assert_eq!(digest, DEFAULTS_DIGEST, "{resave}");
    assert_eq!(
        bytes,
        original.len() + 10,
        "the v9 file is the v8 payload plus the 10-byte config tail: {resave}"
    );
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("save_v9: leg=match notice=none playable=true"),
        "{}",
        report(&output)
    );
    assert_eq!(
        lines.get(3).map(String::as_str),
        Some("save_v9: ok"),
        "{}",
        report(&output)
    );
    assert_eq!(lines.len(), 4, "{}", report(&output));

    // The input is never overwritten.
    let after_input = std::fs::read(temp.path().join("v8.mcvx")).expect("read input");
    assert_eq!(
        after_input, original,
        "the input save must stay byte-identical"
    );

    // The derived output is a v9 save with the defaults config tail.
    let derived = std::fs::read(temp.path().join("v8.v9.mcvx")).expect("read derived v9");
    assert_eq!(read_u32_at(&derived, 4), 9, "derived save must be v9");
    assert_eq!(derived.len(), original.len() + 10);
    assert_v9_tail(&derived, DEFAULTS_DIGEST);

    // Only the config tree and the input/output saves exist: no settings,
    // saves directory, packs, temp files, or other startup state.
    assert_eq!(
        sorted_entries(temp.path()),
        vec!["config", "v8.mcvx", "v8.v9.mcvx"],
        "the proof must create no startup state"
    );
    assert!(!temp.path().join("settings.toml").exists());
    assert!(!temp.path().join("saves").exists());
    assert!(!temp.path().join("packs").exists());
}

#[test]
fn changed_config_warns_once_and_resaves_v9() {
    let temp = tempfile::tempdir().expect("temp dir");
    let config = copy_config(temp.path());
    copy_fixture(temp.path(), "v8.mcvx");

    // First leg: v8 -> v9 under the default config.
    let first = run_demo(temp.path(), "v8.mcvx");
    assert!(first.status.success(), "{}", report(&first));
    let first_v9 = std::fs::read(temp.path().join("v8.v9.mcvx")).expect("read first v9");
    assert_eq!(read_u32_at(&first_v9, 4), 9);

    // Second leg: reload the v9 output under a changed tuning value.
    replace_once(
        &config.join("player.toml"),
        "walk_speed = 4.6",
        "walk_speed = 6.0",
    );

    let output = run_demo(temp.path(), "v8.v9.mcvx");
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines.first().map(String::as_str),
        Some(
            "save_v9: leg=load version=9 packs=0 saved_config=some notice=config changed since this world was saved playable=true"
        ),
        "exactly one non-blocking notice on load\n{}",
        report(&output)
    );
    let resave = lines.get(1).expect("resave line").as_str();
    let (changed_digest, bytes) = parse_resave(resave);
    assert_ne!(
        changed_digest, DEFAULTS_DIGEST,
        "the changed config must move the digest"
    );
    assert_eq!(
        bytes,
        first_v9.len(),
        "the v9-to-v9 resave re-encodes the same payload plus its config tail: {resave}"
    );
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("save_v9: leg=match notice=none playable=true"),
        "the matching-config reload is silent\n{}",
        report(&output)
    );
    assert_eq!(
        lines.get(3).map(String::as_str),
        Some("save_v9: ok"),
        "{}",
        report(&output)
    );
    assert_eq!(lines.len(), 4, "{}", report(&output));

    // The first v9 output was the input and must not be overwritten.
    let after_first = std::fs::read(temp.path().join("v8.v9.mcvx")).expect("reread first v9");
    assert_eq!(
        after_first, first_v9,
        "the input v9 save must stay byte-identical"
    );

    // The new v9 output carries the changed config tail.
    let second_v9 =
        std::fs::read(temp.path().join("v8.v9.v9.mcvx")).expect("read second v9 output");
    assert_eq!(read_u32_at(&second_v9, 4), 9);
    assert_eq!(second_v9.len(), first_v9.len());
    assert_v9_tail(&second_v9, changed_digest);
}

#[test]
fn missing_input_fails_closed() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_demo(temp.path(), "absent.mcvx");
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines,
        vec!["save_v9: failed reason=missing-input".to_string()],
        "{}",
        report(&output)
    );
    assert!(!temp.path().join("absent.mcvx").exists());
    assert!(!temp.path().join("absent.v9.mcvx").exists());
}
