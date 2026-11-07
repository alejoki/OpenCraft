//! Curated, creator-tunable configuration.
//!
//! [`registry`] is the single source of truth for the M014 config contract:
//! the six files, their curated keys, types, defaults, ranges, units, apply
//! modes, and documentation.
//!
//! [`ConfigRuntime`] is the never-fail runtime boundary built on top of it.
//! Loading follows the same two-phase pattern as `crate::settings`: parse the
//! file into a generic [`toml::Value`] tree, then extract each curated row
//! independently so one bad field cannot abort its valid siblings. A missing
//! file is a warning; unreadable/oversize/syntactically-invalid files and
//! cross-field invariant violations fall back to registry defaults and are
//! reported as structured diagnostics. Diagnostics are returned as data —
//! this module never prints or logs.
//!
//! The loader never writes config files. The effective-value digest hashes
//! only the schema version and the resolved values (never file bytes), so
//! comments and formatting are silent while creator edits are visible.

pub mod registry;
pub mod watch;

use std::collections::BTreeMap;
use std::path::Path;

use crate::player::bow::BowRules;
use crate::player::camera::CameraRules;
use crate::player::combat::CombatRules;
use crate::player::eating::EatingRules;
use crate::player::melee::MeleeRules;
use crate::player::physics::MovementRules;
use crate::player::survival::SurvivalRules;
use crate::player::tools::ToolRules;
use crate::settings::SettingsWarning;
use crate::ui::selection::SelectionRules;
use crate::world::arrow::ArrowRules;
use crate::world::daycycle::{ClockRules, RenderRules};
use crate::world::farming::FarmingRules;
use crate::world::season::WeatherRules;
use crate::world::session::InteractionRules;
use crate::world::spawning::SpawnRules;

pub use registry::{
    docs_table, index_of, ApplyMode, ConfigFile, ConfigKind, ConfigRow, ConfigValue, REGISTRY,
};

/// Directory holding the six curated TOML files, relative to the process CWD.
pub const CONFIG_DIR: &str = "config";

/// Version of the effective-value digest / save schema contract.
pub const CONFIG_SCHEMA_VERSION: u16 = 1;

/// Hard size cap for a single config file; anything larger is rejected whole.
const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

/// Exact reason recorded by the loader for an absent curated file.
const MISSING_FILE_REASON: &str = "file is missing; using defaults";

const FILE_COUNT: usize = 6;

/// Where one effective value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueOrigin {
    /// The registry default: the key was absent or invalid.
    Default,
    /// A valid value read from this file (even when it equals the default).
    File(ConfigFile),
}

/// Diagnostic severity. Only [`ConfigSeverity::Error`] keeps `--check-config`
/// non-zero and surfaces through in-game startup warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigSeverity {
    Error,
    Warning,
}

/// One structured loader diagnostic.
///
/// `key` is `None` for file-level problems (missing file, size cap, read or
/// TOML parse failure). `key` is `Some(dotted.key)` for per-field and
/// cross-field problems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    pub file: ConfigFile,
    pub key: Option<String>,
    pub severity: ConfigSeverity,
    pub reason: String,
}

/// Digest boundary persisted by save format v9 (S04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveConfigMeta {
    pub schema: u16,
    pub digest: u64,
}

/// Typed gameplay tuning built from the effective configuration.
///
/// The bundle is a plain value, so startup and live reload hand the same
/// struct to [`crate::world::session::WorldSession::apply_rules`] and can
/// derive it again on every reload without touching loader state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RulesBundle {
    pub movement: MovementRules,
    pub camera: CameraRules,
    pub survival: SurvivalRules,
    pub eating: EatingRules,
    pub melee: MeleeRules,
    pub combat: CombatRules,
    pub bow: BowRules,
    pub arrow: ArrowRules,
    pub clock: ClockRules,
    pub weather: WeatherRules,
    pub spawning: SpawnRules,
    pub farming: FarmingRules,
    pub tools: ToolRules,
    pub interaction: InteractionRules,
    pub render: RenderRules,
    pub selection: SelectionRules,
}

/// Outcome of one [`ConfigRuntime::reload_file`] call.
///
/// `diagnostics` is the effective diagnostic block for the reloaded file after
/// the call: the candidate's own diagnostics on a commit or a candidate-error
/// rejection, or the single synthetic file-level Error used to reject an
/// incomplete write.
#[derive(Clone, Debug)]
pub struct FileReload {
    pub file: ConfigFile,
    pub applied: bool,
    pub changed: usize,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl FileReload {
    /// First [`ConfigSeverity::Error`] diagnostic in the deterministic order
    /// the parser emits: file-level, then registry order, then unknown keys.
    pub fn first_error(&self) -> Option<&ConfigDiagnostic> {
        self.diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
    }

    /// Number of Error diagnostics in this file's block.
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
            .count()
    }

    /// Number of Warning diagnostics in this file's block.
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Warning)
            .count()
    }
}

/// Resolved configuration values and their provenance.
///
/// The value and origin vectors are parallel to [`REGISTRY`]. Cloning is cheap
/// relative to startup and keeps the runtime value-shaped for `GameApp`.
#[derive(Clone, Debug)]
pub struct ConfigRuntime {
    values: Vec<ConfigValue>,
    origins: Vec<ValueOrigin>,
    diagnostics: Vec<ConfigDiagnostic>,
    file_errors: [Option<String>; FILE_COUNT],
    digest: u64,
}

impl ConfigRuntime {
    /// Registry defaults without touching the filesystem.
    pub fn defaults() -> Self {
        let values: Vec<ConfigValue> = REGISTRY.iter().map(|row| row.default).collect();
        let origins = vec![ValueOrigin::Default; REGISTRY.len()];
        let digest = compute_digest(&values);
        Self {
            values,
            origins,
            diagnostics: Vec::new(),
            file_errors: [None, None, None, None, None, None],
            digest,
        }
    }

    /// Load all six files from `dir` in [`ConfigFile::ALL`] order.
    ///
    /// Never fails: a missing directory or file leaves registry defaults in
    /// place. Unexpected files in `dir` are ignored here (the checker reports
    /// them separately).
    pub fn load_dir(dir: &Path) -> Self {
        let mut values: Vec<ConfigValue> = REGISTRY.iter().map(|row| row.default).collect();
        let mut origins = vec![ValueOrigin::Default; REGISTRY.len()];
        let mut diagnostics = Vec::new();
        let mut file_errors: [Option<String>; FILE_COUNT] = std::array::from_fn(|_| None);

        for file in ConfigFile::ALL {
            let load = parse_file(dir, file);
            let rows: Vec<&ConfigRow> = REGISTRY.iter().filter(|row| row.file == file).collect();
            for ((row, value), origin) in rows.iter().zip(load.values).zip(load.origins) {
                let index = index_of(row.key).expect("registry row key must resolve");
                values[index] = value;
                origins[index] = origin;
            }
            diagnostics.extend(load.diagnostics);
            file_errors[file_index(load.file)] = load.file_error;
        }

        let digest = compute_digest(&values);
        Self {
            values,
            origins,
            diagnostics,
            file_errors,
            digest,
        }
    }

    /// Re-parse exactly one curated file and atomically commit it when valid.
    ///
    /// This is the dev live-reload boundary and it never fails: the only
    /// outcomes are a commit or a rejection that leaves the running values
    /// untouched. It never writes, prints, or logs.
    ///
    /// Live reload is deliberately stricter than startup. Startup falls back
    /// per field so one typo cannot erase a whole file; live reload rejects the
    /// whole owning file on any Error, keeping its last good values and
    /// origins. A missing file is the exception: deletion is a valid reset to
    /// registry defaults plus the usual missing-file warning (the watcher's
    /// settle window hides the unlink phase of atomic temp+rename saves).
    ///
    /// Empty and no-recognized-key files are rejected as incomplete writes
    /// with synthetic file-level Errors. This is a deliberate divergence from
    /// `--check-config`, which treats an empty file as all defaults: the
    /// checker audits what would load at startup, while live reload protects
    /// the running game from an editor caught mid-write.
    ///
    /// Rejection replaces only this file's diagnostics block and `file_error`;
    /// sibling files and their diagnostics are never touched, and the digest
    /// only moves when a candidate is actually committed.
    pub fn reload_file(&mut self, dir: &Path, file: ConfigFile) -> FileReload {
        let FileLoad {
            present,
            contents_empty,
            values: candidate_values,
            origins: candidate_origins,
            diagnostics: candidate_diagnostics,
            file_error: candidate_file_error,
            ..
        } = parse_file(dir, file);

        let has_error = candidate_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ConfigSeverity::Error);

        // Any candidate Error rejects the whole file: keep the last good
        // values/origins/digest and surface only the candidate's diagnostics.
        if has_error {
            self.replace_file_diagnostics(file, candidate_diagnostics.clone());
            self.file_errors[file_index(file)] = candidate_file_error;
            return FileReload {
                file,
                applied: false,
                changed: 0,
                diagnostics: candidate_diagnostics,
            };
        }

        // An empty file parses as "all defaults"; at startup that is fine, but
        // live it almost always means the editor truncated the file mid-save.
        if present && contents_empty {
            let diagnostics = vec![ConfigDiagnostic {
                file,
                key: None,
                severity: ConfigSeverity::Error,
                reason: "file is empty; keeping last good values".to_string(),
            }];
            self.replace_file_diagnostics(file, diagnostics.clone());
            self.file_errors[file_index(file)] = None;
            return FileReload {
                file,
                applied: false,
                changed: 0,
                diagnostics,
            };
        }

        // A comment-only file, bare section headers, or only unknown keys also
        // leave every row at Default; treat it as an incomplete write rather
        // than silently resetting the file's live values.
        if present
            && candidate_origins
                .iter()
                .all(|origin| *origin == ValueOrigin::Default)
        {
            let diagnostics = vec![ConfigDiagnostic {
                file,
                key: None,
                severity: ConfigSeverity::Error,
                reason: "file has no recognized keys; keeping last good values".to_string(),
            }];
            self.replace_file_diagnostics(file, diagnostics.clone());
            self.file_errors[file_index(file)] = None;
            return FileReload {
                file,
                applied: false,
                changed: 0,
                diagnostics,
            };
        }

