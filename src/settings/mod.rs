pub mod keybind;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use winit::keyboard::KeyCode;

/// Default settings file path, for S04's wiring to use.
pub const DEFAULT_SETTINGS_PATH: &str = "settings.toml";

/// Inclusive lower bound for `mouse.sensitivity` (f64): values below this
/// clamp up on load and on menu adjustment. Public so the S04 settings menu
/// clamps its live adjustments through the same single source of truth.
pub const SENSITIVITY_MIN: f64 = 0.0001;
/// Inclusive upper bound for `mouse.sensitivity` (f64); see `SENSITIVITY_MIN`.
pub const SENSITIVITY_MAX: f64 = 0.01;
/// Inclusive lower bound for `video.fov` in degrees (f64): values below this
/// clamp up on load and on menu adjustment. Public so the S04 settings menu
/// clamps its live FOV adjustments through the same single source of truth.
pub const FOV_MIN: f64 = 30.0;
/// Inclusive upper bound for `video.fov` in degrees (f64); see `FOV_MIN`.
pub const FOV_MAX: f64 = 120.0;

/// Player keybind preferences, mirroring `InputState` in
/// `src/player/physics.rs`: exactly one physical key per action.
///
/// NOTE: today `src/main.rs` treats `ShiftLeft | ShiftRight` as an equivalent
/// pair for crouch and `ControlLeft | ControlRight` for sprint. The settings
/// model binds exactly one `KeyCode` per action; once S04 wires this in, the
/// un-bound twin stops working. Intentional narrowing, documented here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keybinds {
    pub forward: KeyCode,
    pub backward: KeyCode,
    pub left: KeyCode,
    pub right: KeyCode,
    pub jump_or_up: KeyCode,
    pub crouch_or_down: KeyCode,
    pub sprint: KeyCode,
}

impl Default for Keybinds {
    /// Matches today's hardcoded bindings in `src/main.rs`.
    fn default() -> Self {
        Self {
            forward: KeyCode::KeyW,
            backward: KeyCode::KeyS,
            left: KeyCode::KeyA,
            right: KeyCode::KeyD,
            jump_or_up: KeyCode::Space,
            crouch_or_down: KeyCode::ShiftLeft,
            sprint: KeyCode::ControlLeft,
        }
    }
}

/// Full player preferences. Defaults equal today's hardcoded values so the
/// S04 wiring swap is behavior-neutral: sensitivity 0.0022
/// (`src/main.rs` device_event), FOV 75.0 (`src/player/camera.rs`
/// `Camera::new`), fullscreen false, vsync false (`src/render/context.rs`
/// prefers Immediate -> no vsync).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub keybinds: Keybinds,
    pub mouse_sensitivity: f32,
    pub fov_degrees: f32,
    pub fullscreen: bool,
    pub vsync: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keybinds: Keybinds::default(),
            mouse_sensitivity: 0.0022,
            fov_degrees: 75.0,
            fullscreen: false,
            vsync: false,
        }
    }
}

/// Structured load-time fallback data: which field fell back and why.
/// Returned by value (no logging framework in this slice) so S04 can
/// display which field fell back and the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsWarning {
    pub field: String,
    pub reason: String,
}

/// One duplicate-keybind conflict: the colliding action names and the
/// shared `KeyCode`. Returned as data (not logged) so the loader can warn
/// without blocking launch and S04's rebind flow can reject duplicate
/// assignments from the same one function.
#[derive(Clone, Debug, PartialEq)]
pub struct KeybindConflict {
    pub actions: Vec<&'static str>,
    pub key: KeyCode,
}

