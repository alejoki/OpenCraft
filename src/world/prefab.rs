//! Pure prefab codec for authored world population structures.
//!
//! A prefab is one TOML document: a fixed header (`format`, `name`, `origin`,
//! `size`), a `[palette]` of single printable ASCII characters to built-in
//! block names, `layers` of palette rows (`layers[y][z]` is a row along `+x`,
//! with `.` meaning air), and optional `[[sockets]]` with an origin-relative
//! position plus a cardinal [`Direction`].
//!
//! The parser and serializer are deliberately pure: no window, GPU, save or
//! pack access. The [`PrefabCatalog`] walker is the only layer that touches
//! disk, and every read is capped by [`MAX_PREFAB_FILE_BYTES`].
//!
//! Two failure policies coexist here and must not be confused:
//!
//! * [`PrefabCatalog::load_sorted`] is **fail-soft**. It never returns an
//!   error and never blocks world load: a bad file degrades to one diagnostic
//!   string and is skipped.
//! * [`validate_worldgen_lines`] is **fail-closed**. It reuses the same walk
//!   but reports `ok == false` when any file was rejected, which the
//!   release-facing `--validate-worldgen` switch turns into a non-zero exit.
//!
//! Coordinate model: `origin` is the grid cell aligned to the placement
//! anchor; block positions are grid cells `0..size`; socket positions are
//! stored as `grid - origin`, so they may be negative. Every size and index is
//! checked against [`PREFAB_CAPS`] before it is used, and no authored input can
//! panic this parser or overflow an accumulator.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;

use crate::content::names;
use crate::world::block::BlockId;

/// Directory that owns declarative world-generation content.
pub const WORLDGEN_DIR: &str = "worldgen";

/// Directory, below [`WORLDGEN_DIR`], that owns one TOML file per prefab.
pub const PREFABS_SUBDIR: &str = "prefabs";

/// Hard size cap for a single prefab file; anything larger is rejected whole.
///
/// Mirrors the private `config::MAX_CONFIG_FILE_BYTES` policy so authored
/// content and curated config share the same 1 MiB trust boundary.
pub const MAX_PREFAB_FILE_BYTES: u64 = 1024 * 1024;

/// Per-axis prefab size caps, shared by the parser, the capture writer and
/// every future village assembler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefabCaps {
    pub max_x: u32,
    pub max_y: u32,
    pub max_z: u32,
}

/// The single prefab size cap: 32 cells on each axis.
pub const PREFAB_CAPS: PrefabCaps = PrefabCaps {
    max_x: 32,
    max_y: 32,
    max_z: 32,
};

/// Cardinal facing stored on a prefab socket.
///
/// The world-space convention is pinned here so rotation (S02) and marker
/// derivation (S06) never disagree: `North = -Z`, `South = +Z`, `East = +X`,
/// `West = -X`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    North,
    South,
    East,
    West,
}

impl Direction {
    /// The stable lowercase name used by prefab TOML and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::North => "north",
            Direction::South => "south",
            Direction::East => "east",
            Direction::West => "west",
        }
    }

    /// Parse the stable lowercase name, rejecting anything else.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "north" => Some(Direction::North),
            "south" => Some(Direction::South),
            "east" => Some(Direction::East),
            "west" => Some(Direction::West),
            _ => None,
        }
    }
}

/// One grid cell of a prefab. Positions are `[x, y, z]` inside `0..size`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefabBlock {
    pub position: [u32; 3],
    pub block: BlockId,
}

/// One attached structure socket, stored relative to [`Prefab::origin`].
///
/// Because the origin is the anchor cell, a socket at the grid border on the
/// negative side of the origin has a negative component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefabSocket {
    pub position: [i32; 3],
    pub direction: Direction,
}

/// A fully parsed prefab value.
///
/// `blocks` contains every cell of the grid, air included, in `(y, z, x)`
/// order. `sockets` is sorted by `(position.y, position.z, position.x)`, so
/// two parses of equivalent documents compare equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefab {
    pub name: String,
    pub origin: [u32; 3],
    pub size: [u32; 3],
    pub blocks: Vec<PrefabBlock>,
    pub sockets: Vec<PrefabSocket>,
}

/// One stable, one-line prefab reason.
///
/// `Display` emits exactly the pinned text; TOML line/column noise is
/// deliberately discarded so catalog diagnostics remain greppable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrefabError {
    MissingFormat,
    InvalidFormat,
    UnsupportedFormat(i64),
    MissingName,
    NameTooLong,
    NameInvalidCharacter,
    MissingSize,
    InvalidSize,
    SizeAxisTooSmall,
    SizeExceedsCap([i64; 3]),
    MissingOrigin,
    InvalidOrigin,
    OriginOutside([i64; 3]),
    MissingPalette,
    InvalidPaletteKey(String),
    ReservedPaletteKey,
    UnknownPaletteBlock {
        key: String,
        block: String,
    },
    AirPaletteBlock(String),
    MissingLayers,
    LayerCount {
        found: usize,
        expected: u32,
    },
    RowCount {
        y: usize,
        found: usize,
        expected: u32,
    },
    RowLength {
        y: usize,
        z: usize,
        found: usize,
        expected: u32,
    },
    UndefinedPaletteCharacter(char),
    InvalidSocketPosition,
    SocketOutside([i64; 3]),
    UnknownSocketDirection(String),
    DuplicateSocket([i32; 3]),
    InvalidToml,
    FileTooLarge,
    CouldNotReadFile,
}

