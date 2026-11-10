pub mod keybind;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

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
/// Inclusive lower bound for `video.render_distance` in chunks. Matches
/// `WorldManager`'s own clamp so settings and streaming agree exactly.
pub const RENDER_DISTANCE_MIN: i32 = 4;
/// Inclusive upper bound for `video.render_distance` in chunks.
pub const RENDER_DISTANCE_MAX: i32 = 36;
/// The default render distance in chunks: the fresh-world value.
pub const DEFAULT_RENDER_DISTANCE: i32 = 16;
/// Inclusive lower bound for `audio.volume` in percent: values below this
/// clamp up on load. Public so the settings menu clamps its live
/// adjustments through the same single source of truth.
pub const VOLUME_MIN: i32 = 0;
/// Inclusive upper bound for `audio.volume` in percent; see `VOLUME_MIN`.
pub const VOLUME_MAX: i32 = 100;
/// Menu adjustment granularity for `audio.volume` in percent. Load-time
/// values are not required to be a multiple of this.
pub const VOLUME_STEP: i32 = 10;
/// The default volume in percent: a default save omits the `[audio]` section.
pub const DEFAULT_VOLUME: u8 = 100;

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
/// `Camera::new`), render distance 16 (`WorldSession::new_session`),
/// fullscreen false, vsync false (`src/render/context.rs` prefers
/// Immediate -> no vsync).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub keybinds: Keybinds,
    pub mouse_sensitivity: f32,
    pub fov_degrees: f32,
    pub render_distance: i32,
    pub fullscreen: bool,
    pub vsync: bool,
    /// Ordered, enabled texture pack folder names under `packs/`. Empty
    /// means the built-in Default textures only, so a default save contains
    /// no `[packs]` section at all (byte-identical to the pre-S03 format).
    /// Later entries apply later in the stack; until the Packs screen lands
    /// (T07), the first entry is the legacy single-pack selection.
    pub pack_stack: Vec<String>,
    /// Master volume in percent (0..=100). 100 is the default, so a default
    /// save contains no `[audio]` section at all. Clamped on load; the menu
    /// clamps live adjustments against `VOLUME_MIN`/`VOLUME_MAX`.
    pub volume: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keybinds: Keybinds::default(),
            mouse_sensitivity: 0.0022,
            fov_degrees: 75.0,
            render_distance: DEFAULT_RENDER_DISTANCE,
            fullscreen: false,
            vsync: false,
            pack_stack: Vec::new(),
            volume: DEFAULT_VOLUME,
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

/// Append `.bak` to the full file name (`settings.toml` ->
/// `settings.toml.bak`), unlike `with_extension` which would turn `foo.cfg`
/// into `foo.bak`.
fn backup_path(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(".bak");
    PathBuf::from(raw)
}

/// True when `bytes` are not valid UTF-8 TOML. Mirrors the loader's validity
/// oracle so invalid-UTF-8 files (which `read_to_string` rejects) count as
/// unparseable too.
fn is_unparseable(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Err(_) => true,
        Ok(text) => toml::from_str::<toml::Value>(text).is_err(),
    }
}

/// Copy an existing unparseable settings file to `<path>.bak` exactly once.
///
/// A missing target is fine (no backup is created); a failed read or copy is
/// returned so the caller can abort before the atomic rename destroys the
/// original. `fs::copy` (not `rename`) keeps the original in place until the
/// rename replaces it, so a crash between copy and rename loses nothing.
fn preserve_unparseable_settings(path: &Path) -> std::io::Result<()> {
    if backup_path(path).exists() {
        return Ok(());
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if is_unparseable(&bytes) {
        fs::copy(path, backup_path(path))?;
    }
    Ok(())
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
    /// crash mid-save cannot corrupt the previous file. Before the atomic
    /// write, an existing target that is not valid UTF-8 TOML is copied once
    /// to `<path>.bak` (skipped when that backup already exists); a missing
    /// target creates no backup, and a failed read/copy aborts the save so a
    /// hand-edited typo is never destroyed. IO failures bubble as
    /// `std::io::Result`.
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
                render_distance: self.render_distance,
                fullscreen: self.fullscreen,
                vsync: self.vsync,
            },
            packs: (!self.pack_stack.is_empty()).then(|| PacksToml {
                stack: self.pack_stack.clone(),
            }),
            audio: (self.volume != DEFAULT_VOLUME).then(|| AudioToml {
                volume: u32::from(self.volume),
            }),
        };

        let contents = toml::to_string(&mirror)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        preserve_unparseable_settings(path)?;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    packs: Option<PacksToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<AudioToml>,
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
    render_distance: i32,
    fullscreen: bool,
    vsync: bool,
}

