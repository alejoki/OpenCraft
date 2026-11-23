//! Deterministic `--validate-pack` line rendering.
//!
//! The folder guards, manifest load and registry build stay game-side; this
//! module turns an accepted [`PackContentSpec`], the diagnostics collected for
//! it and its manifest warnings into the stable line set the CLI prints.
//!
//! Output is deterministic across runs: entries are emitted in sorted order and
//! identical diagnostic lines are printed once.

use std::collections::BTreeSet;

use crate::spec::{
    face_direction, BlockAlpha, BlockModel, ContentDiagnostic, FaceSource, PackContentSpec,
    Severity, WorldgenEntry, WorldgenKind,
};
use crate::warning::PackWarning;

/// Render stable output lines for one parsed pack.
///
/// `extra_diagnostics` are the diagnostics that do not live on the accepted
/// spec (parser rejections plus registry-build problems); they share one
/// deduplicated line set with the entry list. Returns `(lines, ok)` where `ok`
/// is true only when no error diagnostics were found.
///
/// The summary counts every accepted authored entry, including
/// `[builtin_icons]` keys.
pub fn render_lines(
    name: &str,
    spec: &PackContentSpec,
    extra_diagnostics: &[ContentDiagnostic],
    warnings: &[PackWarning],
) -> (Vec<String>, bool) {
    // One line per distinct diagnostic. BTreeSet gives both deduplication and
    // deterministic ordering; entry-scoped lines group under their
    // `<kind> <id>` prefix because every severity shares that prefix.
    let mut rendered: BTreeSet<String> = BTreeSet::new();
    let mut errors = 0usize;
    let mut warning_count = 0usize;

    for diagnostic in extra_diagnostics {
        let level = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let line = diagnostic_line(level, name, diagnostic);
        if rendered.insert(line) {
            match diagnostic.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warning_count += 1,
            }
        }
    }

    // Manifest and presentation warnings (v1 content-array gating, corrupt
    // texture/sound fallbacks) are warning-level and never change the exit
    // code. `field` is the authored file or setting that failed.
    for warning in warnings {
        let line = format!("warning: {}: {}", warning.field, warning.reason);
        if rendered.insert(line) {
            warning_count += 1;
        }
    }

    // An accepted entry still prints `ok:` unless a diagnostic rejected it.
    // Rejected entries never reach `spec`, so their error lines stand alone;
    // accepted entries with warning lines keep both.
    let failed_entries: BTreeSet<&str> = extra_diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.entry.as_str())
        .collect();

    let entries = content_entries(spec);
    for entry in &entries {
        if failed_entries.contains(entry.label.as_str()) {
            continue;
        }
        rendered.insert(entry.ok_line());
    }

    let mut lines: Vec<String> = rendered.into_iter().collect();
    lines.push(summary(name, entries.len(), errors, warning_count));
    (lines, errors == 0)
}

/// One summary line shared by success, failure and missing-pack paths.
pub fn summary(name: &str, entries: usize, errors: usize, warnings: usize) -> String {
    format!("validate-pack: {name}: {entries} entries, {errors} errors, {warnings} warnings")
}

/// Render one diagnostic line, special-casing pack-level diagnostics.
fn diagnostic_line(level: &str, name: &str, diagnostic: &ContentDiagnostic) -> String {
    if diagnostic.entry == "pack" {
        format!("{level}: pack {name}: {}", diagnostic.reason)
    } else {
        format!("{level}: {}: {}", diagnostic.entry, diagnostic.reason)
    }
}

/// One accepted authored entry: its stable base label (used for entry
/// counting and diagnostic matching) plus an optional non-default
/// model/alpha suffix shown only on the `ok:` line.
struct ContentEntry {
    label: String,
    metadata: Option<String>,
}

impl ContentEntry {
    fn plain(label: String) -> ContentEntry {
        ContentEntry {
            label,
            metadata: None,
        }
    }

    /// Builds a block/patch entry that appends ` model=... alpha=...` when
    /// either field is non-default. The base label never changes, so a
    /// diagnostic still suppresses exactly the same entry.
    fn with_metadata(label: String, model: BlockModel, alpha: BlockAlpha) -> ContentEntry {
        let metadata = (model != BlockModel::Cube || alpha != BlockAlpha::Opaque)
            .then(|| format!("model={} alpha={}", model.as_str(), alpha.as_str()));
        ContentEntry { label, metadata }
    }