impl fmt::Display for PrefabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrefabError::MissingFormat => write!(f, "missing format"),
            PrefabError::InvalidFormat => write!(f, "invalid format"),
            PrefabError::UnsupportedFormat(version) => {
                write!(f, "unsupported format {version}")
            }
            PrefabError::MissingName => write!(f, "missing name"),
            PrefabError::NameTooLong => write!(f, "name is longer than 64 bytes"),
            PrefabError::NameInvalidCharacter => {
                write!(f, "name may only use a-z, 0-9, _ and -")
            }
            PrefabError::MissingSize => write!(f, "missing size"),
            PrefabError::InvalidSize => write!(f, "size must be [x, y, z]"),
            PrefabError::SizeAxisTooSmall => write!(f, "size axis must be at least 1"),
            PrefabError::SizeExceedsCap([x, y, z]) => {
                write!(f, "size {x}x{y}x{z} exceeds cap 32x32x32")
            }
            PrefabError::MissingOrigin => write!(f, "missing origin"),
            PrefabError::InvalidOrigin => write!(f, "origin must be [x, y, z]"),
            PrefabError::OriginOutside([x, y, z]) => {
                write!(f, "origin [{x}, {y}, {z}] is outside the prefab")
            }
            PrefabError::MissingPalette => write!(f, "missing palette"),
            PrefabError::InvalidPaletteKey(key) => {
                write!(
                    f,
                    "palette key '{key}' must be one printable ASCII character"
                )
            }
            PrefabError::ReservedPaletteKey => {
                write!(f, "palette key '.' is reserved for air")
            }
            PrefabError::UnknownPaletteBlock { key, block } => {
                write!(f, "palette value for '{key}' is unknown block '{block}'")
            }
            PrefabError::AirPaletteBlock(key) => {
                write!(f, "palette value for '{key}' cannot be air")
            }
            PrefabError::MissingLayers => write!(f, "missing layers"),
            PrefabError::LayerCount { found, expected } => {
                write!(f, "layers has {found} rows, expected {expected}")
            }
            PrefabError::RowCount { y, found, expected } => {
                write!(f, "layer {y} has {found} rows, expected {expected}")
            }
            PrefabError::RowLength {
                y,
                z,
                found,
                expected,
            } => {
                write!(f, "row {y}/{z} has length {found}, expected {expected}")
            }
            PrefabError::UndefinedPaletteCharacter(character) => {
                write!(f, "palette character '{character}' is not defined")
            }
            PrefabError::InvalidSocketPosition => {
                write!(f, "socket position must be [x, y, z]")
            }
            PrefabError::SocketOutside([x, y, z]) => {
                write!(f, "socket position [{x}, {y}, {z}] is outside the prefab")
            }
            PrefabError::UnknownSocketDirection(direction) => {
                write!(f, "socket direction '{direction}' is unknown")
            }
            PrefabError::DuplicateSocket([x, y, z]) => {
                write!(f, "duplicate socket at [{x}, {y}, {z}]")
            }
            PrefabError::InvalidToml => write!(f, "invalid TOML"),
            PrefabError::FileTooLarge => write!(f, "file is larger than 1 MiB"),
            PrefabError::CouldNotReadFile => write!(f, "could not read file"),
        }
    }
}

impl std::error::Error for PrefabError {}