        // Commit: a missing file (present == false) resets this file to its
        // registry defaults; otherwise this is a normal valid candidate.
        // Origins count as a change because the overlay's `*` marker is
        // provenance, not value equality.
        let rows: Vec<&ConfigRow> = REGISTRY.iter().filter(|row| row.file == file).collect();
        let mut changed = 0usize;
        for (local, row) in rows.iter().enumerate() {
            let index = index_of(row.key).expect("registry row key must resolve");
            if self.values[index] != candidate_values[local]
                || self.origins[index] != candidate_origins[local]
            {
                changed += 1;
            }
            self.values[index] = candidate_values[local];
            self.origins[index] = candidate_origins[local];
        }
        self.replace_file_diagnostics(file, candidate_diagnostics.clone());
        self.file_errors[file_index(file)] = candidate_file_error;
        self.digest = compute_digest(&self.values);
        FileReload {
            file,
            applied: true,
            changed,
            diagnostics: candidate_diagnostics,
        }
    }

    /// Swap one file's diagnostic block while preserving `ConfigFile::ALL`
    /// order across the whole vector.
    ///
    /// `retain` + `extend` would move the reloaded file to the end; rebuilding
    /// by file order keeps `diagnostics()` stable for the checker and overlay.
    fn replace_file_diagnostics(&mut self, file: ConfigFile, diagnostics: Vec<ConfigDiagnostic>) {
        let mut merged = Vec::with_capacity(self.diagnostics.len() + diagnostics.len());
        for ordered in ConfigFile::ALL {
            if ordered == file {
                merged.extend(diagnostics.iter().cloned());
            } else {
                merged.extend(
                    self.diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.file == ordered)
                        .cloned(),
                );
            }
        }
        self.diagnostics = merged;
    }

    /// Digest schema version carried in [`SaveConfigMeta`].
    pub fn schema(&self) -> u16 {
        CONFIG_SCHEMA_VERSION
    }

    /// FNV-1a 64 over the effective values.
    pub fn digest(&self) -> u64 {
        self.digest
    }

    /// Save-boundary metadata: schema plus effective-value digest.
    pub fn meta(&self) -> SaveConfigMeta {
        SaveConfigMeta {
            schema: self.schema(),
            digest: self.digest(),
        }
    }

    /// Typed gameplay rules from the effective values.
    ///
    /// Every key below is a curated registry row, so a missing or mistyped
    /// key is a programmer error and panics with the key name; creator input
    /// never reaches this path unvalidated (bad files fall back per field).
    pub fn rules(&self) -> RulesBundle {
        RulesBundle {
            movement: MovementRules {
                walk_speed: self.f32("movement.walk_speed"),
                sprint_speed: self.f32("movement.sprint_speed"),
                fly_speed: self.f32("movement.fly_speed"),
                fly_sprint_speed: self.f32("movement.fly_sprint_speed"),
                fly_vertical_factor: self.f32("movement.fly_vertical_factor"),
                jump_velocity: self.f32("movement.jump_velocity"),
                gravity: self.f32("movement.gravity"),
                terminal_velocity: self.f32("movement.terminal_velocity"),
                swim_vertical_drag: self.f32("movement.swim_vertical_drag"),
                swim_rise_accel: self.f32("movement.swim_rise_accel"),
                swim_rise_speed: self.f32("movement.swim_rise_speed"),
                swim_max_sink_speed: self.f32("movement.swim_max_sink_speed"),
                swim_walk_speed_mult: self.f32("movement.swim_walk_speed_mult"),
                swim_sink_accel: self.f32("movement.swim_sink_accel"),
            },
            camera: CameraRules {
                z_near: self.f32("camera.z_near"),
                z_far: self.f32("camera.z_far"),
            },
            survival: SurvivalRules {
                fall_safe_blocks: self.f32("survival.fall_safe_blocks"),
                air_drain_per_sec: self.f32("survival.air_drain_per_sec"),
                drown_interval_secs: self.f32("survival.drown_interval_secs"),
                drown_damage: self.i64("survival.drown_damage") as u16,
                hunger_drain_secs: self.f32("survival.hunger_drain_secs"),
                starve_interval_secs: self.f32("survival.starve_interval_secs"),
                starve_damage: self.i64("survival.starve_damage") as u16,
                regen_hunger_threshold: self.i64("survival.regen_hunger_threshold") as u16,
                regen_interval_secs: self.f32("survival.regen_interval_secs"),
                regen_hunger_cost: self.i64("survival.regen_hunger_cost") as u16,
            },
            eating: EatingRules {
                eat_duration_secs: self.f32("eating.eat_duration_secs"),
                apple_hunger: self.i64("eating.apple_hunger") as u16,
                bread_hunger: self.i64("eating.bread_hunger") as u16,
                raw_meat_hunger: self.i64("eating.raw_meat_hunger") as u16,
                rotten_flesh_hunger: self.i64("eating.rotten_flesh_hunger") as u16,
            },
            melee: MeleeRules {
                fist_damage: self.i64("melee.fist_damage") as u16,
                sword_damage_wood: self.i64("melee.sword_damage_wood") as u16,
                sword_damage_stone: self.i64("melee.sword_damage_stone") as u16,
                sword_damage_iron: self.i64("melee.sword_damage_iron") as u16,
                reach: self.f32("melee.reach"),
                swing_cooldown_secs: self.f32("melee.swing_cooldown_secs"),
            },
            combat: CombatRules {
                iframe_secs: self.f32("player.iframe_secs"),
                hurt_flash_secs: self.f32("player.hurt_flash_secs"),
                knockback_speed: self.f32("player.knockback_speed"),
                knockback_decay: self.f32("player.knockback_decay"),
            },
            bow: BowRules {
                min_charge_secs: self.f32("bow.min_charge_secs"),
                full_charge_secs: self.f32("bow.full_charge_secs"),
            },
            arrow: ArrowRules {
                gravity: self.f32("arrow.gravity"),
                speed_min: self.f32("arrow.speed_min"),
                speed_max: self.f32("arrow.speed_max"),
                damage_min: self.i64("arrow.damage_min") as u16,
                damage_max: self.i64("arrow.damage_max") as u16,
                skeleton_speed: self.f32("arrow.skeleton_speed"),
                skeleton_damage: self.i64("arrow.skeleton_damage") as u16,
                lifetime_secs: self.f32("arrow.lifetime_secs"),
                embed_secs: self.f32("arrow.embed_secs"),
            },
            clock: ClockRules {
                day_seconds: self.f32("clock.day_seconds"),
            },
            weather: WeatherRules {
                winter_snow_percent: self.u32("weather.winter_snow_percent"),
                spring_rain_percent: self.u32("weather.spring_rain_percent"),
                summer_rain_percent: self.u32("weather.summer_rain_percent"),
                autumn_rain_percent: self.u32("weather.autumn_rain_percent"),
            },
            spawning: SpawnRules {
                safe_radius_blocks: self.i32("spawning.safe_radius_blocks"),
                spawn_max_radius_blocks: self.i32("spawning.spawn_max_radius_blocks"),
                despawn_radius_blocks: self.i32("spawning.despawn_radius_blocks"),
                max_mobs_total: self.usize("spawning.max_mobs_total"),
                spawn_region_blocks: self.i32("spawning.spawn_region_blocks"),
                spawn_region_cap: self.usize("spawning.spawn_region_cap"),
                spawn_attempt_secs: self.f32("spawning.spawn_attempt_secs"),
                despawn_scan_secs: self.f32("spawning.despawn_scan_secs"),
                burn_interval_secs: self.f32("spawning.burn_interval_secs"),
                spawn_min_separation: self.f32("spawning.spawn_min_separation"),
                hostile_cave_light: self.u8("spawning.hostile_cave_light"),
                passive_surface_light: self.u8("spawning.passive_surface_light"),
                max_spawn_attempts_per_call: self.u32("spawning.max_spawn_attempts_per_call"),
            },
            farming: FarmingRules {
                wheat_stage_game_secs: self.f32("farming.wheat_stage_game_secs"),
            },
            tools: ToolRules {
                wood_speed_multiplier: self.u8("tools.wood_speed_multiplier"),
                stone_speed_multiplier: self.u8("tools.stone_speed_multiplier"),
                iron_speed_multiplier: self.u8("tools.iron_speed_multiplier"),
            },
            interaction: InteractionRules {
                table_reach: self.f32("interaction.table_reach"),
            },
            render: RenderRules {
                rain_fog_factor: self.f32("fog.rain_factor"),
                snow_fog_factor: self.f32("fog.snow_factor"),
                rain_grey_scale: self.f32("palette.rain_grey_scale"),
                snow_grey_scale: self.f32("palette.snow_grey_scale"),
                rain_sky_lerp: self.f32("palette.rain_sky_lerp"),
                snow_sky_lerp: self.f32("palette.snow_sky_lerp"),
                rain_day_factor: self.f32("palette.rain_day_factor"),
                snow_day_factor: self.f32("palette.snow_day_factor"),
                moonlit_floor: self.f32("palette.moonlit_floor"),
            },
            selection: SelectionRules {
                selected_name_secs: self.f32("ui.selected_name_secs"),
            },
        }
    }

    /// Typed effective value for `key`.
    ///
    /// Panics on an unknown key; this is a programmer error, not creator input
    /// (creator edits fall back to defaults and produce diagnostics instead).
    pub fn value(&self, key: &str) -> ConfigValue {
        let index = index_of(key).unwrap_or_else(|| panic!("unknown config key: {key}"));
        self.values[index]
    }

    /// Provenance of `key` in the loaded tree. Panics on an unknown key.
    pub fn origin(&self, key: &str) -> ValueOrigin {
        let index = index_of(key).unwrap_or_else(|| panic!("unknown config key: {key}"));
        self.origins[index]
    }

    /// Effective `f32` for a Float row. Panics on an unknown key or wrong kind.
    pub fn f32(&self, key: &str) -> f32 {
        match self.value(key) {
            ConfigValue::Float(value) => value,
            other => panic!("config key {key} is not a float: {other:?}"),
        }
    }

    /// Effective `i64` for an Int row. Panics on an unknown key or wrong kind.
    pub fn i64(&self, key: &str) -> i64 {
        match self.value(key) {
            ConfigValue::Int(value) => value,
            other => panic!("config key {key} is not an int: {other:?}"),
        }
    }

    /// Effective integer narrowed to `u32`. Panics on an unknown key, wrong
    /// kind, or a value that does not fit.
    pub fn u32(&self, key: &str) -> u32 {
        let value = self.i64(key);
        u32::try_from(value)
            .unwrap_or_else(|_| panic!("config key {key} value {value} does not fit in u32"))
    }

    /// Effective integer narrowed to `u16`. Panics on an unknown key, wrong
    /// kind, or a value that does not fit.
    pub fn u16(&self, key: &str) -> u16 {
        let value = self.i64(key);
        u16::try_from(value)
            .unwrap_or_else(|_| panic!("config key {key} value {value} does not fit in u16"))
    }

    /// Effective integer narrowed to `u8`. Panics on an unknown key, wrong
    /// kind, or a value that does not fit.
    pub fn u8(&self, key: &str) -> u8 {
        let value = self.i64(key);
        u8::try_from(value)
            .unwrap_or_else(|_| panic!("config key {key} value {value} does not fit in u8"))
    }

    /// Effective integer narrowed to `i32`. Panics on an unknown key, wrong
    /// kind, or a value that does not fit.
    pub fn i32(&self, key: &str) -> i32 {
        let value = self.i64(key);
        i32::try_from(value)
            .unwrap_or_else(|_| panic!("config key {key} value {value} does not fit in i32"))
    }

    /// Effective integer narrowed to `usize`. Panics on an unknown key, wrong
    /// kind, or a value that does not fit.
    pub fn usize(&self, key: &str) -> usize {
        let value = self.i64(key);
        usize::try_from(value)
            .unwrap_or_else(|_| panic!("config key {key} value {value} does not fit in usize"))
    }

    /// Effective bool for a Bool row. Panics on an unknown key or wrong kind.
    pub fn bool(&self, key: &str) -> bool {
        match self.value(key) {
            ConfigValue::Bool(value) => value,
            other => panic!("config key {key} is not a bool: {other:?}"),
        }
    }

    /// Every loader diagnostic, in file order then registry/unknown-key order.
    pub fn diagnostics(&self) -> &[ConfigDiagnostic] {
        &self.diagnostics
    }

    /// Whole-file failure reason for `file` (read/size/TOML parse), if any.
    /// A missing file is a warning and yields `None`.
    pub fn file_error(&self, file: ConfigFile) -> Option<&str> {
        self.file_errors[file_index(file)].as_deref()
    }
}