#[derive(serde::Serialize)]
struct PacksToml {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stack: Vec<String>,
}

#[derive(serde::Serialize)]
struct AudioToml {
    volume: u32,
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
/// (a valid field survives an invalid sibling). Phase 0 reports a missing
/// file distinctly from an unreadable one ("settings file is missing" vs
/// "could not read settings file"), and phase 1 reports syntactic TOML
/// failures as "invalid TOML".
pub fn load_with_warnings(path: &Path) -> (Settings, Vec<SettingsWarning>) {
    let mut settings = Settings::default();
    let mut warnings = Vec::new();

    // Phase 0: read the file. Missing/unreadable -> defaults + one warning,
    // no panic. Never blocks launch. Missing and unreadable stay distinct so
    // the warning names the actual problem.
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            let reason = if err.kind() == std::io::ErrorKind::NotFound {
                "settings file is missing; using defaults".to_string()
            } else {
                format!("could not read settings file: {err}")
            };
            warnings.push(SettingsWarning {
                field: "settings".to_string(),
                reason,
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
                settings.render_distance = load_clamped_i32(
                    table,
                    "render_distance",
                    RENDER_DISTANCE_MIN,
                    RENDER_DISTANCE_MAX,
                    settings.render_distance,
                    "video.render_distance",
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

    // Optional section: missing `[packs]` keeps the empty stack silently.
    // The v2 ordered `stack` array wins over the legacy single
    // `texture_pack` key, which is still read so pre-S02 settings files
    // migrate to a one-entry stack (the compatibility contract for this
    // milestone). An empty string is accepted as-is (runtime pack resolution
    // simply won't find a folder named "").
    if let Some(packs_value) = root.get("packs") {
        match packs_value.as_table() {
            Some(table) => {
                let mut stack_loaded = false;
                if let Some(stack_value) = table.get("stack") {
                    match stack_value.as_array() {
                        Some(items) => {
                            let mut stack = Vec::with_capacity(items.len());
                            for (index, item) in items.iter().enumerate() {
                                match item.as_str() {
                                    Some(name) => {
                                        // Duplicates keep the first occurrence
                                        // (a later duplicate cannot change the
                                        // effective stack order).
                                        if !stack.iter().any(|existing: &String| existing == name) {
                                            stack.push(name.to_string());
                                        }
                                    }
                                    None => warnings.push(SettingsWarning {
                                        field: "packs.stack".to_string(),
                                        reason: format!(
                                            "expected a string at index {index}, found {}",
                                            item.type_str()
                                        ),
                                    }),
                                }
                            }
                            settings.pack_stack = stack;
                            stack_loaded = true;
                        }
                        None => warnings.push(SettingsWarning {
                            field: "packs.stack".to_string(),
                            reason: format!("expected an array, found {}", stack_value.type_str()),
                        }),
                    }
                }
                // A valid `stack` suppresses the legacy key entirely (no
                // warning): the file is already in the v2 format.
                if !stack_loaded {
                    if let Some(name) = load_optional_string(
                        table,
                        "texture_pack",
                        None,
                        "packs.texture_pack",
                        &mut warnings,
                    ) {
                        settings.pack_stack = vec![name];
                    }
                }
            }
            None => warnings.push(SettingsWarning {
                field: "packs".to_string(),
                reason: format!("expected a table, found {}", packs_value.type_str()),
            }),
        }
    }

    // Optional section: missing `[audio]` or missing `volume` keeps the
    // default (100) silently. Values are clamped to `[VOLUME_MIN,
    // VOLUME_MAX]` on load; any in-range value is accepted (the 10-step is
    // menu granularity only, not a file invariant).
    if let Some(audio_value) = root.get("audio") {
        match audio_value.as_table() {
            Some(table) => {
                settings.volume = load_clamped_i32(
                    table,
                    "volume",
                    VOLUME_MIN,
                    VOLUME_MAX,
                    i32::from(settings.volume),
                    "audio.volume",
                    &mut warnings,
                ) as u8;
            }
            None => warnings.push(SettingsWarning {
                field: "audio".to_string(),
                reason: format!("expected a table, found {}", audio_value.type_str()),
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

/// Extract an integer field from a table and clamp to `[min, max]` with a
/// warning (never reject). Wrong type -> current (default) + warning.
fn load_clamped_i32(
    table: &toml::value::Table,
    key: &str,
    min: i32,
    max: i32,
    current: i32,
    field: &str,
    warnings: &mut Vec<SettingsWarning>,
) -> i32 {
    match table.get(key) {
        None => current,
        Some(value) => match value.as_integer() {
            Some(n) => {
                let (min, max) = (i64::from(min), i64::from(max));
                if n < min {
                    warnings.push(SettingsWarning {
                        field: field.to_string(),
                        reason: format!("{n} below minimum {min}, clamped to {min}"),
                    });
                    min as i32
                } else if n > max {
                    warnings.push(SettingsWarning {
                        field: field.to_string(),
                        reason: format!("{n} above maximum {max}, clamped to {max}"),
                    });
                    max as i32
                } else {
                    n as i32
                }
            }
            None => {
                warnings.push(SettingsWarning {
                    field: field.to_string(),
                    reason: format!("expected an integer, found {}", value.type_str()),
                });
                current
            }
        },
    }
}

/// Extract an optional string field from a table. Missing key -> current
/// (Default `None`), no warning. Wrong type -> current + warning naming the
/// full field path.
fn load_optional_string(
    table: &toml::value::Table,
    key: &str,
    current: Option<String>,
    field: &str,
    warnings: &mut Vec<SettingsWarning>,
) -> Option<String> {
    match table.get(key) {
        None => current,
        Some(value) => match value.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                warnings.push(SettingsWarning {
                    field: field.to_string(),
                    reason: format!("expected a string, found {}", value.type_str()),
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
        assert_eq!(d.render_distance, DEFAULT_RENDER_DISTANCE);
        assert_eq!(d.render_distance, 16);
        assert!(!d.fullscreen);
        assert!(!d.vsync);
        assert_eq!(d.volume, DEFAULT_VOLUME);
        assert_eq!(d.volume, 100);
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
    fn render_distance_loads_and_out_of_range_values_clamp_with_warnings() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[video]\nrender_distance = 24\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.render_distance, 24);
        assert!(warnings.is_empty());

        let low = write_settings_file(&dir, "[video]\nrender_distance = 0\n");
        let (settings, warnings) = load_with_warnings(&low);
        assert_eq!(settings.render_distance, RENDER_DISTANCE_MIN);
        assert_eq!(warning_fields(&warnings), vec!["video.render_distance"]);

        let high = write_settings_file(&dir, "[video]\nrender_distance = 900\n");
        let (settings, warnings) = load_with_warnings(&high);
        assert_eq!(settings.render_distance, RENDER_DISTANCE_MAX);
        assert_eq!(warning_fields(&warnings), vec!["video.render_distance"]);
    }

    #[test]
    fn wrong_type_render_distance_falls_back_with_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[video]\nrender_distance = \"far\"\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.render_distance, DEFAULT_RENDER_DISTANCE);
        assert_eq!(warning_fields(&warnings), vec!["video.render_distance"]);
    }

    #[test]
    fn unknown_keys_sections_and_actions_are_silently_ignored() {
        let dir = tempdir().unwrap();
        let contents = r#"
unknown_top_level = 7

[keybinds]
forward = "KeyZ"
unknown_action = "KeyX"

[network]
ping = 7

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
            render_distance: 24,
            fullscreen: true,
            vsync: true,
            pack_stack: Vec::new(),
            volume: 60,
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
        assert!(text.contains("render_distance = 16"));
        assert!(text.contains("fullscreen = false"));
        assert!(text.contains("vsync = false"));
        // Default pack means no [packs] section at all: byte-identical to
        // the pre-S03 format (settings_smoke.sh unchanged-file invariant).
        assert!(
            !text.contains("[packs]"),
            "default save must omit [packs]:\n{text}"
        );
        assert!(!text.contains("texture_pack"));
        assert!(!text.contains("stack ="));
        // Default volume means no [audio] section at all, keeping the default
        // save byte-identical to the pre-audio format.
        assert!(
            !text.contains("[audio]"),
            "default save must omit [audio]:\n{text}"
        );
        assert!(!text.contains("volume"));

        let with_pack = Settings {
            pack_stack: vec!["dungeon".to_string()],
            ..Settings::default()
        };
        with_pack.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[packs]"),
            "named pack must emit [packs]:\n{text}"
        );
        assert!(
            text.contains("stack = [\"dungeon\"]"),
            "a one-entry stack uses the v2 array form:\n{text}"
        );
        assert!(!text.contains("texture_pack"));

        let with_volume = Settings {
            volume: 60,
            ..Settings::default()
        };
        with_volume.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[audio]"),
            "non-default volume must emit [audio]:\n{text}"
        );
        assert!(text.contains("volume = 60"));
    }

    #[test]
    fn pack_stack_round_trips_in_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let original = Settings {
            pack_stack: vec!["alpha".to_string(), "beta".to_string()],
            ..Settings::default()
        };
        original.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("stack = ["),
            "a non-empty stack emits the v2 array key:\n{text}"
        );
        let (loaded, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(loaded, original);
        assert_eq!(loaded.pack_stack, vec!["alpha", "beta"]);
    }

    #[test]
    fn legacy_texture_pack_migrates_to_a_one_entry_stack() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[packs]\ntexture_pack = \"dungeon\"\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(settings.pack_stack, vec!["dungeon"]);

        // Re-saving a migrated file emits the v2 array form, never the legacy
        // key again.
        settings.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("stack = [\"dungeon\"]"), "{text}");
        assert!(!text.contains("texture_pack"), "{text}");
    }

    #[test]
    fn stack_wins_over_legacy_texture_pack() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(
            &dir,
            "[packs]\nstack = [\"alpha\", \"beta\"]\ntexture_pack = \"legacy\"\n",
        );
        let (settings, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(settings.pack_stack, vec!["alpha", "beta"]);
    }

    #[test]
    fn wrong_type_stack_warns_and_falls_back_to_legacy_texture_pack() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(
            &dir,
            "[packs]\nstack = \"alpha\"\ntexture_pack = \"legacy\"\n",
        );
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.pack_stack, vec!["legacy"]);
        assert_eq!(warning_fields(&warnings), vec!["packs.stack"]);
        assert!(warnings[0].reason.contains("expected an array"));
        assert!(warnings[0].reason.contains("string"));
    }

    #[test]
    fn non_string_stack_members_are_skipped_with_warnings() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[packs]\nstack = [\"alpha\", 7, \"beta\"]\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.pack_stack, vec!["alpha", "beta"]);
        assert_eq!(warning_fields(&warnings), vec!["packs.stack"]);
        assert!(
            warnings[0].reason.contains("index 1"),
            "reason names the bad index: {}",
            warnings[0].reason
        );
        assert!(warnings[0].reason.contains("integer"));
    }

    #[test]
    fn empty_stack_array_is_accepted_and_omits_packs_on_save() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[packs]\nstack = []\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(settings.pack_stack.is_empty());

        settings.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("[packs]"),
            "an empty stack must omit the section: {text}"
        );
    }

