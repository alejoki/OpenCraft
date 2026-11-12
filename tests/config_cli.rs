//! Real-binary integration proof for `opencraft --check-config` (M014/S01)
//! and its release rejection (M014/S09).
//!
//! These tests spawn the actual compiled binary in isolated temporary working
//! directories, so the CLI contract is pinned at the process boundary: exit
//! codes, exact stdout lines, logical `config/<file>` paths independent of the
//! process CWD, and the absence of startup side effects. Debug builds prove
//! the audit itself; release builds prove the dev-only gate prints exactly one
//! pinned line and exits 1 before any startup state exists. Loader/renderer
//! unit tests live in `src/config/mod.rs`; this file covers the real launch
//! path.

use std::path::Path;
use std::process::{Command, Output};

#[cfg(debug_assertions)]
use std::path::PathBuf;

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in `config/` tree.
#[cfg(debug_assertions)]
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The six curated files in registry/report order with their value counts.
#[cfg(debug_assertions)]
const CONFIG_FILES: [(&str, usize); 6] = [
    ("player.toml", 16),
    ("survival.toml", 15),
    ("combat.toml", 21),
    ("world.toml", 19),
    ("mining.toml", 5),
    ("render.toml", 10),
];

#[cfg(debug_assertions)]
const SUMMARY_PREFIX: &str =
    "config: files=6 missing=0 warnings=0 errors=0 keys=86 schema=1 digest=0x";
#[cfg(debug_assertions)]
const MISSING_SUMMARY_PREFIX: &str =
    "config: files=6 missing=6 warnings=6 errors=0 keys=86 schema=1 digest=0x";

/// Copy the six checked-in config files byte-for-byte into `dest/config`.
#[cfg(debug_assertions)]
fn copy_config(dest: &Path) -> PathBuf {
    let source = Path::new(MANIFEST_DIR).join("config");
    let config = dest.join("config");
    std::fs::create_dir_all(&config).expect("create dest/config");
    for (name, _) in CONFIG_FILES {
        let from = source.join(name);
        std::fs::copy(&from, config.join(name))
            .unwrap_or_else(|error| panic!("copy {}: {error}", from.display()));
    }
    config
}

/// Run the real binary with `--check-config` from `dir` as the process CWD.
#[cfg(debug_assertions)]
fn run_check(dir: &Path) -> Output {
    Command::new(BIN)
        .arg("--check-config")
        .current_dir(dir)
        .output()
        .expect("spawn opencraft --check-config")
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

#[cfg(debug_assertions)]
fn last_line(lines: &[String]) -> &str {
    lines.last().expect("report must end with a summary line")
}

/// Assert the summary starts with `expected_prefix` and ends with a lowercase
/// 16-hex-digit FNV-1a digest.
#[cfg(debug_assertions)]
fn assert_summary(lines: &[String], expected_prefix: &str) {
    let summary = last_line(lines);
    let digest = summary
        .strip_prefix(expected_prefix)
        .unwrap_or_else(|| panic!("summary must start with {expected_prefix:?}, got {summary:?}"));
    assert_eq!(
        digest.len(),
        16,
        "digest {digest:?} must be 16 hex digits: {summary:?}"
    );
    assert!(
        digest
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)),
        "digest {digest:?} must be lowercase hex"
    );
}

/// Replace exactly one occurrence, proving the fixture edit actually landed.
#[cfg(debug_assertions)]
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

#[cfg(debug_assertions)]
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

#[cfg(debug_assertions)]
#[test]
fn valid_checked_in_config_exits_zero() {
    let temp = tempfile::tempdir().expect("temp dir");
    copy_config(temp.path());

    let output = run_check(temp.path());
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    let ok_lines: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("ok: "))
        .collect();
    assert_eq!(ok_lines.len(), 6, "{}", report(&output));

    for (name, count) in CONFIG_FILES {
        let expected = format!("ok: config/{name}: {count} values");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.as_str() == expected)
                .count(),
            1,
            "expected exactly one {expected:?}\n{}",
            report(&output)
        );
    }

    assert!(
        !lines.iter().any(|line| line.starts_with("error: ")),
        "valid tree must not report errors\n{}",
        report(&output)
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("warning: ")),
        "valid tree must not report warnings\n{}",
        report(&output)
    );

    // Summary must be the seventh and final line for a clean tree.
    assert_eq!(lines.len(), 7, "{}", report(&output));
    assert_summary(&lines, SUMMARY_PREFIX);
}

#[cfg(debug_assertions)]
#[test]
fn invalid_value_exits_nonzero_with_stable_line() {
    let temp = tempfile::tempdir().expect("temp dir");
    let config = copy_config(temp.path());
    replace_once(
        &config.join("player.toml"),
        "walk_speed = 4",
        "walk_speed = 0",
    );

    let output = run_check(temp.path());
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));

    let lines = stdout_lines(&output);
    let expected = "error: config/player.toml: movement.walk_speed: value 0 is below minimum 0.5";
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == expected)
            .count(),
        1,
        "expected exactly one {expected:?}\n{}",
        report(&output)
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("ok: config/player.toml")),
        "a file with an error must not print an ok line\n{}",
        report(&output)
    );
}