/// Convert only [`ConfigSeverity::Error`] diagnostics into startup warnings.
///
/// Field shape is `config/<file>` for file-level errors or
/// `config/<file>: <key>` for per-key errors; the reason is prefixed with
/// `error: `. Missing files and unknown keys stay out of the in-game notice.
pub fn startup_warnings(runtime: &ConfigRuntime) -> Vec<SettingsWarning> {
    runtime
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
        .map(|diagnostic| SettingsWarning {
            field: match &diagnostic.key {
                Some(key) => format!("{}: {key}", diagnostic.file.rel_path()),
                None => diagnostic.file.rel_path().to_string(),
            },
            reason: format!("error: {}", diagnostic.reason),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Headless --check-config report
// ---------------------------------------------------------------------------

/// Render the deterministic `--check-config` report for `dir`.
///
/// Returns `(lines, ok)` where `ok` is true iff the loader produced no Error
/// diagnostics. The function reads `dir` (and scans it for unexpected `*.toml`
/// entries) but never writes, opens a window, touches settings, or reads a
/// pack. Every path in the output is a logical `config/<file>` path, so the
/// report is independent of the process CWD.
///
/// Line contract:
/// - files render in [`ConfigFile::ALL`] order;
/// - per file, diagnostics order file-level, known keys in registry order,
///   then unknown keys lexicographically;
/// - a present file with no Error prints `ok: config/<file>: <N> values`;
/// - a missing file prints one warning and is counted in `missing`;
/// - unexpected `*.toml` entries print sorted `not a config file` warnings;
/// - the final line is the fixed summary carrying counts, schema and digest.
///
/// This module never prints; the caller owns stdout and the exit status
/// (`ok` false means exit non-zero).
pub fn check_config_lines(dir: &Path) -> (Vec<String>, bool) {
    let runtime = ConfigRuntime::load_dir(dir);
    let mut lines = Vec::new();
    let mut missing = 0usize;
    let mut warnings = 0usize;
    let mut errors = 0usize;

    for file in ConfigFile::ALL {
        let diagnostics = ordered_file_diagnostics(&runtime, file);

        // The loader records an absent file as exactly one file-level warning
        // and never records anything else for it.
        let is_missing = diagnostics.len() == 1
            && diagnostics[0].key.is_none()
            && diagnostics[0].severity == ConfigSeverity::Warning
            && diagnostics[0].reason == MISSING_FILE_REASON;
        if is_missing {
            missing += 1;
            warnings += 1;
            lines.push(format!(
                "warning: {}: {MISSING_FILE_REASON}",
                file.rel_path()
            ));
            continue;
        }

        let mut file_has_error = false;
        for diagnostic in diagnostics {
            let label = match diagnostic.severity {
                ConfigSeverity::Error => {
                    errors += 1;
                    file_has_error = true;
                    "error"
                }
                ConfigSeverity::Warning => {
                    warnings += 1;
                    "warning"
                }
            };
            match &diagnostic.key {
                Some(key) => lines.push(format!(
                    "{label}: {}: {key}: {}",
                    file.rel_path(),
                    diagnostic.reason
                )),
                None => lines.push(format!(
                    "{label}: {}: {}",
                    file.rel_path(),
                    diagnostic.reason
                )),
            }
        }

        if !file_has_error {
            let values = REGISTRY.iter().filter(|row| row.file == file).count();
            lines.push(format!("ok: {}: {values} values", file.rel_path()));
        }
    }

    for name in extra_config_files(dir) {
        warnings += 1;
        lines.push(format!("warning: config/{name}: not a config file"));
    }

    lines.push(format!(
        "config: files={} missing={missing} warnings={warnings} errors={errors} \
         keys={} schema={} digest={:#018x}",
        ConfigFile::ALL.len(),
        REGISTRY.len(),
        runtime.schema(),
        runtime.digest(),
    ));

    (lines, errors == 0)
}

/// One file's diagnostics in the report's stable order: file-level first,
/// known keys by registry index, unknown keys lexicographically.
fn ordered_file_diagnostics(runtime: &ConfigRuntime, file: ConfigFile) -> Vec<&ConfigDiagnostic> {
    let mut file_level: Vec<&ConfigDiagnostic> = Vec::new();
    let mut known: Vec<(usize, &ConfigDiagnostic)> = Vec::new();
    let mut unknown: Vec<&ConfigDiagnostic> = Vec::new();

    for diagnostic in runtime
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.file == file)
    {
        match &diagnostic.key {
            None => file_level.push(diagnostic),
            Some(key) => match row_index_for(file, key) {
                Some(index) => known.push((index, diagnostic)),
                None => unknown.push(diagnostic),
            },
        }
    }

    known.sort_by_key(|(index, _)| *index);
    unknown.sort_by(|left, right| left.key.cmp(&right.key));

    let mut ordered = file_level;
    ordered.extend(known.into_iter().map(|(_, diagnostic)| diagnostic));
    ordered.extend(unknown);
    ordered
}

/// Sorted names of `*.toml` entries in `dir` that are not one of the six
/// curated file names. A missing or unreadable directory has no extras; the
/// six expected files still report their own state from the loader.
fn extra_config_files(dir: &Path) -> Vec<String> {
    let expected = ConfigFile::ALL.map(ConfigFile::name);
    let mut extras: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name.ends_with(".toml") && !expected.contains(&name.as_str())).then_some(name)
        })
        .collect();
    extras.sort();
    extras
}

// ---------------------------------------------------------------------------
// Per-file loading
// ---------------------------------------------------------------------------

/// Result of parsing one file before [`ConfigRuntime::load_dir`] merges it.
///
/// S02's per-file live reload reuses this seam: parse a candidate, then commit
/// its values/origins only when it produced no Error diagnostics.
struct FileLoad {
    file: ConfigFile,
    /// Whether a filesystem entry was observed. `false` only for `NotFound`.
    /// Live reload treats `present == false` as a valid reset to defaults.
    present: bool,
    /// Whether the file contents are blank after trimming. Startup ignores
    /// this (an empty file is all defaults); live reload rejects it as an
    /// incomplete write.
    contents_empty: bool,
    values: Vec<ConfigValue>,
    origins: Vec<ValueOrigin>,
    diagnostics: Vec<ConfigDiagnostic>,
    file_error: Option<String>,
}

/// Read one file's metadata and bytes, then delegate to [`parse_contents`].
///
/// `present` is `false` only for `ErrorKind::NotFound`; every other outcome
/// (metadata/read/size/parse failure) is `present == true` with a whole-file
/// Error, so live reload can tell a deletion from a broken file.
fn parse_file(dir: &Path, file: ConfigFile) -> FileLoad {
    let rows: Vec<&ConfigRow> = REGISTRY.iter().filter(|row| row.file == file).collect();
    let defaults: Vec<ConfigValue> = rows.iter().map(|row| row.default).collect();
    let default_origins = vec![ValueOrigin::Default; rows.len()];
    let path = dir.join(file.name());

    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return FileLoad {
                file,
                present: false,
                contents_empty: false,
                values: defaults,
                origins: default_origins,
                diagnostics: vec![ConfigDiagnostic {
                    file,
                    key: None,
                    severity: ConfigSeverity::Warning,
                    reason: MISSING_FILE_REASON.to_string(),
                }],
                file_error: None,
            };
        }
        Err(error) => {
            let reason = format!("could not inspect file: {error}");
            return whole_file_error(file, defaults, default_origins, reason);
        }
    };

    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return whole_file_error(
            file,
            defaults,
            default_origins,
            "file is larger than 1 MiB".to_string(),
        );
    }

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            let reason = format!("could not read file: {error}");
            return whole_file_error(file, defaults, default_origins, reason);
        }
    };

    parse_contents(file, &contents)
}

/// Parse already-read file contents into a [`FileLoad`].
///
/// This is the pure half of [`parse_file`]: no filesystem access, no writing.
/// Every existing reason string, severity, and diagnostic order is preserved
/// byte-for-byte so startup loading and `--check-config` output do not move.
fn parse_contents(file: ConfigFile, contents: &str) -> FileLoad {
    let contents_empty = contents.trim().is_empty();
    let rows: Vec<&ConfigRow> = REGISTRY.iter().filter(|row| row.file == file).collect();
    let defaults: Vec<ConfigValue> = rows.iter().map(|row| row.default).collect();
    let default_origins = vec![ValueOrigin::Default; rows.len()];

    let root: toml::Value = match toml::from_str(contents) {
        Ok(root) => root,
        Err(error) => {
            let reason = format!("invalid TOML: {}", single_line(&error.to_string()));
            return whole_file_error(file, defaults, default_origins, reason);
        }
    };

    let mut leaves = BTreeMap::new();
    if let toml::Value::Table(table) = &root {
        let mut prefix = String::new();
        flatten_table(&mut prefix, table, &mut leaves);
    }

    let unknown_diagnostics: Vec<ConfigDiagnostic> = leaves
        .keys()
        .filter(|key| row_index_for(file, key).is_none())
        .map(|key| ConfigDiagnostic {
            file,
            key: Some(key.clone()),
            severity: ConfigSeverity::Warning,
            reason: "unknown key".to_string(),
        })
        .collect();

    let mut values = defaults.clone();
    let mut origins = default_origins.clone();
    let mut field_diagnostics: Vec<Option<ConfigDiagnostic>> = vec![None; rows.len()];

    for (index, row) in rows.iter().enumerate() {
        let Some(raw) = leaves.get(row.key) else {
            continue;
        };
        match extract_value(row, raw) {
            Ok(value) => {
                values[index] = value;
                origins[index] = ValueOrigin::File(file);
            }
            Err(reason) => {
                field_diagnostics[index] = Some(ConfigDiagnostic {
                    file,
                    key: Some(row.key.to_string()),
                    severity: ConfigSeverity::Error,
                    reason,
                });
            }
        }
    }

    // Cross-field invariants use the effective values (file value or default)
    // and, when violated, reject the whole file: every row returns to its
    // default and one Error names the primary key plus the other participant.
    let mut cross_field = None;
    if let Some((primary_key, reason)) = cross_field_violation(file, &rows, &values) {
        let primary_index = rows
            .iter()
            .position(|row| row.key == primary_key)
            .expect("cross-field primary key belongs to its file");
        values = defaults.clone();
        origins = default_origins.clone();
        cross_field = Some((
            primary_index,
            ConfigDiagnostic {
                file,
                key: Some(primary_key.to_string()),
                severity: ConfigSeverity::Error,
                reason,
            },
        ));
    }

    // Keep diagnostics deterministic: file-level first, then known keys in
    // registry order, then unknown keys (BTreeMap iteration is sorted).
    let mut diagnostics = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if let Some(diagnostic) = field_diagnostics[index].take() {
            diagnostics.push(diagnostic);
        }
        if let Some((primary_index, diagnostic)) = &cross_field {
            if *primary_index == index {
                diagnostics.push(diagnostic.clone());
            }
        }
        if cross_field.is_some() {
            debug_assert_eq!(values[index], row.default);
        }
    }
    diagnostics.extend(unknown_diagnostics);

    FileLoad {
        file,
        present: true,
        contents_empty,
        values,
        origins,
        diagnostics,
        file_error: None,
    }
}

fn whole_file_error(
    file: ConfigFile,
    values: Vec<ConfigValue>,
    origins: Vec<ValueOrigin>,
    reason: String,
) -> FileLoad {
    FileLoad {
        file,
        present: true,
        contents_empty: false,
        values,
        origins,
        diagnostics: vec![ConfigDiagnostic {
            file,
            key: None,
            severity: ConfigSeverity::Error,
            reason: reason.clone(),
        }],
        file_error: Some(reason),
    }
}

fn flatten_table<'a>(
    prefix: &mut String,
    table: &'a toml::Table,
    leaves: &mut BTreeMap<String, &'a toml::Value>,
) {
    for (key, value) in table {
        let previous_len = prefix.len();
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(key);
        match value {
            toml::Value::Table(child) => flatten_table(prefix, child, leaves),
            _ => {
                leaves.insert(prefix.clone(), value);
            }
        }
        prefix.truncate(previous_len);
    }
}

fn row_index_for(file: ConfigFile, key: &str) -> Option<usize> {
    index_of(key).filter(|&index| REGISTRY[index].file == file)
}

fn extract_value(row: &ConfigRow, raw: &toml::Value) -> Result<ConfigValue, String> {
    match row.kind {
        ConfigKind::Float { min, max } => {
            let value = match raw {
                toml::Value::Float(value) => *value as f32,
                // Undotted integer literals are the natural way to type
                // `walk_speed = 5`; coerce them for Float rows.
                toml::Value::Integer(value) => *value as f32,
                other => return Err(type_error("float", other)),
            };
            if !value.is_finite() {
                return Err("value must be finite".to_string());
            }
            if value < min {
                return Err(format!("value {value} is below minimum {min}"));
            }
            if value > max {
                return Err(format!("value {value} is above maximum {max}"));
            }
            Ok(ConfigValue::Float(value))
        }
        ConfigKind::Int { min, max } => {
            let value = match raw {
                toml::Value::Integer(value) => *value,
                toml::Value::Float(value) => {
                    if !value.is_finite() {
                        return Err("value must be finite".to_string());
                    }
                    if value.fract() != 0.0 || *value < i64::MIN as f64 || *value > i64::MAX as f64
                    {
                        return Err(type_error("int", raw));
                    }
                    *value as i64
                }
                other => return Err(type_error("int", other)),
            };
            if value < min {
                return Err(format!("value {value} is below minimum {min}"));
            }
            if value > max {
                return Err(format!("value {value} is above maximum {max}"));
            }
            Ok(ConfigValue::Int(value))
        }
        ConfigKind::Bool => match raw {
            toml::Value::Boolean(value) => Ok(ConfigValue::Bool(*value)),
            other => Err(type_error("bool", other)),
        },
    }
}

