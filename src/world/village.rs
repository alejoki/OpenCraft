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

use crate::world::block::BlockId;
use crate::world::chunk::{ChunkPos, CHUNK_SIZE};
use crate::world::prefab::{Direction, Prefab, PrefabCatalog, MAX_PREFAB_FILE_BYTES, WORLDGEN_DIR};
use crate::world::terrain::{
    biome_at_seeded, find_spawn_column, ruin_covers, terrain_height_at_seeded, Biome, BEACH_BAND,
    MOUNTAIN_HEIGHT_THRESHOLD, SEA_LEVEL,
};

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

// ---------------------------------------------------------------------------
// Deterministic chunk-local planning (S02-T02).
// ---------------------------------------------------------------------------

/// Chunks per coarse village cell on each horizontal axis.
pub const VILLAGE_CELL_CHUNKS: i32 = 8;

/// Block span of one coarse village cell.
pub const VILLAGE_CELL_SIZE: i32 = VILLAGE_CELL_CHUNKS * CHUNK_SIZE as i32;

/// Inset of the hash-derived anchor from every cell edge.
///
/// Kept `>= VILLAGE_MAX_RADIUS` so two adjacent cells' villages can never
/// overlap across the cell boundary: an anchor in `[margin, size - margin]`
/// plus a plan extent of at most `VILLAGE_MAX_RADIUS` stays inside `[0, size]`.
pub const VILLAGE_CELL_MARGIN: i32 = 64;

/// Hard Chebyshev radius around the anchor for every emitted village column.
pub const VILLAGE_MAX_RADIUS: i32 = 64;

/// Cells whose salted hash does not match this mask are empty by construction.
pub const VILLAGE_GATE_MASK: u32 = 3;

/// Minimum Chebyshev distance between the plan bounds and the player spawn.
pub const VILLAGE_SPAWN_EXCLUSION: i32 = 64;

const _: () = assert!(VILLAGE_CELL_MARGIN >= VILLAGE_MAX_RADIUS);

/// S02-only salts. Deliberately distinct from every terrain/pack hash salt so
/// the village gate, anchor and rotation move independently of terrain.
const VILLAGE_SALT_A: u32 = 0x51ED_2A7B;
const VILLAGE_SALT_B: u32 = 0xA7C3_19E5;
const VILLAGE_PLACEMENT_SALT: u32 = 0x3B9A_CA07;

/// Fixed world order in which the four root wings are built.
const VILLAGE_WING_ORDER: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// A coarse village cell in cell coordinates (not blocks, not chunks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VillageCell {
    pub x: i32,
    pub z: i32,
}

/// The cell containing a world column, using euclidean division so negative
/// coordinates map to the cell that actually contains them.
#[inline]
pub fn village_cell_at(wx: i32, wz: i32) -> VillageCell {
    VillageCell {
        x: wx.div_euclid(VILLAGE_CELL_SIZE),
        z: wz.div_euclid(VILLAGE_CELL_SIZE),
    }
}

/// One of the four quarter-turn rotations, applied to *origin-relative*
/// offsets so the prefab's origin cell stays fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}

impl Rotation {
    pub const fn quarter_turns(self) -> u8 {
        match self {
            Rotation::None => 0,
            Rotation::Clockwise90 => 1,
            Rotation::Clockwise180 => 2,
            Rotation::Clockwise270 => 3,
        }
    }

    /// Rotate an origin-relative `(dx, dz)` offset.
    ///
    /// 90 degrees clockwise is pinned as `(dx, dz) -> (-dz, dx)`, matching the
    /// world convention `North = -Z`, `South = +Z`, `East = +X`, `West = -X`.
    /// Because the transform is axis-aligned it maps an axis-aligned footprint
    /// to another axis-aligned footprint.
    pub const fn apply_offset(self, offset: (i32, i32)) -> (i32, i32) {
        let (dx, dz) = offset;
        match self {
            Rotation::None => (dx, dz),
            Rotation::Clockwise90 => (-dz, dx),
            Rotation::Clockwise180 => (-dx, -dz),
            Rotation::Clockwise270 => (dz, -dx),
        }
    }

    pub fn apply_direction(self, direction: Direction) -> Direction {
        let (dx, dz) = direction_offset(direction);
        let (rx, rz) = self.apply_offset((dx, dz));
        offset_direction(rx, rz).expect("a quarter turn of a cardinal is a cardinal")
    }

    /// The unique rotation that maps `local` to `desired`.
    pub fn for_socket_direction(local: Direction, desired: Direction) -> Option<Rotation> {
        [
            Rotation::None,
            Rotation::Clockwise90,
            Rotation::Clockwise180,
            Rotation::Clockwise270,
        ]
        .into_iter()
        .find(|rotation| rotation.apply_direction(local) == desired)
    }

    const fn from_bits(bits: u32) -> Rotation {
        match bits & 3 {
            0 => Rotation::None,
            1 => Rotation::Clockwise90,
            2 => Rotation::Clockwise180,
            _ => Rotation::Clockwise270,
        }
    }
}

#[inline]
fn direction_offset(direction: Direction) -> (i32, i32) {
    match direction {
        Direction::North => (0, -1),
        Direction::South => (0, 1),
        Direction::East => (1, 0),
        Direction::West => (-1, 0),
    }
}

#[inline]
fn offset_direction(dx: i32, dz: i32) -> Option<Direction> {
    match (dx, dz) {
        (0, -1) => Some(Direction::North),
        (0, 1) => Some(Direction::South),
        (1, 0) => Some(Direction::East),
        (-1, 0) => Some(Direction::West),
        _ => None,
    }
}

#[inline]
fn opposite(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::South,
        Direction::South => Direction::North,
        Direction::East => Direction::West,
        Direction::West => Direction::East,
    }
}

/// Local integer mixer with the shape of terrain's private `hash2d`.
///
/// No `DefaultHasher`, clock, RNG or float participates in any plan decision.
#[inline]
fn village_hash2d(x: i32, z: i32, seed: u32, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B9)
        ^ (z as u32).wrapping_mul(0x85EB_CA6B)
        ^ seed.wrapping_mul(0xC2B2_AE35)
        ^ salt;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