impl Settings {
    /// Scan the 7 keybind actions and report one conflict per duplicated
    /// `KeyCode`, listing the actions sharing it. Empty for the defaults.
    pub fn validate_keybinds(&self) -> Vec<KeybindConflict> {
        let Keybinds {
            forward,
            backward,
            left,
            right,
            jump_or_up,
            crouch_or_down,
            sprint,
        } = self.keybinds;
        let actions: [(&'static str, KeyCode); 7] = [
            ("forward", forward),
            ("backward", backward),
            ("left", left),
            ("right", right),
            ("jump_or_up", jump_or_up),
            ("crouch_or_down", crouch_or_down),
            ("sprint", sprint),
        ];

        // Group action names by shared KeyCode, preserving declaration
        // order; any group larger than one is a conflict.
        let mut groups: Vec<(KeyCode, Vec<&'static str>)> = Vec::new();
        for (name, code) in actions {
            match groups.iter_mut().find(|(key, _)| *key == code) {
                Some((_, names)) => names.push(name),
                None => groups.push((code, vec![name])),
            }
        }

        groups
            .into_iter()
            .filter(|(_, names)| names.len() > 1)
            .map(|(key, names)| KeybindConflict {
                actions: names,
                key,
            })
            .collect()
    }

    /// Atomically save settings to `path` as TOML in the documented format.
    ///
    /// Serialized through a private Serialize-only mirror (never
    /// Deserialize: loading goes through the two-phase per-field-fallback
    /// parser so a bad field can never abort its siblings). Writes to a
    /// `.toml.tmp` sibling, syncs, then renames over the target (the
    /// pattern from `src/world/persistence.rs save_world_to_path`), so a
    /// crash mid-save cannot corrupt the previous file. IO failures bubble
    /// as `std::io::Result`.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mirror = SettingsToml {
            keybinds: KeybindsToml {
                forward: keybind_string(self.keybinds.forward, "forward"),
                backward: keybind_string(self.keybinds.backward, "backward"),
                left: keybind_string(self.keybinds.left, "left"),
                right: keybind_string(self.keybinds.right, "right"),
                jump_or_up: keybind_string(self.keybinds.jump_or_up, "jump_or_up"),
                crouch_or_down: keybind_string(self.keybinds.crouch_or_down, "crouch_or_down"),
                sprint: keybind_string(self.keybinds.sprint, "sprint"),
            },
            mouse: MouseToml {
                sensitivity: f64::from(self.mouse_sensitivity),
            },
            video: VideoToml {
                fov: f64::from(self.fov_degrees),
                fullscreen: self.fullscreen,
                vsync: self.vsync,
            },
        };

        let contents = toml::to_string(&mirror)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let tmp_path = path.with_extension("toml.tmp");
        {
            let file = File::create(&tmp_path)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(contents.as_bytes())?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }
        fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

/// Map a keybind to its stable settings string. A key outside the table is
/// a bug at save time (every default is mapped, and the round-trip and
/// format tests guard the mapping), so panic with a precise message.
fn keybind_string(code: KeyCode, action: &'static str) -> String {
    keybind::key_to_string(code)
        .unwrap_or_else(|| panic!("no settings string mapping for {action}'s key {code:?}"))
        .to_owned()
}

/// Serialize-only mirror of `Settings` matching the documented TOML format
/// exactly. Never derive Deserialize here: loading uses the two-phase
/// per-field-fallback parser, not serde (see pattern P001 in KNOWLEDGE.md).
#[derive(serde::Serialize)]
struct SettingsToml {
    keybinds: KeybindsToml,
    mouse: MouseToml,
    video: VideoToml,
}

#[derive(serde::Serialize)]
struct KeybindsToml {
    forward: String,
    backward: String,
    left: String,
    right: String,
    jump_or_up: String,
    crouch_or_down: String,
    sprint: String,
}

#[derive(serde::Serialize)]
struct MouseToml {
    sensitivity: f64,
}

#[derive(serde::Serialize)]
struct VideoToml {
    fov: f64,
    fullscreen: bool,
    vsync: bool,
}

/// Load settings from `path`, discarding warnings. Thin wrapper over
/// `load_with_warnings` for callers that don't need fallback details.
pub fn load(path: &Path) -> Settings {
    load_with_warnings(path).0
}

/// Two-phase TOML load with per-field fallback so a bad or missing
/// settings.toml never blocks launch.
///
/// Phase 0 reads the file; phase 1 parses it into a generic `toml::Value`;
/// phase 2 extracts each field independently with type/range checks. Plain
/// `#[serde(default)]` derive would abort the whole struct on any field
/// type error; the generic tree is what makes per-field fallback possible
/// (a valid field survives an invalid sibling).
pub fn load_with_warnings(path: &Path) -> (Settings, Vec<SettingsWarning>) {
    let mut settings = Settings::default();
    let mut warnings = Vec::new();

    // Phase 0: read the file. Missing/unreadable -> defaults + one warning,
    // no panic. Never blocks launch.
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            warnings.push(SettingsWarning {
                field: "settings".to_string(),
                reason: format!("could not read settings file: {err}"),
            });
            return (settings, warnings);
        }
    };