    fn ok_line(&self) -> String {
        match &self.metadata {
            Some(metadata) => format!("ok: {} {metadata}", self.label),
            None => format!("ok: {}", self.label),
        }
    }
}

/// Stable `<kind> <id>` labels for every accepted authored entry.
///
/// Worldgen rules have no id of their own, so they use the same
/// `worldgen <kind> <block>` shape the parser puts in diagnostics.
fn content_entries(spec: &PackContentSpec) -> Vec<ContentEntry> {
    let mut entries = Vec::new();
    entries.extend(spec.blocks.iter().map(|entry| {
        ContentEntry::with_metadata(format!("block {}", entry.id), entry.model, entry.alpha)
    }));
    entries.extend(
        spec.items
            .iter()
            .map(|entry| ContentEntry::plain(format!("item {}", entry.id))),
    );
    entries.extend(
        spec.tiers
            .iter()
            .map(|entry| ContentEntry::plain(format!("tier {}", entry.id))),
    );
    entries.extend(
        spec.tools
            .iter()
            .map(|entry| ContentEntry::plain(format!("tool {}", entry.id))),
    );
    entries.extend(
        spec.recipes
            .iter()
            .map(|entry| ContentEntry::plain(format!("recipe {}", entry.id))),
    );
    entries.extend(
        spec.worldgen
            .iter()
            .map(|entry| ContentEntry::plain(worldgen_label(entry))),
    );
    entries.extend(spec.block_patches.iter().map(|entry| {
        ContentEntry::with_metadata(
            format!("block_patch {}", entry.id),
            entry.model.unwrap_or_default(),
            entry.alpha.unwrap_or_default(),
        )
    }));
    entries.extend(
        spec.recipe_patches
            .iter()
            .map(|entry| ContentEntry::plain(format!("recipe_patch {}", entry.id))),
    );
    entries.extend(
        spec.builtin_icons
            .keys()
            .map(|name| ContentEntry::plain(format!("builtin_icon {name}"))),
    );
    // One entry per loader-synthesized authored built-in face. `Layer` faces
    // reuse an existing base layer and are not authored content entries.
    entries.extend(spec.builtin_faces.iter().flat_map(|entry| {
        entry
            .faces
            .iter()
            .enumerate()
            .filter_map(|(face, source)| match source {
                Some(FaceSource::Authored(_)) => Some(ContentEntry::plain(format!(
                    "face {} {}",
                    entry.block,
                    face_direction(face).unwrap_or("unknown")
                ))),
                Some(FaceSource::Layer(_)) | None => None,
            })
    }));
    entries
}

