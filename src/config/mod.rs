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

use std::collections::BTreeMap;
use std::path::Path;

use crate::settings::SettingsWarning;

pub use registry::{
    docs_table, index_of, ApplyMode, ConfigFile, ConfigKind, ConfigRow, ConfigValue, REGISTRY,
};

/// Directory holding the six curated TOML files, relative to the process CWD.
pub const CONFIG_DIR: &str = "config";

/// Version of the effective-value digest / save schema contract.
pub const CONFIG_SCHEMA_VERSION: u16 = 1;

/// Hard size cap for a single config file; anything larger is rejected whole.
const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

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
// Per-file loading
// ---------------------------------------------------------------------------

/// Result of parsing one file before [`ConfigRuntime::load_dir`] merges it.
///
/// S02's per-file live reload reuses this seam: parse a candidate, then commit
/// its values/origins only when it produced no Error diagnostics.
struct FileLoad {
    file: ConfigFile,
    values: Vec<ConfigValue>,
    origins: Vec<ValueOrigin>,
    diagnostics: Vec<ConfigDiagnostic>,
    file_error: Option<String>,
}

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
                values: defaults,
                origins: default_origins,
                diagnostics: vec![ConfigDiagnostic {
                    file,
                    key: None,
                    severity: ConfigSeverity::Warning,
                    reason: "file is missing; using defaults".to_string(),
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

    let root: toml::Value = match toml::from_str(&contents) {
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
                        "tool speed multipliers must satisfy wood ({wood}) <= \
                         stone ({stone}) <= iron ({iron})"
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

    fn diagnostics_for<'a>(
        runtime: &'a ConfigRuntime,
        file: ConfigFile,
    ) -> Vec<&'a ConfigDiagnostic> {
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
            "tool speed multipliers must satisfy wood (5) <= stone (3) <= iron (10)"
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
}
