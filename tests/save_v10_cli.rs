//! Real-binary integration proof for `opencraft --demo-save-v10` (M018/S02-T03).
//!
//! These tests spawn the actual compiled binary in isolated temporary working
//! directories, so the v1-v9-to-v10 migration is pinned at the process boundary:
//! a frozen v8 fixture loads without a config notice, the derived v10 archive
//! carries a zero flags word and a sorted directory, a matching-config reload is
//! silent, a v10-to-v10 resave is byte-stable, a changed config moves only the
//! config-section digest, and missing or corrupted inputs fail closed before an
//! output file is created. Codec unit tests live in `src/world/persistence.rs`;
//! this file covers the real launch path.

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
const DEFAULTS_DIGEST: u64 = 0x1bf6_1346_4d10_8311;
/// Fixed v10 sections: content table, world/player, inventory, chests, mobs,
/// content meta, config, crops and chunk index.
const V10_FIXED_SECTIONS: u32 = 9;
/// Config section id in the v10 archive (M018/S02 format, D323).
const V10_SECTION_CONFIG: u32 = 7;
/// v10 archive header size and directory entry size.
const V10_HEADER_LEN: usize = 16;
const V10_ENTRY_LEN: usize = 21;

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

/// Run the real binary with `--demo-save-v10 --save <name>` from `dir`.
fn run_demo(dir: &Path, save: &str) -> Output {
    Command::new(BIN)
        .arg("--demo-save-v10")
        .arg("--save")
        .arg(save)
        .current_dir(dir)
        .output()
        .expect("spawn opencraft --demo-save-v10")
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
        .unwrap_or_else(|| panic!("save must carry a u32 at byte {offset}"));
    u32::from_le_bytes(raw.try_into().expect("4-byte slice"))
}

fn read_u64_at(bytes: &[u8], offset: usize) -> u64 {
    let raw = bytes
        .get(offset..offset + 8)
        .unwrap_or_else(|| panic!("save must carry a u64 at byte {offset}"));
    u64::from_le_bytes(raw.try_into().expect("8-byte slice"))
}

/// Assert the archive header is canonical v10 and return its section count.
fn assert_v10_header(bytes: &[u8]) -> u32 {
    assert!(bytes.len() >= V10_HEADER_LEN, "archive too short");
    assert_eq!(&bytes[..4], b"MCVX", "archive magic");
    assert_eq!(read_u32_at(bytes, 4), 10, "archive version");
    assert_eq!(read_u32_at(bytes, 8), 0, "archive flags");
    let count = read_u32_at(bytes, 12);
    assert!(
        count >= V10_FIXED_SECTIONS,
        "v10 archive carries {count} sections; expected at least {V10_FIXED_SECTIONS}"
    );
    count
}

/// Return the config-section `(schema, digest)` pair from a v10 archive.
fn config_section(bytes: &[u8]) -> (u16, u64) {
    let count = read_u32_at(bytes, 12) as usize;
    for index in 0..count {
        let base = V10_HEADER_LEN + index * V10_ENTRY_LEN;
        let id = read_u32_at(bytes, base);
        if id != V10_SECTION_CONFIG {
            continue;
        }
        let offset = usize::try_from(read_u64_at(bytes, base + 4)).expect("section offset fits");
        let len = read_u32_at(bytes, base + 12) as usize;
        assert_eq!(len, 10, "the config section is `u16` schema + `u64` digest");
        let body = bytes
            .get(offset..offset + len)
            .expect("config section body is in bounds");
        let schema = u16::from_le_bytes(body[..2].try_into().expect("schema word"));
        let digest = u64::from_le_bytes(body[2..10].try_into().expect("digest word"));
        return (schema, digest);
    }
    panic!("v10 archive has no config section");
}

/// Parsed `save_v10: leg=resave ...` line.
struct Resave {
    digest: u64,
    bytes: usize,
    sections: usize,
}