/// Parse one prefab document.
///
/// Checks run in a fixed order so the first reported reason is stable:
/// `format` → `name` → `size` → `origin` → `[palette]` → `layers` →
/// `[[sockets]]`. Any TOML syntax error normalizes to [`PrefabError::InvalidToml`].
pub fn parse_prefab(text: &str) -> Result<Prefab, PrefabError> {
    let root: toml::Value = toml::from_str(text).map_err(|_| PrefabError::InvalidToml)?;
    let root = root.as_table().ok_or(PrefabError::InvalidToml)?;

    let format = root
        .get("format")
        .ok_or(PrefabError::MissingFormat)?
        .as_integer()
        .ok_or(PrefabError::InvalidFormat)?;
    if format != 1 {
        return Err(PrefabError::UnsupportedFormat(format));
    }

    let name = root
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or(PrefabError::MissingName)?;
    validate_prefab_name(name)?;
    let name = name.to_owned();

    let size_raw = integer_triple(root.get("size").ok_or(PrefabError::MissingSize)?)
        .ok_or(PrefabError::InvalidSize)?;
    let caps = [PREFAB_CAPS.max_x, PREFAB_CAPS.max_y, PREFAB_CAPS.max_z];
    for (axis, cap) in size_raw.iter().zip(caps) {
        if *axis < 1 {
            return Err(PrefabError::SizeAxisTooSmall);
        }
        if *axis > i64::from(cap) {
            return Err(PrefabError::SizeExceedsCap(size_raw));
        }
    }
    let size = [size_raw[0] as u32, size_raw[1] as u32, size_raw[2] as u32];

    let origin_raw = integer_triple(root.get("origin").ok_or(PrefabError::MissingOrigin)?)
        .ok_or(PrefabError::InvalidOrigin)?;
    let mut origin = [0u32; 3];
    for axis in 0..3 {
        let value = origin_raw[axis];
        if value < 0 || value >= i64::from(size[axis]) {
            return Err(PrefabError::OriginOutside(origin_raw));
        }
        origin[axis] = value as u32;
    }

    let palette_value = root.get("palette").ok_or(PrefabError::MissingPalette)?;
    let palette_table = palette_value
        .as_table()
        .ok_or(PrefabError::MissingPalette)?;
    let mut palette: BTreeMap<char, BlockId> = BTreeMap::new();
    for (key, value) in palette_table {
        let key_character = palette_key_character(key)?;
        let block_name = value.as_str().unwrap_or_default();
        let block = names::builtin_block_id_by_name(block_name).ok_or_else(|| {
            PrefabError::UnknownPaletteBlock {
                key: key.clone(),
                block: block_name.to_owned(),
            }
        })?;
        if block == BlockId::Air {
            return Err(PrefabError::AirPaletteBlock(key.clone()));
        }
        palette.insert(key_character, block);
    }

    let layers_value = root.get("layers").ok_or(PrefabError::MissingLayers)?;
    let layers = layers_value.as_array().ok_or(PrefabError::MissingLayers)?;
    if layers.len() != size[1] as usize {
        return Err(PrefabError::LayerCount {
            found: layers.len(),
            expected: size[1],
        });
    }

    let mut blocks = Vec::with_capacity(size[0] as usize * size[1] as usize * size[2] as usize);
    for (y, layer_value) in layers.iter().enumerate() {
        let layer = layer_value.as_array().ok_or(PrefabError::InvalidToml)?;
        if layer.len() != size[2] as usize {
            return Err(PrefabError::RowCount {
                y,
                found: layer.len(),
                expected: size[2],
            });
        }
        for (z, row_value) in layer.iter().enumerate() {
            let row = row_value.as_str().ok_or(PrefabError::InvalidToml)?;
            if row.len() != size[0] as usize {
                return Err(PrefabError::RowLength {
                    y,
                    z,
                    found: row.len(),
                    expected: size[0],
                });
            }
            let mut x = 0usize;
            for character in row.chars() {
                if x >= size[0] as usize {
                    // Only reachable when a multi-byte character makes the
                    // byte length match while the character count does not.
                    return Err(PrefabError::RowLength {
                        y,
                        z,
                        found: row.len(),
                        expected: size[0],
                    });
                }
                let block = if character == '.' {
                    BlockId::Air
                } else {
                    *palette
                        .get(&character)
                        .ok_or(PrefabError::UndefinedPaletteCharacter(character))?
                };
                blocks.push(PrefabBlock {
                    position: [x as u32, y as u32, z as u32],
                    block,
                });
                x += 1;
            }
            if x != size[0] as usize {
                return Err(PrefabError::RowLength {
                    y,
                    z,
                    found: row.len(),
                    expected: size[0],
                });
            }
        }
    }

    let mut sockets: Vec<PrefabSocket> = Vec::new();
    if let Some(sockets_value) = root.get("sockets") {
        let sockets_array = sockets_value.as_array().ok_or(PrefabError::InvalidToml)?;
        for socket_value in sockets_array {
            let socket_table = socket_value.as_table().ok_or(PrefabError::InvalidToml)?;
            let position = socket_table
                .get("position")
                .and_then(integer_triple_i32)
                .ok_or(PrefabError::InvalidSocketPosition)?;

            let outside = (0..3).any(|axis| {
                let cell = i64::from(origin[axis]) + i64::from(position[axis]);
                cell < 0 || cell >= i64::from(size[axis])
            });
            if outside {
                return Err(PrefabError::SocketOutside([
                    i64::from(position[0]),
                    i64::from(position[1]),
                    i64::from(position[2]),
                ]));
            }

            let direction_name = socket_table
                .get("direction")
                .and_then(toml::Value::as_str)
                .unwrap_or_default();
            let direction = Direction::from_name(direction_name)
                .ok_or_else(|| PrefabError::UnknownSocketDirection(direction_name.to_owned()))?;

            if sockets.iter().any(|socket| socket.position == position) {
                return Err(PrefabError::DuplicateSocket(position));
            }
            sockets.push(PrefabSocket {
                position,
                direction,
            });
        }
    }
    sockets.sort_by_key(|socket| (socket.position[1], socket.position[2], socket.position[0]));

    Ok(Prefab {
        name,
        origin,
        size,
        blocks,
        sockets,
    })
}

impl Prefab {
    /// Serialize to the canonical deterministic TOML encoding.
    ///
    /// Fixed header order, `layers` in `y` then `z` order, a `[palette]`
    /// assigned in ascending block-id order (so keys come out sorted by
    /// character), and `[[sockets]]` sorted by `(y, z, x)`. `layers` precedes
    /// the `[palette]` header because TOML scopes every key after a table
    /// header to that table. `parse_prefab(&prefab.to_toml_string())`
    /// reproduces an equal value and serializing that value is byte-identical.
    ///
    /// Only built-in blocks are representable; a modded id is written as air
    /// because the prefab format intentionally has no mod namespace.
    pub fn to_toml_string(&self) -> String {
        let mut output = String::new();
        output.push_str("format = 1\n");
        output.push_str(&format!("name = \"{}\"\n", escape_toml_string(&self.name)));
        output.push_str(&format!(
            "origin = [{}, {}, {}]\n",
            self.origin[0], self.origin[1], self.origin[2]
        ));
        output.push_str(&format!(
            "size = [{}, {}, {}]\n",
            self.size[0], self.size[1], self.size[2]
        ));
        output.push('\n');

        let mut distinct: BTreeMap<u16, BlockId> = BTreeMap::new();
        for block in &self.blocks {
            if block.block == BlockId::Air {
                continue;
            }
            if names::builtin_block_id_name(block.block).is_none() {
                continue;
            }
            distinct.entry(block.block.to_raw()).or_insert(block.block);
        }

        let alphabet: Vec<char> = (b'!'..=b'~')
            .map(char::from)
            .filter(|character| !matches!(character, '.' | '"' | '\\'))
            .collect();
        let mut block_characters: BTreeMap<u16, char> = BTreeMap::new();
        let mut palette_entries: Vec<(char, &'static str)> = Vec::new();
        for (index, (raw, block)) in distinct.iter().enumerate() {
            let Some(character) = alphabet.get(index).copied() else {
                break;
            };
            let Some(block_name) = names::builtin_block_id_name(*block) else {
                continue;
            };
            block_characters.insert(*raw, character);
            palette_entries.push((character, block_name));
        }

        let mut grid: BTreeMap<[u32; 3], BlockId> = BTreeMap::new();
        for block in &self.blocks {
            grid.insert(block.position, block.block);
        }
        output.push_str("layers = [\n");
        for y in 0..self.size[1] {
            output.push_str("  [ ");
            for z in 0..self.size[2] {
                if z > 0 {
                    output.push_str(", ");
                }
                let mut row = String::with_capacity(self.size[0] as usize);
                for x in 0..self.size[0] {
                    let block = grid.get(&[x, y, z]).copied().unwrap_or(BlockId::Air);
                    let character = if block == BlockId::Air {
                        '.'
                    } else {
                        block_characters
                            .get(&block.to_raw())
                            .copied()
                            .unwrap_or('.')
                    };
                    row.push(character);
                }
                output.push('"');
                output.push_str(&escape_toml_string(&row));
                output.push('"');
            }
            output.push_str(" ],\n");
        }
        output.push_str("]\n");

        // TOML scopes every key after a table header to that table, so the
        // root-level `layers` array must precede the `[palette]` table.
        output.push('\n');
        output.push_str("[palette]\n");
        for (character, block_name) in &palette_entries {
            output.push_str(&format!(
                "\"{}\" = \"{}\"\n",
                escape_toml_string(&character.to_string()),
                escape_toml_string(block_name)
            ));
        }

        let mut sockets: Vec<&PrefabSocket> = self.sockets.iter().collect();
        sockets.sort_by_key(|socket| (socket.position[1], socket.position[2], socket.position[0]));
        for socket in sockets {
            output.push_str("\n[[sockets]]\n");
            output.push_str(&format!(
                "position = [{}, {}, {}]\n",
                socket.position[0], socket.position[1], socket.position[2]
            ));
            output.push_str(&format!("direction = \"{}\"\n", socket.direction.as_str()));
        }

        output
    }
}

// ---------------------------------------------------------------------------
// Catalog: the only filesystem-facing layer.
// ---------------------------------------------------------------------------

/// One prefab file's outcome during a sorted directory walk.
struct PrefabFile {
    file_name: String,
    outcome: Result<Prefab, String>,
}

/// A name-sorted, fail-soft collection of prefabs loaded from disk.
///
/// [`PrefabCatalog::load_sorted`] never fails: every unreadable, oversize or
/// malformed file degrades to one diagnostic string and is skipped, so world
/// load can always continue with whatever parsed. Only
/// [`validate_worldgen_lines`] turns those rejections into a non-zero exit
/// code; that is the release-facing `--validate-worldgen` contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrefabCatalog {
    prefabs: BTreeMap<String, Prefab>,
}