#[cfg(debug_assertions)]
#[test]
fn cross_field_exits_nonzero_and_names_both_keys() {
    let temp = tempfile::tempdir().expect("temp dir");
    let config = copy_config(temp.path());
    replace_once(
        &config.join("world.toml"),
        "spawn_max_radius_blocks = 48",
        "spawn_max_radius_blocks = 10",
    );

    let output = run_check(temp.path());
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));

    let lines = stdout_lines(&output);
    let error = lines
        .iter()
        .find(|line| {
            line.starts_with("error: config/world.toml: spawning.spawn_max_radius_blocks:")
        })
        .unwrap_or_else(|| panic!("cross-field error line missing\n{}", report(&output)));
    assert!(
        error.contains("spawning.spawn_max_radius_blocks"),
        "{error}"
    );
    assert!(error.contains("spawning.safe_radius_blocks"), "{error}");
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("ok: config/world.toml")),
        "a whole-file cross-field rejection must not print an ok line\n{}",
        report(&output)
    );
}

#[cfg(debug_assertions)]
#[test]
fn missing_config_dir_warns_and_exits_zero() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_check(temp.path());
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    for (name, _) in CONFIG_FILES {
        let expected = format!("warning: config/{name}: file is missing; using defaults");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.as_str() == expected)
                .count(),
            1,
            "expected exactly one {expected:?}\n{}",
            report(&output)
        );
    }
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.ends_with("file is missing; using defaults"))
            .count(),
        6,
        "{}",
        report(&output)
    );
    assert_summary(&lines, MISSING_SUMMARY_PREFIX);

    assert!(
        !temp.path().join("settings.toml").exists(),
        "a missing config tree must not create settings.toml"
    );
    assert!(
        !temp.path().join("saves").exists(),
        "a missing config tree must not create saves/"
    );
}

#[cfg(debug_assertions)]
#[test]
fn extra_toml_warns_and_exits_zero() {
    let temp = tempfile::tempdir().expect("temp dir");
    let config = copy_config(temp.path());
    std::fs::write(config.join("plyer.toml"), "[movement]\n").expect("write extra toml");

    let output = run_check(temp.path());
    assert!(output.status.success(), "{}", report(&output));

    let lines = stdout_lines(&output);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "warning: config/plyer.toml: not a config file")
            .count(),
        1,
        "{}",
        report(&output)
    );
    let summary = last_line(&lines);
    assert!(
        summary.contains("missing=0 warnings=1 errors=0"),
        "{summary}"
    );
    assert_summary(
        &lines,
        "config: files=6 missing=0 warnings=1 errors=0 keys=86 schema=1 digest=0x",
    );
}

#[cfg(debug_assertions)]
#[test]
fn check_config_creates_no_settings_pack_or_save_state() {
    let temp = tempfile::tempdir().expect("temp dir");
    let config = copy_config(temp.path());
    let source = Path::new(MANIFEST_DIR).join("config");

    let original: Vec<Vec<u8>> = CONFIG_FILES
        .iter()
        .map(|(name, _)| {
            std::fs::read(source.join(name))
                .unwrap_or_else(|error| panic!("read source config/{name}: {error}"))
        })
        .collect();
    let before_root = sorted_entries(temp.path());
    let before_config = sorted_entries(&config);

    let output = run_check(temp.path());
    assert!(output.status.success(), "{}", report(&output));

    assert_eq!(
        sorted_entries(temp.path()),
        before_root,
        "the audit must not create startup state (settings.toml/packs/saves)"
    );
    assert_eq!(
        sorted_entries(&config),
        before_config,
        "the audit must not create or remove files in config/"
    );
    for ((name, _), before) in CONFIG_FILES.iter().zip(original) {
        let after = std::fs::read(config.join(name))
            .unwrap_or_else(|error| panic!("read copied config/{name}: {error}"));
        assert_eq!(after, before, "config/{name} was rewritten by the audit");
    }

    assert!(!temp.path().join("settings.toml").exists());
    assert!(!temp.path().join("saves").exists());
    assert!(!temp.path().join("packs").exists());
}

// ---------------------------------------------------------------------------
// Release profile: the dev-only creator switches fail closed before startup.
// ---------------------------------------------------------------------------

/// Run the real binary from `dir` as the process CWD.
#[cfg(not(debug_assertions))]
fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn opencraft {args:?}: {error}"))
}

/// A rejected creator switch must not create any startup state.
#[cfg(not(debug_assertions))]
fn assert_no_startup_state(dir: &Path) {
    for entry in ["settings.toml", "saves", "packs", "config"] {
        assert!(
            !dir.join(entry).exists(),
            "release rejection must not create {entry}"
        );
    }
}

#[cfg(not(debug_assertions))]
#[test]
fn release_check_config_rejected_with_one_pinned_line() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_in(temp.path(), &["--check-config"]);
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["check-config: dev build only".to_string()],
        "{}",
        report(&output)
    );
    assert_no_startup_state(temp.path());
}

#[cfg(not(debug_assertions))]
#[test]
fn release_creator_trace_rejected_with_one_pinned_line() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_in(temp.path(), &["--creator-trace"]);
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["creator-trace: dev build only".to_string()],
        "{}",
        report(&output)
    );
    assert_no_startup_state(temp.path());
}

#[cfg(not(debug_assertions))]
#[test]
fn release_check_config_line_wins_when_both_flags_are_present() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_in(temp.path(), &["--creator-trace", "--check-config"]);
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["check-config: dev build only".to_string()],
        "check-config must win precedence: {}",
        report(&output)
    );
    assert_no_startup_state(temp.path());
}