    // Phase 1: parse into a generic toml::Value. Syntactically invalid TOML
    // -> all defaults + one warning.
    let root: toml::Value = match toml::from_str(&contents) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(SettingsWarning {
                field: "settings".to_string(),
                reason: format!("invalid TOML: {err}"),
            });
            return (settings, warnings);
        }
    };

    // Phase 2: per-field extraction with type/range checks, independent per
    // field so one bad field cannot abort the others. Unknown keys, unknown
    // sections, and unknown keybind actions are silently ignored (forward
    // compatibility): we only look up known fields.

    if let Some(keybinds_value) = root.get("keybinds") {
        match keybinds_value.as_table() {
            Some(table) => {
                let Keybinds {
                    forward,
                    backward,
                    left,
                    right,
                    jump_or_up,
                    crouch_or_down,
                    sprint,
                } = &mut settings.keybinds;
                for (action, slot) in [
                    ("forward", forward),
                    ("backward", backward),
                    ("left", left),
                    ("right", right),
                    ("jump_or_up", jump_or_up),
                    ("crouch_or_down", crouch_or_down),
                    ("sprint", sprint),
                ] {
                    load_keybind_action(table, action, slot, &mut warnings);
                }
            }
            None => warnings.push(SettingsWarning {
                field: "keybinds".to_string(),
                reason: format!("expected a table, found {}", keybinds_value.type_str()),
            }),
        }
    }

    // Duplicate keybinds surface as data: one warning per conflict (never
    // blocking launch), with both values kept for S04's rebind flow to
    // resolve.
    for conflict in settings.validate_keybinds() {
        warnings.push(SettingsWarning {
            field: "keybinds".to_string(),
            reason: format!(
                "duplicate keybind: {:?} is bound to multiple actions: {}",
                conflict.key,
                conflict.actions.join(", ")
            ),
        });
    }

    if let Some(mouse_value) = root.get("mouse") {
        match mouse_value.as_table() {
            Some(table) => {
                settings.mouse_sensitivity = load_clamped_f32(
                    table,
                    "sensitivity",
                    SENSITIVITY_MIN,
                    SENSITIVITY_MAX,
                    settings.mouse_sensitivity,
                    "mouse.sensitivity",
                    &mut warnings,
                );
            }
            None => warnings.push(SettingsWarning {
                field: "mouse".to_string(),
                reason: format!("expected a table, found {}", mouse_value.type_str()),
            }),
        }
    }

    if let Some(video_value) = root.get("video") {
        match video_value.as_table() {
            Some(table) => {
                settings.fov_degrees = load_clamped_f32(
                    table,
                    "fov",
                    FOV_MIN,
                    FOV_MAX,
                    settings.fov_degrees,
                    "video.fov",
                    &mut warnings,
                );
                settings.fullscreen = load_bool(
                    table,
                    "fullscreen",
                    settings.fullscreen,
                    "video.fullscreen",
                    &mut warnings,
                );
                settings.vsync =
                    load_bool(table, "vsync", settings.vsync, "video.vsync", &mut warnings);
            }
            None => warnings.push(SettingsWarning {
                field: "video".to_string(),
                reason: format!("expected a table, found {}", video_value.type_str()),
            }),
        }
    }

    (settings, warnings)
}

