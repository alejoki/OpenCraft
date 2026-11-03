//! Pure `pack.toml` parsing with two-phase per-field fallback.
//!
//! A pack manifest is user-authored, so a malformed file or field must never
//! block loading. This module mirrors the shape of
//! [`crate::settings::load_with_warnings`]: phase 1 parses the text into a
//! generic `toml::Value` (syntax errors become one warning), then phase 2
//! extracts every known field independently so a valid field survives an
//! invalid sibling. Unknown keys and tables are silently ignored for forward
//! compatibility.

use crate::settings::SettingsWarning;

/// Pack format declared by `format_version`.
///
/// Absent or `1` is the original texture-only format; `2` enables the content
/// arrays; anything newer is preserved so the loader can warn once and keep
/// the pack's textures and sounds usable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackFormat {
    /// Original texture/sound-only pack format.
    #[default]
    V1,
    /// Content-array format parsed by [`crate::pack::v2`].
    V2,
    /// A format newer than this build understands.
    Future(u32),
}

/// Manifest values with no filesystem access and no implicit defaults.
///
/// Every field is `None` when absent; the loader decides the fallbacks
/// (for example the pack folder name as the display name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackManifest {
    /// Declared pack format (defaults to [`PackFormat::V1`]).
    pub format_version: PackFormat,
    /// Optional human-readable pack version, part of the registry digest.
    pub version: Option<String>,
    /// Human-readable pack name.
    pub name: Option<String>,
    /// Longer pack description (surfaced by the final presentation slice).
    pub description: Option<String>,
    /// Logo file name inside the pack directory.
    pub logo: Option<String>,
    /// Overlay colour applied to menus, as straight RGBA bytes.
    pub overlay_tint: Option<[u8; 4]>,
}

/// Parse `pack.toml` text into a manifest plus structured warnings.
///
/// Never panics and never returns an error: syntax-corrupt TOML yields an
/// all-`None` manifest with exactly one warning, while a per-field error
/// yields `None` for just that field and keeps valid siblings.
pub fn parse_manifest(text: &str) -> (PackManifest, Vec<SettingsWarning>) {
    let (manifest, _root, warnings) = parse_pack_file(text);
    (manifest, warnings)
}

/// Parse `pack.toml` once and keep the generic TOML tree for v2 arrays.
///
/// v2 content lives in the same file as the manifest. Parsing the root twice
/// would either duplicate warnings or silently accept a second, different
/// tree; this entry point returns the root so [`crate::pack::v2`] reuses the
/// exact tree the manifest fields were read from. The root is `None` when the
/// text is syntactically invalid (one warning already names `pack.toml`).
pub fn parse_pack_file(text: &str) -> (PackManifest, Option<toml::Value>, Vec<SettingsWarning>) {
    let mut manifest = PackManifest::default();
    let mut warnings = Vec::new();

    // Phase 1: generic tree. Syntactically invalid TOML -> all defaults + one
    // warning naming the manifest.
    let root: toml::Value = match toml::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!("invalid TOML: {err}"),
            });
            return (manifest, None, warnings);
        }
    };

    // Phase 2: independent per-field extraction. Absent fields are legal and
    // produce no warning; only present-but-wrong fields do.
    manifest.format_version = read_format_version(&root, &mut warnings);
    manifest.name = read_string(&root, "name", "name", &mut warnings);
    manifest.description = read_string(&root, "description", "description", &mut warnings);
    manifest.version = read_string(&root, "version", "version", &mut warnings);

    if let Some(menu_value) = root.get("menu") {
        match menu_value.as_table() {
            Some(menu) => {
                manifest.logo = read_string(menu_value, "logo", "menu.logo", &mut warnings);
                manifest.overlay_tint = read_overlay_tint(menu.get("overlay_tint"), &mut warnings);
            }
            None => warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!(
                    "expected `menu` to be a table, found {}",
                    menu_value.type_str()
                ),
            }),
        }
    }

    (manifest, Some(root), warnings)
}

/// Read `format_version` with one warning for invalid values.
///
/// Absent -> V1 silently. `1` -> V1, `2` -> V2, any larger integer ->
/// [`PackFormat::Future`]. Zero, negative, or non-integer values warn once and
/// fall back to V1 so the rest of the pack still loads.
fn read_format_version(root: &toml::Value, warnings: &mut Vec<SettingsWarning>) -> PackFormat {
    let Some(value) = root.get("format_version") else {
        return PackFormat::V1;
    };

    let warn = |warnings: &mut Vec<SettingsWarning>, reason: String| {
        warnings.push(SettingsWarning {
            field: "pack.toml".to_string(),
            reason,
        });
    };

    let Some(integer) = value.as_integer() else {
        warn(
            warnings,
            format!(
                "expected `format_version` to be an integer, found {}",
                value.type_str()
            ),
        );
        return PackFormat::V1;
    };

    match integer {
        1 => PackFormat::V1,
        2 => PackFormat::V2,
        3.. => PackFormat::Future(integer.min(i64::from(u32::MAX)) as u32),
        _ => {
            warn(
                warnings,
                format!("`format_version` must be 1 or 2, found {integer}"),
            );
            PackFormat::V1
        }
    }
}