/// The hash-derived anchor of a cell, or `None` when the cell gate rejects it.
fn cell_anchor(cell: VillageCell, seed: u32) -> Option<[i32; 2]> {
    let h1 = village_hash2d(cell.x, cell.z, seed, VILLAGE_SALT_A);
    if h1 & VILLAGE_GATE_MASK != 0 {
        return None;
    }
    let h2 = village_hash2d(cell.x, cell.z, seed, VILLAGE_SALT_B);
    let span = (VILLAGE_CELL_SIZE - 2 * VILLAGE_CELL_MARGIN) as u32;
    let offset_x = ((h1 >> 8) % span) as i32;
    let offset_z = ((h2 >> 8) % span) as i32;
    Some([
        cell.x * VILLAGE_CELL_SIZE + VILLAGE_CELL_MARGIN + offset_x,
        cell.z * VILLAGE_CELL_SIZE + VILLAGE_CELL_MARGIN + offset_z,
    ])
}

#[inline]
fn cell_root_rotation(cell: VillageCell, seed: u32) -> Rotation {
    Rotation::from_bits(village_hash2d(cell.x, cell.z, seed, VILLAGE_SALT_B))
}

/// A cell gate that passed and an anchor column that sits in the village height
/// band with a biome some loaded definition accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VillageSite<'a> {
    pub cell: VillageCell,
    pub anchor: [i32; 2],
    pub definition: &'a VillageDefinition,
}

/// Whether a column's height can host a village site.
///
/// Strict on both sides: the beach shelf (`<= SEA_LEVEL + BEACH_BAND`) and the
/// mountain stone band (`>= MOUNTAIN_HEIGHT_THRESHOLD`) are rejected, so a
/// village never straddles sand or stone.
#[inline]
fn height_is_village_band(height: i32) -> bool {
    height > SEA_LEVEL + BEACH_BAND && height < MOUNTAIN_HEIGHT_THRESHOLD
}

impl VillageCatalog {
    /// Cheap deterministic gate: cell hash, anchor height band and a definition
    /// whose biome list contains the anchor biome.
    pub fn site(&self, cell: VillageCell, seed: u32) -> Option<VillageSite<'_>> {
        if self.villages.is_empty() {
            return None;
        }
        let anchor = cell_anchor(cell, seed)?;
        let height = terrain_height_at_seeded(anchor[0], anchor[1], seed);
        if !height_is_village_band(height) {
            return None;
        }
        let biome = biome_at_seeded(anchor[0], anchor[1], seed);
        let definition = self
            .villages
            .values()
            .find(|definition| definition.biomes.contains(&biome))?;
        Some(VillageSite {
            cell,
            anchor,
            definition,
        })
    }

    /// Whether the cell passes the full site gate.
    pub fn cell_is_candidate(&self, cell: VillageCell, seed: u32) -> bool {
        self.site(cell, seed).is_some()
    }

    /// Conservative per-chunk gate: a site exists and the chunk's x/z span
    /// intersects the anchor's `VILLAGE_MAX_RADIUS` box.
    ///
    /// Every emitted village column is validated inside that box by the fit
    /// pass, so `false` proves the chunk cannot contain any village cell.
    pub fn chunk_may_hold_village(&self, pos: ChunkPos, seed: u32) -> bool {
        let cell = village_cell_at(pos.x * CHUNK_SIZE as i32, pos.z * CHUNK_SIZE as i32);
        let Some(site) = self.site(cell, seed) else {
            return false;
        };
        let chunk_min_x = pos.x * CHUNK_SIZE as i32;
        let chunk_max_x = chunk_min_x + CHUNK_SIZE as i32 - 1;
        let chunk_min_z = pos.z * CHUNK_SIZE as i32;
        let chunk_max_z = chunk_min_z + CHUNK_SIZE as i32 - 1;
        chunk_max_x >= site.anchor[0] - VILLAGE_MAX_RADIUS
            && chunk_min_x <= site.anchor[0] + VILLAGE_MAX_RADIUS
            && chunk_max_z >= site.anchor[1] - VILLAGE_MAX_RADIUS
            && chunk_min_z <= site.anchor[1] + VILLAGE_MAX_RADIUS
    }
}

/// One sampled footprint column of a placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FootprintColumn {
    pub world_x: i32,
    pub world_z: i32,
    pub terrain_y: i32,
}

/// One prefab socket after rotation and translation into world space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedSocket {
    pub position: [i32; 3],
    pub direction: Direction,
    pub linked: bool,
}

/// One accepted prefab placement. Borrows its prefab from the catalog; nothing
/// is cloned per chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VillagePlacement<'a> {
    pub prefab: &'a Prefab,
    pub rotation: Rotation,
    pub origin_x: i32,
    pub origin_z: i32,
    pub base_y: i32,
    /// `[min_x, max_x, min_z, max_z]` in world coordinates.
    pub footprint: [i32; 4],
    pub columns: Vec<FootprintColumn>,
    pub sockets: Vec<PlacedSocket>,
}

/// One accepted parent -> child socket connection.
///
/// `road` is filled in by the T03 road carve; in T02 it is empty and only the
/// socket adjacency is pinned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketLink {
    pub parent: usize,
    pub parent_socket: usize,
    pub child: usize,
    pub child_socket: usize,
    pub road: Vec<usize>,
}

/// One road surface cell (T03 fills these; T02 only defines the shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoadCell {
    pub world: [i32; 3],
    pub terrain_y: i32,
}

/// Exact world-space AABB of every cell the plan can emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VillageBounds {
    pub min_x: i32,
    pub max_x: i32,
    pub min_y: i32,
    pub max_y: i32,
    pub min_z: i32,
    pub max_z: i32,
}

impl VillageBounds {
    pub fn intersects_chunk(&self, pos: ChunkPos) -> bool {
        let chunk_min_x = pos.x * CHUNK_SIZE as i32;
        let chunk_max_x = chunk_min_x + CHUNK_SIZE as i32 - 1;
        let chunk_min_y = pos.y * CHUNK_SIZE as i32;
        let chunk_max_y = chunk_min_y + CHUNK_SIZE as i32 - 1;
        let chunk_min_z = pos.z * CHUNK_SIZE as i32;
        let chunk_max_z = chunk_min_z + CHUNK_SIZE as i32 - 1;
        self.max_x >= chunk_min_x
            && self.min_x <= chunk_max_x
            && self.max_y >= chunk_min_y
            && self.min_y <= chunk_max_y
            && self.max_z >= chunk_min_z
            && self.min_z <= chunk_max_z
    }
}

/// One block the village plan wants written into a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VillageBlock {
    pub world: [i32; 3],
    pub block: BlockId,
}

/// A pure village plan derived only from `(cell, seed, catalog)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VillagePlan<'a> {
    pub cell: VillageCell,
    pub seed: u32,
    pub catalog_fingerprint: u64,
    pub definition: String,
    pub anchor: [i32; 2],
    pub placements: Vec<VillagePlacement<'a>>,
    pub socket_links: Vec<SocketLink>,
    /// Filled by T03's road carve; empty in T02.
    pub road_cells: Vec<RoadCell>,
    pub bounds: VillageBounds,
}