impl PrefabCatalog {
    /// Walk `root/prefabs` in raw file-name order and load every `*.toml`
    /// regular file.
    ///
    /// Fail-soft by contract. A missing root, a non-directory root, a missing
    /// `prefabs/` subdirectory, or an unreadable directory yields an empty
    /// catalog with no diagnostics. Per file the first failure wins and the
    /// file is skipped:
    ///
    /// 1. metadata/read error -> `could not read file: <error>`;
    /// 2. more than [`MAX_PREFAB_FILE_BYTES`] -> `file is larger than 1 MiB`;
    /// 3. [`parse_prefab`] error -> its `Display` reason;
    /// 4. duplicate header `name` -> the sorted-first file wins and the later
    ///    file reports `duplicate prefab name '<name>'`.
    ///
    /// Non-regular files, directories and non-`toml` extensions are ignored
    /// silently. Diagnostics always use the logical `worldgen/prefabs/<file>`
    /// label, never the caller's spelling of `root`.
    pub fn load_sorted(root: &Path) -> (Self, Vec<String>) {
        let mut catalog = PrefabCatalog::default();
        let mut diagnostics = Vec::new();
        for file in scan_prefab_dir(root) {
            match file.outcome {
                Ok(prefab) => {
                    catalog.prefabs.insert(prefab.name.clone(), prefab);
                }
                Err(reason) => diagnostics.push(logical_file_label(&file.file_name, &reason)),
            }
        }
        (catalog, diagnostics)
    }

    /// Look a prefab up by its header `name`.
    pub fn get(&self, name: &str) -> Option<&Prefab> {
        self.prefabs.get(name)
    }

    /// Iterate loaded prefab names in ascending order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.prefabs.keys().map(String::as_str)
    }

    /// Number of accepted prefabs.
    pub fn len(&self) -> usize {
        self.prefabs.len()
    }

    /// Whether no prefab was accepted.
    pub fn is_empty(&self) -> bool {
        self.prefabs.is_empty()
    }

    /// Build a catalog directly from parsed prefabs.
    ///
    /// Test-only: production content always enters through the sorted,
    /// fail-soft filesystem walk. This constructor exists so village planner
    /// tests can exercise synthetic non-square and hand-built geometries
    /// without writing TOML fixtures. Later prefabs with a duplicate name win,
    /// mirroring a `BTreeMap` insert.
    #[cfg(test)]
    pub(crate) fn from_prefabs(prefabs: Vec<Prefab>) -> Self {
        let mut catalog = PrefabCatalog::default();
        for prefab in prefabs {
            catalog.prefabs.insert(prefab.name.clone(), prefab);
        }
        catalog
    }
}

/// Run the sorted walk and render the stable `--validate-worldgen` report.
///
/// One `ok: prefab <name>` line per accepted file, one
/// `error: worldgen/prefabs/<file>: <reason>` line per rejected file (which
/// suppresses that file's `ok:` line), then always a final
/// `validate-worldgen: <n> prefabs, <e> errors, <w> warnings` summary where
/// `w` is currently always `0` and stays wired for the village pass.
///
/// Returns `(lines, ok)` with `ok == (errors == 0)`. Missing or unreadable
/// trees are valid empty content: only the zero summary line, `ok == true`.
/// This is the fail-closed half of the catalog policy: unlike
/// [`PrefabCatalog::load_sorted`], any rejected file makes `ok` false.
pub fn validate_worldgen_lines(root: &Path) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut accepted = 0usize;
    let mut errors = 0usize;
    for file in scan_prefab_dir(root) {
        match file.outcome {
            Ok(prefab) => {
                accepted += 1;
                lines.push(format!("ok: prefab {}", prefab.name));
            }
            Err(reason) => {
                errors += 1;
                lines.push(format!(
                    "error: {}",
                    logical_file_label(&file.file_name, &reason)
                ));
            }
        }
    }
    let warnings = 0usize;
    lines.push(format!(
        "validate-worldgen: {accepted} prefabs, {errors} errors, {warnings} warnings"
    ));
    (lines, errors == 0)
}