fn type_error(expected: &str, actual: &toml::Value) -> String {
    format!("expected {expected}, got {}", actual.type_str())
}

fn effective_value(rows: &[&ConfigRow], values: &[ConfigValue], key: &str) -> ConfigValue {
    let index = rows
        .iter()
        .position(|row| row.key == key)
        .unwrap_or_else(|| panic!("cross-field key {key} is not in this file"));
    values[index]
}

fn expect_float(value: ConfigValue, key: &str) -> f32 {
    match value {
        ConfigValue::Float(value) => value,
        other => panic!("registry key {key} must be a float, got {other:?}"),
    }
}

fn expect_int(value: ConfigValue, key: &str) -> i64 {
    match value {
        ConfigValue::Int(value) => value,
        other => panic!("registry key {key} must be an int, got {other:?}"),
    }
}

fn cross_field_violation(
    file: ConfigFile,
    rows: &[&ConfigRow],
    values: &[ConfigValue],
) -> Option<(&'static str, String)> {
    let value = |key: &str| effective_value(rows, values, key);

    match file {
        ConfigFile::Player => {
            let near = expect_float(value("camera.z_near"), "camera.z_near");
            let far = expect_float(value("camera.z_far"), "camera.z_far");
            if near >= far {
                return Some((
                    "camera.z_near",
                    format!("{near} must be less than camera.z_far ({far})"),
                ));
            }
            None
        }
        ConfigFile::Combat => {
            let min_charge = expect_float(value("bow.min_charge_secs"), "bow.min_charge_secs");
            let full_charge = expect_float(value("bow.full_charge_secs"), "bow.full_charge_secs");
            if min_charge > full_charge {
                return Some((
                    "bow.min_charge_secs",
                    format!(
                        "{min_charge} must be less than or equal to \
                         bow.full_charge_secs ({full_charge})"
                    ),
                ));
            }

            let speed_min = expect_float(value("arrow.speed_min"), "arrow.speed_min");
            let speed_max = expect_float(value("arrow.speed_max"), "arrow.speed_max");
            if speed_min > speed_max {
                return Some((
                    "arrow.speed_min",
                    format!(
                        "{speed_min} must be less than or equal to \
                         arrow.speed_max ({speed_max})"
                    ),
                ));
            }

            let damage_min = expect_int(value("arrow.damage_min"), "arrow.damage_min");
            let damage_max = expect_int(value("arrow.damage_max"), "arrow.damage_max");
            if damage_min > damage_max {
                return Some((
                    "arrow.damage_min",
                    format!(
                        "{damage_min} must be less than or equal to \
                         arrow.damage_max ({damage_max})"
                    ),
                ));
            }
            None
        }
        ConfigFile::World => {
            let safe = expect_int(
                value("spawning.safe_radius_blocks"),
                "spawning.safe_radius_blocks",
            );
            let spawn_max = expect_int(
                value("spawning.spawn_max_radius_blocks"),
                "spawning.spawn_max_radius_blocks",
            );
            let despawn = expect_int(
                value("spawning.despawn_radius_blocks"),
                "spawning.despawn_radius_blocks",
            );
            if safe >= spawn_max {
                return Some((
                    "spawning.spawn_max_radius_blocks",
                    format!(
                        "{spawn_max} must be greater than \
                         spawning.safe_radius_blocks ({safe})"
                    ),
                ));
            }
            if spawn_max >= despawn {
                return Some((
                    "spawning.despawn_radius_blocks",
                    format!(
                        "{despawn} must be greater than \
                         spawning.spawn_max_radius_blocks ({spawn_max})"
                    ),
                ));
            }

            let region_cap = expect_int(
                value("spawning.spawn_region_cap"),
                "spawning.spawn_region_cap",
            );
            let max_total = expect_int(value("spawning.max_mobs_total"), "spawning.max_mobs_total");
            if region_cap >= max_total {
                return Some((
                    "spawning.spawn_region_cap",
                    format!(
                        "{region_cap} must be less than \
                         spawning.max_mobs_total ({max_total})"
                    ),
                ));
            }
            None
        }
        ConfigFile::Mining => {
            let wood = expect_int(
                value("tools.wood_speed_multiplier"),
                "tools.wood_speed_multiplier",
            );
            let stone = expect_int(
                value("tools.stone_speed_multiplier"),
                "tools.stone_speed_multiplier",
            );
            let iron = expect_int(
                value("tools.iron_speed_multiplier"),
                "tools.iron_speed_multiplier",
            );
            if wood > stone || stone > iron {
                return Some((
                    "tools.stone_speed_multiplier",
                    format!(
                        "tool speed multipliers must satisfy \
                         tools.wood_speed_multiplier ({wood}) <= \
                         tools.stone_speed_multiplier ({stone}) <= \
                         tools.iron_speed_multiplier ({iron})"
                    ),
                ));
            }
            None
        }
        ConfigFile::Survival | ConfigFile::Render => None,
    }
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const fn file_index(file: ConfigFile) -> usize {
    match file {
        ConfigFile::Player => 0,
        ConfigFile::Survival => 1,
        ConfigFile::Combat => 2,
        ConfigFile::World => 3,
        ConfigFile::Mining => 4,
        ConfigFile::Render => 5,
    }
}

// ---------------------------------------------------------------------------
// Digest
// ---------------------------------------------------------------------------

/// Minimal FNV-1a 64 sink duplicated from `content::registry`'s private sink.
///
/// The content digest is part of the pack contract and must not move, and the
/// save/config digest must stay decoupled from it, so this module owns its own
/// copy rather than widening the content one. Never uses `DefaultHasher`: the
/// digest must be reproducible across processes, platforms and Rust versions.
struct Digest(u64);

impl Digest {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Digest {
        Digest(Self::OFFSET_BASIS)
    }

    fn finish(self) -> u64 {
        self.0
    }

    fn byte(&mut self, value: u8) {
        self.0 ^= u64::from(value);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    fn bytes(&mut self, value: &[u8]) {
        for &byte in value {
            self.byte(byte);
        }
    }

    fn u8(&mut self, value: u8) {
        self.byte(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.bytes(&value.to_bits().to_le_bytes());
    }
}

/// FNV-1a 64 over the schema version and every effective value, with rows in
/// lexicographic key order.
///
/// The stream is: `u16 CONFIG_SCHEMA_VERSION`, then for each row
/// `u16 key_len`, key bytes, `u8 kind_tag`, and the value bits. Origins,
/// formatting, comments, units and descriptions are deliberately excluded.
fn compute_digest(values: &[ConfigValue]) -> u64 {
    let mut order: Vec<usize> = (0..REGISTRY.len()).collect();
    order.sort_by(|&left, &right| REGISTRY[left].key.cmp(REGISTRY[right].key));

    let mut digest = Digest::new();
    digest.u16(CONFIG_SCHEMA_VERSION);
    for index in order {
        let row = &REGISTRY[index];
        let key = row.key.as_bytes();
        digest.u16(u16::try_from(key.len()).unwrap_or(u16::MAX));
        digest.bytes(key);
        match values[index] {
            ConfigValue::Float(value) => {
                digest.u8(0);
                digest.f32(value);
            }
            ConfigValue::Int(value) => {
                digest.u8(1);
                digest.i64(value);
            }
            ConfigValue::Bool(value) => {
                digest.u8(2);
                digest.u8(u8::from(value));
            }
        }
    }
    digest.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Pinned defaults digest. Changing the digest stream format or a registry
    /// default must be a deliberate contract change (S04 persists this value).
    const DEFAULT_DIGEST: u64 = 0x7aa3_5ddb_db5a_30b7;

    fn tempdir_with_config() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(CONFIG_DIR);
        std::fs::create_dir(&config).unwrap();
        (temp, config)
    }

    fn write_file(config_dir: &Path, file: ConfigFile, contents: &str) {
        std::fs::write(config_dir.join(file.name()), contents).unwrap();
    }

    fn default_of(key: &str) -> ConfigValue {
        REGISTRY[index_of(key).expect("registry row")].default
    }

    fn diagnostics_for(runtime: &ConfigRuntime, file: ConfigFile) -> Vec<&ConfigDiagnostic> {
        runtime
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.file == file)
            .collect()
    }

    #[test]
    fn defaults_match_registry_and_have_no_diagnostics() {
        let runtime = ConfigRuntime::defaults();
        assert_eq!(runtime.schema(), CONFIG_SCHEMA_VERSION);
        assert!(runtime.diagnostics().is_empty());
        for row in REGISTRY {
            assert_eq!(runtime.value(row.key), row.default, "{}", row.key);
            assert_eq!(runtime.origin(row.key), ValueOrigin::Default, "{}", row.key);
        }
        for file in ConfigFile::ALL {
            assert!(runtime.file_error(file).is_none(), "{}", file.name());
        }
    }

    #[test]
    fn rules_bundle_matches_defaults_and_reads_effective_values() {
        assert_eq!(ConfigRuntime::defaults().rules(), RulesBundle::default());

        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 9.0\n\n[camera]\nz_far = 512.0\n",
        );
        let loaded = ConfigRuntime::load_dir(&config);
        let rules = loaded.rules();

        assert_eq!(rules.movement.walk_speed, 9.0);
        assert_eq!(
            rules.movement.sprint_speed,
            MovementRules::default().sprint_speed,
            "an absent movement field must keep its registry default"
        );
        assert_eq!(rules.camera.z_far, 512.0);
        assert_eq!(
            rules.camera.z_near,
            CameraRules::default().z_near,
            "an absent camera field must keep its registry default"
        );
    }

    #[test]
    fn rules_bundle_reads_survival_eating_and_combat_domains() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Survival,
            "[survival]\nfall_safe_blocks = 9.0\ndrown_damage = 5\nhunger_drain_secs = 2.5\n\n[eating]\neat_duration_secs = 0.75\napple_hunger = 7\n",
        );
        write_file(
            &config,
            ConfigFile::Combat,
            "[melee]\nreach = 7.5\nfist_damage = 4\n\n[player]\niframe_secs = 1.25\nknockback_speed = 12.0\n\n[bow]\nfull_charge_secs = 0.5\n\n[arrow]\nspeed_max = 48.0\ndamage_max = 9\nlifetime_secs = 12.0\n",
        );

        let rules = ConfigRuntime::load_dir(&config).rules();
        assert_eq!(rules.survival.fall_safe_blocks, 9.0);
        assert_eq!(rules.survival.drown_damage, 5);
        assert_eq!(rules.survival.hunger_drain_secs, 2.5);
        assert_eq!(rules.eating.eat_duration_secs, 0.75);
        assert_eq!(rules.eating.apple_hunger, 7);
        assert_eq!(rules.melee.reach, 7.5);
        assert_eq!(rules.melee.fist_damage, 4);
        assert_eq!(rules.combat.iframe_secs, 1.25);
        assert_eq!(rules.combat.knockback_speed, 12.0);
        assert_eq!(rules.bow.full_charge_secs, 0.5);
        assert_eq!(rules.arrow.speed_max, 48.0);
        assert_eq!(rules.arrow.damage_max, 9);
        assert_eq!(rules.arrow.lifetime_secs, 12.0);

        // Absent rows keep their registry defaults; a partial file is not a
        // reason to re-default its siblings.
        assert_eq!(
            rules.survival.air_drain_per_sec,
            SurvivalRules::default().air_drain_per_sec
        );
        assert_eq!(
            rules.combat.hurt_flash_secs,
            CombatRules::default().hurt_flash_secs
        );
        assert_eq!(rules.arrow.gravity, ArrowRules::default().gravity);
        assert_eq!(
            rules.melee.swing_cooldown_secs,
            MeleeRules::default().swing_cooldown_secs
        );
    }

    #[test]
    fn rules_bundle_reads_world_mining_and_render_domains() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::World,
            "[clock]\nday_seconds = 600.0\n\n[weather]\nwinter_snow_percent = 50\nspring_rain_percent = 25\nsummer_rain_percent = 10\nautumn_rain_percent = 45\n\n[spawning]\nsafe_radius_blocks = 20\nspawn_max_radius_blocks = 40\ndespawn_radius_blocks = 80\nmax_mobs_total = 40\nspawn_region_blocks = 24\nspawn_region_cap = 6\nspawn_attempt_secs = 0.5\ndespawn_scan_secs = 2.0\nburn_interval_secs = 1.0\nspawn_min_separation = 2.0\nhostile_cave_light = 5\npassive_surface_light = 12\nmax_spawn_attempts_per_call = 8\n\n[farming]\nwheat_stage_game_secs = 30.0\n",
        );
        write_file(
            &config,
            ConfigFile::Mining,
            "[tools]\nwood_speed_multiplier = 8\nstone_speed_multiplier = 12\niron_speed_multiplier = 16\n\n[interaction]\ntable_reach = 12.0\n",
        );
        write_file(
            &config,
            ConfigFile::Render,
            "[fog]\nrain_factor = 0.5\nsnow_factor = 0.6\n\n[palette]\nrain_grey_scale = 0.8\nsnow_grey_scale = 1.3\nrain_sky_lerp = 0.4\nsnow_sky_lerp = 0.7\nrain_day_factor = 0.6\nsnow_day_factor = 0.8\nmoonlit_floor = 0.2\n\n[ui]\nselected_name_secs = 0.5\n",
        );

        let rules = ConfigRuntime::load_dir(&config).rules();

        assert_eq!(rules.clock.day_seconds, 600.0);
        assert_eq!(rules.weather.winter_snow_percent, 50);
        assert_eq!(rules.weather.spring_rain_percent, 25);
        assert_eq!(rules.weather.summer_rain_percent, 10);
        assert_eq!(rules.weather.autumn_rain_percent, 45);
        assert_eq!(rules.spawning.safe_radius_blocks, 20);
        assert_eq!(rules.spawning.spawn_max_radius_blocks, 40);
        assert_eq!(rules.spawning.despawn_radius_blocks, 80);
        assert_eq!(rules.spawning.max_mobs_total, 40);
        assert_eq!(rules.spawning.spawn_region_blocks, 24);
        assert_eq!(rules.spawning.spawn_region_cap, 6);
        assert_eq!(rules.spawning.spawn_attempt_secs, 0.5);
        assert_eq!(rules.spawning.despawn_scan_secs, 2.0);
        assert_eq!(rules.spawning.burn_interval_secs, 1.0);
        assert_eq!(rules.spawning.spawn_min_separation, 2.0);
        assert_eq!(rules.spawning.hostile_cave_light, 5);
        assert_eq!(rules.spawning.passive_surface_light, 12);
        assert_eq!(rules.spawning.max_spawn_attempts_per_call, 8);
        assert_eq!(rules.farming.wheat_stage_game_secs, 30.0);
        assert_eq!(rules.tools.wood_speed_multiplier, 8);
        assert_eq!(rules.tools.stone_speed_multiplier, 12);
        assert_eq!(rules.tools.iron_speed_multiplier, 16);
        assert_eq!(rules.interaction.table_reach, 12.0);
        assert_eq!(rules.render.rain_fog_factor, 0.5);
        assert_eq!(rules.render.snow_fog_factor, 0.6);
        assert_eq!(rules.render.rain_grey_scale, 0.8);
        assert_eq!(rules.render.snow_grey_scale, 1.3);
        assert_eq!(rules.render.rain_sky_lerp, 0.4);
        assert_eq!(rules.render.snow_sky_lerp, 0.7);
        assert_eq!(rules.render.rain_day_factor, 0.6);
        assert_eq!(rules.render.snow_day_factor, 0.8);
        assert_eq!(rules.render.moonlit_floor, 0.2);
        assert_eq!(rules.selection.selected_name_secs, 0.5);

        // Files outside this domain were not written, so their rows must keep
        // their registry defaults.
        assert_eq!(
            rules.movement.walk_speed,
            MovementRules::default().walk_speed
        );
        assert_eq!(rules.arrow.gravity, ArrowRules::default().gravity);
    }

    #[test]
    fn checked_in_config_tree_is_complete_and_default() {
        let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_DIR);
        let runtime = ConfigRuntime::load_dir(&config_dir);

        assert_eq!(REGISTRY.len(), 85);
        assert!(
            runtime.diagnostics().is_empty(),
            "checked-in config tree must load without diagnostics: {:?}",
            runtime.diagnostics()
        );
        assert_eq!(runtime.digest(), ConfigRuntime::defaults().digest());
        for row in REGISTRY {
            assert_eq!(runtime.value(row.key), row.default, "{}", row.key);
            assert_eq!(
                runtime.origin(row.key),
                ValueOrigin::File(row.file),
                "{}",
                row.key
            );
        }
    }

    #[test]
    fn default_digest_is_pinned_and_stable() {
        let first = ConfigRuntime::defaults();
        let second = ConfigRuntime::defaults();
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.digest(), DEFAULT_DIGEST);
        assert_eq!(
            first.meta(),
            SaveConfigMeta {
                schema: CONFIG_SCHEMA_VERSION,
                digest: DEFAULT_DIGEST,
            }
        );

        // A missing directory is all defaults, so the digest must match.
        let temp = tempfile::tempdir().unwrap();
        let missing = ConfigRuntime::load_dir(&temp.path().join("absent"));
        assert_eq!(missing.digest(), DEFAULT_DIGEST);
    }

    #[test]
    fn missing_dir_yields_six_warnings_and_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = ConfigRuntime::load_dir(&temp.path().join("absent"));

        let warnings: Vec<&ConfigDiagnostic> = runtime
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Warning)
            .collect();
        assert_eq!(warnings.len(), 6);
        for warning in warnings {
            assert_eq!(warning.key, None);
            assert_eq!(warning.reason, "file is missing; using defaults");
        }
        assert!(runtime
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.severity == ConfigSeverity::Warning));
        assert!(ConfigFile::ALL
            .iter()
            .all(|&file| runtime.file_error(file).is_none()));
        assert!(startup_warnings(&runtime).is_empty());
        for row in REGISTRY {
            assert_eq!(runtime.value(row.key), row.default);
            assert_eq!(runtime.origin(row.key), ValueOrigin::Default);
        }
    }

    #[test]
    fn missing_file_keeps_other_files_and_warns() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);
        assert_eq!(
            runtime.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );

        let player_diagnostics = diagnostics_for(&runtime, ConfigFile::Player);
        assert!(player_diagnostics.is_empty());
        for file in ConfigFile::ALL {
            if file == ConfigFile::Player {
                continue;
            }
            let diagnostics = diagnostics_for(&runtime, file);
            assert_eq!(diagnostics.len(), 1, "{}", file.name());
            assert_eq!(diagnostics[0].severity, ConfigSeverity::Warning);
            assert_eq!(diagnostics[0].key, None);
            assert_eq!(diagnostics[0].reason, "file is missing; using defaults");
        }
    }

    #[test]
    fn invalid_toml_falls_back_whole_file_only() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        write_file(&config, ConfigFile::Survival, "[survival\n");

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);
        assert_eq!(
            runtime.value("survival.fall_safe_blocks"),
            default_of("survival.fall_safe_blocks")
        );
        assert_eq!(
            runtime.origin("survival.fall_safe_blocks"),
            ValueOrigin::Default
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Survival);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, ConfigSeverity::Error);
        assert_eq!(diagnostics[0].key, None);
        assert!(
            diagnostics[0].reason.starts_with("invalid TOML: "),
            "{}",
            diagnostics[0].reason
        );
        assert!(!diagnostics[0].reason.contains('\n'));
        assert!(runtime
            .file_error(ConfigFile::Survival)
            .unwrap()
            .starts_with("invalid TOML: "));
    }

    #[test]
    fn unknown_keys_warn_and_are_ignored_sorted() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\nzzz_unknown = 1\naaa_unknown = 2\n\
             [camera]\nmystery = true\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);
        assert_eq!(runtime.f32("camera.z_near"), {
            match default_of("camera.z_near") {
                ConfigValue::Float(value) => value,
                _ => unreachable!(),
            }
        });

        let unknown: Vec<&ConfigDiagnostic> = diagnostics_for(&runtime, ConfigFile::Player)
            .into_iter()
            .filter(|diagnostic| {
                diagnostic
                    .key
                    .as_deref()
                    .is_some_and(|key| key.ends_with("unknown") || key == "camera.mystery")
            })
            .collect();
        let keys: Vec<&str> = unknown
            .iter()
            .filter_map(|diagnostic| diagnostic.key.as_deref())
            .collect();
        assert_eq!(
            keys,
            vec![
                "camera.mystery",
                "movement.aaa_unknown",
                "movement.zzz_unknown"
            ]
        );
        for diagnostic in unknown {
            assert_eq!(diagnostic.severity, ConfigSeverity::Warning);
            assert_eq!(diagnostic.reason, "unknown key");
        }

        // Unknown keys are silent at startup and do not move the digest.
        assert!(startup_warnings(&runtime).is_empty());
        let (_temp2, config2) = tempdir_with_config();
        write_file(
            &config2,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        assert_eq!(runtime.digest(), ConfigRuntime::load_dir(&config2).digest());
    }

    #[test]
    fn type_error_falls_back_only_that_field() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = \"fast\"\nsprint_speed = 8.0\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("movement.walk_speed"),
            default_of("movement.walk_speed")
        );
        assert_eq!(runtime.origin("movement.walk_speed"), ValueOrigin::Default);
        assert_eq!(runtime.f32("movement.sprint_speed"), 8.0);
        assert_eq!(
            runtime.origin("movement.sprint_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Player);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, ConfigSeverity::Error);
        assert_eq!(diagnostics[0].key.as_deref(), Some("movement.walk_speed"));
        assert_eq!(diagnostics[0].reason, "expected float, got string");
        assert!(runtime.file_error(ConfigFile::Player).is_none());
    }

    #[test]
    fn out_of_range_falls_back_to_default() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0\n[camera]\nz_far = 9000.0\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("movement.walk_speed"),
            default_of("movement.walk_speed")
        );
        assert_eq!(runtime.value("camera.z_far"), default_of("camera.z_far"));

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Player);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].key.as_deref(), Some("movement.walk_speed"));
        assert_eq!(diagnostics[0].reason, "value 0 is below minimum 0.5");
        assert_eq!(diagnostics[1].key.as_deref(), Some("camera.z_far"));
        assert_eq!(diagnostics[1].reason, "value 9000 is above maximum 8192");
    }

    #[test]
    fn non_finite_floats_fall_back_to_default() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = nan\nsprint_speed = inf\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("movement.walk_speed"),
            default_of("movement.walk_speed")
        );
        assert_eq!(
            runtime.value("movement.sprint_speed"),
            default_of("movement.sprint_speed")
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Player);
        assert_eq!(diagnostics.len(), 2);
        for diagnostic in diagnostics {
            assert_eq!(diagnostic.severity, ConfigSeverity::Error);
            assert_eq!(diagnostic.reason, "value must be finite");
        }
    }

    #[test]
    fn numeric_coercion_accepts_integers_and_integral_floats() {
        let (_temp, config) = tempdir_with_config();
        write_file(&config, ConfigFile::Player, "[movement]\nwalk_speed = 5\n");
        write_file(
            &config,
            ConfigFile::Survival,
            "[survival]\ndrown_damage = 3.0\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("movement.walk_speed"),
            ConfigValue::Float(5.0)
        );
        assert_eq!(runtime.value("survival.drown_damage"), ConfigValue::Int(3));
        assert_eq!(
            runtime.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        assert_eq!(
            runtime.origin("survival.drown_damage"),
            ValueOrigin::File(ConfigFile::Survival)
        );
        for file in [ConfigFile::Player, ConfigFile::Survival] {
            let errors: Vec<&ConfigDiagnostic> = diagnostics_for(&runtime, file)
                .into_iter()
                .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
                .collect();
            assert!(errors.is_empty(), "{}", file.name());
        }
    }

    #[test]
    fn non_integral_float_for_int_row_is_type_error() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Survival,
            "[survival]\ndrown_damage = 3.5\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("survival.drown_damage"),
            default_of("survival.drown_damage")
        );
        assert_eq!(
            runtime.origin("survival.drown_damage"),
            ValueOrigin::Default
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Survival);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].key.as_deref(), Some("survival.drown_damage"));
        assert_eq!(diagnostics[0].reason, "expected int, got float");
    }

    #[test]
    fn cross_field_violation_rejects_whole_file() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Mining,
            "[tools]\nwood_speed_multiplier = 5\nstone_speed_multiplier = 3\n\
             iron_speed_multiplier = 10\n[interaction]\ntable_reach = 8.0\n",
        );
        write_file(
            &config,
            ConfigFile::Survival,
            "[survival]\nfall_safe_blocks = 5.0\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);

        // The whole mining file fell back, including the otherwise valid
        // table_reach and wood/iron values.
        for key in [
            "tools.wood_speed_multiplier",
            "tools.stone_speed_multiplier",
            "tools.iron_speed_multiplier",
            "interaction.table_reach",
        ] {
            assert_eq!(runtime.value(key), default_of(key), "{key}");
            assert_eq!(runtime.origin(key), ValueOrigin::Default, "{key}");
        }

        // A sibling file keeps its file values.
        assert_eq!(runtime.f32("survival.fall_safe_blocks"), 5.0);
        assert_eq!(
            runtime.origin("survival.fall_safe_blocks"),
            ValueOrigin::File(ConfigFile::Survival)
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Mining);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, ConfigSeverity::Error);
        assert_eq!(
            diagnostics[0].key.as_deref(),
            Some("tools.stone_speed_multiplier")
        );
        assert_eq!(
            diagnostics[0].reason,
            "tool speed multipliers must satisfy tools.wood_speed_multiplier (5) <= \
             tools.stone_speed_multiplier (3) <= tools.iron_speed_multiplier (10)"
        );
        assert!(startup_warnings(&runtime)
            .iter()
            .any(|warning| warning.field == "config/mining.toml: tools.stone_speed_multiplier"));
    }

    #[test]
    fn world_cross_field_error_names_both_radius_keys() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nsafe_radius_blocks = 24\nspawn_max_radius_blocks = 10\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("spawning.safe_radius_blocks"),
            default_of("spawning.safe_radius_blocks")
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::World);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].key.as_deref(),
            Some("spawning.spawn_max_radius_blocks")
        );
        assert_eq!(
            diagnostics[0].reason,
            "10 must be greater than spawning.safe_radius_blocks (24)"
        );
    }

    #[test]
    fn combat_cross_field_invariants_reject_whole_file() {
        let cases = [
            (
                "[bow]\nmin_charge_secs = 5.0\nfull_charge_secs = 2.0\n",
                "bow.min_charge_secs",
                "5 must be less than or equal to bow.full_charge_secs (2)",
            ),
            (
                "[arrow]\nspeed_min = 100.0\nspeed_max = 50.0\n",
                "arrow.speed_min",
                "100 must be less than or equal to arrow.speed_max (50)",
            ),
            (
                "[arrow]\ndamage_min = 10\ndamage_max = 5\n",
                "arrow.damage_min",
                "10 must be less than or equal to arrow.damage_max (5)",
            ),
        ];

        for (body, expected_key, expected_reason) in cases {
            let (_temp, config) = tempdir_with_config();
            write_file(&config, ConfigFile::Combat, body);
            let runtime = ConfigRuntime::load_dir(&config);

            for row in REGISTRY.iter().filter(|row| row.file == ConfigFile::Combat) {
                assert_eq!(runtime.origin(row.key), ValueOrigin::Default, "{body}");
            }
            let diagnostics = diagnostics_for(&runtime, ConfigFile::Combat);
            assert_eq!(diagnostics.len(), 1, "{body}");
            assert_eq!(diagnostics[0].key.as_deref(), Some(expected_key), "{body}");
            assert_eq!(diagnostics[0].reason, expected_reason, "{body}");
        }
    }

    #[test]
    fn world_cross_field_checks_cover_despawn_and_region_bounds() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nspawn_max_radius_blocks = 100\ndespawn_radius_blocks = 50\n",
        );
        let runtime = ConfigRuntime::load_dir(&config);
        let diagnostics = diagnostics_for(&runtime, ConfigFile::World);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].key.as_deref(),
            Some("spawning.despawn_radius_blocks")
        );
        assert_eq!(
            diagnostics[0].reason,
            "50 must be greater than spawning.spawn_max_radius_blocks (100)"
        );

        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nspawn_region_cap = 10\nmax_mobs_total = 5\n",
        );
        let runtime = ConfigRuntime::load_dir(&config);
        let diagnostics = diagnostics_for(&runtime, ConfigFile::World);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].key.as_deref(),
            Some("spawning.spawn_region_cap")
        );
        assert_eq!(
            diagnostics[0].reason,
            "10 must be less than spawning.max_mobs_total (5)"
        );
    }

    #[test]
    fn origins_track_file_versus_default() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 4.6\nsprint_speed = 8.0\ngravity = 0\n",
        );

        let runtime = ConfigRuntime::load_dir(&config);
        // A file value equal to the default is still File-sourced.
        assert_eq!(
            runtime.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        assert_eq!(
            runtime.origin("movement.sprint_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        // Absent and invalid keys are Default-sourced.
        assert_eq!(
            runtime.origin("movement.jump_velocity"),
            ValueOrigin::Default
        );
        assert_eq!(runtime.origin("movement.gravity"), ValueOrigin::Default);
    }

    #[test]
    fn digest_ignores_formatting_and_tracks_values() {
        let (_temp_a, config_a) = tempdir_with_config();
        write_file(
            &config_a,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 5\nsprint_speed = 7.2\n",
        );

        let (_temp_b, config_b) = tempdir_with_config();
        write_file(
            &config_b,
            ConfigFile::Player,
            "# tuned by hand\n[movement] # speed block\nwalk_speed = 5.0 # blocks/s\n\
             sprint_speed = 7.2\n",
        );

        let a = ConfigRuntime::load_dir(&config_a);
        let b = ConfigRuntime::load_dir(&config_b);
        assert_eq!(a.digest(), b.digest());

        let (_temp_c, config_c) = tempdir_with_config();
        write_file(
            &config_c,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\nsprint_speed = 7.2\n",
        );
        let c = ConfigRuntime::load_dir(&config_c);
        assert_ne!(a.digest(), c.digest());
        assert_eq!(
            a.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        assert_eq!(
            c.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
    }

    #[test]
    fn startup_warnings_include_only_errors_with_exact_field_shape() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0\nmystery = 1\n",
        );
        write_file(&config, ConfigFile::Survival, "[survival\n");

        let runtime = ConfigRuntime::load_dir(&config);
        let warnings = startup_warnings(&runtime);
        assert_eq!(warnings.len(), 2);

        let player = warnings
            .iter()
            .find(|warning| warning.field == "config/player.toml: movement.walk_speed")
            .expect("per-key error warning");
        assert_eq!(player.reason, "error: value 0 is below minimum 0.5");

        let survival = warnings
            .iter()
            .find(|warning| warning.field == "config/survival.toml")
            .expect("file-level error warning");
        assert!(survival.reason.starts_with("error: invalid TOML: "));
    }

    #[test]
    fn load_dir_never_writes_or_creates_files() {
        let (temp, config) = tempdir_with_config();
        let player = config.join(ConfigFile::Player.name());
        std::fs::write(&player, "[movement]\nwalk_speed = 6.0\n").unwrap();

        let before_entries: Vec<std::ffi::OsString> = std::fs::read_dir(&config)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let before_temp_entries = std::fs::read_dir(temp.path()).unwrap().count();
        let before_bytes = std::fs::read(&player).unwrap();

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);

        let after_entries: Vec<std::ffi::OsString> = std::fs::read_dir(&config)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before_entries, after_entries);
        assert_eq!(std::fs::read(&player).unwrap(), before_bytes);
        // Only the `config` directory exists in the temp root; no missing
        // files were created.
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            before_temp_entries
        );
    }

    #[test]
    fn defaults_performs_no_io() {
        // Guard the contract that `defaults()` is pure: no filesystem calls in
        // its body. (`load_dir` is the only I/O entry point.)
        let source = include_str!("mod.rs");
        let start = source.find("pub fn defaults()").expect("defaults exists");
        let rest = &source[start + 1..];
        let end = rest
            .find("\n    pub fn ")
            .map(|offset| start + 1 + offset)
            .unwrap_or(source.len());
        let body = &source[start..end];
        assert!(
            !body.contains("std::fs") && !body.contains("fs::"),
            "defaults() must not perform I/O: {body}"
        );
    }

    #[test]
    fn oversize_file_is_rejected_whole() {
        let (_temp, config) = tempdir_with_config();
        let path = config.join(ConfigFile::Player.name());
        std::fs::write(&path, vec![b' '; (MAX_CONFIG_FILE_BYTES + 1) as usize]).unwrap();

        let runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(
            runtime.value("movement.walk_speed"),
            default_of("movement.walk_speed")
        );

        let diagnostics = diagnostics_for(&runtime, ConfigFile::Player);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, ConfigSeverity::Error);
        assert_eq!(diagnostics[0].key, None);
        assert_eq!(diagnostics[0].reason, "file is larger than 1 MiB");
        assert_eq!(
            runtime.file_error(ConfigFile::Player),
            Some("file is larger than 1 MiB")
        );
    }

    #[test]
    fn read_error_is_rejected_whole() {
        let (_temp, config) = tempdir_with_config();
        // A directory where the file should be makes metadata succeed but the
        // UTF-8 read fail.
        std::fs::create_dir(config.join(ConfigFile::Player.name())).unwrap();

        let runtime = ConfigRuntime::load_dir(&config);
        let diagnostics = diagnostics_for(&runtime, ConfigFile::Player);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, ConfigSeverity::Error);
        assert_eq!(diagnostics[0].key, None);
        assert!(
            diagnostics[0].reason.starts_with("could not read file: "),
            "{}",
            diagnostics[0].reason
        );
        assert!(runtime.file_error(ConfigFile::Player).is_some());
    }

    #[test]
    fn integer_accessors_narrow_checked_values() {
        let runtime = ConfigRuntime::defaults();
        let key = "survival.drown_damage";
        let ConfigValue::Int(value) = default_of(key) else {
            panic!("{key} must default to an int");
        };
        assert_eq!(runtime.i64(key), value);
        assert_eq!(runtime.u32(key), value as u32);
        assert_eq!(runtime.u16(key), value as u16);
        assert_eq!(runtime.u8(key), value as u8);
    }

    #[test]
    #[should_panic(expected = "unknown config key: movement.not_a_key")]
    fn accessor_panics_on_unknown_key() {
        ConfigRuntime::defaults().f32("movement.not_a_key");
    }

    #[test]
    #[should_panic(expected = "config key movement.walk_speed is not a bool")]
    fn accessor_panics_on_wrong_kind() {
        ConfigRuntime::defaults().bool("movement.walk_speed");
    }

    // -----------------------------------------------------------------
    // --check-config report
    // -----------------------------------------------------------------

    /// Copy the git-tracked `config/` tree so tests can mutate a real tree
    /// without touching the checkout.
    fn tempdir_with_checked_in_config() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(CONFIG_DIR);
        std::fs::create_dir(&config).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_DIR);
        for file in ConfigFile::ALL {
            std::fs::copy(source.join(file.name()), config.join(file.name())).unwrap();
        }
        (temp, config)
    }

    fn expected_ok_line(file: ConfigFile) -> String {
        let values = REGISTRY.iter().filter(|row| row.file == file).count();
        format!("ok: {}: {values} values", file.rel_path())
    }

    fn summary_digest(lines: &[String]) -> &str {
        lines
            .last()
            .expect("summary line")
            .rsplit_once("digest=0x")
            .expect("summary digest")
            .1
    }

    fn assert_lower_hex_digest(digest: &str) {
        assert_eq!(digest.len(), 16, "digest {digest:?} must be 16 hex digits");
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "digest {digest:?} must be lowercase hex"
        );
    }

    #[test]
    fn check_config_accepts_checked_in_tree() {
        let (_temp, config) = tempdir_with_checked_in_config();
        let (lines, ok) = check_config_lines(&config);

        assert!(ok, "checked-in tree must pass: {lines:#?}");
        for file in ConfigFile::ALL {
            assert!(
                lines.contains(&expected_ok_line(file)),
                "{} missing from {lines:#?}",
                file.rel_path()
            );
        }
        assert_eq!(
            lines.iter().filter(|line| line.starts_with("ok: ")).count(),
            6
        );

        let digest = format!("{:#018x}", ConfigRuntime::defaults().digest());
        assert_eq!(
            lines.last().unwrap(),
            &format!(
                "config: files=6 missing=0 warnings=0 errors=0 keys=85 schema=1 digest={digest}"
            )
        );
        assert_lower_hex_digest(summary_digest(&lines));
    }

    #[test]
    fn check_config_reports_field_error_and_omits_ok_line() {
        let (_temp, config) = tempdir_with_checked_in_config();
        write_file(&config, ConfigFile::Player, "[movement]\nwalk_speed = 0\n");

        let (lines, ok) = check_config_lines(&config);
        assert!(!ok);

        let expected =
            "error: config/player.toml: movement.walk_speed: value 0 is below minimum 0.5";
        assert_eq!(
            lines.iter().filter(|line| *line == expected).count(),
            1,
            "{lines:#?}"
        );
        assert!(!lines.contains(&expected_ok_line(ConfigFile::Player)));
        let summary = lines.last().unwrap();
        assert!(summary.contains("missing=0"), "{summary}");
        assert!(summary.contains("warnings=0"), "{summary}");
        assert!(summary.contains("errors=1"), "{summary}");
    }

    #[test]
    fn check_config_cross_field_error_names_both_keys_and_falls_back_whole_file() {
        let (_temp, config) = tempdir_with_checked_in_config();
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nspawn_max_radius_blocks = 10\n",
        );

        let (lines, ok) = check_config_lines(&config);
        assert!(!ok);

        let error = lines
            .iter()
            .find(|line| {
                line.starts_with("error: config/world.toml: spawning.spawn_max_radius_blocks:")
            })
            .unwrap_or_else(|| panic!("cross-field error line missing: {lines:#?}"));
        assert!(
            error.contains("spawning.spawn_max_radius_blocks"),
            "{error}"
        );
        assert!(error.contains("spawning.safe_radius_blocks"), "{error}");
        assert!(!lines.contains(&expected_ok_line(ConfigFile::World)));

        // The whole world file fell back, so the effective digest is the
        // registry-default digest even though the file exists and parses.
        let defaults = format!("{:016x}", ConfigRuntime::defaults().digest());
        assert_eq!(summary_digest(&lines), defaults.as_str());
    }

    #[test]
    fn check_config_missing_file_warns_without_failing() {
        let (_temp, config) = tempdir_with_checked_in_config();
        std::fs::remove_file(config.join(ConfigFile::Player.name())).unwrap();

        let (lines, ok) = check_config_lines(&config);
        assert!(ok);
        assert!(lines
            .contains(&"warning: config/player.toml: file is missing; using defaults".to_string()));
        assert!(!lines.contains(&expected_ok_line(ConfigFile::Player)));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("warning: config/"))
                .count(),
            1,
            "{lines:#?}"
        );
        let summary = lines.last().unwrap();
        assert!(summary.contains("files=6 missing=1"), "{summary}");
        assert!(summary.contains("warnings=1 errors=0"), "{summary}");
    }

    #[test]
    fn check_config_empty_or_missing_dir_reports_six_missing_files() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(CONFIG_DIR);
        std::fs::create_dir(&config).unwrap();

        for dir in [config, temp.path().join("absent")] {
            let (lines, ok) = check_config_lines(&dir);
            assert!(ok, "{dir:?}");
            for file in ConfigFile::ALL {
                assert!(
                    lines.contains(&format!(
                        "warning: {}: file is missing; using defaults",
                        file.rel_path()
                    )),
                    "{} missing from {lines:#?}",
                    file.rel_path()
                );
            }
            assert_eq!(
                lines.iter().filter(|line| line.starts_with("ok: ")).count(),
                0
            );
            let summary = lines.last().unwrap();
            assert!(
                summary.contains("files=6 missing=6 warnings=6 errors=0"),
                "{summary}"
            );
            assert!(summary.contains("keys=85 schema=1"), "{summary}");
        }
    }

    #[test]
    fn check_config_unknown_key_warns_and_still_passes() {
        let (_temp, config) = tempdir_with_checked_in_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 5.0\nmystery = 1\n",
        );

        let (lines, ok) = check_config_lines(&config);
        assert!(ok);
        assert!(lines
            .contains(&"warning: config/player.toml: movement.mystery: unknown key".to_string()));
        assert!(lines.contains(&expected_ok_line(ConfigFile::Player)));
        assert!(lines.last().unwrap().contains("warnings=1 errors=0"));
    }

    #[test]
    fn check_config_extra_toml_warns_sorted_and_still_passes() {
        let (_temp, config) = tempdir_with_checked_in_config();
        std::fs::write(config.join("plyer.toml"), "[movement]\n").unwrap();
        std::fs::write(config.join("aaa.toml"), "").unwrap();

        let (lines, ok) = check_config_lines(&config);
        assert!(ok);

        let aaa = lines
            .iter()
            .position(|line| line == "warning: config/aaa.toml: not a config file")
            .expect("aaa.toml warning");
        let plyer = lines
            .iter()
            .position(|line| line == "warning: config/plyer.toml: not a config file")
            .expect("plyer.toml warning");
        assert!(aaa < plyer, "extra files must be sorted: {lines:#?}");
        assert!(plyer < lines.len() - 1, "summary must stay last");
        assert!(lines.last().unwrap().contains("warnings=2 errors=0"));
    }

    #[test]
    fn check_config_reports_fatal_file_errors() {
        struct Case {
            file: ConfigFile,
            contents: Vec<u8>,
            expected: &'static str,
        }

        let cases = [
            Case {
                file: ConfigFile::Survival,
                contents: b"[survival\n".to_vec(),
                expected: "error: config/survival.toml: invalid TOML: ",
            },
            Case {
                file: ConfigFile::Player,
                contents: b"[movement]\nwalk_speed = \"fast\"\n".to_vec(),
                expected:
                    "error: config/player.toml: movement.walk_speed: expected float, got string",
            },
            Case {
                file: ConfigFile::Player,
                contents: b"[movement]\nwalk_speed = nan\n".to_vec(),
                expected: "error: config/player.toml: movement.walk_speed: value must be finite",
            },
            Case {
                file: ConfigFile::Player,
                contents: vec![b' '; (MAX_CONFIG_FILE_BYTES + 1) as usize],
                expected: "error: config/player.toml: file is larger than 1 MiB",
            },
        ];

        for case in cases {
            let (_temp, config) = tempdir_with_checked_in_config();
            std::fs::write(config.join(case.file.name()), &case.contents).unwrap();

            let (lines, ok) = check_config_lines(&config);
            assert!(!ok, "{} must fail: {lines:#?}", case.expected);
            let found = lines.iter().any(|line| {
                if case.expected.ends_with("invalid TOML: ") {
                    line.starts_with(case.expected)
                } else {
                    line == case.expected
                }
            });
            assert!(found, "expected {:?} in {lines:#?}", case.expected);
            assert!(
                !lines.contains(&expected_ok_line(case.file)),
                "{} must not print ok",
                case.file.name()
            );
            assert!(lines.last().unwrap().contains("errors=1"), "{lines:#?}");
        }
    }

    #[test]
    fn check_config_orders_files_and_diagnostics_deterministically() {
        let (_temp, config) = tempdir_with_checked_in_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0\nzzz = 1\naaa = 2\n",
        );
        write_file(
            &config,
            ConfigFile::Combat,
            "[bow]\nmin_charge_secs = 9.0\n",
        );
        write_file(&config, ConfigFile::World, "[spawning]\nmystery = true\n");

        let (lines, ok) = check_config_lines(&config);
        assert!(!ok);

        let position = |prefix: &str| {
            lines
                .iter()
                .position(|line| line.starts_with(prefix))
                .unwrap_or_else(|| panic!("no line starts with {prefix:?}: {lines:#?}"))
        };

        // Known-key error before unknown keys inside one file.
        assert!(
            position("error: config/player.toml: movement.walk_speed:")
                < position("warning: config/player.toml: movement.aaa:")
        );
        assert!(
            position("warning: config/player.toml: movement.aaa:")
                < position("warning: config/player.toml: movement.zzz:")
        );
        // Files render in ConfigFile::ALL order, ok lines after diagnostics.
        assert!(position("ok: config/survival.toml:") < position("error: config/combat.toml:"));
        assert!(position("error: config/combat.toml:") < position("warning: config/world.toml:"));
        assert!(position("warning: config/world.toml:") < position("ok: config/world.toml:"));

        let digest = format!("{:#018x}", ConfigRuntime::defaults().digest());
        assert_eq!(
            lines.last().unwrap(),
            &format!(
                "config: files=6 missing=0 warnings=3 errors=2 keys=85 schema=1 digest={digest}"
            )
        );
    }

    #[test]
    fn check_config_is_read_only() {
        let (temp, config) = tempdir_with_checked_in_config();

        let read_entries = |dir: &Path| {
            let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };

        let before_config = read_entries(&config);
        let before_root = read_entries(temp.path());
        let before_bytes: Vec<Vec<u8>> = ConfigFile::ALL
            .iter()
            .map(|file| std::fs::read(config.join(file.name())).unwrap())
            .collect();

        let (_, ok) = check_config_lines(&config);
        assert!(ok);

        assert_eq!(read_entries(&config), before_config);
        assert_eq!(read_entries(temp.path()), before_root);
        for (file, before) in ConfigFile::ALL.iter().zip(before_bytes) {
            assert_eq!(
                std::fs::read(config.join(file.name())).unwrap(),
                before,
                "{} changed",
                file.name()
            );
        }
    }

    // -----------------------------------------------------------------
    // Live reload (`ConfigRuntime::reload_file`)
    // -----------------------------------------------------------------

    #[test]
    fn reload_valid_edit_commits_value_origin_and_digest() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 4.6\nsprint_speed = 8.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);
        let before = runtime.digest();

        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.5\nsprint_speed = 8.0\n",
        );
        let report = runtime.reload_file(&config, ConfigFile::Player);

        assert!(report.applied);
        assert_eq!(report.file, ConfigFile::Player);
        assert_eq!(report.changed, 1);
        assert_eq!(report.error_count(), 0);
        assert_eq!(report.warning_count(), 0);
        assert!(report.first_error().is_none());
        assert!(report.diagnostics.is_empty());

        assert_eq!(runtime.f32("movement.walk_speed"), 6.5);
        assert_eq!(
            runtime.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        assert_ne!(runtime.digest(), before);
        assert!(diagnostics_for(&runtime, ConfigFile::Player).is_empty());
    }

    #[test]
    fn reload_invalid_value_rejects_whole_file_and_keeps_last_good() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 4.6\nsprint_speed = 8.0\n[camera]\nz_far = 512.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);
        let before = runtime.digest();

        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0.0\nsprint_speed = 9.0\n[camera]\nz_far = 600.0\n",
        );
        let report = runtime.reload_file(&config, ConfigFile::Player);

        assert!(!report.applied);
        assert_eq!(report.changed, 0);
        assert_eq!(runtime.digest(), before);
        // The valid sibling edits in the same file must not leak through.
        assert_eq!(runtime.f32("movement.walk_speed"), 4.6);
        assert_eq!(runtime.f32("movement.sprint_speed"), 8.0);
        assert_eq!(runtime.f32("camera.z_far"), 512.0);
        assert_eq!(
            runtime.origin("movement.sprint_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );

        assert_eq!(report.error_count(), 1);
        assert_eq!(report.warning_count(), 0);
        let error = report.first_error().expect("one keyed error");
        assert_eq!(error.key.as_deref(), Some("movement.walk_speed"));
        assert_eq!(error.reason, "value 0 is below minimum 0.5");
        assert!(runtime.file_error(ConfigFile::Player).is_none());
        assert_eq!(diagnostics_for(&runtime, ConfigFile::Player).len(), 1);
    }

    #[test]
    fn reload_cross_field_violation_rejects_world_and_keeps_last_good() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nsafe_radius_blocks = 24\nspawn_max_radius_blocks = 48\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);
        let before = runtime.digest();

        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nsafe_radius_blocks = 24\nspawn_max_radius_blocks = 10\n",
        );
        let report = runtime.reload_file(&config, ConfigFile::World);

        assert!(!report.applied);
        assert_eq!(report.changed, 0);
        assert_eq!(runtime.digest(), before);
        assert_eq!(runtime.i64("spawning.safe_radius_blocks"), 24);
        assert_eq!(runtime.i64("spawning.spawn_max_radius_blocks"), 48);
        assert_eq!(
            runtime.origin("spawning.spawn_max_radius_blocks"),
            ValueOrigin::File(ConfigFile::World)
        );

        let error = report.first_error().expect("cross-field error");
        assert_eq!(
            error.key.as_deref(),
            Some("spawning.spawn_max_radius_blocks")
        );
        assert!(
            error.reason.contains("spawning.safe_radius_blocks"),
            "{}",
            error.reason
        );
        assert!(
            error.reason.contains("10 must be greater than"),
            "{}",
            error.reason
        );
        assert!(runtime.file_error(ConfigFile::World).is_none());
    }

    #[test]
    fn reload_invalid_toml_and_oversize_keep_last_good_and_set_file_error() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);
        let before = runtime.digest();

        write_file(&config, ConfigFile::Player, "[movement\n");
        let report = runtime.reload_file(&config, ConfigFile::Player);
        assert!(!report.applied);
        assert_eq!(runtime.digest(), before);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);
        assert!(report
            .first_error()
            .expect("file-level error")
            .reason
            .starts_with("invalid TOML: "));
        assert!(runtime
            .file_error(ConfigFile::Player)
            .expect("file error")
            .starts_with("invalid TOML: "));

        let path = config.join(ConfigFile::Player.name());
        std::fs::write(&path, vec![b' '; (MAX_CONFIG_FILE_BYTES + 1) as usize]).unwrap();
        let report = runtime.reload_file(&config, ConfigFile::Player);
        assert!(!report.applied);
        assert_eq!(report.changed, 0);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);
        assert_eq!(runtime.digest(), before);
        assert_eq!(
            runtime.file_error(ConfigFile::Player),
            Some("file is larger than 1 MiB")
        );
        assert_eq!(report.error_count(), 1);
        assert!(report.diagnostics[0].key.is_none());
    }

    #[test]
    fn reload_empty_comment_only_and_unknown_only_are_incomplete() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);
        let before = runtime.digest();

        let cases = [
            ("", "file is empty; keeping last good values"),
            ("   \n\t\n", "file is empty; keeping last good values"),
            (
                "# only a comment\n",
                "file has no recognized keys; keeping last good values",
            ),
            (
                "[movement]\n",
                "file has no recognized keys; keeping last good values",
            ),
            (
                "[movement]\nmystery = 1\n",
                "file has no recognized keys; keeping last good values",
            ),
        ];

        for (contents, expected_reason) in cases {
            write_file(&config, ConfigFile::Player, contents);
            let report = runtime.reload_file(&config, ConfigFile::Player);

            assert!(!report.applied, "{contents:?}");
            assert_eq!(report.changed, 0, "{contents:?}");
            assert_eq!(runtime.f32("movement.walk_speed"), 6.0, "{contents:?}");
            assert_eq!(runtime.digest(), before, "{contents:?}");
            assert_eq!(report.diagnostics.len(), 1, "{contents:?}");
            assert_eq!(report.diagnostics[0].key, None, "{contents:?}");
            assert_eq!(
                report.diagnostics[0].severity,
                ConfigSeverity::Error,
                "{contents:?}"
            );
            assert_eq!(
                report.diagnostics[0].reason, expected_reason,
                "{contents:?}"
            );
            assert_eq!(report.error_count(), 1, "{contents:?}");
            assert!(
                runtime.file_error(ConfigFile::Player).is_none(),
                "{contents:?}"
            );
        }
    }

    #[test]
    fn reload_missing_file_applies_defaults_with_missing_warning() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);

        std::fs::remove_file(config.join(ConfigFile::Player.name())).unwrap();
        let report = runtime.reload_file(&config, ConfigFile::Player);

        assert!(report.applied);
        assert_eq!(report.changed, 1);
        assert_eq!(report.error_count(), 0);
        assert_eq!(report.warning_count(), 1);
        assert_eq!(
            runtime.value("movement.walk_speed"),
            default_of("movement.walk_speed")
        );
        assert_eq!(runtime.origin("movement.walk_speed"), ValueOrigin::Default);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].severity, ConfigSeverity::Warning);
        assert_eq!(
            report.diagnostics[0].reason,
            "file is missing; using defaults"
        );
        assert!(runtime.file_error(ConfigFile::Player).is_none());
    }

    #[test]
    fn reload_back_to_default_still_commits_and_keeps_file_origin() {
        let (_temp, config) = tempdir_with_config();
        let ConfigValue::Float(default_walk) = default_of("movement.walk_speed") else {
            panic!("walk_speed must be a float");
        };
        let ConfigValue::Float(default_sprint) = default_of("movement.sprint_speed") else {
            panic!("sprint_speed must be a float");
        };

        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 9.0\nsprint_speed = 12.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);
        let before = runtime.digest();

        write_file(
            &config,
            ConfigFile::Player,
            &format!("[movement]\nwalk_speed = {default_walk}\nsprint_speed = {default_sprint}\n"),
        );
        let report = runtime.reload_file(&config, ConfigFile::Player);

        assert!(report.applied);
        assert_eq!(report.changed, 2);
        assert_eq!(runtime.f32("movement.walk_speed"), default_walk);
        assert_eq!(runtime.f32("movement.sprint_speed"), default_sprint);
        // Origin is provenance: a file value equal to the default is still
        // file-sourced and must not gain the overlay's `*` marker.
        assert_eq!(
            runtime.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        assert_eq!(
            runtime.origin("movement.sprint_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        assert_ne!(runtime.digest(), before);
    }

    #[test]
    fn reload_warnings_do_not_block_a_commit() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);

        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 7.0\nmystery = 1\n",
        );
        let report = runtime.reload_file(&config, ConfigFile::Player);

        assert!(report.applied);
        assert_eq!(report.error_count(), 0);
        assert_eq!(report.warning_count(), 1);
        assert!(report.first_error().is_none());
        assert_eq!(
            report.diagnostics[0].key.as_deref(),
            Some("movement.mystery")
        );
        assert_eq!(runtime.f32("movement.walk_speed"), 7.0);
    }

    #[test]
    fn reload_rejection_does_not_disturb_siblings_and_later_valid_sibling_applies() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        write_file(
            &config,
            ConfigFile::Survival,
            "[survival]\nfall_safe_blocks = 5.0\n",
        );
        let mut runtime = ConfigRuntime::load_dir(&config);

        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0.0\n",
        );
        let report = runtime.reload_file(&config, ConfigFile::Player);
        assert!(!report.applied);
        assert_eq!(runtime.f32("survival.fall_safe_blocks"), 5.0);
        assert_eq!(
            runtime.origin("survival.fall_safe_blocks"),
            ValueOrigin::File(ConfigFile::Survival)
        );
        assert!(diagnostics_for(&runtime, ConfigFile::Survival).is_empty());
        assert_eq!(diagnostics_for(&runtime, ConfigFile::Player).len(), 1);

        write_file(
            &config,
            ConfigFile::Survival,
            "[survival]\nfall_safe_blocks = 7.0\n",
        );
        let report = runtime.reload_file(&config, ConfigFile::Survival);
        assert!(report.applied);
        assert_eq!(report.changed, 1);
        assert_eq!(runtime.f32("survival.fall_safe_blocks"), 7.0);
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);
        // The earlier player rejection block survives an unrelated commit.
        assert_eq!(diagnostics_for(&runtime, ConfigFile::Player).len(), 1);
    }

    #[test]
    fn reload_preserves_diagnostics_in_config_file_order() {
        let (_temp, config) = tempdir_with_config();
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0.0\n",
        );
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nsafe_radius_blocks = 24\nspawn_max_radius_blocks = 10\n",
        );
        fn first_position(runtime: &ConfigRuntime, file: ConfigFile) -> usize {
            runtime
                .diagnostics()
                .iter()
                .position(|diagnostic| diagnostic.file == file)
                .unwrap_or_else(|| panic!("no diagnostics for {}", file.name()))
        }
        let mut runtime = ConfigRuntime::load_dir(&config);
        assert_eq!(diagnostics_for(&runtime, ConfigFile::Player).len(), 1);
        assert_eq!(diagnostics_for(&runtime, ConfigFile::World).len(), 1);
        assert!(
            first_position(&runtime, ConfigFile::Player)
                < first_position(&runtime, ConfigFile::World)
        );

        // Replacing the player block must keep it first, not move it last.
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nsprint_speed = 0.0\n",
        );
        runtime.reload_file(&config, ConfigFile::Player);
        assert_eq!(diagnostics_for(&runtime, ConfigFile::Player).len(), 1);
        assert_eq!(diagnostics_for(&runtime, ConfigFile::World).len(), 1);
        assert!(
            first_position(&runtime, ConfigFile::Player)
                < first_position(&runtime, ConfigFile::World)
        );

        // Replacing the later world block also keeps ALL order.
        write_file(
            &config,
            ConfigFile::World,
            "[spawning]\nsafe_radius_blocks = 24\nspawn_max_radius_blocks = 11\n",
        );
        runtime.reload_file(&config, ConfigFile::World);
        assert!(
            first_position(&runtime, ConfigFile::Player)
                < first_position(&runtime, ConfigFile::World)
        );
    }

    #[test]
    fn reload_never_writes_config_files_or_creates_entries() {
        let (_temp, config) = tempdir_with_checked_in_config();
        let mut runtime = ConfigRuntime::load_dir(&config);

        let snapshots = || -> Vec<Vec<u8>> {
            ConfigFile::ALL
                .iter()
                .map(|file| std::fs::read(config.join(file.name())).unwrap())
                .collect()
        };
        let entries = || -> Vec<std::ffi::OsString> {
            let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(&config)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };

        // Committed reload.
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 6.0\n",
        );
        let before_bytes = snapshots();
        let before_entries = entries();
        let report = runtime.reload_file(&config, ConfigFile::Player);
        assert!(report.applied);
        assert_eq!(snapshots(), before_bytes);
        assert_eq!(entries(), before_entries);

        // Rejected reload.
        write_file(
            &config,
            ConfigFile::Player,
            "[movement]\nwalk_speed = 0.0\n",
        );
        let before_bytes = snapshots();
        let before_entries = entries();
        let report = runtime.reload_file(&config, ConfigFile::Player);
        assert!(!report.applied);
        assert_eq!(snapshots(), before_bytes);
        assert_eq!(entries(), before_entries);
    }
}