/// Extract one keybind action from the `[keybinds]` table into `slot`.
/// Missing action -> keep default, no warning (partial files are fine).
/// Unparsable string or wrong type -> that field's default + warning.
fn load_keybind_action(
    table: &toml::value::Table,
    action: &str,
    slot: &mut KeyCode,
    warnings: &mut Vec<SettingsWarning>,
) {
    match table.get(action) {
        None => {}
        Some(toml::Value::String(s)) => match keybind::parse_key(s) {
            Some(code) => *slot = code,
            None => warnings.push(SettingsWarning {
                field: format!("keybinds.{action}"),
                reason: format!("unrecognized key '{s}'"),
            }),
        },
        Some(other) => warnings.push(SettingsWarning {
            field: format!("keybinds.{action}"),
            reason: format!("expected a keybind string, found {}", other.type_str()),
        }),
    }
}

/// Extract a float field from a table and clamp to `[min, max]` with a
/// warning (never reject). Wrong type -> current (default) + warning.
/// TOML floats are f64; cast to f32 on the way out.
fn load_clamped_f32(
    table: &toml::value::Table,
    key: &str,
    min: f64,
    max: f64,
    current: f32,
    field: &str,
    warnings: &mut Vec<SettingsWarning>,
) -> f32 {
    match table.get(key) {
        None => current,
        Some(value) => match value.as_float() {
            Some(f) => {
                if f < min {
                    warnings.push(SettingsWarning {
                        field: field.to_string(),
                        reason: format!("{f} below minimum {min}, clamped to {min}"),
                    });
                    min as f32
                } else if f > max {
                    warnings.push(SettingsWarning {
                        field: field.to_string(),
                        reason: format!("{f} above maximum {max}, clamped to {max}"),
                    });
                    max as f32
                } else {
                    f as f32
                }
            }
            None => {
                warnings.push(SettingsWarning {
                    field: field.to_string(),
                    reason: format!("expected a float, found {}", value.type_str()),
                });
                current
            }
        },
    }
}

