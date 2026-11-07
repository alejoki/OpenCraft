//! Pure village definition codec and fail-soft [`VillageCatalog`].
//!
//! A village definition is one TOML document under `worldgen/villages/`. It
//! names a biome allow-list (M015 supports only `plains` and `forest`), the
//! `hub` prefab that anchors every village of this kind, integer sizing knobs
//! (`spacing`, `min_prefabs`, `max_prefabs`, `max_slope`) and an optional
//! `[pool]` table of authored prefab names for the `house`, `farm`, `well` and
//! `junction` roles.
//!
//! This module owns two deliberately separate policies, mirroring
//! [`crate::world::prefab`]:
//!
//! * [`parse_village_definition`] is **strict**: the first structural problem
//!   rejects the whole definition with one stable, greppable reason.
//! * [`VillageCatalog::load_sorted`] is **fail-soft**: a broken file degrades
//!   to one diagnostic and is skipped, so a chunk can always generate. Pool
//!   entries that name unknown prefabs are dropped with a diagnostic while the
//!   rest of the definition survives ("reduced village").
//!
//! Authored TOML is untrusted. Every read is capped by
//! [`MAX_PREFAB_FILE_BYTES`], pool arrays are bounded to
//! [`MAX_POOL_ENTRIES`], every number is range-checked, and prefab lookups are
//! the only way a name becomes geometry. No authored input can panic this
//! parser or allocate unbounded.
//!
//! Ordering is part of the contract: the directory walk sorts raw file names
//! (`fs::read_dir` order is nondeterministic), duplicate definition names keep
//! the sorted-first file, and [`VillageCatalog::fingerprint`] hashes canonical
//! field values rather than raw file bytes so whitespace-only edits do not move
//! the layout digest.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;

use crate::world::prefab::{PrefabCatalog, MAX_PREFAB_FILE_BYTES, WORLDGEN_DIR};
use crate::world::terrain::Biome;

/// Directory, below [`WORLDGEN_DIR`], that owns one TOML file per village
/// definition.
pub const VILLAGES_SUBDIR: &str = "villages";

/// Upper bound on authored entries per `[pool]` key.
///
/// The 1 MiB file cap already bounds the raw TOML, but a pool is copied into a
/// `Vec<String>` and later validated prefab-by-prefab; capping the processed
/// entries keeps a pathological file from turning into a large per-chunk cost.
pub const MAX_POOL_ENTRIES: usize = 64;

/// One stable, one-line village-definition reason.
///
/// `Display` emits exactly the pinned text; TOML line/column noise is
/// deliberately discarded so catalog diagnostics remain greppable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VillageError {
    MissingFormat,
    InvalidFormat,
    UnsupportedFormat(i64),
    MissingName,
    NameTooLong,
    NameInvalidCharacter,
    MissingBiomes,
    InvalidBiomes,
    UnsupportedBiome(String),
    MissingHub,
    UnknownHub(String),
    InvalidSpacing,
    InvalidMinPrefabs,
    InvalidMaxPrefabs,
    MinExceedsMax,
    InvalidMaxSlope,
    PrefabNoSockets(String),
    SocketNotAtRoadLevel { prefab: String, index: usize },
    NoLeafPrefabs,
    InvalidToml,
    FileTooLarge,
    CouldNotReadFile,
}

impl fmt::Display for VillageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VillageError::MissingFormat => write!(f, "missing format"),
            VillageError::InvalidFormat => write!(f, "invalid format"),
            VillageError::UnsupportedFormat(version) => {
                write!(f, "unsupported format {version}")
            }
            VillageError::MissingName => write!(f, "missing name"),
            VillageError::NameTooLong => write!(f, "name is longer than 64 bytes"),
            VillageError::NameInvalidCharacter => {
                write!(f, "name may only use a-z, 0-9, _ and -")
            }
            VillageError::MissingBiomes => write!(f, "missing biomes"),
            VillageError::InvalidBiomes => write!(f, "biomes must be a non-empty array"),
            VillageError::UnsupportedBiome(biome) => {
                write!(f, "biome '{biome}' is not supported")
            }
            VillageError::MissingHub => write!(f, "missing hub"),
            VillageError::UnknownHub(prefab) => {
                write!(f, "hub prefab '{prefab}' is not in the catalog")
            }
            VillageError::InvalidSpacing => write!(f, "spacing must be an integer in 1..=16"),
            VillageError::InvalidMinPrefabs => {
                write!(f, "min_prefabs must be an integer in 10..=20")
            }
            VillageError::InvalidMaxPrefabs => {
                write!(f, "max_prefabs must be an integer in 10..=20")
            }
            VillageError::MinExceedsMax => write!(f, "min_prefabs must not exceed max_prefabs"),
            VillageError::InvalidMaxSlope => {
                write!(f, "max_slope must be an integer in 0..=16")
            }
            VillageError::PrefabNoSockets(prefab) => {
                write!(f, "prefab '{prefab}' has no sockets")
            }
            VillageError::SocketNotAtRoadLevel { prefab, index } => {
                write!(f, "prefab '{prefab}' socket {index} is not at grid y=1")
            }
            VillageError::NoLeafPrefabs => write!(f, "no leaf prefabs"),
            VillageError::InvalidToml => write!(f, "invalid TOML"),
            VillageError::FileTooLarge => write!(f, "file is larger than 1 MiB"),
            VillageError::CouldNotReadFile => write!(f, "could not read file"),
        }
    }
}

