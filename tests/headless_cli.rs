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

/// Stage one pack folder under `dir/packs/<name>` with the given manifest
/// bytes, exactly as a creator who drops a downloaded pack into `packs/`.
fn stage_pack(dir: &Path, name: &str, manifest: &[u8]) -> PathBuf {
    let pack = dir.join("packs").join(name);
    std::fs::create_dir_all(&pack)
        .unwrap_or_else(|error| panic!("create {}: {error}", pack.display()));
    std::fs::write(pack.join("pack.toml"), manifest)
        .unwrap_or_else(|error| panic!("write {}: {error}", pack.display()));
    pack
}

/// Standard PNG CRC-32 (IEEE 802.3) over the chunk type plus data.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// A tiny PNG whose IHDR declares `width`x`height` and whose IDAT is empty.
///
/// The empty IDAT is load-bearing: the decoder stops at the first IDAT, so the
/// declared dimensions reach the pack guards instead of failing earlier with
/// `MissingImageData`. This duplicates the library's `#[cfg(test)]` helper
/// because an integration crate cannot see that item.
fn png_bomb_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &[]));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
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

#[test]
fn validate_pack_warns_on_hostile_inputs_without_panicking() {
    let temp = tempfile::tempdir().expect("temp dir");
    let hostile = stage_pack(temp.path(), "hostile", b"name = \"Hostile\"\n");
    std::fs::write(hostile.join("stone.png"), png_bomb_bytes(30000, 30000))
        .expect("write stone.png");
    std::fs::write(hostile.join("dirt.png"), png_bomb_bytes(512, 512)).expect("write dirt.png");
    std::fs::write(hostile.join("sand.png"), vec![b'x'; 1_048_577]).expect("write sand.png");

    let output = run_in(temp.path(), &["--validate-pack", "hostile"]);
    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    let lines = stdout_lines(&output);
    let warnings: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("warning: "))
        .collect();
    assert_eq!(
        warnings.len(),
        3,
        "one warning per hostile input: {}",
        report(&output)
    );
    for name in ["stone.png", "dirt.png", "sand.png"] {
        assert!(
            warnings.iter().any(|line| line.contains(name)),
            "{name} is named in its warning: {}",
            report(&output)
        );
    }
    let joined = lines.join("\n");
    assert!(
        joined.contains("30000x30000"),
        "the IHDR dimensions are reported: {}",
        report(&output)
    );
    assert!(
        joined.contains("1048576"),
        "the byte cap is reported: {}",
        report(&output)
    );
    assert_eq!(
        last_line(&lines),
        "validate-pack: hostile: 0 entries, 0 errors, 3 warnings",
        "{}",
        report(&output)
    );
    assert!(
        !joined.contains("panicked")
            && !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "a hostile pack must never panic: {}",
        report(&output)
    );

    stage_pack(temp.path(), "huge", &vec![b' '; 1_048_577]);
    let output = run_in(temp.path(), &["--validate-pack", "huge"]);
    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    let lines = stdout_lines(&output);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("warning: pack.toml:")),
        "the oversized manifest falls back with a warning: {}",
        report(&output)
    );
    assert_eq!(
        last_line(&lines),
        "validate-pack: huge: 0 entries, 0 errors, 1 warnings",
        "{}",
        report(&output)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("panicked")
            && !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "an oversized manifest must never panic: {}",
        report(&output)
    );
}

#[test]
fn validate_pack_fails_on_a_truncated_v2_asset() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pack = stage_pack(
        temp.path(),
        "broken",
        br#"format_version = 2

[[blocks]]
id = "broken:truncated"
textures = { all = "broken.png" }
hardness = 1.0
"#,
    );
    let stone =
        std::fs::read(Path::new(MANIFEST_DIR).join("tests/fixtures/packs/ruby-overlay/stone.png"))
            .expect("read the reference stone.png");
    assert!(stone.len() > 2, "the reference stone.png is a real PNG");
    std::fs::write(pack.join("broken.png"), &stone[..stone.len() / 2])
        .expect("write the truncated asset");

    let output = run_in(temp.path(), &["--validate-pack", "broken"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a broken asset fails validation (a panic would exit 101): {}",
        report(&output)
    );
    let lines = stdout_lines(&output);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("error: asset broken.png")),
        "the broken asset is named as an error: {}",
        report(&output)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("panicked")
            && !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "a broken asset must never panic: {}",
        report(&output)
    );
}

#[test]
fn validate_pack_reports_the_overlay_fixture_clean() {
    let temp = tempfile::tempdir().expect("temp dir");
    copy_dir_all(
        &Path::new(MANIFEST_DIR).join("tests/fixtures/packs"),
        &temp.path().join("packs"),
    );

    let output = run_in(temp.path(), &["--validate-pack", "ruby-overlay"]);
    assert_eq!(output.status.code(), Some(0), "{}", report(&output));
    assert_eq!(
        last_line(&stdout_lines(&output)),
        "validate-pack: ruby-overlay: 0 entries, 0 errors, 0 warnings",
        "{}",
        report(&output)
    );
}