/// `<kind> <block>` label matching the v2 parser's worldgen diagnostics.
fn worldgen_label(entry: &WorldgenEntry) -> String {
    match &entry.kind {
        WorldgenKind::Ore { block, .. } => format!("worldgen ore {block}"),
        WorldgenKind::Patch { block, .. } => format!("worldgen patch {block}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::spec::{BlockEntry, BlockTextures, BuiltinFaceEntry, FaceSource, ItemEntry};

    fn block(id: &str) -> BlockEntry {
        BlockEntry {
            id: id.to_string(),
            display_name: None,
            textures: BlockTextures::default(),
            hardness: 1.0,
            drop: None,
            tool: None,
            stack_limit: 64,
            model: BlockModel::Cube,
            alpha: BlockAlpha::Opaque,
        }
    }

    fn item(id: &str) -> ItemEntry {
        ItemEntry {
            id: id.to_string(),
            display_name: None,
            icon: "icon.png".to_string(),
            stack_limit: 64,
            food: None,
        }
    }

    fn diagnostic(severity: Severity, entry: &str, reason: &str) -> ContentDiagnostic {
        ContentDiagnostic {
            severity,
            entry: entry.to_string(),
            reason: reason.to_string(),
        }
    }

    fn last_line(lines: &[String]) -> &str {
        lines.last().map(String::as_str).unwrap_or("<no lines>")
    }

    #[test]
    fn accepted_entries_render_ok_lines_and_summary() {
        let spec = PackContentSpec {
            blocks: vec![block("mymod:ruby_ore")],
            items: vec![item("mymod:ruby")],
            ..PackContentSpec::default()
        };

        let (lines, ok) = render_lines("ruby", &spec, &[], &[]);
        let text = lines.join("\n");
        assert!(ok, "a clean spec must validate: {text}");
        assert!(text.contains("ok: block mymod:ruby_ore"), "{text}");
        assert!(text.contains("ok: item mymod:ruby"), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: ruby: 2 entries, 0 errors, 0 warnings"
        );
    }

    #[test]
    fn duplicate_error_suppresses_ok_and_counts_once() {
        let spec = PackContentSpec {
            blocks: vec![block("broken:missing_art")],
            items: vec![item("broken:ok_item")],
            ..PackContentSpec::default()
        };
        let rejected = diagnostic(
            Severity::Error,
            "block broken:missing_art",
            "asset missing.png: not found",
        );

        let (lines, ok) = render_lines("broken", &spec, &[rejected.clone(), rejected], &[]);
        let text = lines.join("\n");
        assert!(!ok, "an error must fail validation: {text}");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("error: block broken:missing_art:"))
                .count(),
            1,
            "identical diagnostics must render once: {text}"
        );
        assert!(!text.contains("ok: block broken:missing_art"), "{text}");
        assert!(text.contains("ok: item broken:ok_item"), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: broken: 2 entries, 1 errors, 0 warnings"
        );
    }

    #[test]
    fn warnings_render_and_count_without_failing() {
        let spec = PackContentSpec {
            items: vec![item("warn:ruby")],
            ..PackContentSpec::default()
        };
        let diagnostics = [diagnostic(
            Severity::Warning,
            "item warn:ruby",
            "icon fell back",
        )];
        let manifest_warnings = [PackWarning {
            field: "pack.toml".to_string(),
            reason: "unknown key `nope`".to_string(),
        }];

        let (lines, ok) = render_lines("warn", &spec, &diagnostics, &manifest_warnings);
        let text = lines.join("\n");
        assert!(ok, "warnings must not fail validation: {text}");
        assert!(
            text.contains("warning: item warn:ruby: icon fell back"),
            "{text}"
        );
        assert!(
            text.contains("warning: pack.toml: unknown key `nope`"),
            "{text}"
        );
        assert!(text.contains("ok: item warn:ruby"), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: warn: 1 entries, 0 errors, 2 warnings"
        );
    }

    #[test]
    fn extra_diagnostics_chain_with_accepted_entries() {
        let spec = PackContentSpec {
            blocks: vec![block("chain:stone")],
            ..PackContentSpec::default()
        };
        // Parser-style rejection followed by a registry-style problem: both
        // sources share one deduplicated, sorted line set.
        let diagnostics = [
            diagnostic(Severity::Error, "pack", "root is not a table"),
            diagnostic(
                Severity::Warning,
                "block chain:stone",
                "drop reference fell back",
            ),
        ];

        let (lines, ok) = render_lines("chain", &spec, &diagnostics, &[]);
        let text = lines.join("\n");
        assert!(!ok, "the pack-level error must fail validation: {text}");
        assert!(
            text.contains("error: pack chain: root is not a table"),
            "{text}"
        );
        assert!(
            text.contains("warning: block chain:stone: drop reference fell back"),
            "{text}"
        );
        assert!(text.contains("ok: block chain:stone"), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: chain: 1 entries, 1 errors, 1 warnings"
        );
    }

    #[test]
    fn authored_builtin_face_renders_an_ok_entry() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: [
                None,
                None,
                Some(FaceSource::Authored("stone_top.png".to_string())),
                None,
                None,
                None,
            ],
        });

        let (lines, ok) = render_lines("faces", &spec, &[], &[]);
        let text = lines.join("\n");
        assert!(ok, "{text}");
        assert!(text.contains("ok: face stone top"), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: faces: 1 entries, 0 errors, 0 warnings",
            "one authored face slot is one entry"
        );
    }

    #[test]
    fn layer_builtin_faces_add_no_entries() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: std::array::from_fn(|_| Some(FaceSource::Layer(20))),
        });

        let (lines, ok) = render_lines("layers", &spec, &[], &[]);
        let text = lines.join("\n");
        assert!(ok, "{text}");
        assert!(!text.contains("ok: face "), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: layers: 0 entries, 0 errors, 0 warnings"
        );
    }

    #[test]
    fn missing_authored_face_suppresses_its_ok_line() {
        let mut spec = PackContentSpec::default();
        spec.builtin_faces.push(BuiltinFaceEntry {
            block: "stone".to_string(),
            faces: [
                None,
                None,
                Some(FaceSource::Authored("missing.png".to_string())),
                None,
                None,
                None,
            ],
        });
        // The registry build reports the failure under the same stable label
        // the entry list uses, so the `ok:` line is suppressed.
        let diagnostics = [diagnostic(
            Severity::Error,
            "face stone top",
            "texture `missing.png` is not a loaded asset",
        )];

        let (lines, ok) = render_lines("faces", &spec, &diagnostics, &[]);
        let text = lines.join("\n");
        assert!(!ok, "{text}");
        assert!(!text.contains("ok: face stone top"), "{text}");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("error: face stone top:"))
                .count(),
            1,
            "{text}"
        );
        assert_eq!(
            last_line(&lines),
            "validate-pack: faces: 1 entries, 1 errors, 0 warnings"
        );
    }

    #[test]
    fn builtin_icon_override_counts_as_an_entry() {
        let spec = PackContentSpec {
            builtin_icons: BTreeMap::from([("apple".to_string(), "apple.png".to_string())]),
            ..PackContentSpec::default()
        };

        let (lines, ok) = render_lines("icons", &spec, &[], &[]);
        let text = lines.join("\n");
        assert!(ok, "{text}");
        assert!(text.contains("ok: builtin_icon apple"), "{text}");
        assert_eq!(
            last_line(&lines),
            "validate-pack: icons: 1 entries, 0 errors, 0 warnings"
        );
    }

    #[test]
    fn non_default_model_and_alpha_render_a_suffix() {
        let mut wheat = block("mymod:wheat");
        wheat.model = BlockModel::Cross;
        wheat.alpha = BlockAlpha::Cutout;
        let mut glass = block("mymod:glass");
        glass.alpha = BlockAlpha::Blend;

        let spec = PackContentSpec {
            blocks: vec![wheat, glass, block("mymod:stone")],
            ..PackContentSpec::default()
        };

        let (lines, ok) = render_lines("models", &spec, &[], &[]);
        let text = lines.join("\n");
        assert!(ok, "{text}");
        assert!(
            text.contains("ok: block mymod:wheat model=cross alpha=cutout"),
            "{text}"
        );
        assert!(
            text.contains("ok: block mymod:glass model=cube alpha=blend"),
            "{text}"
        );
        assert!(
            text.contains("ok: block mymod:stone"),
            "the default block keeps its base line: {text}"
        );
        assert!(
            !text.contains("ok: block mymod:stone model"),
            "the default block must not gain a suffix: {text}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("model=cube alpha=opaque")),
            "default metadata must not render: {text}"
        );
        assert_eq!(
            last_line(&lines),
            "validate-pack: models: 3 entries, 0 errors, 0 warnings"
        );
    }

    #[test]
    fn non_default_block_patch_renders_a_suffix_but_still_matches_its_error() {
        let mut spec = PackContentSpec::default();
        spec.block_patches.push(crate::spec::BlockPatch {
            id: "oak_leaves".to_string(),
            alpha: Some(BlockAlpha::Cutout),
            ..crate::spec::BlockPatch::default()
        });
        let failed = diagnostic(
            Severity::Error,
            "block_patch oak_leaves",
            "texture fell back",
        );

        let (lines, ok) = render_lines("patches", &spec, &[failed], &[]);
        let text = lines.join("\n");
        assert!(!ok, "{text}");
        assert!(
            !text.contains("ok: block_patch oak_leaves"),
            "the base label still suppresses the ok line: {text}"
        );

        let (lines, ok) = render_lines("patches", &spec, &[], &[]);
        let text = lines.join("\n");
        assert!(ok, "{text}");
        assert!(
            text.contains("ok: block_patch oak_leaves model=cube alpha=cutout"),
            "{text}"
        );
    }
}
