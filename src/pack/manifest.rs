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

/// Manifest values with no filesystem access and no implicit defaults.
///
/// Every field is `None` when absent; the loader decides the fallbacks
/// (for example the pack folder name as the display name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackManifest {
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
    let mut manifest = PackManifest::default();
    let mut warnings = Vec::new();

    // Phase 1: generic tree. Syntactically invalid TOML -> all None + one
    // warning naming the manifest.
    let root: toml::Value = match toml::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!("invalid TOML: {err}"),
            });
            return (manifest, warnings);
        }
    };

    // Phase 2: independent per-field extraction. Absent fields are legal and
    // produce no warning; only present-but-wrong fields do.
    manifest.name = read_string(&root, "name", "name", &mut warnings);
    manifest.description = read_string(&root, "description", "description", &mut warnings);

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

    (manifest, warnings)
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
    fn empty_manifest_is_an_empty_table() {
        let (manifest, warnings) = parse_manifest("   \n");
        assert_eq!(manifest, PackManifest::default());
        assert!(warnings.is_empty());
    }
}