/// Extract a boolean field from a table. Wrong type -> current + warning.
fn load_bool(
    table: &toml::value::Table,
    key: &str,
    current: bool,
    field: &str,
    warnings: &mut Vec<SettingsWarning>,
) -> bool {
    match table.get(key) {
        None => current,
        Some(value) => match value.as_bool() {
            Some(b) => b,
            None => {
                warnings.push(SettingsWarning {
                    field: field.to_string(),
                    reason: format!("expected a boolean, found {}", value.type_str()),
                });
                current
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn write_settings_file(dir: &tempfile::TempDir, contents: &str) -> PathBuf {
        let path = dir.path().join("settings.toml");
        fs::write(&path, contents).unwrap();
        path
    }

    fn warning_fields(warnings: &[SettingsWarning]) -> Vec<&str> {
        warnings.iter().map(|w| w.field.as_str()).collect()
    }

    #[test]
    fn keybinds_defaults_match_todays_hardcoded_bindings() {
        let d = Keybinds::default();
        assert_eq!(d.forward, KeyCode::KeyW);
        assert_eq!(d.backward, KeyCode::KeyS);
        assert_eq!(d.left, KeyCode::KeyA);
        assert_eq!(d.right, KeyCode::KeyD);
        assert_eq!(d.jump_or_up, KeyCode::Space);
        assert_eq!(d.crouch_or_down, KeyCode::ShiftLeft);
        assert_eq!(d.sprint, KeyCode::ControlLeft);
    }

    #[test]
    fn settings_defaults_match_todays_hardcoded_values() {
        let d = Settings::default();
        assert_eq!(d.mouse_sensitivity, 0.0022);
        assert_eq!(d.fov_degrees, 75.0);
        assert!(!d.fullscreen);
        assert!(!d.vsync);
        assert_eq!(d.keybinds, Keybinds::default());
    }

    #[test]
    fn missing_file_yields_defaults_with_exactly_one_warning() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml"); // never written
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "settings");
    }

    #[test]
    fn syntactically_invalid_toml_yields_all_defaults_with_one_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "not [valid");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "settings");
    }

    #[test]
    fn bad_fov_type_falls_back_while_valid_fields_survive() {
        let dir = tempdir().unwrap();
        let contents = r#"
[keybinds]
forward = "KeyZ"
jump_or_up = "Enter"

[mouse]
sensitivity = 0.005

[video]
fov = "wide"
fullscreen = true
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.keybinds.forward, KeyCode::KeyZ);
        assert_eq!(settings.keybinds.jump_or_up, KeyCode::Enter);
        assert_eq!(settings.mouse_sensitivity, 0.005f64 as f32);
        assert_eq!(settings.fov_degrees, Settings::default().fov_degrees);
        assert!(settings.fullscreen);
        assert_eq!(warning_fields(&warnings), vec!["video.fov"]);
    }

    #[test]
    fn unknown_keys_sections_and_actions_are_silently_ignored() {
        let dir = tempdir().unwrap();
        let contents = r#"
unknown_top_level = 7

[keybinds]
forward = "KeyZ"
unknown_action = "KeyX"

[audio]
volume = 11

[mouse]
sensitivity = 0.005

[video]
vsync = true
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.keybinds.forward, KeyCode::KeyZ);
        assert_eq!(settings.keybinds.backward, KeyCode::KeyS); // untouched default
        assert_eq!(settings.mouse_sensitivity, 0.005f64 as f32);
        assert!(settings.vsync);
        assert!(warnings.is_empty());
    }

    #[test]
    fn out_of_range_values_above_maximum_are_clamped_with_warnings() {
        let dir = tempdir().unwrap();
        let contents = r#"
[video]
fov = 500.0

[mouse]
sensitivity = 0.5
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.fov_degrees, 120.0);
        assert_eq!(settings.mouse_sensitivity, 0.01f64 as f32);
        let fields = warning_fields(&warnings);
        assert!(fields.contains(&"video.fov"));
        assert!(fields.contains(&"mouse.sensitivity"));
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn out_of_range_values_below_minimum_are_clamped_with_warnings() {
        let dir = tempdir().unwrap();
        let contents = r#"
[video]
fov = 10.0

[mouse]
sensitivity = 0.000001
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.fov_degrees, 30.0);
        assert_eq!(settings.mouse_sensitivity, 0.0001f64 as f32);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn boundary_values_are_accepted_without_warnings() {
        let dir = tempdir().unwrap();
        let contents = r#"
[mouse]
sensitivity = 0.0001

[video]
fov = 120.0
vsync = true
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.mouse_sensitivity, 0.0001f64 as f32);
        assert_eq!(settings.fov_degrees, 120.0);
        assert!(settings.vsync);
        assert!(warnings.is_empty());
    }

    #[test]
    fn wrong_type_mouse_and_video_fields_fall_back_with_warnings() {
        let dir = tempdir().unwrap();
        let contents = r#"
[mouse]
sensitivity = "fast"

[video]
fullscreen = 1
vsync = "yes"
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.mouse_sensitivity, 0.0022);
        assert!(!settings.fullscreen);
        assert!(!settings.vsync);
        let fields = warning_fields(&warnings);
        assert!(fields.contains(&"mouse.sensitivity"));
        assert!(fields.contains(&"video.fullscreen"));
        assert!(fields.contains(&"video.vsync"));
    }

    #[test]
    fn unparsable_keybind_string_falls_back_with_warning() {
        let dir = tempdir().unwrap();
        let contents = r#"
[keybinds]
forward = "NotAKey"
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.keybinds.forward, KeyCode::KeyW); // default
        assert_eq!(warning_fields(&warnings), vec!["keybinds.forward"]);
    }

    #[test]
    fn wrong_type_keybind_falls_back_with_warning() {
        let dir = tempdir().unwrap();
        let contents = r#"
[keybinds]
sprint = 3
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.keybinds.sprint, KeyCode::ControlLeft); // default
        assert_eq!(warning_fields(&warnings), vec!["keybinds.sprint"]);
    }

    #[test]
    fn missing_keybind_actions_keep_defaults_without_warnings() {
        let dir = tempdir().unwrap();
        let contents = r#"
[keybinds]
forward = "KeyZ"
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.keybinds.forward, KeyCode::KeyZ);
        assert_eq!(settings.keybinds.jump_or_up, KeyCode::Space); // default
        assert!(warnings.is_empty());
    }

    #[test]
    fn valid_minimal_file_parses_with_zero_warnings() {
        let dir = tempdir().unwrap();
        let contents = r#"
[mouse]
sensitivity = 0.0022

[video]
fov = 75.0
fullscreen = false
vsync = false
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn load_discards_warnings_and_never_panics() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("settings.toml");
        assert_eq!(load(&missing), Settings::default());

        let bad = write_settings_file(&dir, "]]garbage[[");
        assert_eq!(load(&bad), Settings::default());
    }

    // --- T03: save, duplicate-keybind validation, round-trip ---

    #[test]
    fn round_trip_preserves_non_default_settings_with_zero_warnings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let original = Settings {
            keybinds: Keybinds {
                jump_or_up: KeyCode::ArrowUp,
                ..Keybinds::default()
            },
            mouse_sensitivity: 0.005,
            fov_degrees: 90.0,
            fullscreen: true,
            vsync: true,
        };
        original.save(&path).unwrap();
        let (loaded, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(loaded, original);
    }

    #[test]
    fn defaults_round_trip_exactly_with_zero_warnings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        Settings::default().save(&path).unwrap();
        let (loaded, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn saved_file_matches_the_documented_format() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        Settings::default().save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[keybinds]"));
        assert!(text.contains("forward = \"KeyW\""));
        assert!(text.contains("jump_or_up = \"Space\""));
        assert!(text.contains("crouch_or_down = \"ShiftLeft\""));
        assert!(text.contains("[mouse]"));
        assert!(text.contains("[video]"));
        assert!(text.contains("fullscreen = false"));
        assert!(text.contains("vsync = false"));
    }

    #[test]
    fn duplicate_keybinds_load_with_conflict_warning_and_both_values_kept() {
        let dir = tempdir().unwrap();
        let contents = r#"
[keybinds]
forward = "KeyW"
jump_or_up = "KeyW"
"#;
        let path = write_settings_file(&dir, contents);
        let (settings, warnings) = load_with_warnings(&path);
        // never blocks launch: both values load and are kept
        assert_eq!(settings.keybinds.forward, KeyCode::KeyW);
        assert_eq!(settings.keybinds.jump_or_up, KeyCode::KeyW);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "keybinds");
        assert!(warnings[0].reason.contains("forward"));
        assert!(warnings[0].reason.contains("jump_or_up"));
        assert!(warnings[0].reason.contains("KeyW"));
        // the same validate function reports the conflict for S04's rebind flow
        let conflicts = settings.validate_keybinds();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, KeyCode::KeyW);
        assert!(conflicts[0].actions.contains(&"forward"));
        assert!(conflicts[0].actions.contains(&"jump_or_up"));
    }

    #[test]
    fn validate_keybinds_on_defaults_reports_no_conflicts() {
        assert!(Settings::default().validate_keybinds().is_empty());
    }

    #[test]
    fn triple_duplicate_reports_one_conflict_listing_all_actions() {
        let keybinds = Keybinds {
            left: KeyCode::KeyW,
            jump_or_up: KeyCode::KeyW,
            ..Keybinds::default()
        };
        let settings = Settings {
            keybinds,
            ..Settings::default()
        };
        let conflicts = settings.validate_keybinds();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, KeyCode::KeyW);
        assert_eq!(conflicts[0].actions, vec!["forward", "left", "jump_or_up"]);
    }

    #[test]
    fn save_creates_missing_parent_directories_and_overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("settings.toml");
        Settings::default().save(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(load(&path), Settings::default());

        let changed = Settings {
            mouse_sensitivity: 0.005,
            fov_degrees: 90.0,
            vsync: true,
            ..Settings::default()
        };
        changed.save(&path).unwrap();
        assert_eq!(load(&path), changed);
    }

    #[test]
    fn save_leaves_no_tmp_file_behind_on_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        Settings::default().save(&path).unwrap();
        assert!(path.is_file());
        assert!(!path.with_extension("toml.tmp").exists());
        assert!(dir.path().read_dir().unwrap().count() == 1);
    }
}