/// Read an optional string field, warning when it is present but not a string.
fn read_string(
    table: &toml::Value,
    key: &str,
    field: &str,
    warnings: &mut Vec<SettingsWarning>,
) -> Option<String> {
    let value = table.get(key)?;
    match value.as_str() {
        Some(s) => Some(s.to_string()),
        None => {
            warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!(
                    "expected `{field}` to be a string, found {}",
                    value.type_str()
                ),
            });
            None
        }
    }
}

/// Read `menu.overlay_tint` as exactly four `u8` integers.
fn read_overlay_tint(
    value: Option<&toml::Value>,
    warnings: &mut Vec<SettingsWarning>,
) -> Option<[u8; 4]> {
    let value = value?;
    let warn = |warnings: &mut Vec<SettingsWarning>, reason: String| {
        warnings.push(SettingsWarning {
            field: "pack.toml".to_string(),
            reason,
        });
    };

    let Some(items) = value.as_array() else {
        warn(
            warnings,
            format!(
                "expected `menu.overlay_tint` to be an array of 4 integers, found {}",
                value.type_str()
            ),
        );
        return None;
    };

    if items.len() != 4 {
        warn(
            warnings,
            format!(
                "expected `menu.overlay_tint` to have 4 elements, found {}",
                items.len()
            ),
        );
        return None;
    }

    let mut tint = [0u8; 4];
    for (index, item) in items.iter().enumerate() {
        let Some(channel) = item.as_integer() else {
            warn(
                warnings,
                format!(
                    "expected `menu.overlay_tint[{index}]` to be an integer, found {}",
                    item.type_str()
                ),
            );
            return None;
        };
        match u8::try_from(channel) {
            Ok(byte) => tint[index] = byte,
            Err(_) => {
                warn(
                    warnings,
                    format!("`menu.overlay_tint[{index}]` must be in 0..=255, found {channel}"),
                );
                return None;
            }
        }
    }

    Some(tint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(warnings: &[SettingsWarning]) -> Vec<&str> {
        warnings.iter().map(|w| w.field.as_str()).collect()
    }

    #[test]
    fn parses_all_valid_fields() {
        let text = r#"
name = "Stonecraft"
description = "Gritty default textures"

[menu]
logo = "logo.png"
overlay_tint = [180, 200, 255, 96]
"#;
        let (manifest, warnings) = parse_manifest(text);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(manifest.name.as_deref(), Some("Stonecraft"));
        assert_eq!(
            manifest.description.as_deref(),
            Some("Gritty default textures")
        );
        assert_eq!(manifest.logo.as_deref(), Some("logo.png"));
        assert_eq!(manifest.overlay_tint, Some([180, 200, 255, 96]));
    }

    #[test]
    fn missing_everything_is_all_none_without_warnings() {
        let (manifest, warnings) = parse_manifest("");
        assert_eq!(manifest, PackManifest::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn syntax_corrupt_toml_yields_one_warning_and_all_none() {
        let (manifest, warnings) = parse_manifest("name = [unterminated");
        assert_eq!(manifest, PackManifest::default());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "pack.toml");
        assert!(warnings[0].reason.contains("invalid TOML"));
    }

    #[test]
    fn wrong_typed_name_warns_but_siblings_survive() {
        let text = r#"
name = 42
description = "still here"

[menu]
logo = "logo.png"
"#;
        let (manifest, warnings) = parse_manifest(text);
        assert_eq!(manifest.name, None);
        assert_eq!(manifest.description.as_deref(), Some("still here"));
        assert_eq!(manifest.logo.as_deref(), Some("logo.png"));
        assert_eq!(warnings.len(), 1);
        assert_eq!(fields(&warnings), vec!["pack.toml"]);
        assert!(warnings[0].reason.contains("name"));
    }

    #[test]
    fn overlay_tint_wrong_length_warns() {
        let (manifest, warnings) = parse_manifest("[menu]\noverlay_tint = [1, 2, 3]\n");
        assert_eq!(manifest.overlay_tint, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("overlay_tint"));
    }

    #[test]
    fn overlay_tint_out_of_range_warns() {
        let (manifest, warnings) = parse_manifest("[menu]\noverlay_tint = [1, 2, 3, 256]\n");
        assert_eq!(manifest.overlay_tint, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("overlay_tint"));
    }

    #[test]
    fn overlay_tint_non_integer_warns() {
        let (manifest, warnings) = parse_manifest("[menu]\noverlay_tint = [1, 2, \"3\", 4]\n");
        assert_eq!(manifest.overlay_tint, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("overlay_tint"));
    }

    #[test]
    fn unknown_keys_and_tables_are_silently_ignored() {
        let text = r#"
name = "Pack"
future_key = 12

[menu]
logo = "logo.png"
future_menu_key = true

[future_section]
anything = "goes"
"#;
        let (manifest, warnings) = parse_manifest(text);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(manifest.name.as_deref(), Some("Pack"));
        assert_eq!(manifest.logo.as_deref(), Some("logo.png"));
    }

    #[test]
    fn wrong_typed_logo_warns_while_name_survives() {
        let text = "name = \"Pack\"\n[menu]\nlogo = 7\n";
        let (manifest, warnings) = parse_manifest(text);
        assert_eq!(manifest.name.as_deref(), Some("Pack"));
        assert_eq!(manifest.logo, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("menu.logo"));
    }

    #[test]
    fn format_version_absent_is_v1() {
        let (manifest, warnings) = parse_manifest("name = \"Pack\"\n");
        assert_eq!(manifest.format_version, PackFormat::V1);
        assert_eq!(manifest.version, None);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn format_version_one_and_two_are_recognised() {
        let (v1, warnings) = parse_manifest("format_version = 1\n");
        assert_eq!(v1.format_version, PackFormat::V1);
        assert!(warnings.is_empty());

        let (v2, warnings) =
            parse_manifest("format_version = 2\nversion = \"1.0.0\"\nname = \"P\"\n");
        assert_eq!(v2.format_version, PackFormat::V2);
        assert_eq!(v2.version.as_deref(), Some("1.0.0"));
        assert_eq!(v2.name.as_deref(), Some("P"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn future_format_version_is_preserved_without_warning() {
        let (manifest, warnings) = parse_manifest("format_version = 3\nname = \"P\"\n");
        assert_eq!(manifest.format_version, PackFormat::Future(3));
        assert_eq!(manifest.name.as_deref(), Some("P"));
        assert!(
            warnings.is_empty(),
            "the version gate warns later, not the manifest: {warnings:?}"
        );
    }

    #[test]
    fn invalid_format_version_warns_once_and_keeps_siblings() {
        for text in [
            "format_version = 0\nname = \"P\"\n",
            "format_version = -2\nname = \"P\"\n",
            "format_version = \"two\"\nname = \"P\"\n",
        ] {
            let (manifest, warnings) = parse_manifest(text);
            assert_eq!(manifest.format_version, PackFormat::V1, "text: {text}");
            assert_eq!(manifest.name.as_deref(), Some("P"));
            assert_eq!(warnings.len(), 1, "text: {text}");
            assert_eq!(warnings[0].field, "pack.toml");
            assert!(
                warnings[0].reason.contains("format_version"),
                "reason should name the field: {}",
                warnings[0].reason
            );
        }
    }

    #[test]
    fn wrong_typed_version_warns_but_other_fields_survive() {
        let (manifest, warnings) = parse_manifest("version = 4\nname = \"P\"\n");
        assert_eq!(manifest.version, None);
        assert_eq!(manifest.name.as_deref(), Some("P"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("version"));
    }

    #[test]
    fn parse_pack_file_returns_root_for_v2_arrays() {
        let text = r#"
format_version = 2
name = "P"

[[blocks]]
id = "mymod:ruby"
"#;
        let (manifest, root, warnings) = parse_pack_file(text);
        assert_eq!(manifest.format_version, PackFormat::V2);
        assert!(warnings.is_empty());
        let root = root.expect("valid TOML returns the generic tree");
        assert_eq!(
            root.get("blocks")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );

        let (manifest, warnings) = parse_manifest(text);
        assert_eq!(manifest.format_version, PackFormat::V2);
        assert!(warnings.is_empty());
    }

    #[test]
    fn corrupt_text_has_no_root() {
        let (manifest, root, warnings) = parse_pack_file("name = [unterminated");
        assert_eq!(manifest, PackManifest::default());
        assert!(root.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("invalid TOML"));
    }

    #[test]
    fn empty_manifest_is_an_empty_table() {
        let (manifest, warnings) = parse_manifest("   \n");
        assert_eq!(manifest, PackManifest::default());
        assert!(warnings.is_empty());
    }
}