impl std::error::Error for VillageError {}

/// A parsed, validated village definition.
///
/// `biomes` preserves authored order (it is hashed that way). `house`, `farm`,
/// `well` and `junction` hold only catalog prefab names, deduped in authored
/// order. `junction` defaults to `[hub]` when the authored pool is absent or
/// empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VillageDefinition {
    pub name: String,
    pub biomes: Vec<Biome>,
    pub hub: String,
    pub spacing: i32,
    pub min_prefabs: usize,
    pub max_prefabs: usize,
    pub max_slope: i32,
    pub house: Vec<String>,
    pub farm: Vec<String>,
    pub well: Vec<String>,
    pub junction: Vec<String>,
}

/// Parse one village definition against the already-loaded prefab catalog.
///
/// Checks run in a fixed first-failure order so the reported reason is stable:
/// `format` -> `name` -> `biomes` -> `hub` exists -> `spacing` -> `min_prefabs`
/// / `max_prefabs` -> `max_slope` -> `[pool]`. Unknown top-level keys are
/// ignored for forward compatibility.
///
/// Pool entries naming unknown prefabs are dropped with one diagnostic each
/// (`pool '<key>' names unknown prefab '<name>'`) while the definition
/// survives. After pool resolution the hub and every kept prefab must have at
/// least one socket and every socket must sit at grid `y == 1`
/// (`origin[1] + position[1] == 1`), the road-alignment invariant.
pub fn parse_village_definition(
    text: &str,
    prefabs: &PrefabCatalog,
) -> Result<(VillageDefinition, Vec<String>), VillageError> {
    let root: toml::Value = toml::from_str(text).map_err(|_| VillageError::InvalidToml)?;
    let root = root.as_table().ok_or(VillageError::InvalidToml)?;

    let format = root
        .get("format")
        .ok_or(VillageError::MissingFormat)?
        .as_integer()
        .ok_or(VillageError::InvalidFormat)?;
    if format != 1 {
        return Err(VillageError::UnsupportedFormat(format));
    }

    let name = root
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or(VillageError::MissingName)?;
    validate_village_name(name)?;
    let name = name.to_owned();

    let biomes_value = root.get("biomes").ok_or(VillageError::MissingBiomes)?;
    let biomes_array = biomes_value.as_array().ok_or(VillageError::InvalidBiomes)?;
    if biomes_array.is_empty() {
        return Err(VillageError::InvalidBiomes);
    }
    let mut biomes = Vec::with_capacity(biomes_array.len());
    for value in biomes_array {
        let biome_name = value.as_str().ok_or(VillageError::InvalidBiomes)?;
        let biome = match biome_name {
            "plains" => Biome::Plains,
            "forest" => Biome::Forest,
            other => return Err(VillageError::UnsupportedBiome(other.to_owned())),
        };
        biomes.push(biome);
    }

    let hub = root
        .get("hub")
        .and_then(toml::Value::as_str)
        .ok_or(VillageError::MissingHub)?
        .to_owned();
    if prefabs.get(&hub).is_none() {
        return Err(VillageError::UnknownHub(hub));
    }

    let spacing = root
        .get("spacing")
        .and_then(toml::Value::as_integer)
        .filter(|value| (1..=16).contains(value))
        .ok_or(VillageError::InvalidSpacing)? as i32;

    let min_prefabs = root
        .get("min_prefabs")
        .and_then(toml::Value::as_integer)
        .filter(|value| (10..=20).contains(value))
        .ok_or(VillageError::InvalidMinPrefabs)? as usize;
    let max_prefabs = root
        .get("max_prefabs")
        .and_then(toml::Value::as_integer)
        .filter(|value| (10..=20).contains(value))
        .ok_or(VillageError::InvalidMaxPrefabs)? as usize;
    if min_prefabs > max_prefabs {
        return Err(VillageError::MinExceedsMax);
    }

    let max_slope = root
        .get("max_slope")
        .and_then(toml::Value::as_integer)
        .filter(|value| (0..=16).contains(value))
        .ok_or(VillageError::InvalidMaxSlope)? as i32;

    let pool = match root.get("pool") {
        Some(value) => Some(value.as_table().ok_or(VillageError::InvalidToml)?),
        None => None,
    };
    let mut diagnostics = Vec::new();
    let house = pool_names(pool, "house", prefabs, &mut diagnostics)?;
    let farm = pool_names(pool, "farm", prefabs, &mut diagnostics)?;
    let well = pool_names(pool, "well", prefabs, &mut diagnostics)?;
    let mut junction = pool_names(pool, "junction", prefabs, &mut diagnostics)?;
    if junction.is_empty() {
        junction.push(hub.clone());
    }
    if house.is_empty() && farm.is_empty() && well.is_empty() {
        return Err(VillageError::NoLeafPrefabs);
    }

    validate_road_prefab(&hub, prefabs)?;
    for names in [&house, &farm, &well, &junction] {
        for prefab_name in names {
            validate_road_prefab(prefab_name, prefabs)?;
        }
    }

    Ok((
        VillageDefinition {
            name,
            biomes,
            hub,
            spacing,
            min_prefabs,
            max_prefabs,
            max_slope,
            house,
            farm,
            well,
            junction,
        },
        diagnostics,
    ))
}