impl VillagePlan<'_> {
    pub fn placement_count(&self) -> usize {
        self.placements.len()
    }

    pub fn socket_link_count(&self) -> usize {
        self.socket_links.len()
    }

    pub fn road_cell_count(&self) -> usize {
        self.road_cells.len()
    }

    /// Placed sockets that no link uses, i.e. side sockets and branches that
    /// stopped before finding a continuation.
    pub fn unmatched_socket_count(&self) -> usize {
        let placed: usize = self.placements.iter().map(|p| p.sockets.len()).sum();
        placed.saturating_sub(2 * self.socket_links.len())
    }
}

/// Origin-relative AABB `[min_x, max_x, min_z, max_z]` of a rotated prefab.
///
/// A quarter turn maps an axis-aligned rectangle to an axis-aligned rectangle,
/// so the four grid corners determine the exact bounds; the fit pass still
/// samples every column, not just the corners.
fn rotated_footprint(prefab: &Prefab, rotation: Rotation) -> [i32; 4] {
    let origin_x = prefab.origin[0] as i32;
    let origin_z = prefab.origin[2] as i32;
    let max_grid_x = prefab.size[0] as i32 - 1;
    let max_grid_z = prefab.size[2] as i32 - 1;
    let mut bounds = [i32::MAX, i32::MIN, i32::MAX, i32::MIN];
    for (grid_x, grid_z) in [
        (0, 0),
        (max_grid_x, 0),
        (0, max_grid_z),
        (max_grid_x, max_grid_z),
    ] {
        let (dx, dz) = rotation.apply_offset((grid_x - origin_x, grid_z - origin_z));
        bounds[0] = bounds[0].min(dx);
        bounds[1] = bounds[1].max(dx);
        bounds[2] = bounds[2].min(dz);
        bounds[3] = bounds[3].max(dz);
    }
    bounds
}

/// Immutable per-plan site inputs shared by every fit attempt.
struct FitSite<'a> {
    anchor: [i32; 2],
    cell_min: [i32; 2],
    seed: u32,
    definition: &'a VillageDefinition,
}

/// Sample and validate one candidate placement, returning it only when every
/// column passes the terrain, biome, ruin, overlap and slope gates.
#[allow(clippy::too_many_arguments)]
fn fit_placement<'a>(
    prefab: &'a Prefab,
    rotation: Rotation,
    origin_x: i32,
    origin_z: i32,
    site: &FitSite<'_>,
    existing: &[VillagePlacement<'a>],
    road_cells: &BTreeMap<(i32, i32), i32>,
) -> Option<VillagePlacement<'a>> {
    let local_bounds = rotated_footprint(prefab, rotation);
    let min_x = origin_x + local_bounds[0];
    let max_x = origin_x + local_bounds[1];
    let min_z = origin_z + local_bounds[2];
    let max_z = origin_z + local_bounds[3];

    let radius_min_x = site.anchor[0] - VILLAGE_MAX_RADIUS;
    let radius_max_x = site.anchor[0] + VILLAGE_MAX_RADIUS;
    let radius_min_z = site.anchor[1] - VILLAGE_MAX_RADIUS;
    let radius_max_z = site.anchor[1] + VILLAGE_MAX_RADIUS;
    if min_x < radius_min_x || max_x > radius_max_x || min_z < radius_min_z || max_z > radius_max_z
    {
        return None;
    }

    // Anchors are inset by `VILLAGE_CELL_MARGIN >= VILLAGE_MAX_RADIUS`, so the
    // radius box already sits inside the cell; this guard keeps the invariant
    // explicit if either constant ever moves.
    let cell_max_x = site.cell_min[0] + VILLAGE_CELL_SIZE;
    let cell_max_z = site.cell_min[1] + VILLAGE_CELL_SIZE;
    if min_x < site.cell_min[0]
        || max_x > cell_max_x
        || min_z < site.cell_min[1]
        || max_z > cell_max_z
    {
        return None;
    }

    for placed in existing {
        let overlaps = min_x <= placed.footprint[1]
            && max_x >= placed.footprint[0]
            && min_z <= placed.footprint[3]
            && max_z >= placed.footprint[2];
        if overlaps {
            return None;
        }
    }

    if road_cells
        .range((min_x, min_z)..=(max_x, max_z))
        .next()
        .is_some()
    {
        return None;
    }

    let origin_grid_x = prefab.origin[0] as i32;
    let origin_grid_z = prefab.origin[2] as i32;
    let mut columns = Vec::with_capacity((prefab.size[0] * prefab.size[2]) as usize);
    let mut min_terrain = i32::MAX;
    let mut max_terrain = i32::MIN;
    for grid_x in 0..prefab.size[0] as i32 {
        for grid_z in 0..prefab.size[2] as i32 {
            let (dx, dz) = rotation.apply_offset((grid_x - origin_grid_x, grid_z - origin_grid_z));
            let world_x = origin_x + dx;
            let world_z = origin_z + dz;
            if world_x < radius_min_x
                || world_x > radius_max_x
                || world_z < radius_min_z
                || world_z > radius_max_z
            {
                return None;
            }
            let terrain_y = terrain_height_at_seeded(world_x, world_z, site.seed);
            if !height_is_village_band(terrain_y) {
                return None;
            }
            if !site
                .definition
                .biomes
                .contains(&biome_at_seeded(world_x, world_z, site.seed))
            {
                return None;
            }
            if ruin_covers(world_x, world_z, site.seed) {
                return None;
            }
            min_terrain = min_terrain.min(terrain_y);
            max_terrain = max_terrain.max(terrain_y);
            columns.push(FootprintColumn {
                world_x,
                world_z,
                terrain_y,
            });
        }
    }

    let base_y = max_terrain;
    if base_y - min_terrain > site.definition.max_slope {
        return None;
    }

    let mut sockets = Vec::with_capacity(prefab.sockets.len());
    for socket in &prefab.sockets {
        let (dx, dz) = rotation.apply_offset((socket.position[0], socket.position[2]));
        sockets.push(PlacedSocket {
            position: [origin_x + dx, base_y + socket.position[1], origin_z + dz],
            direction: rotation.apply_direction(socket.direction),
            linked: false,
        });
    }

    Some(VillagePlacement {
        prefab,
        rotation,
        origin_x,
        origin_z,
        base_y,
        footprint: [min_x, max_x, min_z, max_z],
        columns,
        sockets,
    })
}