/// Logical `worldgen/prefabs/<file>: <reason>` label used by every diagnostic.
fn logical_file_label(file_name: &str, reason: &str) -> String {
    format!("{WORLDGEN_DIR}/{PREFABS_SUBDIR}/{file_name}: {reason}")
}

/// Walk `root/prefabs` in raw file-name order, applying the per-file policy.
///
/// Sorting by `OsString` (raw bytes on Unix) removes `fs::read_dir` ordering
/// nondeterminism, so first-wins duplicate handling and report order are
/// stable across filesystems.
fn scan_prefab_dir(root: &Path) -> Vec<PrefabFile> {
    let dir = root.join(PREFABS_SUBDIR);
    let Ok(read_dir) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut entries: Vec<_> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(|left| left.file_name());

    let mut seen_names: BTreeSet<String> = BTreeSet::new();
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
                files.push(PrefabFile {
                    file_name,
                    outcome: Err(format!("could not read file: {error}")),
                });
                continue;
            }
        }

        let outcome = match read_prefab_file(&path) {
            Ok(text) => parse_prefab(&text)
                .map_err(|error| error.to_string())
                .and_then(|prefab| {
                    if seen_names.contains(&prefab.name) {
                        Err(format!("duplicate prefab name '{}'", prefab.name))
                    } else {
                        seen_names.insert(prefab.name.clone());
                        Ok(prefab)
                    }
                }),
            Err(reason) => Err(reason),
        };
        files.push(PrefabFile { file_name, outcome });
    }
    files
}

/// Read one prefab file with a hard byte cap.
///
/// The metadata check rejects oversize files before allocating, and the
/// `take(MAX + 1)` read catches a file that grew between stat and read. Both
/// failures return the pinned `file is larger than 1 MiB` reason. Non-UTF-8
/// bytes normalize to `invalid TOML`.
fn read_prefab_file(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| format!("could not read file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not read file: {error}"))?;
    if metadata.len() > MAX_PREFAB_FILE_BYTES {
        return Err(PrefabError::FileTooLarge.to_string());
    }

    let mut bytes = Vec::new();
    file.take(MAX_PREFAB_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read file: {error}"))?;
    if bytes.len() as u64 > MAX_PREFAB_FILE_BYTES {
        return Err(PrefabError::FileTooLarge.to_string());
    }
    String::from_utf8(bytes).map_err(|_| PrefabError::InvalidToml.to_string())
}

pub(crate) fn validate_prefab_name(name: &str) -> Result<(), PrefabError> {
    if name.len() > 64 {
        return Err(PrefabError::NameTooLong);
    }
    if name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(PrefabError::NameInvalidCharacter);
    }
    Ok(())
}

fn palette_key_character(key: &str) -> Result<char, PrefabError> {
    let mut characters = key.chars();
    let Some(character) = characters.next() else {
        return Err(PrefabError::InvalidPaletteKey(key.to_owned()));
    };
    if characters.next().is_some() || !character.is_ascii_graphic() {
        return Err(PrefabError::InvalidPaletteKey(key.to_owned()));
    }
    if character == '.' {
        return Err(PrefabError::ReservedPaletteKey);
    }
    if matches!(character, '"' | '\\') {
        return Err(PrefabError::InvalidPaletteKey(key.to_owned()));
    }
    Ok(character)
}

fn integer_triple(value: &toml::Value) -> Option<[i64; 3]> {
    let values = value.as_array()?;
    if values.len() != 3 {
        return None;
    }
    Some([
        values[0].as_integer()?,
        values[1].as_integer()?,
        values[2].as_integer()?,
    ])
}

fn integer_triple_i32(value: &toml::Value) -> Option<[i32; 3]> {
    let values = integer_triple(value)?;
    Some([
        i32::try_from(values[0]).ok()?,
        i32::try_from(values[1]).ok()?,
        i32::try_from(values[2]).ok()?,
    ])
}

fn escape_toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUSE: &str = r######"format = 1
name = "small_house"
origin = [0, 0, 0]
size = [5, 4, 5]

layers = [
  [ "#####", "#...#", "#...#", "#...#", "#####" ],
  [ "#####", "#gg.#", "#...#", "#...#", "#####" ],
  [ "#####", "#...#", "#...#", "#...#", "#####" ],
  [ "LLLLL", "L...L", "L...L", "L...L", "LLLLL" ],
]

[palette]
"#" = "oak_planks"
"L" = "oak_log"
"c" = "cobblestone"
"g" = "chest"

[[sockets]]
position = [2, 1, 0]
direction = "north"
"######;

    const MINIMAL: &str = r###"format = 1
name = "tiny"
origin = [0, 0, 0]
size = [2, 1, 1]

layers = [
  [ "##" ],
]