fn validate_village_name(name: &str) -> Result<(), VillageError> {
    if name.len() > 64 {
        return Err(VillageError::NameTooLong);
    }
    if name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(VillageError::NameInvalidCharacter);
    }
    Ok(())
}

/// Resolve one authored pool key to catalog prefab names.
///
/// Absent keys and a missing `[pool]` table yield an empty pool. A present key
/// must be an array of strings; a non-array value or non-string entry is
/// malformed TOML for this schema and rejects the definition. Unknown prefab
/// names are dropped with one diagnostic each; duplicates are deduped while
/// preserving the first authored position.
fn pool_names(
    pool: Option<&toml::Table>,
    key: &str,
    prefabs: &PrefabCatalog,
    diagnostics: &mut Vec<String>,
) -> Result<Vec<String>, VillageError> {
    let Some(table) = pool else {
        return Ok(Vec::new());
    };
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or(VillageError::InvalidToml)?;
    let mut names: Vec<String> = Vec::new();
    for entry in array.iter().take(MAX_POOL_ENTRIES) {
        let name = entry.as_str().ok_or(VillageError::InvalidToml)?;
        if prefabs.get(name).is_none() {
            diagnostics.push(format!("pool '{key}' names unknown prefab '{name}'"));
            continue;
        }
        if !names.iter().any(|kept| kept == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Enforce the road-alignment invariant on one catalog prefab.
///
/// Callers only pass names already filtered to the catalog, so the lookup is
/// an invariant assertion rather than an authored failure path.
fn validate_road_prefab(name: &str, prefabs: &PrefabCatalog) -> Result<(), VillageError> {
    let prefab = prefabs
        .get(name)
        .expect("pool entries and hub are filtered to catalog prefabs");
    if prefab.sockets.is_empty() {
        return Err(VillageError::PrefabNoSockets(name.to_owned()));
    }
    for (index, socket) in prefab.sockets.iter().enumerate() {
        let grid_y = i64::from(prefab.origin[1]) + i64::from(socket.position[1]);
        if grid_y != 1 {
            return Err(VillageError::SocketNotAtRoadLevel {
                prefab: name.to_owned(),
                index,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Catalog: the only filesystem-facing layer.
// ---------------------------------------------------------------------------

/// One village file's outcome during a sorted directory walk.
struct VillageFile {
    file_name: String,
    outcome: Result<(VillageDefinition, Vec<String>), String>,
}

/// A name-sorted, fail-soft collection of village definitions plus the prefab
/// catalog they were validated against.
///
/// [`VillageCatalog::load_sorted`] never fails: every unreadable, oversize or
/// malformed definition degrades to one diagnostic and is skipped. Prefab
/// diagnostics from [`PrefabCatalog::load_sorted`] are included first, in the
/// same order, so callers see one combined startup warning list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VillageCatalog {
    prefabs: PrefabCatalog,
    villages: BTreeMap<String, VillageDefinition>,
}

impl VillageCatalog {
    /// Walk `root/prefabs` and `root/villages` and load both catalogs.
    ///
    /// Fail-soft by contract. Missing, non-directory or unreadable `root` /
    /// `root/villages` is valid empty content with no diagnostics. Per village
    /// file the first failure wins and the file is skipped:
    ///
    /// 1. metadata/read error -> `could not read file: <error>`;
    /// 2. more than [`MAX_PREFAB_FILE_BYTES`] -> `file is larger than 1 MiB`;
    /// 3. [`parse_village_definition`] error -> its `Display` reason;
    /// 4. duplicate header `name` -> the sorted-first file wins and the later
    ///    file reports `duplicate village name '<name>'`.
    ///
    /// Pool diagnostics from accepted definitions are prefixed with the same
    /// logical `worldgen/villages/<file>` label. Non-regular files,
    /// directories and non-`toml` extensions are ignored silently.
    pub fn load_sorted(root: &Path) -> (Self, Vec<String>) {
        let (prefabs, mut diagnostics) = PrefabCatalog::load_sorted(root);
        let mut villages = BTreeMap::new();
        for file in scan_village_dir(root, &prefabs) {
            match file.outcome {
                Ok((definition, pool_diagnostics)) => {
                    if villages.contains_key(&definition.name) {
                        diagnostics.push(logical_file_label(
                            &file.file_name,
                            &format!("duplicate village name '{}'", definition.name),
                        ));
                        continue;
                    }
                    let prefix = logical_file_prefix(&file.file_name);
                    for diagnostic in pool_diagnostics {
                        diagnostics.push(format!("{prefix}: {diagnostic}"));
                    }
                    villages.insert(definition.name.clone(), definition);
                }
                Err(reason) => diagnostics.push(logical_file_label(&file.file_name, &reason)),
            }
        }
        (VillageCatalog { prefabs, villages }, diagnostics)
    }

    /// Look a definition up by its header `name`.
    pub fn get(&self, name: &str) -> Option<&VillageDefinition> {
        self.villages.get(name)
    }

    /// Iterate loaded definition names in ascending order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.villages.keys().map(String::as_str)
    }

    /// Number of accepted definitions.
    pub fn len(&self) -> usize {
        self.villages.len()
    }

    /// Whether no definition was accepted.
    pub fn is_empty(&self) -> bool {
        self.villages.is_empty()
    }

    /// The prefab catalog these definitions were validated against.
    pub fn prefabs(&self) -> &PrefabCatalog {
        &self.prefabs
    }

    /// FNV-1a 64 over the canonical content of this catalog.
    ///
    /// Prefabs contribute, in sorted name order, their name and
    /// [`crate::world::prefab::Prefab::to_toml_string`] bytes. Definitions
    /// contribute, in sorted name order, their canonical fields (name, biomes
    /// in authored order, hub, spacing, min/max, max_slope, house, farm, well,
    /// junction). Raw file bytes are never hashed, so whitespace-only edits do
    /// not move the digest.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        for name in self.prefabs.names() {
            hash = fnv1a(hash, name.as_bytes());
            hash = fnv1a(hash, &[0]);
            if let Some(prefab) = self.prefabs.get(name) {
                hash = fnv1a(hash, prefab.to_toml_string().as_bytes());
            }
        }
        hash = fnv1a(hash, &[0xff]);

        for definition in self.villages.values() {
            hash = fnv1a(hash, definition.name.as_bytes());
            hash = fnv1a(hash, &[0]);
            for biome in &definition.biomes {
                hash = fnv1a(hash, biome_label(*biome).as_bytes());
                hash = fnv1a(hash, &[0]);
            }
            hash = fnv1a(hash, &[0xff]);
            hash = fnv1a(hash, definition.hub.as_bytes());
            hash = fnv1a(hash, &[0]);
            hash = fnv1a(hash, &definition.spacing.to_le_bytes());
            hash = fnv1a(hash, &(definition.min_prefabs as u64).to_le_bytes());
            hash = fnv1a(hash, &(definition.max_prefabs as u64).to_le_bytes());
            hash = fnv1a(hash, &definition.max_slope.to_le_bytes());
            for pool in [
                &definition.house,
                &definition.farm,
                &definition.well,
                &definition.junction,
            ] {
                for prefab_name in pool {
                    hash = fnv1a(hash, prefab_name.as_bytes());
                    hash = fnv1a(hash, &[0]);
                }
                hash = fnv1a(hash, &[0xff]);
            }
        }
        hash
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn biome_label(biome: Biome) -> &'static str {
    match biome {
        Biome::Plains => "plains",
        Biome::Forest => "forest",
        Biome::Mountains => "mountains",
        Biome::Ocean => "ocean",
    }
}

/// Logical `worldgen/villages/<file>: <reason>` label used by diagnostics.
fn logical_file_label(file_name: &str, reason: &str) -> String {
    format!("{}: {reason}", logical_file_prefix(file_name))
}

fn logical_file_prefix(file_name: &str) -> String {
    format!("{WORLDGEN_DIR}/{VILLAGES_SUBDIR}/{file_name}")
}

/// Walk `root/villages` in raw file-name order, applying the per-file policy.
///
/// Sorting by `OsString` (raw bytes on Unix) removes `fs::read_dir` ordering
/// nondeterminism, so first-wins duplicate handling and diagnostic order are
/// stable across filesystems.
fn scan_village_dir(root: &Path, prefabs: &PrefabCatalog) -> Vec<VillageFile> {
    let dir = root.join(VILLAGES_SUBDIR);
    let Ok(read_dir) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut entries: Vec<_> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(|left| left.file_name());

    let mut files = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) != Some("toml") {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().into_owned();
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) => {
                files.push(VillageFile {
                    file_name,
                    outcome: Err(format!("could not read file: {error}")),
                });
                continue;
            }
        }

        let outcome = match read_village_file(&path) {
            Ok(text) => parse_village_definition(&text, prefabs).map_err(|error| error.to_string()),
            Err(reason) => Err(reason),
        };
        files.push(VillageFile { file_name, outcome });
    }
    files
}

/// Read one village file with a hard byte cap.
///
/// The metadata check rejects oversize files before allocating, and the
/// `take(MAX + 1)` read catches a file that grew between stat and read. Both
/// failures return the pinned `file is larger than 1 MiB` reason. Non-UTF-8
/// bytes normalize to `invalid TOML`.
fn read_village_file(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| format!("could not read file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not read file: {error}"))?;
    if metadata.len() > MAX_PREFAB_FILE_BYTES {
        return Err(VillageError::FileTooLarge.to_string());
    }

    let mut bytes = Vec::new();
    file.take(MAX_PREFAB_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read file: {error}"))?;
    if bytes.len() as u64 > MAX_PREFAB_FILE_BYTES {
        return Err(VillageError::FileTooLarge.to_string());
    }
    String::from_utf8(bytes).map_err(|_| VillageError::InvalidToml.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::prefab::PREFABS_SUBDIR;
    use std::path::PathBuf;

    const HUB: &str = r#"format = 1
name = "test_hub"
origin = [0, 0, 0]
size = [3, 3, 3]

layers = [
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
]

[palette]
"x" = "stone"

[[sockets]]
position = [1, 1, 0]
direction = "north"
"#;

    const HOUSE: &str = r#"format = 1
name = "test_house"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]

[palette]
"x" = "oak_planks"

[[sockets]]
position = [0, 1, 0]
direction = "north"
"#;

    const FARM: &str = r#"format = 1
name = "test_farm"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]

[palette]
"x" = "farmland"

[[sockets]]
position = [0, 1, 0]
direction = "north"
"#;

    const NO_SOCKET: &str = r#"format = 1
name = "no_socket"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]

[palette]
"x" = "stone"
"#;

    const MISALIGNED: &str = r#"format = 1
name = "misaligned"
origin = [0, 0, 0]
size = [2, 2, 2]

layers = [
  [ "xx", "xx" ],
  [ "xx", "xx" ],
]

[palette]
"x" = "stone"

[[sockets]]
position = [0, 0, 0]
direction = "north"
"#;

    const VALID: &str = r#"format = 1
name = "plains"
biomes = ["plains", "forest"]
hub = "test_hub"
spacing = 4
min_prefabs = 10
max_prefabs = 20
max_slope = 6

[pool]
house = ["test_house"]
farm = ["test_farm"]
well = []
junction = ["test_hub"]
"#;

    fn write_prefab(root: &Path, file_name: &str, text: &str) {
        let dir = root.join(PREFABS_SUBDIR);
        fs::create_dir_all(&dir).expect("create prefab fixture dir");
        fs::write(dir.join(file_name), text).expect("write prefab fixture");
    }

    fn write_village(root: &Path, file_name: &str, text: &str) {
        let dir = root.join(VILLAGES_SUBDIR);
        fs::create_dir_all(&dir).expect("create village fixture dir");
        fs::write(dir.join(file_name), text).expect("write village fixture");
    }

    /// Temp dir plus a catalog holding every prefab fixture used by the tests.
    fn standard_catalog() -> (tempfile::TempDir, PrefabCatalog) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prefab(temp.path(), "hub.toml", HUB);
        write_prefab(temp.path(), "house.toml", HOUSE);
        write_prefab(temp.path(), "farm.toml", FARM);
        write_prefab(temp.path(), "no_socket.toml", NO_SOCKET);
        write_prefab(temp.path(), "misaligned.toml", MISALIGNED);
        let (catalog, diagnostics) = PrefabCatalog::load_sorted(temp.path());
        assert!(
            diagnostics.is_empty(),
            "prefab diagnostics: {diagnostics:?}"
        );
        assert_eq!(catalog.len(), 5);
        (temp, catalog)
    }

    fn parse(text: &str) -> Result<(VillageDefinition, Vec<String>), VillageError> {
        let (_temp, catalog) = standard_catalog();
        parse_village_definition(text, &catalog)
    }

    #[test]
    fn valid_plains_definition_parses() {
        let (definition, diagnostics) = parse(VALID).expect("valid definition must parse");
        assert_eq!(definition.name, "plains");
        assert_eq!(definition.biomes, vec![Biome::Plains, Biome::Forest]);
        assert_eq!(definition.hub, "test_hub");
        assert_eq!(definition.spacing, 4);
        assert_eq!(definition.min_prefabs, 10);
        assert_eq!(definition.max_prefabs, 20);
        assert_eq!(definition.max_slope, 6);
        assert_eq!(definition.house, vec!["test_house"]);
        assert_eq!(definition.farm, vec!["test_farm"]);
        assert!(definition.well.is_empty());
        assert_eq!(definition.junction, vec!["test_hub"]);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn missing_header_fields_are_rejected_in_order() {
        assert_eq!(
            parse(&VALID.replace("format = 1\n", "")),
            Err(VillageError::MissingFormat)
        );
        assert_eq!(
            parse(&VALID.replace("name = \"plains\"\n", "")),
            Err(VillageError::MissingName)
        );
        assert_eq!(
            parse(&VALID.replace("biomes = [\"plains\", \"forest\"]\n", "")),
            Err(VillageError::MissingBiomes)
        );
        assert_eq!(
            parse(&VALID.replace("hub = \"test_hub\"\n", "")),
            Err(VillageError::MissingHub)
        );
    }

    #[test]
    fn malformed_format_and_name_are_rejected() {
        assert_eq!(
            parse(&VALID.replace("format = 1", "format = \"1\"")),
            Err(VillageError::InvalidFormat)
        );
        assert_eq!(
            parse(&VALID.replace("format = 1", "format = 2")),
            Err(VillageError::UnsupportedFormat(2))
        );
        assert_eq!(
            parse(&VALID.replace("name = \"plains\"", "name = \"Plains\"")),
            Err(VillageError::NameInvalidCharacter)
        );
        let long_name = "a".repeat(65);
        assert_eq!(
            parse(&VALID.replace("name = \"plains\"", &format!("name = \"{long_name}\""))),
            Err(VillageError::NameTooLong)
        );
    }

    #[test]
    fn biome_list_is_validated() {
        assert_eq!(
            parse(&VALID.replace("biomes = [\"plains\", \"forest\"]", "biomes = []")),
            Err(VillageError::InvalidBiomes)
        );
        assert_eq!(
            parse(&VALID.replace("biomes = [\"plains\", \"forest\"]", "biomes = \"plains\"")),
            Err(VillageError::InvalidBiomes)
        );
        assert_eq!(
            parse(&VALID.replace(
                "biomes = [\"plains\", \"forest\"]",
                "biomes = [\"mountains\"]"
            )),
            Err(VillageError::UnsupportedBiome("mountains".to_owned()))
        );
    }

    #[test]
    fn unknown_hub_prefab_is_rejected() {
        assert_eq!(
            parse(&VALID.replace("hub = \"test_hub\"", "hub = \"ghost\"")),
            Err(VillageError::UnknownHub("ghost".to_owned()))
        );
    }

    #[test]
    fn numeric_ranges_are_enforced() {
        assert_eq!(
            parse(&VALID.replace("spacing = 4", "spacing = 0")),
            Err(VillageError::InvalidSpacing)
        );
        assert_eq!(
            parse(&VALID.replace("spacing = 4", "spacing = 17")),
            Err(VillageError::InvalidSpacing)
        );
        assert_eq!(
            parse(&VALID.replace("min_prefabs = 10", "min_prefabs = 9")),
            Err(VillageError::InvalidMinPrefabs)
        );
        assert_eq!(
            parse(&VALID.replace("max_prefabs = 20", "max_prefabs = 21")),
            Err(VillageError::InvalidMaxPrefabs)
        );
        assert_eq!(
            parse(
                &VALID
                    .replace("min_prefabs = 10", "min_prefabs = 20")
                    .replace("max_prefabs = 20", "max_prefabs = 10")
            ),
            Err(VillageError::MinExceedsMax)
        );
        assert_eq!(
            parse(&VALID.replace("max_slope = 6", "max_slope = 17")),
            Err(VillageError::InvalidMaxSlope)
        );
    }

    #[test]
    fn prefabs_without_usable_sockets_are_rejected() {
        assert_eq!(
            parse(&VALID.replace("hub = \"test_hub\"", "hub = \"no_socket\"")),
            Err(VillageError::PrefabNoSockets("no_socket".to_owned()))
        );
        assert_eq!(
            parse(&VALID.replace("hub = \"test_hub\"", "hub = \"misaligned\"")),
            Err(VillageError::SocketNotAtRoadLevel {
                prefab: "misaligned".to_owned(),
                index: 0,
            })
        );
        assert_eq!(
            parse(&VALID.replace("house = [\"test_house\"]", "house = [\"misaligned\"]")),
            Err(VillageError::SocketNotAtRoadLevel {
                prefab: "misaligned".to_owned(),
                index: 0,
            })
        );
    }

    #[test]
    fn unknown_pool_prefab_is_dropped_with_a_diagnostic() {
        let text = VALID.replace(
            "house = [\"test_house\"]",
            "house = [\"test_house\", \"ghost\", \"test_house\"]",
        );
        let (definition, diagnostics) = parse(&text).expect("definition must survive");
        assert_eq!(definition.house, vec!["test_house"]);
        assert_eq!(
            diagnostics,
            vec!["pool 'house' names unknown prefab 'ghost'".to_owned()]
        );
    }

    #[test]
    fn junction_defaults_to_the_hub_when_absent_or_empty() {
        let absent = VALID.replace("junction = [\"test_hub\"]\n", "");
        let (definition, diagnostics) = parse(&absent).expect("definition must parse");
        assert_eq!(definition.junction, vec!["test_hub"]);
        assert!(diagnostics.is_empty());

        let empty = VALID.replace("junction = [\"test_hub\"]", "junction = []");
        let (definition, diagnostics) = parse(&empty).expect("definition must parse");
        assert_eq!(definition.junction, vec!["test_hub"]);
        assert!(diagnostics.is_empty());

        let unknown_only = VALID.replace("junction = [\"test_hub\"]", "junction = [\"ghost\"]");
        let (definition, diagnostics) = parse(&unknown_only).expect("definition must parse");
        assert_eq!(definition.junction, vec!["test_hub"]);
        assert_eq!(
            diagnostics,
            vec!["pool 'junction' names unknown prefab 'ghost'".to_owned()]
        );
    }

    #[test]
    fn all_leaf_pools_empty_is_rejected() {
        let text = VALID
            .replace("house = [\"test_house\"]", "house = []")
            .replace("farm = [\"test_farm\"]", "farm = []");
        assert_eq!(parse(&text), Err(VillageError::NoLeafPrefabs));
    }

    #[test]
    fn invalid_toml_and_non_string_pool_entries_are_rejected() {
        assert_eq!(parse("not toml at all"), Err(VillageError::InvalidToml));
        assert_eq!(
            parse(&VALID.replace("house = [\"test_house\"]", "house = [1]")),
            Err(VillageError::InvalidToml)
        );
        assert_eq!(
            parse(&VALID.replace("house = [\"test_house\"]", "house = \"test_house\"")),
            Err(VillageError::InvalidToml)
        );
    }

    #[test]
    fn unknown_top_level_and_pool_keys_are_ignored() {
        let text = format!("{VALID}\nunknown_key = 7\n");
        let (definition, diagnostics) = parse(&text).expect("extras are ignored");
        assert_eq!(definition.name, "plains");
        assert!(diagnostics.is_empty());

        let text = VALID.replace(
            "junction = [\"test_hub\"]",
            "junction = [\"test_hub\"]\nunknown_pool = [\"ghost\"]",
        );
        let (definition, diagnostics) = parse(&text).expect("extras are ignored");
        assert_eq!(definition.junction, vec!["test_hub"]);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn error_display_is_the_pinned_reason() {
        assert_eq!(
            VillageError::SocketNotAtRoadLevel {
                prefab: "small_house".to_owned(),
                index: 2,
            }
            .to_string(),
            "prefab 'small_house' socket 2 is not at grid y=1"
        );
        assert_eq!(
            VillageError::UnsupportedBiome("mountains".to_owned()).to_string(),
            "biome 'mountains' is not supported"
        );
        assert_eq!(
            VillageError::InvalidMinPrefabs.to_string(),
            "min_prefabs must be an integer in 10..=20"
        );
    }

    #[test]
    fn checked_in_worldgen_root_loads_clean() {
        let (catalog, diagnostics) = VillageCatalog::load_sorted(Path::new(WORLDGEN_DIR));
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert_eq!(catalog.prefabs().len(), 3);
        assert_eq!(catalog.len(), 1);
        let definition = catalog.get("plains").expect("reference plains definition");
        assert_eq!(definition.biomes, vec![Biome::Plains, Biome::Forest]);
        assert_eq!(definition.hub, "plains_hub");
        assert_eq!(definition.spacing, 4);
        assert_eq!(definition.min_prefabs, 10);
        assert_eq!(definition.max_prefabs, 20);
        assert_eq!(definition.max_slope, 6);
        assert_eq!(definition.house, vec!["small_house"]);
        assert_eq!(definition.farm, vec!["wheat_farm"]);
        assert_eq!(definition.junction, vec!["plains_hub"]);
    }

    #[test]
    fn missing_villages_dir_is_valid_empty_content() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prefab(temp.path(), "hub.toml", HUB);

        let (catalog, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert_eq!(catalog.prefabs().len(), 1);
        assert!(catalog.is_empty());
        assert_eq!(catalog.len(), 0);
        assert_eq!(catalog.names().count(), 0);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");

        let absent = temp.path().join("absent");
        let (catalog, diagnostics) = VillageCatalog::load_sorted(&absent);
        assert!(catalog.prefabs().is_empty());
        assert!(catalog.is_empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn duplicate_definition_keeps_the_sorted_first_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prefab(temp.path(), "hub.toml", HUB);
        write_prefab(temp.path(), "house.toml", HOUSE);
        write_prefab(temp.path(), "farm.toml", FARM);
        write_village(temp.path(), "a.toml", VALID);
        write_village(
            temp.path(),
            "b.toml",
            &VALID.replace("spacing = 4", "spacing = 5"),
        );

        let (catalog, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.get("plains").expect("first file wins").spacing, 4);
        assert_eq!(
            diagnostics,
            vec!["worldgen/villages/b.toml: duplicate village name 'plains'".to_owned()]
        );
    }

    #[test]
    fn broken_village_file_yields_one_diagnostic_and_siblings_still_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prefab(temp.path(), "hub.toml", HUB);
        write_prefab(temp.path(), "house.toml", HOUSE);
        write_prefab(temp.path(), "farm.toml", FARM);
        write_village(temp.path(), "a_valid.toml", VALID);
        write_village(temp.path(), "b_broken.toml", "format = [\n");
        write_village(
            temp.path(),
            "c_unknown.toml",
            &VALID
                .replace("name = \"plains\"", "name = \"second\"")
                .replace("hub = \"test_hub\"", "hub = \"ghost\""),
        );

        let (catalog, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("plains").is_some());
        assert_eq!(
            diagnostics,
            vec![
                "worldgen/villages/b_broken.toml: invalid TOML".to_owned(),
                "worldgen/villages/c_unknown.toml: hub prefab 'ghost' is not in the catalog"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn oversize_village_file_is_rejected_before_parsing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join(VILLAGES_SUBDIR);
        fs::create_dir_all(&dir).expect("create villages dir");
        fs::write(
            dir.join("huge.toml"),
            vec![b' '; MAX_PREFAB_FILE_BYTES as usize + 1],
        )
        .expect("write oversize village");

        let (catalog, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert!(catalog.is_empty());
        assert_eq!(
            diagnostics,
            vec!["worldgen/villages/huge.toml: file is larger than 1 MiB".to_owned()]
        );
    }

    #[test]
    fn non_utf8_village_file_reports_invalid_toml() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join(VILLAGES_SUBDIR);
        fs::create_dir_all(&dir).expect("create villages dir");
        fs::write(dir.join("binary.toml"), [0xff, 0xfe, 0x00, 0x01]).expect("write binary village");

        let (catalog, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert!(catalog.is_empty());
        assert_eq!(
            diagnostics,
            vec!["worldgen/villages/binary.toml: invalid TOML".to_owned()]
        );
    }

    #[test]
    fn custom_root_spelling_still_uses_the_logical_label() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root: PathBuf = temp.path().join("custom").join("gen");
        write_prefab(&root, "hub.toml", HUB);
        write_village(&root, "bad.toml", "not toml at all");

        let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
        assert!(catalog.is_empty());
        assert_eq!(
            diagnostics,
            vec!["worldgen/villages/bad.toml: invalid TOML".to_owned()]
        );
        assert!(!diagnostics.iter().any(|line| line.contains("custom")));
    }

    #[test]
    fn fingerprint_is_stable_and_moves_with_spacing() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_prefab(temp.path(), "hub.toml", HUB);
        write_prefab(temp.path(), "house.toml", HOUSE);
        write_prefab(temp.path(), "farm.toml", FARM);
        write_village(temp.path(), "plains.toml", VALID);

        let (first, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        let (second, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert!(diagnostics.is_empty());
        assert_eq!(first.fingerprint(), second.fingerprint());

        // Whitespace-only edits must not move the digest.
        write_village(temp.path(), "plains.toml", &format!("\n{VALID}\n\n"));
        let (reformatted, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert!(diagnostics.is_empty());
        assert_eq!(first.fingerprint(), reformatted.fingerprint());

        write_village(
            temp.path(),
            "plains.toml",
            &VALID.replace("spacing = 4", "spacing = 5"),
        );
        let (changed, diagnostics) = VillageCatalog::load_sorted(temp.path());
        assert!(diagnostics.is_empty());
        assert_ne!(first.fingerprint(), changed.fingerprint());
    }
}