    #[test]
    fn duplicate_stack_names_keep_the_first_occurrence() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[packs]\nstack = [\"alpha\", \"beta\", \"alpha\"]\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty(), "duplicates are silent: {warnings:?}");
        assert_eq!(settings.pack_stack, vec!["alpha", "beta"]);
    }

    #[test]
    fn wrong_type_texture_pack_falls_back_to_default_with_one_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[packs]\ntexture_pack = 7\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert!(settings.pack_stack.is_empty());
        assert_eq!(warning_fields(&warnings), vec!["packs.texture_pack"]);
        assert!(warnings[0].reason.contains("expected a string"));
        assert!(warnings[0].reason.contains("integer"));
    }

    #[test]
    fn missing_packs_section_or_key_keeps_default_pack_without_warnings() {
        let dir = tempdir().unwrap();

        let no_section = write_settings_file(&dir, "[video]\nvsync = false\n");
        let (settings, warnings) = load_with_warnings(&no_section);
        assert!(settings.pack_stack.is_empty());
        assert!(warnings.is_empty());

        let empty_section = write_settings_file(&dir, "[packs]\nunknown = 1\n");
        let (settings, warnings) = load_with_warnings(&empty_section);
        assert!(settings.pack_stack.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn empty_texture_pack_string_is_accepted_without_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[packs]\ntexture_pack = \"\"\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.pack_stack, vec![""]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn wrong_type_packs_section_falls_back_with_one_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "packs = 3\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert!(settings.pack_stack.is_empty());
        assert_eq!(warning_fields(&warnings), vec!["packs"]);
    }

    #[test]
    fn volume_round_trips_through_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        for volume in [0u8, 60, 100] {
            let original = Settings {
                volume,
                ..Settings::default()
            };
            original.save(&path).unwrap();
            let (loaded, warnings) = load_with_warnings(&path);
            assert!(warnings.is_empty(), "volume {volume}: {warnings:?}");
            assert_eq!(loaded, original, "volume {volume}");
        }
    }

    #[test]
    fn volume_clamps_out_of_range_values_with_warnings() {
        let dir = tempdir().unwrap();

        let low = write_settings_file(&dir, "[audio]\nvolume = -5\n");
        let (settings, warnings) = load_with_warnings(&low);
        assert_eq!(settings.volume, 0);
        assert_eq!(warning_fields(&warnings), vec!["audio.volume"]);

        let high = write_settings_file(&dir, "[audio]\nvolume = 250\n");
        let (settings, warnings) = load_with_warnings(&high);
        assert_eq!(settings.volume, 100);
        assert_eq!(warning_fields(&warnings), vec!["audio.volume"]);
    }

    #[test]
    fn volume_boundaries_and_arbitrary_in_range_values_load_silently() {
        let dir = tempdir().unwrap();
        for volume in [0u8, 1, 37, 99, 100] {
            let path = write_settings_file(&dir, &format!("[audio]\nvolume = {volume}\n"));
            let (settings, warnings) = load_with_warnings(&path);
            assert_eq!(settings.volume, volume);
            assert!(warnings.is_empty(), "volume {volume}: {warnings:?}");
        }
    }

    #[test]
    fn wrong_type_volume_falls_back_to_default_with_one_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[audio]\nvolume = \"loud\"\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.volume, DEFAULT_VOLUME);
        assert_eq!(warning_fields(&warnings), vec!["audio.volume"]);
        assert!(warnings[0].reason.contains("expected an integer"));
    }

    #[test]
    fn missing_audio_section_or_key_keeps_default_volume_without_warnings() {
        let dir = tempdir().unwrap();

        let no_section = write_settings_file(&dir, "[video]\nvsync = false\n");
        let (settings, warnings) = load_with_warnings(&no_section);
        assert_eq!(settings.volume, DEFAULT_VOLUME);
        assert!(warnings.is_empty());

        let empty_section = write_settings_file(&dir, "[audio]\nunknown = 1\n");
        let (settings, warnings) = load_with_warnings(&empty_section);
        assert_eq!(settings.volume, DEFAULT_VOLUME);
        assert!(warnings.is_empty());
    }

    #[test]
    fn wrong_type_audio_section_falls_back_with_one_warning() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "audio = 3\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings.volume, DEFAULT_VOLUME);
        assert_eq!(warning_fields(&warnings), vec!["audio"]);
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
            render_distance: 20,
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

    #[test]
    fn backup_path_appends_bak_to_the_full_file_name() {
        assert_eq!(
            backup_path(Path::new("dir/foo.cfg")),
            PathBuf::from("dir/foo.cfg.bak")
        );
        assert_eq!(
            backup_path(Path::new("settings.toml")),
            PathBuf::from("settings.toml.bak")
        );
    }

    #[test]
    fn saving_defaults_over_a_typoed_file_preserves_the_original_bytes() {
        let dir = tempdir().unwrap();
        let typo = "not [valid\n";
        let path = write_settings_file(&dir, typo);

        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("invalid TOML"));

        Settings::default().save(&path).unwrap();

        let backup = dir.path().join("settings.toml.bak");
        assert_eq!(fs::read(&backup).unwrap(), typo.as_bytes());
        let (reloaded, warnings) = load_with_warnings(&path);
        assert_eq!(reloaded, Settings::default());
        assert!(warnings.is_empty());
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn saving_defaults_for_a_missing_file_creates_no_bak() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");

        Settings::default().save(&path).unwrap();

        assert!(path.is_file());
        assert!(!dir.path().join("settings.toml.bak").exists());
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn saving_over_a_valid_file_creates_no_bak() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[video]\nfov = 95.0\n");
        let (loaded, warnings) = load_with_warnings(&path);
        assert!(warnings.is_empty());

        let changed = Settings {
            fov_degrees: 80.0,
            ..loaded
        };
        changed.save(&path).unwrap();

        assert!(!dir.path().join("settings.toml.bak").exists());
        assert_eq!(load(&path), changed);
    }

    #[test]
    fn bak_is_written_once_and_survives_later_typoed_saves() {
        let dir = tempdir().unwrap();
        let typo_a = "first [typo\n";
        let typo_b = "second [typo\n";
        let path = write_settings_file(&dir, typo_a);
        Settings::default().save(&path).unwrap();

        let backup = dir.path().join("settings.toml.bak");
        assert_eq!(fs::read(&backup).unwrap(), typo_a.as_bytes());

        fs::write(&path, typo_b).unwrap();
        Settings::default().save(&path).unwrap();

        assert_eq!(fs::read(&backup).unwrap(), typo_a.as_bytes());
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn invalid_utf8_settings_file_is_preserved_before_save() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let seeded = [0xff, 0xfe, b'[', b'v', b'i', b'd'];
        fs::write(&path, seeded).unwrap();

        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "settings");

        Settings::default().save(&path).unwrap();

        let backup = dir.path().join("settings.toml.bak");
        assert_eq!(fs::read(&backup).unwrap(), seeded);
        let (reloaded, warnings) = load_with_warnings(&path);
        assert_eq!(reloaded, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn per_field_failure_does_not_create_a_bak() {
        let dir = tempdir().unwrap();
        let path = write_settings_file(&dir, "[video]\nfov = \"wide\"\n");
        let (settings, warnings) = load_with_warnings(&path);
        assert_eq!(settings, Settings::default());
        assert_eq!(warning_fields(&warnings), vec!["video.fov"]);

        Settings::default().save(&path).unwrap();

        assert!(!dir.path().join("settings.toml.bak").exists());
        let (reloaded, warnings) = load_with_warnings(&path);
        assert_eq!(reloaded, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn missing_and_parse_failures_report_distinct_readable_reasons() {
        let dir = tempdir().unwrap();
        let missing_path = dir.path().join("never-written.toml");
        let (_, missing_warnings) = load_with_warnings(&missing_path);
        assert_eq!(missing_warnings.len(), 1);
        assert_eq!(missing_warnings[0].field, "settings");
        assert!(missing_warnings[0].reason.contains("missing"));

        let typo_path = write_settings_file(&dir, "not [valid\n");
        let (_, typo_warnings) = load_with_warnings(&typo_path);
        assert_eq!(typo_warnings.len(), 1);
        assert_eq!(typo_warnings[0].field, "settings");
        assert!(typo_warnings[0].reason.contains("invalid TOML"));

        assert_ne!(missing_warnings[0].reason, typo_warnings[0].reason);
    }

    #[test]
    fn unreadable_target_aborts_the_save_before_the_rename() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::create_dir(&path).unwrap();

        assert!(Settings::default().save(&path).is_err());
        assert!(path.is_dir());
        assert!(!path.with_extension("toml.tmp").exists());
    }
}