[palette]
"#" = "stone"
"###;

    #[test]
    fn house_round_trips_byte_identically() {
        let prefab = parse_prefab(HOUSE).expect("house must parse");
        assert_eq!(prefab.size, [5, 4, 5]);
        assert_eq!(prefab.blocks.len(), 5 * 4 * 5);
        assert_eq!(prefab.sockets.len(), 1);
        assert_eq!(prefab.sockets[0].position, [2, 1, 0]);
        assert_eq!(prefab.sockets[0].direction, Direction::North);

        let serialized = prefab.to_toml_string();
        let reparsed = parse_prefab(&serialized).expect("serialized house must parse");
        assert_eq!(prefab, reparsed);
        assert_eq!(serialized, reparsed.to_toml_string());
    }

    #[test]
    fn interior_air_is_preserved_as_air_blocks() {
        let prefab = parse_prefab(HOUSE).expect("house must parse");
        let interior = prefab
            .blocks
            .iter()
            .find(|block| block.position == [3, 1, 1])
            .expect("interior cell exists");
        assert_eq!(interior.block, BlockId::Air);
        assert!(prefab
            .blocks
            .iter()
            .any(|block| block.block == BlockId::Air));
    }

    #[test]
    fn non_zero_origin_and_negative_socket_survive_round_trip() {
        let text = r####"format = 1
name = "doorway"
origin = [1, 1, 1]
size = [3, 3, 3]

layers = [
  [ "###", "###", "###" ],
  [ "###", "###", "###" ],
  [ "###", "###", "###" ],
]

[palette]
"#" = "stone"

[[sockets]]
position = [-1, 0, 0]
direction = "west"
"####;
        let prefab = parse_prefab(text).expect("doorway must parse");
        assert_eq!(prefab.origin, [1, 1, 1]);
        assert_eq!(prefab.sockets[0].position, [-1, 0, 0]);
        let reparsed = parse_prefab(&prefab.to_toml_string()).expect("round-trip must parse");
        assert_eq!(prefab, reparsed);
    }

    #[test]
    fn unsupported_format_is_rejected() {
        let text = HOUSE.replace("format = 1", "format = 2");
        assert_eq!(parse_prefab(&text), Err(PrefabError::UnsupportedFormat(2)));
    }

    #[test]
    fn bad_name_charset_and_length_are_rejected() {
        let bad_charset = HOUSE.replace("small_house", "Small_House");
        assert_eq!(
            parse_prefab(&bad_charset),
            Err(PrefabError::NameInvalidCharacter)
        );

        let long_name = "a".repeat(65);
        let bad_length = HOUSE.replace("small_house", &long_name);
        assert_eq!(parse_prefab(&bad_length), Err(PrefabError::NameTooLong));
    }

    #[test]
    fn zero_and_oversize_axes_are_rejected() {
        let zero = HOUSE.replace("size = [5, 4, 5]", "size = [0, 4, 5]");
        assert_eq!(parse_prefab(&zero), Err(PrefabError::SizeAxisTooSmall));

        let oversize = HOUSE.replace("size = [5, 4, 5]", "size = [33, 4, 5]");
        assert_eq!(
            parse_prefab(&oversize),
            Err(PrefabError::SizeExceedsCap([33, 4, 5]))
        );
    }

    #[test]
    fn origin_outside_the_prefab_is_rejected() {
        let text = HOUSE.replace("origin = [0, 0, 0]", "origin = [5, 0, 0]");
        assert_eq!(
            parse_prefab(&text),
            Err(PrefabError::OriginOutside([5, 0, 0]))
        );
    }

    #[test]
    fn unknown_palette_block_is_rejected() {
        let text = HOUSE.replace("\"c\" = \"cobblestone\"", "\"c\" = \"moon_rock\"");
        assert_eq!(
            parse_prefab(&text),
            Err(PrefabError::UnknownPaletteBlock {
                key: "c".to_owned(),
                block: "moon_rock".to_owned(),
            })
        );
    }

    #[test]
    fn air_palette_value_is_rejected() {
        let text = HOUSE.replace("\"c\" = \"cobblestone\"", "\"c\" = \"air\"");
        assert_eq!(
            parse_prefab(&text),
            Err(PrefabError::AirPaletteBlock("c".to_owned()))
        );
    }

    #[test]
    fn palette_key_rules_are_enforced() {
        let long_key = MINIMAL.replace("\"#\" = \"stone\"", "\"##\" = \"stone\"");
        assert_eq!(
            parse_prefab(&long_key),
            Err(PrefabError::InvalidPaletteKey("##".to_owned()))
        );

        let reserved = MINIMAL.replace("\"#\" = \"stone\"", "\".\" = \"stone\"");
        assert_eq!(
            parse_prefab(&reserved),
            Err(PrefabError::ReservedPaletteKey)
        );
    }

    #[test]
    fn layer_row_and_row_length_errors_are_reported() {
        let layer_count = MINIMAL.replace("size = [2, 1, 1]", "size = [2, 2, 1]");
        assert_eq!(
            parse_prefab(&layer_count),
            Err(PrefabError::LayerCount {
                found: 1,
                expected: 2,
            })
        );

        let row_count = MINIMAL.replace("size = [2, 1, 1]", "size = [2, 1, 2]");
        assert_eq!(
            parse_prefab(&row_count),
            Err(PrefabError::RowCount {
                y: 0,
                found: 1,
                expected: 2,
            })
        );

        let row_length = MINIMAL.replace("\"##\"", "\"###\"");
        assert_eq!(
            parse_prefab(&row_length),
            Err(PrefabError::RowLength {
                y: 0,
                z: 0,
                found: 3,
                expected: 2,
            })
        );
    }

    #[test]
    fn undefined_palette_character_is_rejected() {
        let text = MINIMAL.replace("\"##\"", "\"#?\"");
        assert_eq!(
            parse_prefab(&text),
            Err(PrefabError::UndefinedPaletteCharacter('?'))
        );
    }

    #[test]
    fn socket_range_direction_and_duplicates_are_enforced() {
        let out_of_range =
            format!("{MINIMAL}\n[[sockets]]\nposition = [2, 0, 0]\ndirection = \"north\"\n");
        assert_eq!(
            parse_prefab(&out_of_range),
            Err(PrefabError::SocketOutside([2, 0, 0]))
        );

        let unknown_direction =
            format!("{MINIMAL}\n[[sockets]]\nposition = [0, 0, 0]\ndirection = \"up\"\n");
        assert_eq!(
            parse_prefab(&unknown_direction),
            Err(PrefabError::UnknownSocketDirection("up".to_owned()))
        );

        let duplicate = format!(
            "{MINIMAL}\n[[sockets]]\nposition = [0, 0, 0]\ndirection = \"north\"\n\
             \n[[sockets]]\nposition = [0, 0, 0]\ndirection = \"south\"\n"
        );
        assert_eq!(
            parse_prefab(&duplicate),
            Err(PrefabError::DuplicateSocket([0, 0, 0]))
        );
    }

    #[test]
    fn invalid_toml_and_missing_fields_are_rejected() {
        assert_eq!(
            parse_prefab("not toml at all"),
            Err(PrefabError::InvalidToml)
        );

        let missing_format = MINIMAL.replace("format = 1\n", "");
        assert_eq!(
            parse_prefab(&missing_format),
            Err(PrefabError::MissingFormat)
        );

        let missing_name = MINIMAL.replace("name = \"tiny\"\n", "");
        assert_eq!(parse_prefab(&missing_name), Err(PrefabError::MissingName));

        let missing_size = MINIMAL.replace("size = [2, 1, 1]\n", "");
        assert_eq!(parse_prefab(&missing_size), Err(PrefabError::MissingSize));

        let missing_origin = MINIMAL.replace("origin = [0, 0, 0]\n", "");
        assert_eq!(
            parse_prefab(&missing_origin),
            Err(PrefabError::MissingOrigin)
        );

        let missing_palette = MINIMAL.replace("[palette]\n\"#\" = \"stone\"\n", "");
        assert_eq!(
            parse_prefab(&missing_palette),
            Err(PrefabError::MissingPalette)
        );

        let missing_layers = MINIMAL.replace("layers = [\n  [ \"##\" ],\n]\n", "");
        assert_eq!(
            parse_prefab(&missing_layers),
            Err(PrefabError::MissingLayers)
        );
    }

    #[test]
    fn boundary_cap_prefab_is_accepted() {
        let mut text = String::from(
            "format = 1\nname = \"big\"\norigin = [0, 0, 0]\nsize = [32, 32, 32]\n\nlayers = [\n",
        );
        for _ in 0..32 {
            text.push_str("  [ ");
            for z in 0..32 {
                if z > 0 {
                    text.push_str(", ");
                }
                text.push('"');
                text.push_str(&"#".repeat(32));
                text.push('"');
            }
            text.push_str(" ],\n");
        }
        text.push_str("]\n\n[palette]\n\"#\" = \"stone\"\n");

        let prefab = parse_prefab(&text).expect("boundary prefab must parse");
        assert_eq!(prefab.size, [32, 32, 32]);
        assert_eq!(prefab.blocks.len(), 32 * 32 * 32);
        assert_eq!(parse_prefab(&prefab.to_toml_string()), Ok(prefab));
    }

    #[test]
    fn error_display_is_the_pinned_reason() {
        assert_eq!(
            PrefabError::SizeExceedsCap([33, 4, 5]).to_string(),
            "size 33x4x5 exceeds cap 32x32x32"
        );
        assert_eq!(
            PrefabError::UnknownPaletteBlock {
                key: "c".to_owned(),
                block: "moon_rock".to_owned(),
            }
            .to_string(),
            "palette value for 'c' is unknown block 'moon_rock'"
        );
        assert_eq!(
            PrefabError::DuplicateSocket([1, -2, 3]).to_string(),
            "duplicate socket at [1, -2, 3]"
        );
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    const ZERO_SUMMARY: &str = "validate-worldgen: 0 prefabs, 0 errors, 0 warnings";

    fn prefab_text(name: &str) -> String {
        format!(
            "format = 1\nname = \"{name}\"\norigin = [0, 0, 0]\nsize = [2, 1, 1]\n\n\
             layers = [\n  [ \"##\" ],\n]\n\n[palette]\n\"#\" = \"stone\"\n"
        )
    }

    fn sized_prefab_text(name: &str, size: [u32; 3]) -> String {
        let mut rows = String::new();
        for _ in 0..size[1] {
            rows.push_str("  [ ");
            for z in 0..size[2] {
                if z > 0 {
                    rows.push_str(", ");
                }
                rows.push('"');
                rows.push_str(&"#".repeat(size[0] as usize));
                rows.push('"');
            }
            rows.push_str(" ],\n");
        }
        format!(
            "format = 1\nname = \"{name}\"\norigin = [0, 0, 0]\nsize = [{}, {}, {}]\n\n\
             layers = [\n{rows}]\n\n[palette]\n\"#\" = \"stone\"\n",
            size[0], size[1], size[2]
        )
    }

    fn write_prefab(root: &Path, file_name: &str, contents: impl AsRef<[u8]>) {
        let path = root.join(PREFABS_SUBDIR).join(file_name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create prefab fixture dir");
        }
        fs::write(path, contents).expect("write prefab fixture");
    }

    #[test]
    fn missing_root_and_missing_prefabs_dir_are_valid_empty() {
        let temp = tempfile::tempdir().expect("tempdir");

        let absent_root = temp.path().join("absent");
        let (catalog, diagnostics) = PrefabCatalog::load_sorted(&absent_root);
        assert!(catalog.is_empty());
        assert_eq!(catalog.len(), 0);
        assert_eq!(catalog.names().count(), 0);
        assert!(diagnostics.is_empty());
        assert_eq!(
            validate_worldgen_lines(&absent_root),
            (vec![ZERO_SUMMARY.to_owned()], true)
        );

        let empty_prefabs = temp.path().join("empty");
        fs::create_dir_all(empty_prefabs.join(PREFABS_SUBDIR)).expect("create prefabs dir");
        let (catalog, diagnostics) = PrefabCatalog::load_sorted(&empty_prefabs);
        assert!(catalog.is_empty());
        assert!(diagnostics.is_empty());
        assert_eq!(
            validate_worldgen_lines(&empty_prefabs),
            (vec![ZERO_SUMMARY.to_owned()], true)
        );

        let root_is_file = temp.path().join("root_is_file");
        fs::write(&root_is_file, b"not a directory").expect("write root file");
        let (catalog, diagnostics) = PrefabCatalog::load_sorted(&root_is_file);
        assert!(catalog.is_empty());
        assert!(diagnostics.is_empty());
        assert_eq!(
            validate_worldgen_lines(&root_is_file),
            (vec![ZERO_SUMMARY.to_owned()], true)
        );
    }

    #[test]
    fn non_toml_files_and_directories_are_ignored_silently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_prefab(root, "valid.toml", prefab_text("kept"));
        write_prefab(root, "notes.txt", prefab_text("ignored_text"));
        write_prefab(root, "UPPER.TOML", prefab_text("ignored_upper"));
        write_prefab(root, "nested/inner.toml", prefab_text("ignored_nested"));
        fs::create_dir_all(root.join(PREFABS_SUBDIR).join("subdir.toml")).expect("create fake dir");

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(root);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("kept").is_some());
        assert!(catalog.get("ignored_text").is_none());
        assert_eq!(
            validate_worldgen_lines(root),
            (
                vec![
                    "ok: prefab kept".to_owned(),
                    "validate-worldgen: 1 prefabs, 0 errors, 0 warnings".to_owned(),
                ],
                true,
            )
        );
    }

    #[test]
    fn files_load_in_sorted_filename_order_not_creation_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_prefab(root, "b.toml", prefab_text("zeta"));
        write_prefab(root, "a.toml", prefab_text("alpha"));

        let (lines, ok) = validate_worldgen_lines(root);
        assert!(ok);
        assert_eq!(
            lines,
            vec![
                "ok: prefab alpha".to_owned(),
                "ok: prefab zeta".to_owned(),
                "validate-worldgen: 2 prefabs, 0 errors, 0 warnings".to_owned(),
            ]
        );

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(root);
        assert!(diagnostics.is_empty());
        assert_eq!(catalog.names().collect::<Vec<_>>(), vec!["alpha", "zeta"]);
    }

    #[test]
    fn duplicate_header_name_keeps_the_sorted_first_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_prefab(root, "a.toml", sized_prefab_text("same", [2, 1, 1]));
        write_prefab(root, "b.toml", sized_prefab_text("same", [3, 1, 1]));

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(root);
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            catalog.get("same").expect("first file wins").size,
            [2, 1, 1]
        );
        assert_eq!(
            diagnostics,
            vec!["worldgen/prefabs/b.toml: duplicate prefab name 'same'".to_owned()]
        );

        let (lines, ok) = validate_worldgen_lines(root);
        assert!(!ok);
        assert_eq!(
            lines,
            vec![
                "ok: prefab same".to_owned(),
                "error: worldgen/prefabs/b.toml: duplicate prefab name 'same'".to_owned(),
                "validate-worldgen: 1 prefabs, 1 errors, 0 warnings".to_owned(),
            ]
        );
    }

    #[test]
    fn invalid_file_yields_one_error_line_and_valid_sibling_still_loads() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_prefab(root, "a_valid.toml", prefab_text("valid"));
        write_prefab(root, "b_broken.toml", "format = [\n");

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(root);
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            diagnostics,
            vec!["worldgen/prefabs/b_broken.toml: invalid TOML".to_owned()]
        );

        let (lines, ok) = validate_worldgen_lines(root);
        assert!(!ok);
        assert_eq!(
            lines,
            vec![
                "ok: prefab valid".to_owned(),
                "error: worldgen/prefabs/b_broken.toml: invalid TOML".to_owned(),
                "validate-worldgen: 1 prefabs, 1 errors, 0 warnings".to_owned(),
            ]
        );
    }

    #[test]
    fn oversize_file_is_rejected_before_parsing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_prefab(
            root,
            "huge.toml",
            vec![b' '; MAX_PREFAB_FILE_BYTES as usize + 1],
        );

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(root);
        assert!(catalog.is_empty());
        assert_eq!(
            diagnostics,
            vec!["worldgen/prefabs/huge.toml: file is larger than 1 MiB".to_owned()]
        );

        let (lines, ok) = validate_worldgen_lines(root);
        assert!(!ok);
        assert_eq!(
            lines,
            vec![
                "error: worldgen/prefabs/huge.toml: file is larger than 1 MiB".to_owned(),
                "validate-worldgen: 0 prefabs, 1 errors, 0 warnings".to_owned(),
            ]
        );
    }

    #[test]
    fn custom_root_spelling_still_uses_the_logical_label() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("custom").join("gen");
        write_prefab(&root, "bad.toml", "not toml at all");

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(&root);
        assert!(catalog.is_empty());
        assert_eq!(
            diagnostics,
            vec!["worldgen/prefabs/bad.toml: invalid TOML".to_owned()]
        );

        let (lines, _ok) = validate_worldgen_lines(&root);
        assert_eq!(
            lines,
            vec![
                "error: worldgen/prefabs/bad.toml: invalid TOML".to_owned(),
                "validate-worldgen: 0 prefabs, 1 errors, 0 warnings".to_owned(),
            ]
        );
        assert!(!lines.iter().any(|line| line.contains("custom")));
    }

    #[test]
    fn non_utf8_file_reports_invalid_toml_without_panicking() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_prefab(root, "binary.toml", [0xff, 0xfe, 0x00, 0x01]);

        let (catalog, diagnostics) = PrefabCatalog::load_sorted(root);
        assert!(catalog.is_empty());
        assert_eq!(
            diagnostics,
            vec!["worldgen/prefabs/binary.toml: invalid TOML".to_owned()]
        );
    }
}