/// Parses `save_v10: leg=resave version=10 config_schema=1 config_digest=0x<16 hex> bytes=<n> sections=<n>`.
fn parse_resave(line: &str) -> Resave {
    let rest = line
        .strip_prefix("save_v10: leg=resave version=10 config_schema=1 config_digest=0x")
        .unwrap_or_else(|| panic!("resave line has the pinned prefix: {line:?}"));
    let (digest, rest) = rest
        .split_once(" bytes=")
        .unwrap_or_else(|| panic!("resave line has a bytes token: {line:?}"));
    let (bytes, sections) = rest
        .split_once(" sections=")
        .unwrap_or_else(|| panic!("resave line has a sections token: {line:?}"));
    assert_eq!(digest.len(), 16, "digest must be 16 hex digits: {line:?}");
    let digest = u64::from_str_radix(digest, 16).expect("digest must be lowercase hex");
    Resave {
        digest,
        bytes: bytes.parse::<usize>().expect("bytes must be a number"),
        sections: sections
            .parse::<usize>()
            .expect("sections must be a number"),
    }
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

#[test]
fn v8_fixture_migrates_to_v10_and_reloads_silently() {
    let temp = tempfile::tempdir().expect("temp dir");
    copy_config(temp.path());
    let original = copy_fixture(temp.path(), "v8.mcvx");

    let output = run_demo(temp.path(), "v8.mcvx");
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("save_v10: leg=load version=8 packs=0 saved_config=none notice=none playable=true"),
        "{}",
        report(&output)
    );
    let resave = lines.get(1).expect("resave line").as_str();
    let parsed = parse_resave(resave);
    assert_eq!(parsed.digest, DEFAULTS_DIGEST, "{resave}");
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("save_v10: leg=match notice=none playable=true"),
        "{}",
        report(&output)
    );
    assert_eq!(
        lines.get(3).map(String::as_str),
        Some("save_v10: ok"),
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

    // The derived output is a canonical v10 archive. The demo-reported section
    // count is the one the archive actually carries.
    let derived = std::fs::read(temp.path().join("v8.v10.mcvx")).expect("read derived v10");
    let section_count = assert_v10_header(&derived);
    assert_eq!(
        parsed.sections, section_count as usize,
        "the resave line reports the archive section count"
    );
    assert_eq!(
        parsed.bytes,
        derived.len(),
        "the resave line reports the file length"
    );
    assert_eq!(
        config_section(&derived),
        (DEFAULTS_SCHEMA, DEFAULTS_DIGEST),
        "the derived archive carries the defaults config section"
    );

    // Only the config tree and the input/output saves exist: no settings,
    // saves directory, packs, temp files, or other startup state.
    assert_eq!(
        sorted_entries(temp.path()),
        vec!["config", "v8.mcvx", "v8.v10.mcvx"],
        "the proof must create no startup state"
    );
    assert!(!temp.path().join("settings.toml").exists());
    assert!(!temp.path().join("saves").exists());
    assert!(!temp.path().join("packs").exists());
}

#[test]
fn v10_to_v10_resave_is_byte_stable() {
    let temp = tempfile::tempdir().expect("temp dir");
    copy_config(temp.path());
    copy_fixture(temp.path(), "v8.mcvx");

    let first = run_demo(temp.path(), "v8.mcvx");
    assert!(first.status.success(), "{}", report(&first));
    let first_v10 = std::fs::read(temp.path().join("v8.v10.mcvx")).expect("read first v10");
    assert_v10_header(&first_v10);

    // Second leg: reload the v10 output under the same effective config.
    let second = run_demo(temp.path(), "v8.v10.mcvx");
    assert!(second.status.success(), "{}", report(&second));
    let lines = stdout_lines(&second);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("save_v10: leg=load version=10 packs=0 saved_config=some notice=none playable=true"),
        "{}",
        report(&second)
    );
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("save_v10: leg=match notice=none playable=true"),
        "{}",
        report(&second)
    );

    let second_v10 =
        std::fs::read(temp.path().join("v8.v10.v10.mcvx")).expect("read second v10 output");
    assert_eq!(
        second_v10, first_v10,
        "a v10-to-v10 resave under the same config must be byte-stable"
    );

    // The first v10 output was the input and must not be overwritten.
    let after_first = std::fs::read(temp.path().join("v8.v10.mcvx")).expect("reread first v10");
    assert_eq!(
        after_first, first_v10,
        "the input v10 save must stay byte-identical"
    );
}