fn junction_candidates<'a>(
    catalog: &'a VillageCatalog,
    definition: &VillageDefinition,
) -> Vec<&'a Prefab> {
    definition
        .junction
        .iter()
        .filter_map(|name| catalog.prefabs().get(name))
        .collect()
}

/// Leaf candidate order: a hash of `(cell, placement index, seed)` chooses the
/// first non-empty pool, then the remaining pools in the fixed
/// house/farm/well order are tried, so both houses and farms appear across a
/// village and a prefab that does not fit cannot starve the others.
fn leaf_candidates<'a>(
    catalog: &'a VillageCatalog,
    definition: &VillageDefinition,
    cell: VillageCell,
    seed: u32,
    placement_index: usize,
) -> Vec<&'a Prefab> {
    let pools: [&Vec<String>; 3] = [&definition.house, &definition.farm, &definition.well];
    let non_empty: Vec<&Vec<String>> = pools.into_iter().filter(|pool| !pool.is_empty()).collect();
    if non_empty.is_empty() {
        return Vec::new();
    }
    let mixed_seed = seed.wrapping_add((placement_index as u32).wrapping_mul(0x9E37_79B9));
    let start = (village_hash2d(cell.x, cell.z, mixed_seed, VILLAGE_PLACEMENT_SALT) as usize)
        % non_empty.len();
    let mut candidates = Vec::new();
    for offset in 0..non_empty.len() {
        let pool = non_empty[(start + offset) % non_empty.len()];
        for name in pool.iter() {
            if let Some(prefab) = catalog.prefabs().get(name) {
                candidates.push(prefab);
            }
        }
    }
    candidates
}

/// Plan a village for `cell` using the caller's spawn column.
///
/// This is the only spawn-aware entry point; callers evaluating many cells must
/// compute [`find_spawn_column`] once and pass it in because the search is
/// bounded at 512 rings and is not meant to run per cell.
///
/// Returns `None` for a gated-out cell, an invalid site, a root that does not
/// fit, or a plan whose bounds sit inside [`VILLAGE_SPAWN_EXCLUSION`]. A plan
/// with fewer than `min_prefabs` placements is still returned: the subset
/// policy generates whatever fits rather than failing the chunk.
pub fn village_plan_with_spawn<'a>(
    cell: VillageCell,
    seed: u32,
    catalog: &'a VillageCatalog,
    spawn: (i32, i32),
) -> Option<VillagePlan<'a>> {
    let site = catalog.site(cell, seed)?;
    let definition = site.definition;
    let fit_site = FitSite {
        anchor: site.anchor,
        cell_min: [cell.x * VILLAGE_CELL_SIZE, cell.z * VILLAGE_CELL_SIZE],
        seed,
        definition,
    };

    let root_prefab = catalog.prefabs().get(&definition.hub)?;
    let root_rotation = cell_root_rotation(cell, seed);
    let root = fit_placement(
        root_prefab,
        root_rotation,
        site.anchor[0],
        site.anchor[1],
        &fit_site,
        &[],
        &BTreeMap::new(),
    )?;

    let mut placements = vec![root];
    let mut socket_links: Vec<SocketLink> = Vec::new();
    let road_cells: BTreeMap<(i32, i32), i32> = BTreeMap::new();

    for wing_direction in VILLAGE_WING_ORDER {
        let Some(root_socket) = placements[0]
            .sockets
            .iter()
            .position(|socket| socket.direction == wing_direction)
        else {
            continue;
        };
        let mut parent = 0usize;
        let mut parent_socket = root_socket;

        for step in 1..=4usize {
            if placements.len() >= definition.max_prefabs {
                break;
            }
            let is_leaf = step == 4;
            let candidates = if is_leaf {
                leaf_candidates(catalog, definition, cell, seed, placements.len())
            } else {
                junction_candidates(catalog, definition)
            };

            let parent_position = placements[parent].sockets[parent_socket].position;
            let parent_direction = placements[parent].sockets[parent_socket].direction;
            let (parent_dx, parent_dz) = direction_offset(parent_direction);
            let road_length = definition.spacing + 1;
            let q = [
                parent_position[0] + parent_dx * road_length,
                parent_position[2] + parent_dz * road_length,
            ];
            let desired_child_direction = opposite(parent_direction);

            let mut advanced = false;
            'candidates: for prefab in candidates {
                for (socket_index, socket) in prefab.sockets.iter().enumerate() {
                    let Some(rotation) =
                        Rotation::for_socket_direction(socket.direction, desired_child_direction)
                    else {
                        continue;
                    };
                    let (socket_dx, socket_dz) =
                        rotation.apply_offset((socket.position[0], socket.position[2]));
                    let origin_x = q[0] - socket_dx;
                    let origin_z = q[1] - socket_dz;
                    let Some(placement) = fit_placement(
                        prefab,
                        rotation,
                        origin_x,
                        origin_z,
                        &fit_site,
                        &placements,
                        &road_cells,
                    ) else {
                        continue;
                    };

                    let child = placements.len();
                    placements.push(placement);
                    placements[parent].sockets[parent_socket].linked = true;
                    placements[child].sockets[socket_index].linked = true;
                    socket_links.push(SocketLink {
                        parent,
                        parent_socket,
                        child,
                        child_socket: socket_index,
                        road: Vec::new(),
                    });

                    if is_leaf {
                        break 'candidates;
                    }

                    // Continue along the street: the child socket that points
                    // the same way as the parent's outward direction. Side
                    // sockets stay unmatched (no road, no orphan).
                    let continuation = placements[child]
                        .sockets
                        .iter()
                        .position(|candidate| candidate.direction == parent_direction);
                    if let Some(continuation) = continuation {
                        parent = child;
                        parent_socket = continuation;
                        advanced = true;
                    }
                    break 'candidates;
                }
            }

            if !advanced {
                break;
            }
        }
    }

    let mut bounds = VillageBounds {
        min_x: i32::MAX,
        max_x: i32::MIN,
        min_y: i32::MAX,
        max_y: i32::MIN,
        min_z: i32::MAX,
        max_z: i32::MIN,
    };
    for placement in &placements {
        bounds.min_x = bounds.min_x.min(placement.footprint[0]);
        bounds.max_x = bounds.max_x.max(placement.footprint[1]);
        bounds.min_z = bounds.min_z.min(placement.footprint[2]);
        bounds.max_z = bounds.max_z.max(placement.footprint[3]);
        for column in &placement.columns {
            bounds.min_y = bounds.min_y.min(column.terrain_y);
        }
        bounds.max_y = bounds
            .max_y
            .max(placement.base_y + placement.prefab.size[1] as i32 - 1);
    }

    // AABB Chebyshev distance to the spawn column (not just the anchor, so a
    // wing cannot creep inside the exclusion zone).
    let spawn_dx = if spawn.0 < bounds.min_x {
        bounds.min_x - spawn.0
    } else if spawn.0 > bounds.max_x {
        spawn.0 - bounds.max_x
    } else {
        0
    };
    let spawn_dz = if spawn.1 < bounds.min_z {
        bounds.min_z - spawn.1
    } else if spawn.1 > bounds.max_z {
        spawn.1 - bounds.max_z
    } else {
        0
    };
    if spawn_dx.max(spawn_dz) <= VILLAGE_SPAWN_EXCLUSION {
        return None;
    }

    Some(VillagePlan {
        cell,
        seed,
        catalog_fingerprint: catalog.fingerprint(),
        definition: definition.name.clone(),
        anchor: site.anchor,
        placements,
        socket_links,
        road_cells: Vec::new(),
        bounds,
    })
}

