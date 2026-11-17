//! Headless `--validate-pack` support.
//!
//! This module is deliberately pure: it reads pack folders but never touches a
//! world, save, settings file, window, GPU or audio device. The binary's
//! `main` prints the returned lines and exits non-zero when any authored entry
//! was rejected, so validation is the loud counterpart to the game's
//! never-fail loader.
//!
//! The deterministic renderer lives in `opencraft_content::validate`; this
//! module keeps the folder guards, pack load and registry build (the only
//! game-side seam).

use std::path::Path;

use opencraft_content::validate::{render_lines, summary};

use crate::content::{build_registry, ContentRegistry, PackIdentity};
use crate::pack::spec::load_spec_from_dir;

/// Validate one pack folder under `packs_root` and render stable output lines.
///
/// Returns `(lines, ok)` where `ok` is true only when no error diagnostics were
/// found. Warning-level problems (manifest fallbacks, corrupt texture PNGs,
/// v1 packs that declare content arrays) never fail validation; rejected
/// entries and missing packs do.
///
/// A missing or non-directory `packs_root/name` is always an error. An empty
/// name is treated the same way so a `--validate-pack` typo fails closed
/// instead of validating the `packs/` root as a pack.
pub fn validate_pack_lines(packs_root: &Path, name: &str) -> (Vec<String>, bool) {
    if name.is_empty() {
        return (
            vec![
                "error: pack : no pack name given".to_string(),
                summary(name, 0, 1, 0),
            ],
            false,
        );
    }

    let dir = packs_root.join(name);
    if !dir.is_dir() {
        return (
            vec![
                format!("error: pack {name}: folder {} is missing", dir.display()),
                summary(name, 0, 1, 0),
            ],
            false,
        );
    }

    let spec = load_spec_from_dir(&dir);
    let identity = PackIdentity {
        folder: spec.folder.clone(),
        version: spec.version.clone(),
    };
    let (_registry, report) = build_registry(
        ContentRegistry::builtin(),
        &[(identity, spec.content.clone())],
    );

    // Parser diagnostics live beside the accepted content, so merge them with
    // the registry report before handing the whole set to the renderer.
    let mut diagnostics = spec.diagnostics.clone();
    diagnostics.extend(report.diagnostics.iter().cloned());

    render_lines(name, &spec.content, &diagnostics, &spec.warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    (x.wrapping_mul(7).wrapping_add(3)) as u8,
                    (y.wrapping_mul(11).wrapping_add(5)) as u8,
                    ((x + y).wrapping_mul(3)) as u8,
                    255,
                ]);
            }
        }
        data
    }

    fn write_png(path: &Path, dim: u32) {
        let file = fs::File::create(path).expect("create fixture png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), dim, dim);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        writer
            .write_image_data(&gradient(dim, dim))
            .expect("write png image data");
    }

    fn pack_dir(root: &tempfile::TempDir, name: &str) -> PathBuf {
        let dir = root.path().join("packs").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_manifest(dir: &Path, text: &str) {
        fs::write(dir.join("pack.toml"), text).unwrap();
    }

    fn lines_joined(lines: &[String]) -> String {
        lines.join("\n")
    }

    #[test]
    fn clean_v2_pack_reports_ok_entries_and_summary() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "ruby");
        write_manifest(
            &dir,
            r#"
format_version = 2
name = "Ruby Mod"

[[blocks]]
id = "mymod:ruby_ore"
textures = { all = "ruby_ore.png" }
hardness = 3.0
drop = "mymod:ruby"

[[items]]
id = "mymod:ruby"
icon = "ruby.png"
stack = 64
"#,
        );
        write_png(&dir.join("ruby_ore.png"), 16);
        write_png(&dir.join("ruby.png"), 16);

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "ruby");
        let text = lines_joined(&lines);
        assert!(ok, "clean pack must validate: {text}");
        assert!(text.contains("ok: block mymod:ruby_ore"), "{text}");
        assert!(text.contains("ok: item mymod:ruby"), "{text}");
        assert!(
            text.contains("validate-pack: ruby: 2 entries, 0 errors, 0 warnings"),
            "{text}"
        );
        assert!(!text.contains("error:"), "{text}");
        // The pack name identifies entries; no absolute tempdir path leaks.
        assert!(
            !text.contains(&root.path().display().to_string()),
            "output must not contain the pack path: {text}"
        );
    }

    #[test]
    fn rejected_entry_fails_but_valid_sibling_still_registers() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "broken");
        write_manifest(
            &dir,
            r#"
format_version = 2

[[blocks]]
id = "broken:missing_art"
textures = { all = "missing.png" }
hardness = 1.0

[[items]]
id = "broken:ok_item"
icon = "ok.png"
stack = 16
"#,
        );
        write_png(&dir.join("ok.png"), 16);

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "broken");
        let text = lines_joined(&lines);
        assert!(!ok, "a missing asset must fail validation: {text}");
        assert!(
            text.contains("error: block broken:missing_art:"),
            "the rejected entry must be named: {text}"
        );
        assert!(
            text.contains("ok: item broken:ok_item"),
            "the valid sibling must still register: {text}"
        );
        assert!(!text.contains("ok: block broken:missing_art"), "{text}");
        assert!(text.contains("validate-pack: broken:"), "{text}");
    }

    #[test]
    fn missing_folder_is_an_error_with_a_stable_line() {
        let root = tempfile::tempdir().unwrap();
        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "ghost");
        assert!(!ok);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("error: pack ghost: folder "),
            "{lines:?}"
        );
        assert!(lines[0].contains("is missing"), "{lines:?}");
        assert_eq!(
            lines[1],
            "validate-pack: ghost: 0 entries, 1 errors, 0 warnings"
        );
    }

    #[test]
    fn empty_name_fails_closed_instead_of_validating_the_root() {
        let root = tempfile::tempdir().unwrap();
        let packs = root.path().join("packs");
        fs::create_dir_all(&packs).unwrap();
        let (lines, ok) = validate_pack_lines(&packs, "");
        assert!(!ok);
        assert!(lines[0].contains("no pack name given"), "{lines:?}");
    }

    #[test]
    fn v1_manifest_only_pack_validates_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "classic");
        write_manifest(
            &dir,
            "name = \"Classic\"\n[menu]\noverlay_tint = [1, 2, 3, 4]\n",
        );

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "classic");
        let text = lines_joined(&lines);
        assert!(ok, "v1 packs keep validating: {text}");
        assert!(
            text.contains("validate-pack: classic: 0 entries, 0 errors, 0 warnings"),
            "{text}"
        );
    }

    #[test]
    fn v1_pack_with_content_array_warns_but_still_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "legacy");
        write_manifest(
            &dir,
            r#"
name = "Legacy"

[[blocks]]
id = "legacy:ore"
textures = { all = "ore.png" }
hardness = 1.0
"#,
        );

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "legacy");
        let text = lines_joined(&lines);
        assert!(ok, "a warning must not fail validation: {text}");
        assert!(
            text.contains("warning: pack.toml: content arrays require format_version = 2"),
            "{text}"
        );
        assert!(
            text.contains("validate-pack: legacy: 0 entries, 0 errors, 1 warnings"),
            "{text}"
        );
    }

    #[test]
    fn output_is_deterministic_across_runs() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "sorted");
        write_manifest(
            &dir,
            r#"
format_version = 2

[[items]]
id = "sorted:z_item"
icon = "z.png"
stack = 1

[[items]]
id = "sorted:a_item"
icon = "a.png"
stack = 1

[[blocks]]
id = "sorted:block"
textures = { all = "block.png" }
hardness = 2.0
"#,
        );
        write_png(&dir.join("a.png"), 16);
        write_png(&dir.join("z.png"), 16);
        write_png(&dir.join("block.png"), 16);

        let packs = root.path().join("packs");
        let first = validate_pack_lines(&packs, "sorted");
        let second = validate_pack_lines(&packs, "sorted");
        assert_eq!(first, second);
        assert!(first.1);

        let text = lines_joined(&first.0);
        let block = text.find("ok: block sorted:block").unwrap();
        let a_item = text.find("ok: item sorted:a_item").unwrap();
        let z_item = text.find("ok: item sorted:z_item").unwrap();
        assert!(block < a_item && a_item < z_item, "sorted output: {text}");
    }

    #[test]
    fn builtin_icon_entry_prints_ok_and_counts_in_summary() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "icons");
        write_manifest(
            &dir,
            r#"
format_version = 2
name = "Icons"

[builtin_icons]
apple = "apple.png"
wood_pickaxe = "pickaxe.png"
"#,
        );
        write_png(&dir.join("apple.png"), 16);
        write_png(&dir.join("pickaxe.png"), 16);

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "icons");
        let text = lines_joined(&lines);
        assert!(ok, "valid overrides must validate: {text}");
        assert!(text.contains("ok: builtin_icon apple"), "{text}");
        assert!(text.contains("ok: builtin_icon wood_pickaxe"), "{text}");
        assert!(
            text.contains("validate-pack: icons: 2 entries, 0 errors, 0 warnings"),
            "the accepted override count is included: {text}"
        );
    }

    #[test]
    fn builtin_icon_with_missing_asset_reports_error_and_warning() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "badicon");
        write_manifest(
            &dir,
            r#"
format_version = 2

[builtin_icons]
apple = "missing.png"
"#,
        );

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "badicon");
        let text = lines_joined(&lines);
        assert!(!ok, "a missing override file is an error: {text}");
        assert!(text.contains("error: asset missing.png:"), "{text}");
        assert!(
            text.contains("warning: builtin_icon apple: icon `missing.png` is not a loaded asset"),
            "the built-in fallback is explained: {text}"
        );
        assert!(
            text.contains("ok: builtin_icon apple"),
            "the override entry itself was accepted: {text}"
        );
        assert!(
            text.contains("validate-pack: badicon: 1 entries, 1 errors, 1 warnings"),
            "{text}"
        );
    }

    #[test]
    fn oversized_manifest_warns_but_exits_ok() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "huge");
        fs::write(
            dir.join("pack.toml"),
            vec![b' '; crate::pack::asset::PACK_ASSET_BYTE_CAP as usize + 1],
        )
        .unwrap();

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "huge");
        let text = lines_joined(&lines);
        assert!(ok, "an oversized manifest must not fail validation: {text}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("warning: pack.toml:")),
            "the manifest fallback is explained: {text}"
        );
        assert!(!text.contains("error:"), "{text}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("validate-pack: huge: 0 entries, 0 errors, 1 warnings"),
            "{text}"
        );
    }

    #[test]
    fn truncated_asset_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root, "broken");
        write_manifest(
            &dir,
            r#"
format_version = 2

[[blocks]]
id = "broken:truncated"
textures = { all = "broken.png" }
hardness = 1.0
"#,
        );
        write_png(&dir.join("broken.png"), 16);
        let bytes = fs::read(dir.join("broken.png")).unwrap();
        fs::write(dir.join("broken.png"), &bytes[..bytes.len() / 2]).unwrap();

        let (lines, ok) = validate_pack_lines(&root.path().join("packs"), "broken");
        let text = lines_joined(&lines);
        assert!(!ok, "a truncated asset must fail validation: {text}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("error: asset broken.png")),
            "the bad asset is named: {text}"
        );
        assert!(
            !text.contains("ok: block broken:truncated"),
            "the rejected entry must not also claim success: {text}"
        );
    }
}