#[test]
fn changed_config_moves_only_the_config_section_digest() {
    let temp = tempfile::tempdir().expect("temp dir");
    let config = copy_config(temp.path());
    copy_fixture(temp.path(), "v8.mcvx");

    // First leg: v8 -> v10 under the default config.
    let first = run_demo(temp.path(), "v8.mcvx");
    assert!(first.status.success(), "{}", report(&first));
    let first_v10 = std::fs::read(temp.path().join("v8.v10.mcvx")).expect("read first v10");
    assert_v10_header(&first_v10);

    // Second leg: reload the v10 output under a changed tuning value.
    replace_once(
        &config.join("player.toml"),
        "walk_speed = 4",
        "walk_speed = 6.0",
    );

    let output = run_demo(temp.path(), "v8.v10.mcvx");
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines.first().map(String::as_str),
        Some(
            "save_v10: leg=load version=10 packs=0 saved_config=some notice=config changed since this world was saved playable=true"
        ),
        "exactly one non-blocking notice on load\n{}",
        report(&output)
    );
    let resave = lines.get(1).expect("resave line").as_str();
    let parsed = parse_resave(resave);
    assert_ne!(
        parsed.digest, DEFAULTS_DIGEST,
        "the changed config must move the digest"
    );
    assert_eq!(
        parsed.bytes,
        first_v10.len(),
        "the config section is fixed-size, so the resave keeps the archive length: {resave}"
    );
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("save_v10: leg=match notice=none playable=true"),
        "the matching-config reload is silent\n{}",
        report(&output)
    );
    assert_eq!(
        lines.get(3).map(String::as_str),
        Some("save_v10: ok"),
        "{}",
        report(&output)
    );

    // The first v10 output was the input and must not be overwritten.
    let after_first = std::fs::read(temp.path().join("v8.v10.mcvx")).expect("reread first v10");
    assert_eq!(
        after_first, first_v10,
        "the input v10 save must stay byte-identical"
    );

    // The new v10 output carries the changed config section. The demo line's
    // digest must be the one actually written to section 7.
    let second_v10 =
        std::fs::read(temp.path().join("v8.v10.v10.mcvx")).expect("read second v10 output");
    assert_eq!(second_v10.len(), first_v10.len());
    assert_v10_header(&second_v10);
    assert_eq!(
        config_section(&second_v10),
        (DEFAULTS_SCHEMA, parsed.digest),
        "the config section carries the changed effective-value digest"
    );
}

#[test]
fn missing_input_fails_closed() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_demo(temp.path(), "absent.mcvx");
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines,
        vec!["save_v10: failed reason=missing-input".to_string()],
        "{}",
        report(&output)
    );
    assert!(!temp.path().join("absent.mcvx").exists());
    assert!(!temp.path().join("absent.v10.mcvx").exists());
}

#[test]
fn corrupted_archives_fail_closed_before_writing_output() {
    let temp = tempfile::tempdir().expect("temp dir");
    copy_config(temp.path());
    copy_fixture(temp.path(), "v8.mcvx");

    // Produce a real v10 archive first, then corrupt copies of it.
    let output = run_demo(temp.path(), "v8.mcvx");
    assert!(output.status.success(), "{}", report(&output));
    let valid = std::fs::read(temp.path().join("v8.v10.mcvx")).expect("read valid v10");
    assert_v10_header(&valid);

    // Nonzero archive flags must fail the neutral archive parser.
    let mut bad_flags = valid.clone();
    bad_flags[8..12].copy_from_slice(&1u32.to_le_bytes());
    std::fs::write(temp.path().join("bad-flags.mcvx"), &bad_flags).expect("write bad flags");

    // A truncated archive must fail before any section is trusted.
    let truncated = &valid[..valid.len() - 1];
    std::fs::write(temp.path().join("truncated.mcvx"), truncated).expect("write truncated");

    for (input, reason) in [
        ("bad-flags.mcvx", "save_v10: failed reason=load"),
        ("truncated.mcvx", "save_v10: failed reason=load"),
    ] {
        let output = run_demo(temp.path(), input);
        assert_eq!(output.status.code(), Some(1), "{}", report(&output));
        assert_eq!(
            stdout_lines(&output),
            vec![reason.to_string()],
            "{}",
            report(&output)
        );
        let derived = Path::new(input).with_extension("v10.mcvx");
        assert!(
            !temp.path().join(derived).exists(),
            "a failed load must not write {input}'s derived output"
        );
    }

    // The convention is still intact after the failures.
    assert_eq!(
        std::fs::read(temp.path().join("v8.v10.mcvx")).expect("reread valid v10"),
        valid,
        "a failed proof must never touch another archive"
    );
}
