//! Real-binary integration proof for `opencraft --demo-restart` (M014/S09-T02).
//!
//! These tests spawn the actual compiled binary in isolated temporary working
//! directories, so the same-save recipe restart boundary is pinned at the
//! process boundary: leg one saves a fixture world with the checked-in
//! `mymod-ruby` pack's stew recipe count at 1, the test edits only that
//! recipe block to count 2, and leg two loads the same save in a fresh process,
//! previews/crafts the new count, and reports the existing one-time content
//! notice. The demo is release-capable, so this file passes under
//! `cargo test --release` too. Negative cases pin the missing-recipe,
//! malformed-save, and save-path-required failure paths and prove none writes
//! a save.

use std::path::Path;
use std::process::{Command, Output};

/// The binary Cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_opencraft");
/// The checkout root containing the checked-in fixture pack.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The save line leg one must print.
const SAVE_LINE: &str =
    "restart: leg=save recipe=mymod:ruby_stew count=1 day=3 tick=6000 saved=true";
/// The load line leg two must print after the recipe count edit.
const LOAD_LINE: &str = "restart: leg=load recipe=mymod:ruby_stew count=2 preview=2 crafted=2 unresolved=0 day=3 tick=6000 notice=content definitions changed playable=true";
/// The checked-in stew recipe block, matched exactly so the `[[items]]` entry
/// with the same id is never touched.
const STEW_RECIPE_BLOCK: &str = "[[recipes]]\nid = \"mymod:ruby_stew\"\npattern = [\"RR\", \"WW\"]\nkey = { R = \"mymod:ruby\", W = \"wheat\" }\noutput = \"mymod:ruby_stew\"\ncount = 1\n";
/// The same block with only the crafted count edited.
const STEW_RECIPE_BLOCK_EDITED: &str = "[[recipes]]\nid = \"mymod:ruby_stew\"\npattern = [\"RR\", \"WW\"]\nkey = { R = \"mymod:ruby\", W = \"wheat\" }\noutput = \"mymod:ruby_stew\"\ncount = 2\n";

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

/// Install the checked-in `mymod-ruby` pack plus a settings stack that selects
/// it, exactly as a creator would after dropping the pack into `packs/`.
fn install_ruby_pack(dir: &Path) {
    let source = Path::new(MANIFEST_DIR).join("tests/fixtures/packs/mymod-ruby");
    copy_dir_all(&source, &dir.join("packs/mymod-ruby"));
    std::fs::write(
        dir.join("settings.toml"),
        "[packs]\nstack = [\"mymod-ruby\"]\n",
    )
    .expect("write settings.toml");
}

/// Run the real binary with `args` from `dir`.
fn run_demo(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn opencraft --demo-restart")
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

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Edit ONLY the `[[recipes]] mymod:ruby_stew` block's count 1 -> 2.
fn edit_stew_recipe_count(pack_toml: &Path) {
    let contents = std::fs::read_to_string(pack_toml)
        .unwrap_or_else(|error| panic!("read {}: {error}", pack_toml.display()));
    assert_eq!(
        contents.matches(STEW_RECIPE_BLOCK).count(),
        1,
        "the exact stew recipe block must appear once in {}",
        pack_toml.display()
    );
    assert_eq!(
        contents.matches("count = 1").count(),
        2,
        "the fixture has two count=1 recipes before the edit"
    );
    let edited = contents.replacen(STEW_RECIPE_BLOCK, STEW_RECIPE_BLOCK_EDITED, 1);
    assert_eq!(
        edited.matches(STEW_RECIPE_BLOCK_EDITED).count(),
        1,
        "the stew recipe block must carry the edited count"
    );
    assert_eq!(
        edited.matches("count = 1").count(),
        1,
        "the pickaxe recipe must keep its untouched count"
    );
    std::fs::write(pack_toml, edited)
        .unwrap_or_else(|error| panic!("write {}: {error}", pack_toml.display()));
}

#[test]
fn two_runs_load_the_edited_recipe_on_the_same_save() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_ruby_pack(temp.path());
    let pack_toml = temp.path().join("packs/mymod-ruby/pack.toml");

    // Leg one: fresh world, pinned clock, saved through the production site.
    let first = run_demo(
        temp.path(),
        &["--demo-restart", "--new", "--save", "recipe.mcvx"],
    );
    assert!(first.status.success(), "{}", report(&first));
    assert_eq!(
        stdout_lines(&first),
        vec![SAVE_LINE.to_string(), "restart: ok".to_string()],
        "{}",
        report(&first)
    );
    assert!(
        temp.path().join("recipe.mcvx").is_file(),
        "leg one must write the explicit --save fixture"
    );

    // The creator's edit: only the stew recipe count moves.
    edit_stew_recipe_count(&pack_toml);

    // Leg two: fresh process, same save, edited registry.
    let second = run_demo(temp.path(), &["--demo-restart", "--save", "recipe.mcvx"]);
    assert!(second.status.success(), "{}", report(&second));
    assert_eq!(
        stdout_lines(&second),
        vec![LOAD_LINE.to_string(), "restart: ok".to_string()],
        "{}",
        report(&second)
    );

    // The proof writes only the settings file, the copied pack, and the
    // explicit fixture save: no default saves directory or extra state.
    assert!(!temp.path().join("saves").exists());
}

#[test]
fn missing_recipe_fails_closed_without_writing_a_save() {
    let temp = tempfile::tempdir().expect("temp dir");
    let pack = temp.path().join("packs/minimal");
    std::fs::create_dir_all(&pack).expect("create minimal pack");
    std::fs::write(
        pack.join("pack.toml"),
        "format_version = 2\nname = \"Minimal\"\n",
    )
    .expect("write minimal pack");
    std::fs::write(
        temp.path().join("settings.toml"),
        "[packs]\nstack = [\"minimal\"]\n",
    )
    .expect("write settings.toml");

    let output = run_demo(
        temp.path(),
        &["--demo-restart", "--new", "--save", "recipe.mcvx"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["restart: failed reason=missing-recipe".to_string()],
        "{}",
        report(&output)
    );
    assert!(
        !temp.path().join("recipe.mcvx").exists(),
        "a missing recipe must fail before any save"
    );
}

#[test]
fn malformed_save_fails_closed_on_load() {
    let temp = tempfile::tempdir().expect("temp dir");
    install_ruby_pack(temp.path());
    std::fs::write(temp.path().join("broken.mcvx"), b"not a save").expect("write malformed save");

    let output = run_demo(temp.path(), &["--demo-restart", "--save", "broken.mcvx"]);
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["restart: failed reason=load".to_string()],
        "{}",
        report(&output)
    );
    assert!(!temp.path().join("saves").exists());
}

#[test]
fn save_path_required_fails_closed_without_creating_saves() {
    let temp = tempfile::tempdir().expect("temp dir");

    let output = run_demo(temp.path(), &["--demo-restart", "--new"]);
    assert_eq!(output.status.code(), Some(1), "{}", report(&output));
    assert_eq!(
        stdout_lines(&output),
        vec!["restart: failed reason=save-path-required".to_string()],
        "{}",
        report(&output)
    );
    assert!(
        !temp.path().join("saves").exists(),
        "the default save path must be refused before any write"
    );
}