/// Plan a village for `cell`, computing the spawn column once via the bounded
/// [`find_spawn_column`] search.
///
/// Callers evaluating many cells should prefer [`village_plan_with_spawn`] with
/// one precomputed spawn column.
pub fn village_plan<'a>(
    cell: VillageCell,
    seed: u32,
    catalog: &'a VillageCatalog,
) -> Option<VillagePlan<'a>> {
    village_plan_with_spawn(cell, seed, catalog, find_spawn_column(seed))
}

/// Build a catalog directly from parsed content.
///
/// Test-only: production content enters through [`VillageCatalog::load_sorted`].
/// Definitions with duplicate names follow `BTreeMap` insert semantics (the
/// later value wins); the planner tests only pass unique names.
#[cfg(test)]
pub(crate) fn catalog_for_tests(
    prefabs: Vec<Prefab>,
    definitions: Vec<VillageDefinition>,
) -> VillageCatalog {
    let mut villages = BTreeMap::new();
    for definition in definitions {
        villages.insert(definition.name.clone(), definition);
    }
    VillageCatalog {
        prefabs: PrefabCatalog::from_prefabs(prefabs),
        villages,
    }
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

    // ---- S02-T02: cell gate, rotation, fit and socket graph --------------------

    use crate::world::prefab::PrefabSocket;

    fn synthetic_prefab(
        name: &str,
        size: [u32; 3],
        origin: [u32; 3],
        sockets: &[([i32; 3], Direction)],
    ) -> Prefab {
        Prefab {
            name: name.to_owned(),
            origin,
            size,
            blocks: Vec::new(),
            sockets: sockets
                .iter()
                .map(|(position, direction)| PrefabSocket {
                    position: *position,
                    direction: *direction,
                })
                .collect(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn synthetic_definition(
        name: &str,
        hub: &str,
        house: Vec<&str>,
        farm: Vec<&str>,
        well: Vec<&str>,
        junction: Vec<&str>,
        max_slope: i32,
    ) -> VillageDefinition {
        let owned = |names: Vec<&str>| names.into_iter().map(str::to_owned).collect();
        VillageDefinition {
            name: name.to_owned(),
            biomes: vec![Biome::Plains, Biome::Forest],
            hub: hub.to_owned(),
            spacing: 4,
            min_prefabs: 10,
            max_prefabs: 20,
            max_slope,
            house: owned(house),
            farm: owned(farm),
            well: owned(well),
            junction: owned(junction),
        }
    }

    fn reference_catalog() -> VillageCatalog {
        let (catalog, diagnostics) = VillageCatalog::load_sorted(Path::new(WORLDGEN_DIR));
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        catalog
    }

    #[test]
    fn village_cell_at_handles_negative_coordinates() {
        assert_eq!(village_cell_at(0, 0), VillageCell { x: 0, z: 0 });
        assert_eq!(village_cell_at(255, 255), VillageCell { x: 0, z: 0 });
        assert_eq!(village_cell_at(256, 256), VillageCell { x: 1, z: 1 });
        assert_eq!(village_cell_at(-1, -1), VillageCell { x: -1, z: -1 });
        assert_eq!(village_cell_at(-256, -256), VillageCell { x: -1, z: -1 });
        assert_eq!(village_cell_at(-257, 255), VillageCell { x: -2, z: 0 });
    }

    #[test]
    fn rotation_maps_non_square_footprint_exactly() {
        // 5 wide, 3 deep, origin at x=1 (off-centre) proves x/z are not swapped.
        let prefab = synthetic_prefab("lopsided", [5, 2, 3], [1, 0, 0], &[]);
        assert_eq!(rotated_footprint(&prefab, Rotation::None), [-1, 3, 0, 2]);
        assert_eq!(
            rotated_footprint(&prefab, Rotation::Clockwise90),
            [-2, 0, -1, 3]
        );
        assert_eq!(
            rotated_footprint(&prefab, Rotation::Clockwise180),
            [-3, 1, -2, 0]
        );
        assert_eq!(
            rotated_footprint(&prefab, Rotation::Clockwise270),
            [0, 2, -3, 1]
        );
    }

    #[test]
    fn rotation_preserves_the_origin_cell() {
        for rotation in [
            Rotation::None,
            Rotation::Clockwise90,
            Rotation::Clockwise180,
            Rotation::Clockwise270,
        ] {
            assert_eq!(rotation.apply_offset((0, 0)), (0, 0));
        }
        assert_eq!(
            Rotation::Clockwise90.apply_direction(Direction::North),
            Direction::East
        );
        assert_eq!(
            Rotation::Clockwise90.apply_direction(Direction::East),
            Direction::South
        );
        assert_eq!(
            Rotation::Clockwise90.apply_direction(Direction::South),
            Direction::West
        );
        assert_eq!(
            Rotation::Clockwise90.apply_direction(Direction::West),
            Direction::North
        );
        assert_eq!(
            Rotation::Clockwise90.apply_offset((2, 0)),
            (0, 2),
            "clockwise: east becomes south"
        );
    }

    #[test]
    fn for_socket_direction_is_a_total_bijection() {
        let directions = [
            Direction::North,
            Direction::East,
            Direction::South,
            Direction::West,
        ];
        for local in directions {
            for desired in directions {
                let rotation = Rotation::for_socket_direction(local, desired)
                    .expect("every cardinal pair has a rotation");
                assert_eq!(rotation.apply_direction(local), desired);
                let matches = [
                    Rotation::None,
                    Rotation::Clockwise90,
                    Rotation::Clockwise180,
                    Rotation::Clockwise270,
                ]
                .into_iter()
                .filter(|candidate| candidate.apply_direction(local) == desired)
                .count();
                assert_eq!(matches, 1, "rotation for {local:?} -> {desired:?}");
            }
        }
        assert_eq!(
            Rotation::for_socket_direction(Direction::North, Direction::East),
            Some(Rotation::Clockwise90)
        );
    }

    #[test]
    fn height_band_rejects_ocean_beach_and_mountain() {
        assert!(!height_is_village_band(SEA_LEVEL - 1));
        assert!(!height_is_village_band(SEA_LEVEL));
        assert!(!height_is_village_band(SEA_LEVEL + BEACH_BAND));
        assert!(height_is_village_band(SEA_LEVEL + BEACH_BAND + 1));
        assert!(height_is_village_band(MOUNTAIN_HEIGHT_THRESHOLD - 1));
        assert!(!height_is_village_band(MOUNTAIN_HEIGHT_THRESHOLD));
    }

    #[test]
    fn site_returns_only_banded_columns_and_expected_biome() {
        let catalog = reference_catalog();
        let seed = 424_242;
        let spawn = find_spawn_column(seed);
        let mut sites = 0usize;
        for cz in -6..=6 {
            for cx in -6..=6 {
                let cell = VillageCell { x: cx, z: cz };
                let Some(site) = catalog.site(cell, seed) else {
                    continue;
                };
                sites += 1;
                let height = terrain_height_at_seeded(site.anchor[0], site.anchor[1], seed);
                assert!(height_is_village_band(height), "height {height}");
                let biome = biome_at_seeded(site.anchor[0], site.anchor[1], seed);
                assert!(site.definition.biomes.contains(&biome));
                // The anchor is always an inset of its own cell.
                assert!(site.anchor[0] >= cell.x * VILLAGE_CELL_SIZE + VILLAGE_CELL_MARGIN);
                assert!(
                    site.anchor[0]
                        <= cell.x * VILLAGE_CELL_SIZE + VILLAGE_CELL_SIZE - VILLAGE_CELL_MARGIN
                );
                assert!(site.anchor[1] >= cell.z * VILLAGE_CELL_SIZE + VILLAGE_CELL_MARGIN);
                assert!(
                    site.anchor[1]
                        <= cell.z * VILLAGE_CELL_SIZE + VILLAGE_CELL_SIZE - VILLAGE_CELL_MARGIN
                );
            }
        }
        assert!(sites > 0, "no candidate sites in the pinned window");
        // Spawn exclusion is an AABB rule, not an anchor rule.
        let site = catalog.site(village_cell_at(spawn.0, spawn.1), seed);
        if let Some(site) = site {
            assert!(village_plan_with_spawn(site.cell, seed, &catalog, spawn).is_none());
        }
    }

    #[test]
    fn biome_mismatch_rejects_the_site() {
        let seed = 424_242;
        let hub = synthetic_prefab(
            "hub",
            [3, 3, 3],
            [1, 0, 1],
            &[([0, 1, -1], Direction::North)],
        );
        let mut definition = synthetic_definition(
            "plains-only",
            "hub",
            vec!["hub"],
            vec![],
            vec![],
            vec!["hub"],
            6,
        );
        definition.biomes = vec![Biome::Plains];
        let mut forest_definition = definition.clone();
        forest_definition.name = "forest-only".to_owned();
        forest_definition.biomes = vec![Biome::Forest];
        let catalog = catalog_for_tests(vec![hub], vec![definition]);

        let mut found_forest = false;
        for cz in -8..=8 {
            for cx in -8..=8 {
                let cell = VillageCell { x: cx, z: cz };
                let Some(anchor) = cell_anchor(cell, seed) else {
                    continue;
                };
                let height = terrain_height_at_seeded(anchor[0], anchor[1], seed);
                if !height_is_village_band(height) {
                    continue;
                }
                if biome_at_seeded(anchor[0], anchor[1], seed) == Biome::Forest {
                    found_forest = true;
                    assert!(catalog.site(cell, seed).is_none());
                }
            }
        }
        assert!(found_forest, "no gated forest anchor in the scan window");

        let forest_catalog = catalog_for_tests(
            vec![synthetic_prefab(
                "hub",
                [3, 3, 3],
                [1, 0, 1],
                &[([0, 1, -1], Direction::North)],
            )],
            vec![forest_definition],
        );
        let mut matched = false;
        for cz in -8..=8 {
            for cx in -8..=8 {
                if forest_catalog
                    .site(VillageCell { x: cx, z: cz }, seed)
                    .is_some()
                {
                    matched = true;
                }
            }
        }
        assert!(matched, "forest definition must match a forest anchor");
    }

    #[test]
    fn synthetic_one_socket_content_yields_a_two_placement_subset() {
        let seed = 424_242;
        let hub = synthetic_prefab(
            "stub_hub",
            [3, 3, 3],
            [1, 0, 1],
            &[([0, 1, -1], Direction::North)],
        );
        let leaf = synthetic_prefab(
            "stub_leaf",
            [3, 2, 3],
            [1, 0, 1],
            &[([0, 1, 1], Direction::South)],
        );
        let definition = synthetic_definition(
            "stub",
            "stub_hub",
            vec!["stub_leaf"],
            vec![],
            vec![],
            vec!["stub_hub"],
            16,
        );
        let catalog = catalog_for_tests(vec![hub, leaf], vec![definition]);
        let spawn = find_spawn_column(seed);
        let mut found = None;
        for cz in -8..=8 {
            for cx in -8..=8 {
                let cell = VillageCell { x: cx, z: cz };
                if let Some(plan) = village_plan_with_spawn(cell, seed, &catalog, spawn) {
                    found = Some(plan);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let plan = found.expect("a 1-socket hub must still yield a subset plan");
        assert!(
            plan.placement_count() < 10,
            "subset policy, not a 10+ village"
        );
        assert_eq!(plan.placement_count(), 2);
        assert_eq!(plan.socket_link_count(), 1);
        assert!(plan.road_cells.is_empty());
        assert!(plan.bounds.min_x <= plan.anchor[0]);
        assert!(plan.bounds.max_x >= plan.anchor[0]);
        assert!(plan.bounds.min_z <= plan.anchor[1]);
        assert!(plan.bounds.max_z >= plan.anchor[1]);
    }

    #[test]
    fn catalog_margin_covers_the_max_radius() {
        const _: () = assert!(VILLAGE_CELL_MARGIN >= VILLAGE_MAX_RADIUS);
        assert_eq!(VILLAGE_CELL_SIZE, 256);
        assert_eq!(VILLAGE_CELL_CHUNKS * CHUNK_SIZE as i32, VILLAGE_CELL_SIZE);
    }

    #[test]
    fn fit_rejects_a_slope_above_max() {
        let seed = 424_242;
        let prefab = synthetic_prefab(
            "plate",
            [5, 2, 5],
            [2, 0, 2],
            &[([0, 1, -2], Direction::North)],
        );
        let mut definition = synthetic_definition(
            "flat",
            "plate",
            vec!["plate"],
            vec![],
            vec![],
            vec!["plate"],
            0,
        );
        definition.spacing = 4;
        let spawn = find_spawn_column(seed);
        // Find a legal column band where the 5x5 plate has a small height variance.
        let mut checked = false;
        'scan: for wz in (spawn.1 - 128)..=(spawn.1 + 128) {
            for wx in (spawn.0 - 128)..=(spawn.0 + 128) {
                let mut min_h = i32::MAX;
                let mut max_h = i32::MIN;
                let mut viable = true;
                for dz in -2..=2 {
                    for dx in -2..=2 {
                        let h = terrain_height_at_seeded(wx + dx, wz + dz, seed);
                        if !height_is_village_band(h) || ruin_covers(wx + dx, wz + dz, seed) {
                            viable = false;
                            break;
                        }
                        if !definition
                            .biomes
                            .contains(&biome_at_seeded(wx + dx, wz + dz, seed))
                        {
                            viable = false;
                            break;
                        }
                        min_h = min_h.min(h);
                        max_h = max_h.max(h);
                    }
                    if !viable {
                        break;
                    }
                }
                let variance = max_h - min_h;
                if !viable || !(1..=16).contains(&variance) {
                    continue;
                }
                checked = true;
                let cell_min = [wx - VILLAGE_CELL_SIZE / 2, wz - VILLAGE_CELL_SIZE / 2];
                let site = FitSite {
                    anchor: [wx, wz],
                    cell_min,
                    seed,
                    definition: &definition,
                };
                let too_flat = fit_placement(
                    &prefab,
                    Rotation::None,
                    wx,
                    wz,
                    &site,
                    &[],
                    &BTreeMap::new(),
                );
                assert!(
                    too_flat.is_none(),
                    "slope 0 must reject variance {min_h}..{max_h}"
                );
                let mut relaxed = definition.clone();
                relaxed.max_slope = 16;
                let relaxed_site = FitSite {
                    anchor: [wx, wz],
                    cell_min,
                    seed,
                    definition: &relaxed,
                };
                assert!(fit_placement(
                    &prefab,
                    Rotation::None,
                    wx,
                    wz,
                    &relaxed_site,
                    &[],
                    &BTreeMap::new()
                )
                .is_some());
                break 'scan;
            }
        }
        assert!(checked, "no sloped 5x5 plate found near spawn");
    }

    #[test]
    fn fit_rejects_ruin_covered_columns() {
        let seed = 424_242;
        let prefab = synthetic_prefab(
            "pebble",
            [1, 2, 1],
            [0, 0, 0],
            &[([0, 1, 0], Direction::North)],
        );
        let definition = synthetic_definition(
            "ruined",
            "pebble",
            vec!["pebble"],
            vec![],
            vec![],
            vec!["pebble"],
            16,
        );
        let mut found = false;
        for wz in -256..=256 {
            for wx in -256..=256 {
                if !ruin_covers(wx, wz, seed) {
                    continue;
                }
                let site = FitSite {
                    anchor: [wx, wz],
                    cell_min: [wx - VILLAGE_CELL_SIZE, wz - VILLAGE_CELL_SIZE],
                    seed,
                    definition: &definition,
                };
                let placement = fit_placement(
                    &prefab,
                    Rotation::None,
                    wx,
                    wz,
                    &site,
                    &[],
                    &BTreeMap::new(),
                );
                assert!(placement.is_none(), "ruin-covered column must be rejected");
                found = true;
                break;
            }
            if found {
                break;
            }
        }
        assert!(found, "no ruin-covered column found near the origin");
    }

    #[test]
    fn fit_rejects_overlapping_placements() {
        let catalog = reference_catalog();
        let spawn = find_spawn_column(PINNED_DEMO_SEED);
        let plan = village_plan_with_spawn(PINNED_DEMO_CELL, PINNED_DEMO_SEED, &catalog, spawn)
            .expect("pinned plan");
        let root = &plan.placements[0];
        let definition = catalog.get("plains").expect("plains definition");
        let site = FitSite {
            anchor: plan.anchor,
            cell_min: [
                PINNED_DEMO_CELL.x * VILLAGE_CELL_SIZE,
                PINNED_DEMO_CELL.z * VILLAGE_CELL_SIZE,
            ],
            seed: PINNED_DEMO_SEED,
            definition,
        };
        // The same prefab shifted by one block overlaps the accepted root.
        let overlap = fit_placement(
            root.prefab,
            root.rotation,
            root.origin_x + 1,
            root.origin_z,
            &site,
            std::slice::from_ref(root),
            &BTreeMap::new(),
        );
        assert!(overlap.is_none(), "overlapping footprint must be rejected");
    }

    #[test]
    fn plan_borrows_prefabs_from_the_catalog() {
        let catalog = reference_catalog();
        let seed = 424_242;
        let spawn = find_spawn_column(seed);
        let plan = scan_for_plan(&catalog, seed, spawn, |_| true).expect("a plan exists");
        for placement in &plan.placements {
            let from_catalog = catalog
                .prefabs()
                .get(&placement.prefab.name)
                .expect("placement prefab is in the catalog");
            assert!(std::ptr::eq(placement.prefab, from_catalog));
        }
    }

    fn scan_for_plan<'a>(
        catalog: &'a VillageCatalog,
        seed: u32,
        spawn: (i32, i32),
        accept: impl Fn(&VillagePlan<'a>) -> bool,
    ) -> Option<VillagePlan<'a>> {
        for cz in -6..=6 {
            for cx in -6..=6 {
                let cell = VillageCell { x: cx, z: cz };
                if let Some(plan) = village_plan_with_spawn(cell, seed, catalog, spawn) {
                    if accept(&plan) {
                        return Some(plan);
                    }
                }
            }
        }
        None
    }

    /// Cell pinned by T02 for seed `424_242`; T05's demo reuses it. The scan of
    /// the fixed -6..=6 window finds it with 17 placements / 16 links.
    const PINNED_DEMO_SEED: u32 = 424_242;
    const PINNED_DEMO_CELL: VillageCell = VillageCell { x: 3, z: 5 };

    #[test]
    fn checked_in_catalog_plan_is_pinned() {
        let catalog = reference_catalog();
        let spawn = find_spawn_column(PINNED_DEMO_SEED);
        let plan = village_plan_with_spawn(PINNED_DEMO_CELL, PINNED_DEMO_SEED, &catalog, spawn)
            .expect("pinned demo cell must plan");

        assert_eq!(plan.cell, PINNED_DEMO_CELL);
        assert_eq!(plan.seed, PINNED_DEMO_SEED);
        assert_eq!(plan.definition, "plains");
        assert_eq!(plan.anchor, [916, 1446]);
        assert_eq!(plan.catalog_fingerprint, catalog.fingerprint());
        assert_eq!(plan.placement_count(), 17);
        assert_eq!(plan.socket_link_count(), 16);
        // 13 hub placements x 4 sockets + 4 leaf sockets = 56; 16 links use 32.
        assert_eq!(plan.unmatched_socket_count(), 24);
        assert!(plan.road_cells.is_empty(), "roads are T03 scope");

        // Root is the hub at the anchor; every placement stays inside the
        // conservative anchor radius and no two AABBs overlap.
        assert_eq!(plan.placements[0].prefab.name, "plains_hub");
        assert_eq!(plan.placements[0].origin_x, plan.anchor[0]);
        assert_eq!(plan.placements[0].origin_z, plan.anchor[1]);
        for placement in &plan.placements {
            assert!(placement.footprint[0] >= plan.anchor[0] - VILLAGE_MAX_RADIUS);
            assert!(placement.footprint[1] <= plan.anchor[0] + VILLAGE_MAX_RADIUS);
            assert!(placement.footprint[2] >= plan.anchor[1] - VILLAGE_MAX_RADIUS);
            assert!(placement.footprint[3] <= plan.anchor[1] + VILLAGE_MAX_RADIUS);
            assert!(matches!(
                placement.prefab.name.as_str(),
                "plains_hub" | "small_house" | "wheat_farm"
            ));
        }
        for (index, left) in plan.placements.iter().enumerate() {
            for right in plan.placements.iter().skip(index + 1) {
                let overlaps = left.footprint[0] <= right.footprint[1]
                    && left.footprint[1] >= right.footprint[0]
                    && left.footprint[2] <= right.footprint[3]
                    && left.footprint[3] >= right.footprint[2];
                assert!(
                    !overlaps,
                    "{} overlaps {}",
                    left.prefab.name, right.prefab.name
                );
            }
        }

        // Every leaf's only socket is linked and every junction's connection
        // and continuation sockets are linked: no branch stopped early.
        let names: Vec<&str> = plan
            .placements
            .iter()
            .map(|placement| placement.prefab.name.as_str())
            .collect();
        assert_eq!(
            names.iter().filter(|name| **name == "plains_hub").count(),
            13
        );
        assert_eq!(
            names.iter().filter(|name| **name == "small_house").count(),
            2
        );
        assert_eq!(
            names.iter().filter(|name| **name == "wheat_farm").count(),
            2
        );
        for placement in &plan.placements {
            if placement.prefab.name != "plains_hub" {
                assert_eq!(placement.sockets.len(), 1);
                assert!(placement.sockets[0].linked, "leaf socket must be linked");
            } else {
                assert!(
                    placement
                        .sockets
                        .iter()
                        .filter(|socket| socket.linked)
                        .count()
                        >= 2
                );
            }
        }
    }

    #[test]
    fn pinned_window_contains_a_ten_to_twenty_placement_cell() {
        let catalog = reference_catalog();
        let spawn = find_spawn_column(PINNED_DEMO_SEED);
        let mut found = Vec::new();
        for cz in -6..=6 {
            for cx in -6..=6 {
                let cell = VillageCell { x: cx, z: cz };
                if let Some(plan) = village_plan_with_spawn(cell, PINNED_DEMO_SEED, &catalog, spawn)
                {
                    if (10..=20).contains(&plan.placement_count()) {
                        found.push((cell, plan.placement_count(), plan.socket_link_count()));
                    }
                }
            }
        }
        assert!(!found.is_empty(), "no 10..=20 cell in the pinned window");
        assert!(found.contains(&(PINNED_DEMO_CELL, 17, 16)));
    }

    #[test]
    fn spawn_exclusion_uses_the_plan_bounds() {
        let catalog = reference_catalog();
        let spawn = find_spawn_column(PINNED_DEMO_SEED);
        assert!(
            village_plan_with_spawn(PINNED_DEMO_CELL, PINNED_DEMO_SEED, &catalog, spawn).is_some()
        );
        let plan = village_plan_with_spawn(PINNED_DEMO_CELL, PINNED_DEMO_SEED, &catalog, spawn)
            .expect("baseline plan");
        let anchor = plan.anchor;
        assert!(
            village_plan_with_spawn(
                PINNED_DEMO_CELL,
                PINNED_DEMO_SEED,
                &catalog,
                (anchor[0], anchor[1])
            )
            .is_none(),
            "a spawn at the anchor is inside the exclusion"
        );
        // Just outside the AABB by Chebyshev distance the plan is allowed again.
        let outside = (plan.bounds.max_x + VILLAGE_SPAWN_EXCLUSION + 1, anchor[1]);
        assert!(
            village_plan_with_spawn(PINNED_DEMO_CELL, PINNED_DEMO_SEED, &catalog, outside)
                .is_some()
        );
    }

    #[test]
    fn chunk_gate_covers_every_emitted_column() {
        let catalog = reference_catalog();
        let spawn = find_spawn_column(PINNED_DEMO_SEED);
        let plan = village_plan_with_spawn(PINNED_DEMO_CELL, PINNED_DEMO_SEED, &catalog, spawn)
            .expect("pinned plan");
        for placement in &plan.placements {
            for column in &placement.columns {
                let pos = ChunkPos::new(
                    column.world_x.div_euclid(CHUNK_SIZE as i32),
                    placement.base_y.div_euclid(CHUNK_SIZE as i32),
                    column.world_z.div_euclid(CHUNK_SIZE as i32),
                );
                assert!(
                    catalog.chunk_may_hold_village(pos, PINNED_DEMO_SEED),
                    "chunk {pos:?} holds a village column but the gate said no"
                );
            }
        }
        let far = ChunkPos::new(10_000, 0, 10_000);
        assert!(!catalog.chunk_may_hold_village(far, PINNED_DEMO_SEED));
    }
}
