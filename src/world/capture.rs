//! Pure two-corner region capture: world blocks in, one validated prefab file out.
//!
//! This layer never touches winit, the GPU, text input or a save file. It owns
//! exactly four responsibilities:
//!
//! 1. normalize two inclusive world corners ([`region_from_corners`]);
//! 2. fail-closed validation in one fixed order — size caps, world height,
//!    then chunk residency ([`validate_region`]);
//! 3. extraction with the D245 normalization rules ([`extract_prefab`]);
//! 4. one deterministic write to `root/prefabs/<name>.toml`, re-parsed before
//!    the first byte reaches disk ([`write_prefab`], [`capture_to_file`]).
//!
//! Two hazards shape the order. [`WorldManager::get_block`] returns air for a
//! missing chunk, so residency is checked explicitly before extraction; and
//! [`Prefab::to_toml_string`] silently writes modded ids as air, so non-built-in
//! blocks are rejected with their coordinate instead of being lost.
//!
//! [`WorldManager::get_block`]: crate::world::manager::WorldManager::get_block
//! [`Prefab::to_toml_string`]: crate::world::prefab::Prefab::to_toml_string

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use glam::IVec3;

use crate::world::block::BlockId;
use crate::world::chunk::{world_to_chunk_and_local, ChunkPos, CHUNK_SIZE};
use crate::world::manager::{WorldManager, MAX_CHUNK_Y, MIN_CHUNK_Y};
use crate::world::prefab::{
    parse_prefab, prefab_identities, validate_prefab_name, Direction, Prefab, PrefabBlock,
    PrefabError, PrefabSocket, PREFABS_SUBDIR, PREFAB_CAPS, WORLDGEN_DIR,
};

/// Lowest valid world block row (`MIN_CHUNK_Y * CHUNK_SIZE`).
const WORLD_MIN_Y: i32 = MIN_CHUNK_Y * CHUNK_SIZE as i32;

/// One above the highest valid world block row (`(MAX_CHUNK_Y + 1) * CHUNK_SIZE`).
const WORLD_MAX_Y_EXCLUSIVE: i32 = (MAX_CHUNK_Y + 1) * CHUNK_SIZE as i32;

/// One inclusive, normalized world-space selection.
///
/// `min` and `max` are both part of the region, so a one-block selection has
/// `min == max` and its size is `[1, 1, 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureRegion {
    pub min: IVec3,
    pub max: IVec3,
}

/// Normalize two corner coordinates into one inclusive region.
pub fn region_from_corners(a: IVec3, b: IVec3) -> CaptureRegion {
    CaptureRegion {
        min: a.min(b),
        max: a.max(b),
    }
}

/// One stable, one-line capture reason.
///
/// `Display` emits exactly the pinned text: gameplay posts it verbatim and the
/// headless demo digests it, so the wording is part of the observable contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// The region exceeds [`PREFAB_CAPS`] on at least one axis.
    SizeExceedsCap { size: [i64; 3] },
    /// The region leaves the valid world block rows `-32..=159`.
    OutsideWorldHeight,
    /// The first non-resident chunk in `(y, z, x)` chunk order.
    UnloadedChunk { pos: ChunkPos },
    /// The name failed the shared prefab name rule.
    InvalidName(PrefabError),
    /// A socket marker whose horizontal neighbours do not pick exactly one axis.
    AmbiguousSocket { pos: IVec3 },
    /// A non-built-in (modded) block would silently serialize as air.
    UnsupportedBlock { pos: IVec3, block: BlockId },
    /// A regular file already exists at the target path and is left untouched.
    AlreadyExists { name: String, file: String },
    /// Serializing and re-parsing the captured value did not reproduce it.
    SerializeMismatch,
    /// The target directory or file could not be written.
    WriteFailed { file: String, reason: String },
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::SizeExceedsCap { size } => write!(
                f,
                "selection is {}x{}x{}, exceeds prefab cap 32x32x32",
                size[0], size[1], size[2]
            ),
            CaptureError::OutsideWorldHeight => {
                write!(f, "selection is outside the world height (-32..=159)")
            }
            CaptureError::UnloadedChunk { pos } => {
                write!(f, "chunk ({}, {}, {}) is not loaded", pos.x, pos.y, pos.z)
            }
            CaptureError::InvalidName(error) => write!(f, "invalid prefab name: {error}"),
            CaptureError::AmbiguousSocket { pos } => {
                write!(
                    f,
                    "socket marker at [{}, {}, {}] is ambiguous",
                    pos.x, pos.y, pos.z
                )
            }
            CaptureError::UnsupportedBlock { pos, block } => write!(
                f,
                "block modded:{} at [{}, {}, {}] is not supported",
                block.to_raw(),
                pos.x,
                pos.y,
                pos.z
            ),
            CaptureError::AlreadyExists { name, file } => {
                write!(f, "prefab '{name}' already exists at {file}")
            }
            CaptureError::SerializeMismatch => write!(f, "captured prefab did not round-trip"),
            CaptureError::WriteFailed { file, reason } => {
                write!(f, "could not write {file}: {reason}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

/// Validate a normalized region in the fixed first-failure order.
///
/// Checks per-axis size against [`PREFAB_CAPS`], then world height, then chunk
/// residency in `(y, z, x)` chunk order. Returns the inclusive size as
/// `[x, y, z]` so callers never recompute it.
pub fn validate_region(
    world: &WorldManager,
    region: &CaptureRegion,
) -> Result<[u32; 3], CaptureError> {
    let size = region_size(region);
    let caps = [
        i64::from(PREFAB_CAPS.max_x),
        i64::from(PREFAB_CAPS.max_y),
        i64::from(PREFAB_CAPS.max_z),
    ];
    if (0..3).any(|axis| size[axis] > caps[axis]) {
        return Err(CaptureError::SizeExceedsCap { size });
    }

    if region.min.y < WORLD_MIN_Y || region.max.y >= WORLD_MAX_Y_EXCLUSIVE {
        return Err(CaptureError::OutsideWorldHeight);
    }

    let (min_chunk, _) = world_to_chunk_and_local(region.min.x, region.min.y, region.min.z);
    let (max_chunk, _) = world_to_chunk_and_local(region.max.x, region.max.y, region.max.z);
    for cy in min_chunk.y..=max_chunk.y {
        for cz in min_chunk.z..=max_chunk.z {
            for cx in min_chunk.x..=max_chunk.x {
                let pos = ChunkPos::new(cx, cy, cz);
                if !world.is_resident(pos) {
                    return Err(CaptureError::UnloadedChunk { pos });
                }
            }
        }
    }

    Ok([size[0] as u32, size[1] as u32, size[2] as u32])
}

/// Resolve a marker's facing from its four horizontal in-region neighbours.
///
/// Neighbour order is pinned as `[west, east, north, south]`, at the marker's
/// own `y`: west/east are `x -/+ 1`, north/south are `z -/+ 1`.
///
/// An axis qualifies only when exactly one side is solid; the direction is the
/// open side (`+x` → [`Direction::East`], `-x` → [`Direction::West`], `+z` →
/// [`Direction::South`], `-z` → [`Direction::North`]). Exactly one qualifying
/// axis yields the direction; zero or two are ambiguous and fail the capture
/// with the marker coordinate.
///
/// A neighbour coordinate outside `region` counts as non-solid and its
/// `world.get_block` is never called, so an edge marker cannot read an
/// unloaded neighbouring chunk.
pub fn direction_from_marker_neighbours(
    world: &WorldManager,
    region: &CaptureRegion,
    pos: IVec3,
) -> Result<Direction, CaptureError> {
    let offsets = [
        IVec3::new(-1, 0, 0),
        IVec3::new(1, 0, 0),
        IVec3::new(0, 0, -1),
        IVec3::new(0, 0, 1),
    ];
    let mut solid = [false; 4];
    for (index, offset) in offsets.iter().enumerate() {
        let neighbour = pos + *offset;
        let inside = neighbour.x >= region.min.x
            && neighbour.x <= region.max.x
            && neighbour.y >= region.min.y
            && neighbour.y <= region.max.y
            && neighbour.z >= region.min.z
            && neighbour.z <= region.max.z;
        if inside {
            solid[index] = world
                .get_block(neighbour.x, neighbour.y, neighbour.z)
                .is_solid();
        }
    }
    direction_from_solid_neighbours(solid, pos)
}

/// Pure core of [`direction_from_marker_neighbours`].
///
/// `solid` is ordered `[west, east, north, south]`. Solid includes
/// [`BlockId::SocketMarker`] and `Modded(_)` (non-air, non-water), so an
/// adjacent marker counts as solid; `Air` and `Water` do not.
pub(crate) fn direction_from_solid_neighbours(
    solid: [bool; 4],
    pos: IVec3,
) -> Result<Direction, CaptureError> {
    let [west, east, north, south] = solid;
    let mut open: Option<Direction> = None;

    if west != east {
        open = Some(if east {
            Direction::West
        } else {
            Direction::East
        });
    }
    if north != south {
        if open.is_some() {
            return Err(CaptureError::AmbiguousSocket { pos });
        }
        open = Some(if south {
            Direction::North
        } else {
            Direction::South
        });
    }

    open.ok_or(CaptureError::AmbiguousSocket { pos })
}

/// Extract one prefab from a validated region.
///
/// Every grid cell is emitted in `(y, z, x)` order, air included, so
/// `parse_prefab(&prefab.to_toml_string()) == Ok(prefab)`. `LootChest`
/// normalizes to an empty `Chest`, wheat stages 1..=3 normalize to stage 0,
/// and air, water, bedrock, farmland and every other built-in block keep their
/// identity. A socket marker becomes one [`PrefabSocket`] (origin-relative,
/// facing derived from its four horizontal neighbour cells) and the marker
/// cell is emitted as air; an ambiguous marker fails before anything is
/// written, naming its world coordinate. A modded block fails with its own
/// world coordinate. The first failure in `(y, z, x)` order wins.
///
/// The origin is `[size_x / 2, 0, size_z / 2]`; `origin[1] == 0` keeps the
/// prefab's bottom layer aligned to the selection's bottom row for the S06
/// socket/road invariant.
pub fn extract_prefab(
    name: &str,
    world: &WorldManager,
    region: CaptureRegion,
) -> Result<Prefab, CaptureError> {
    let size = validate_region(world, &region)?;
    validate_prefab_name(name).map_err(CaptureError::InvalidName)?;

    let cell_count = size[0] as usize * size[1] as usize * size[2] as usize;
    let mut blocks = Vec::with_capacity(cell_count);
    let mut sockets: Vec<PrefabSocket> = Vec::new();
    let origin = [size[0] / 2, 0, size[2] / 2];
    let socket_origin = [origin[0] as i32, origin[1] as i32, origin[2] as i32];
    for y in region.min.y..=region.max.y {
        for z in region.min.z..=region.max.z {
            for x in region.min.x..=region.max.x {
                let block = world.get_block(x, y, z);
                let pos = IVec3::new(x, y, z);
                if block == BlockId::SocketMarker {
                    let direction = direction_from_marker_neighbours(world, &region, pos)?;
                    sockets.push(PrefabSocket {
                        position: [
                            x - region.min.x - socket_origin[0],
                            y - region.min.y - socket_origin[1],
                            z - region.min.z - socket_origin[2],
                        ],
                        direction,
                    });
                    blocks.push(PrefabBlock {
                        position: [
                            (x - region.min.x) as u32,
                            (y - region.min.y) as u32,
                            (z - region.min.z) as u32,
                        ],
                        block: BlockId::Air,
                    });
                    continue;
                }
                if !block.is_builtin() {
                    return Err(CaptureError::UnsupportedBlock { pos, block });
                }
                blocks.push(PrefabBlock {
                    position: [
                        (x - region.min.x) as u32,
                        (y - region.min.y) as u32,
                        (z - region.min.z) as u32,
                    ],
                    block: normalize_block(block),
                });
            }
        }
    }

    Ok(Prefab {
        name: name.to_owned(),
        origin,
        size,
        blocks,
        sockets,
    })
}

/// Serialize, self-check, then write exactly one `root/prefabs/<name>.toml`.
///
/// The caller owns the collision policy; this function intentionally does not
/// refuse an existing path so a filesystem failure (for example the target
/// being a directory) stays observable as [`CaptureError::WriteFailed`]. On a
/// failed write a best-effort `remove_file` prevents a partial file surviving.
pub fn write_prefab(root: &Path, prefab: &Prefab) -> Result<PathBuf, CaptureError> {
    let file = logical_file(&prefab.name);
    let text = prefab.to_toml_string();
    match parse_prefab(&text) {
        Ok(reparsed) if reparsed == *prefab => {}
        _ => return Err(CaptureError::SerializeMismatch),
    }

    let directory = root.join(PREFABS_SUBDIR);
    let path = directory.join(format!("{}.toml", prefab.name));
    fs::create_dir_all(&directory).map_err(|error| CaptureError::WriteFailed {
        file: file.clone(),
        reason: io_reason(&error),
    })?;
    if let Err(error) = fs::write(&path, text.as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(CaptureError::WriteFailed {
            file,
            reason: io_reason(&error),
        });
    }
    Ok(path)
}

/// Capture two inclusive world corners to one prefab file.
///
/// Fixed first-failure order: normalize corners, per-axis size, world height,
/// first unloaded chunk, name rule, collision, extraction, serialization
/// self-check, `create_dir_all`, one `fs::write`. Every failure returns before
/// any write; a collision leaves the existing file byte-identical.
///
/// The collision preflight covers both the target file stem and every header
/// `name` declared by an existing valid prefab, so a renamed file such as
/// `renamed.toml` declaring `name = "capture"` can never be silently shadowed
/// or duplicated by a new `capture.toml`.
pub fn capture_to_file(
    root: &Path,
    name: &str,
    world: &WorldManager,
    a: IVec3,
    b: IVec3,
) -> Result<(PathBuf, Prefab), CaptureError> {
    let region = region_from_corners(a, b);
    validate_region(world, &region)?;
    validate_prefab_name(name).map_err(CaptureError::InvalidName)?;

    if let Some(identity) = prefab_identities(root).into_iter().find(|identity| {
        identity.is_regular_file
            && (identity.stem == name || identity.declared_name.as_deref() == Some(name))
    }) {
        return Err(CaptureError::AlreadyExists {
            name: name.to_owned(),
            file: format!("{WORLDGEN_DIR}/{PREFABS_SUBDIR}/{}", identity.file_name),
        });
    }

    let prefab = extract_prefab(name, world, region)?;
    let path = write_prefab(root, &prefab)?;
    Ok((path, prefab))
}

/// Inclusive region size as `[x, y, z]`.
fn region_size(region: &CaptureRegion) -> [i64; 3] {
    [
        i64::from(region.max.x) - i64::from(region.min.x) + 1,
        i64::from(region.max.y) - i64::from(region.min.y) + 1,
        i64::from(region.max.z) - i64::from(region.min.z) + 1,
    ]
}

/// D245 block normalization applied at capture time.
fn normalize_block(block: BlockId) -> BlockId {
    match block {
        BlockId::LootChest => BlockId::Chest,
        BlockId::WheatStage1 | BlockId::WheatStage2 | BlockId::WheatStage3 => BlockId::WheatStage0,
        other => other,
    }
}

/// The logical `worldgen/prefabs/<name>.toml` label used in diagnostics.
fn logical_file(name: &str) -> String {
    format!("{WORLDGEN_DIR}/{PREFABS_SUBDIR}/{name}.toml")
}

/// Stable, platform-independent write-reason text for the common error kinds.
fn io_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        io::ErrorKind::NotFound => "not found".to_owned(),
        io::ErrorKind::AlreadyExists => "already exists".to_owned(),
        io::ErrorKind::IsADirectory => "is a directory".to_owned(),
        io::ErrorKind::NotADirectory => "not a directory".to_owned(),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use crate::world::chunk::Chunk;
    use crate::world::persistence::ChunkEditMap;
    use crate::world::prefab::validate_worldgen_lines;

    const CAPTURE_DEMO_SEED: u32 = 4;

    fn empty_world() -> WorldManager {
        WorldManager::with_seed_and_edits(4, CAPTURE_DEMO_SEED, ChunkEditMap::default())
    }

    /// Insert one uniform chunk per chunk position touching `region`.
    fn resident_world(region: &CaptureRegion, fill: BlockId) -> WorldManager {
        let mut world = empty_world();
        let (min_chunk, _) = world_to_chunk_and_local(region.min.x, region.min.y, region.min.z);
        let (max_chunk, _) = world_to_chunk_and_local(region.max.x, region.max.y, region.max.z);
        for cy in min_chunk.y..=max_chunk.y {
            for cz in min_chunk.z..=max_chunk.z {
                for cx in min_chunk.x..=max_chunk.x {
                    world
                        .insert_chunk_for_test(ChunkPos::new(cx, cy, cz), Chunk::new_uniform(fill));
                }
            }
        }
        world
    }

    fn set(world: &mut WorldManager, pos: IVec3, block: BlockId) {
        world.set_block_immediate(pos.x, pos.y, pos.z, block);
    }

    /// Recursive `relative path -> bytes` snapshot; a missing root is empty.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        collect_files(root, root, &mut files);
        files
    }

    fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(read_dir) = fs::read_dir(dir) else {
            return;
        };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => collect_files(root, &path, files),
                Ok(_) => {
                    let bytes = fs::read(&path).unwrap_or_default();
                    files.insert(relative, bytes);
                }
                Err(_) => {}
            }
        }
    }

    fn house_region() -> CaptureRegion {
        region_from_corners(IVec3::new(0, 40, 0), IVec3::new(4, 43, 4))
    }

    fn stone_house_world() -> WorldManager {
        let mut world = resident_world(&house_region(), BlockId::Stone);
        set(&mut world, IVec3::new(1, 41, 1), BlockId::Air);
        set(&mut world, IVec3::new(2, 41, 1), BlockId::Water);
        set(&mut world, IVec3::new(3, 41, 1), BlockId::LootChest);
        set(&mut world, IVec3::new(1, 41, 2), BlockId::WheatStage1);
        set(&mut world, IVec3::new(2, 41, 2), BlockId::WheatStage2);
        set(&mut world, IVec3::new(3, 41, 2), BlockId::WheatStage3);
        set(&mut world, IVec3::new(1, 41, 3), BlockId::Bedrock);
        set(&mut world, IVec3::new(2, 41, 3), BlockId::Farmland);
        world
    }

    #[test]
    fn corners_normalize_in_either_order() {
        let a = IVec3::new(9, 42, -3);
        let b = IVec3::new(-2, 40, 4);
        let region = region_from_corners(a, b);
        assert_eq!(
            region,
            CaptureRegion {
                min: IVec3::new(-2, 40, -3),
                max: IVec3::new(9, 42, 4),
            }
        );
        assert_eq!(region, region_from_corners(b, a));
        assert_eq!(region_size(&region), [12, 3, 8]);
    }

    #[test]
    fn one_block_region_has_inclusive_size_origin_and_full_cell_round_trip() {
        let region = region_from_corners(IVec3::new(4, 40, 4), IVec3::new(4, 40, 4));
        let mut world = resident_world(&region, BlockId::Air);
        set(&mut world, IVec3::new(4, 40, 4), BlockId::Stone);

        let prefab = extract_prefab("one", &world, region).expect("one block captures");
        assert_eq!(prefab.size, [1, 1, 1]);
        assert_eq!(prefab.origin, [0, 0, 0]);
        assert_eq!(prefab.blocks.len(), 1);
        assert_eq!(prefab.blocks[0].position, [0, 0, 0]);
        assert_eq!(prefab.blocks[0].block, BlockId::Stone);
        assert_eq!(parse_prefab(&prefab.to_toml_string()), Ok(prefab));
    }

    #[test]
    fn origin_and_every_grid_cell_round_trip() {
        let world = stone_house_world();
        let prefab = extract_prefab("house", &world, house_region()).expect("house captures");

        assert_eq!(prefab.size, [5, 4, 5]);
        assert_eq!(prefab.origin, [2, 0, 2]);
        assert_eq!(prefab.blocks.len(), 5 * 4 * 5);
        assert!(prefab.sockets.is_empty());

        for (index, block) in prefab.blocks.iter().enumerate() {
            let expected = [
                (index % 5) as u32,
                ((index / 25) % 4) as u32,
                ((index / 5) % 5) as u32,
            ];
            assert_eq!(block.position, expected, "blocks are in (y, z, x) order");
        }

        let serialized = prefab.to_toml_string();
        let reparsed = parse_prefab(&serialized).expect("written prefab parses");
        assert_eq!(reparsed, prefab);
        assert_eq!(reparsed.to_toml_string(), serialized);
    }

    #[test]
    fn capture_normalizes_chests_and_crops_and_preserves_air_water_and_bedrock() {
        let world = stone_house_world();
        let prefab = extract_prefab("house", &world, house_region()).expect("house captures");
        let find = |position: [u32; 3]| {
            prefab
                .blocks
                .iter()
                .find(|block| block.position == position)
                .expect("cell exists")
                .block
        };

        assert_eq!(find([1, 1, 1]), BlockId::Air);
        assert_eq!(find([2, 1, 1]), BlockId::Water);
        assert_eq!(find([3, 1, 1]), BlockId::Chest);
        assert_eq!(find([1, 1, 2]), BlockId::WheatStage0);
        assert_eq!(find([2, 1, 2]), BlockId::WheatStage0);
        assert_eq!(find([3, 1, 2]), BlockId::WheatStage0);
        assert_eq!(find([1, 1, 3]), BlockId::Bedrock);
        assert_eq!(find([2, 1, 3]), BlockId::Farmland);
        assert_eq!(find([0, 0, 0]), BlockId::Stone);
        assert!(!prefab
            .blocks
            .iter()
            .any(|block| block.block == BlockId::LootChest));
    }

    #[test]
    fn oversize_region_fails_before_reading_or_writing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let region = region_from_corners(IVec3::new(0, 40, 0), IVec3::new(32, 40, 0));
        let world = resident_world(&region, BlockId::Stone);
        let before = snapshot(temp.path());

        let error =
            capture_to_file(temp.path(), "big", &world, region.min, region.max).unwrap_err();
        assert_eq!(error, CaptureError::SizeExceedsCap { size: [33, 1, 1] });
        assert_eq!(snapshot(temp.path()), before);
    }

    #[test]
    fn region_outside_world_height_fails_before_residency() {
        let tall = region_from_corners(IVec3::new(0, 159, 0), IVec3::new(0, 160, 0));
        let world = resident_world(&tall, BlockId::Stone);
        assert_eq!(
            validate_region(&world, &tall),
            Err(CaptureError::OutsideWorldHeight)
        );

        let low = region_from_corners(IVec3::new(0, -33, 0), IVec3::new(0, -32, 0));
        let world = resident_world(&low, BlockId::Stone);
        assert_eq!(
            validate_region(&world, &low),
            Err(CaptureError::OutsideWorldHeight)
        );

        let bottom = region_from_corners(IVec3::new(0, -32, 0), IVec3::new(0, -31, 0));
        let world = resident_world(&bottom, BlockId::Stone);
        assert_eq!(validate_region(&world, &bottom), Ok([1, 2, 1]));

        let top = region_from_corners(IVec3::new(0, 158, 0), IVec3::new(0, 159, 0));
        let world = resident_world(&top, BlockId::Stone);
        assert_eq!(validate_region(&world, &top), Ok([1, 2, 1]));
    }

    #[test]
    fn first_unloaded_chunk_follows_y_then_z_then_x_order() {
        let mut world = empty_world();
        world.insert_chunk_for_test(ChunkPos::new(0, 1, 0), Chunk::new_uniform(BlockId::Stone));

        // Two chunks along +x: (0, 1, 0) is resident, so x = 1 is first.
        let along_x = region_from_corners(IVec3::new(1, 40, 0), IVec3::new(32, 40, 0));
        assert_eq!(
            validate_region(&world, &along_x),
            Err(CaptureError::UnloadedChunk {
                pos: ChunkPos::new(1, 1, 0)
            })
        );

        // Two chunks along +z with only z = 0 resident.
        let along_z = region_from_corners(IVec3::new(0, 40, 1), IVec3::new(0, 40, 32));
        assert_eq!(
            validate_region(&world, &along_z),
            Err(CaptureError::UnloadedChunk {
                pos: ChunkPos::new(0, 1, 1)
            })
        );

        // Two chunk rows along +y with only y = 1 resident; y is checked
        // before z and x, so the next row is the first offender.
        let along_y = region_from_corners(IVec3::new(0, 40, 0), IVec3::new(0, 71, 0));
        assert_eq!(
            validate_region(&world, &along_y),
            Err(CaptureError::UnloadedChunk {
                pos: ChunkPos::new(0, 2, 0)
            })
        );
    }

    fn doorway_world() -> WorldManager {
        let region = house_region();
        let mut world = resident_world(&region, BlockId::Stone);
        // Hollow the interior above the stone floor.
        for y in 41..=43 {
            for z in 1..=3 {
                for x in 1..=3 {
                    set(&mut world, IVec3::new(x, y, z), BlockId::Air);
                }
            }
        }
        // Two-tall doorway in the min-z wall at x = 2.
        set(&mut world, IVec3::new(2, 41, 0), BlockId::Air);
        set(&mut world, IVec3::new(2, 42, 0), BlockId::Air);
        // Cobblestone step one cell inside the doorway, at the lower opening.
        set(&mut world, IVec3::new(2, 41, 1), BlockId::Cobblestone);
        // Marker in the only doorway opening.
        set(&mut world, IVec3::new(2, 41, 0), BlockId::SocketMarker);
        world
    }

    fn two_doorway_world() -> WorldManager {
        let region = house_region();
        let mut world = resident_world(&region, BlockId::Stone);
        for y in 41..=43 {
            for z in 1..=3 {
                for x in 1..=3 {
                    set(&mut world, IVec3::new(x, y, z), BlockId::Air);
                }
            }
        }
        // A min-z doorway opening north and a max-z doorway opening south.
        set(&mut world, IVec3::new(2, 41, 0), BlockId::Air);
        set(&mut world, IVec3::new(2, 42, 0), BlockId::Air);
        set(&mut world, IVec3::new(2, 41, 1), BlockId::Cobblestone);
        set(&mut world, IVec3::new(2, 41, 0), BlockId::SocketMarker);

        set(&mut world, IVec3::new(2, 41, 4), BlockId::Air);
        set(&mut world, IVec3::new(2, 42, 4), BlockId::Air);
        set(&mut world, IVec3::new(2, 41, 3), BlockId::Cobblestone);
        set(&mut world, IVec3::new(2, 41, 4), BlockId::SocketMarker);
        world
    }

    fn solid_pattern(positions: &[usize]) -> [bool; 4] {
        let mut solid = [false; 4];
        for &index in positions {
            solid[index] = true;
        }
        solid
    }

    #[test]
    fn solid_neighbour_matrix_picks_the_single_open_axis() {
        let pos = IVec3::new(10, 40, 10);
        let ambiguous = || Err(CaptureError::AmbiguousSocket { pos });

        // 0 solids: neither axis opens.
        assert_eq!(
            direction_from_solid_neighbours([false; 4], pos),
            ambiguous()
        );

        // 1 solid: the only opening is the opposite side.
        for (index, expected) in [
            (0, Direction::East),
            (1, Direction::West),
            (2, Direction::South),
            (3, Direction::North),
        ] {
            assert_eq!(
                direction_from_solid_neighbours(solid_pattern(&[index]), pos),
                Ok(expected),
                "single solid at index {index}"
            );
        }

        // 2 solids on the same axis: both sides equal, the axis stays silent.
        for pair in [[0, 1], [2, 3]] {
            assert_eq!(
                direction_from_solid_neighbours(solid_pattern(&pair), pos),
                ambiguous(),
                "paired axis {pair:?}"
            );
        }

        // 2 solids on different axes: both axes qualify.
        for pair in [[0, 2], [0, 3], [1, 2], [1, 3]] {
            assert_eq!(
                direction_from_solid_neighbours(solid_pattern(&pair), pos),
                ambiguous(),
                "crossed axes {pair:?}"
            );
        }

        // 3 solids: only the missing side is open.
        for (missing, expected) in [
            (0, Direction::West),
            (1, Direction::East),
            (2, Direction::North),
            (3, Direction::South),
        ] {
            let solid =
                solid_pattern(&(0..4).filter(|&index| index != missing).collect::<Vec<_>>());
            assert_eq!(
                direction_from_solid_neighbours(solid, pos),
                Ok(expected),
                "missing index {missing}"
            );
        }

        // 4 solids: fully enclosed.
        assert_eq!(direction_from_solid_neighbours([true; 4], pos), ambiguous());
    }

    #[test]
    fn neighbour_reads_respect_region_bounds_and_block_solidity() {
        let region = house_region();
        let marker = IVec3::new(2, 41, 2);
        let neighbour_world = |marker: IVec3, solid: [bool; 4]| {
            let mut world = resident_world(&region, BlockId::Air);
            let offsets = [
                IVec3::new(-1, 0, 0),
                IVec3::new(1, 0, 0),
                IVec3::new(0, 0, -1),
                IVec3::new(0, 0, 1),
            ];
            for (index, offset) in offsets.iter().enumerate() {
                if solid[index] {
                    set(&mut world, marker + *offset, BlockId::Stone);
                }
            }
            world
        };

        // The residency-checked path must agree with the pure core on the
        // whole matrix, including its pinned `[west, east, north, south]`
        // order.
        for solid in [
            [false, false, false, false],
            [true, false, false, false],
            [false, true, false, false],
            [false, false, true, false],
            [false, false, false, true],
            [true, true, false, false],
            [false, false, true, true],
            [true, false, true, false],
            [true, false, false, true],
            [false, true, true, false],
            [false, true, false, true],
            [true, true, true, true],
        ] {
            assert_eq!(
                direction_from_marker_neighbours(&neighbour_world(marker, solid), &region, marker),
                direction_from_solid_neighbours(solid, marker),
                "real path must match the pure core for {solid:?}"
            );
        }

        // A marker on each region edge derives the direction of the side that
        // opens out of the region; the outside neighbour is never read.
        for (edge, inside_side, expected) in [
            (IVec3::new(0, 41, 2), 1, Direction::West),
            (IVec3::new(4, 41, 2), 0, Direction::East),
            (IVec3::new(2, 41, 0), 3, Direction::North),
            (IVec3::new(2, 41, 4), 2, Direction::South),
        ] {
            let mut solid = [false; 4];
            solid[inside_side] = true;
            assert_eq!(
                direction_from_marker_neighbours(&neighbour_world(edge, solid), &region, edge),
                Ok(expected),
                "edge marker {edge:?}"
            );
        }

        // Water is non-solid: Stone west + Water east still leaves exactly one
        // opening (east). If water counted as solid this would be ambiguous.
        let mut world = resident_world(&region, BlockId::Air);
        set(&mut world, marker + IVec3::new(-1, 0, 0), BlockId::Stone);
        set(&mut world, marker + IVec3::new(1, 0, 0), BlockId::Water);
        assert_eq!(
            direction_from_marker_neighbours(&world, &region, marker),
            Ok(Direction::East)
        );

        // SocketMarker and Modded neighbours are solid, so they close a side.
        for block in [BlockId::SocketMarker, BlockId::Modded(24)] {
            let mut world = resident_world(&region, BlockId::Air);
            set(&mut world, marker + IVec3::new(0, 0, 1), block);
            assert_eq!(
                direction_from_marker_neighbours(&world, &region, marker),
                Ok(Direction::North),
                "{block:?} neighbour is solid"
            );
        }
    }

    #[test]
    fn doorway_marker_becomes_one_north_socket_and_an_air_cell() {
        let prefab = extract_prefab("doorway_house", &doorway_world(), house_region())
            .expect("doorway house captures");

        assert_eq!(prefab.sockets.len(), 1);
        assert_eq!(
            prefab.sockets[0],
            PrefabSocket {
                position: [0, 1, -2],
                direction: Direction::North,
            }
        );
        assert!(!prefab
            .blocks
            .iter()
            .any(|block| block.block == BlockId::SocketMarker));

        let marker_cell = prefab
            .blocks
            .iter()
            .find(|block| block.position == [2, 1, 0])
            .expect("the marker grid cell is preserved");
        assert_eq!(marker_cell.block, BlockId::Air);

        let text = prefab.to_toml_string();
        assert!(!text.contains("socket_marker"));
        assert_eq!(parse_prefab(&text), Ok(prefab));
    }

    #[test]
    fn multiple_markers_become_sorted_round_tripping_sockets() {
        let prefab = extract_prefab("two_doors", &two_doorway_world(), house_region())
            .expect("two-door house captures");

        let sockets: Vec<([i32; 3], Direction)> = prefab
            .sockets
            .iter()
            .map(|socket| (socket.position, socket.direction))
            .collect();
        assert_eq!(
            sockets,
            vec![
                ([0, 1, -2], Direction::North),
                ([0, 1, 2], Direction::South),
            ]
        );

        let text = prefab.to_toml_string();
        assert_eq!(parse_prefab(&text), Ok(prefab));

        // A marker-free house still yields an empty socket list.
        let plain = extract_prefab("house", &stone_house_world(), house_region())
            .expect("marker-free house captures");
        assert!(plain.sockets.is_empty());
    }

    #[test]
    fn socket_marker_and_modded_blocks_fail_with_their_coordinates() {
        let region = house_region();

        // Every neighbour is Stone, so all four sides are solid: no axis opens.
        let mut world = resident_world(&region, BlockId::Stone);
        set(&mut world, IVec3::new(1, 41, 1), BlockId::SocketMarker);
        assert_eq!(
            extract_prefab("house", &world, region),
            Err(CaptureError::AmbiguousSocket {
                pos: IVec3::new(1, 41, 1)
            })
        );

        let mut world = resident_world(&region, BlockId::Stone);
        set(&mut world, IVec3::new(2, 41, 2), BlockId::Modded(24));
        assert_eq!(
            extract_prefab("house", &world, region),
            Err(CaptureError::UnsupportedBlock {
                pos: IVec3::new(2, 41, 2),
                block: BlockId::Modded(24),
            })
        );
    }

    #[test]
    fn invalid_and_too_long_names_fail_without_writing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let world = resident_world(&house_region(), BlockId::Stone);
        let before = snapshot(temp.path());

        let invalid = capture_to_file(
            temp.path(),
            "Bad Name",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(0, 40, 0),
        )
        .unwrap_err();
        assert_eq!(
            invalid,
            CaptureError::InvalidName(PrefabError::NameInvalidCharacter)
        );

        let long_name = "a".repeat(65);
        let too_long = capture_to_file(
            temp.path(),
            &long_name,
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(0, 40, 0),
        )
        .unwrap_err();
        assert_eq!(
            too_long,
            CaptureError::InvalidName(PrefabError::NameTooLong)
        );
        assert_eq!(snapshot(temp.path()), before);
    }

    #[test]
    fn collision_refuses_and_leaves_the_existing_file_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prefabs = temp.path().join(PREFABS_SUBDIR);
        fs::create_dir_all(&prefabs).expect("create prefabs");
        let existing = prefabs.join("capture.toml");
        fs::write(&existing, b"format = 1\nname = \"capture\"\n").expect("write existing");
        let before = snapshot(temp.path());

        let world = resident_world(&house_region(), BlockId::Stone);
        let error = capture_to_file(
            temp.path(),
            "capture",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(4, 43, 4),
        )
        .unwrap_err();
        assert_eq!(
            error,
            CaptureError::AlreadyExists {
                name: "capture".to_owned(),
                file: "worldgen/prefabs/capture.toml".to_owned(),
            }
        );
        assert_eq!(snapshot(temp.path()), before);
        assert_eq!(
            fs::read(&existing).expect("existing file readable"),
            b"format = 1\nname = \"capture\"\n"
        );
    }

    #[test]
    fn declared_name_collision_refuses_and_leaves_the_tree_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prefabs = temp.path().join(PREFABS_SUBDIR);
        fs::create_dir_all(&prefabs).expect("create prefabs");
        // `renamed.toml` has a free stem but its header claims `capture`.
        let declared = Prefab {
            name: "capture".to_owned(),
            origin: [0, 0, 0],
            size: [1, 1, 1],
            blocks: vec![PrefabBlock {
                position: [0, 0, 0],
                block: BlockId::Stone,
            }],
            sockets: Vec::new(),
        };
        let existing = prefabs.join("renamed.toml");
        fs::write(&existing, declared.to_toml_string()).expect("write renamed prefab");
        let before = snapshot(temp.path());

        let world = resident_world(&house_region(), BlockId::Stone);
        let error = capture_to_file(
            temp.path(),
            "capture",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(4, 43, 4),
        )
        .unwrap_err();
        assert_eq!(
            error,
            CaptureError::AlreadyExists {
                name: "capture".to_owned(),
                file: "worldgen/prefabs/renamed.toml".to_owned(),
            }
        );
        assert_eq!(snapshot(temp.path()), before, "nothing may be written");
        assert!(!prefabs.join("capture.toml").exists());
    }

    #[test]
    fn declared_name_scan_still_allows_a_distinct_name_and_keeps_the_tree_valid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prefabs = temp.path().join(PREFABS_SUBDIR);
        fs::create_dir_all(&prefabs).expect("create prefabs");
        let declared = Prefab {
            name: "capture".to_owned(),
            origin: [0, 0, 0],
            size: [1, 1, 1],
            blocks: vec![PrefabBlock {
                position: [0, 0, 0],
                block: BlockId::Stone,
            }],
            sockets: Vec::new(),
        };
        fs::write(prefabs.join("renamed.toml"), declared.to_toml_string())
            .expect("write renamed prefab");

        let world = resident_world(&house_region(), BlockId::Stone);
        let (path, _) = capture_to_file(
            temp.path(),
            "capture_2",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(4, 43, 4),
        )
        .expect("a distinct name is still free");
        assert_eq!(path, prefabs.join("capture_2.toml"));

        let (lines, ok) = validate_worldgen_lines(temp.path());
        assert!(ok, "the tree must keep validating: {lines:?}");
        assert!(lines.contains(&"ok: prefab capture_2".to_owned()));
        assert!(lines.contains(&"ok: prefab capture".to_owned()));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("validate-worldgen: 2 prefabs, 0 errors, 0 warnings")
        );
    }

    #[test]
    fn successful_capture_writes_exactly_one_valid_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let world = stone_house_world();

        let (path, prefab) = capture_to_file(
            temp.path(),
            "capture",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(4, 43, 4),
        )
        .expect("capture succeeds");

        assert_eq!(path, temp.path().join(PREFABS_SUBDIR).join("capture.toml"));
        let text = fs::read_to_string(&path).expect("written file readable");
        assert_eq!(parse_prefab(&text), Ok(prefab));

        let files = snapshot(temp.path());
        assert_eq!(files.len(), 1, "exactly one file, no strays");
        assert!(files.contains_key(Path::new("prefabs/capture.toml")));

        assert_eq!(
            validate_worldgen_lines(temp.path()),
            (
                vec![
                    "ok: prefab capture".to_owned(),
                    "validate-worldgen: 1 prefabs, 0 errors, 0 warnings".to_owned(),
                ],
                true,
            )
        );
    }

    #[test]
    fn target_directory_write_fails_without_a_partial_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join(PREFABS_SUBDIR).join("capture.toml");
        fs::create_dir_all(&target).expect("create directory at target path");
        let before = snapshot(temp.path());

        let world = resident_world(&house_region(), BlockId::Stone);
        let error = capture_to_file(
            temp.path(),
            "capture",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(4, 43, 4),
        )
        .unwrap_err();

        match error {
            CaptureError::WriteFailed { file, reason } => {
                assert_eq!(file, "worldgen/prefabs/capture.toml");
                assert_eq!(reason, "is a directory");
            }
            other => panic!("expected WriteFailed, got {other:?}"),
        }
        assert!(target.is_dir(), "the blocking directory is left in place");
        assert_eq!(snapshot(temp.path()), before);
    }

    #[test]
    fn extraction_rejects_marker_before_serializing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut world = resident_world(&house_region(), BlockId::Stone);
        set(&mut world, IVec3::new(0, 40, 0), BlockId::SocketMarker);
        let before = snapshot(temp.path());

        // The min corner is open on both axes (west and north are outside the
        // region), so the whole capture fails before any byte is written.
        let error = capture_to_file(
            temp.path(),
            "house",
            &world,
            IVec3::new(0, 40, 0),
            IVec3::new(4, 43, 4),
        )
        .unwrap_err();
        assert_eq!(
            error,
            CaptureError::AmbiguousSocket {
                pos: IVec3::new(0, 40, 0)
            }
        );
        assert_eq!(
            error.to_string(),
            "socket marker at [0, 40, 0] is ambiguous"
        );
        assert_eq!(snapshot(temp.path()), before);
    }

    #[test]
    fn every_display_reason_is_pinned() {
        let cases = [
            (
                CaptureError::SizeExceedsCap { size: [33, 4, 5] },
                "selection is 33x4x5, exceeds prefab cap 32x32x32",
            ),
            (
                CaptureError::OutsideWorldHeight,
                "selection is outside the world height (-32..=159)",
            ),
            (
                CaptureError::UnloadedChunk {
                    pos: ChunkPos::new(-1, 2, 3),
                },
                "chunk (-1, 2, 3) is not loaded",
            ),
            (
                CaptureError::InvalidName(PrefabError::NameTooLong),
                "invalid prefab name: name is longer than 64 bytes",
            ),
            (
                CaptureError::AmbiguousSocket {
                    pos: IVec3::new(1, 41, -2),
                },
                "socket marker at [1, 41, -2] is ambiguous",
            ),
            (
                CaptureError::UnsupportedBlock {
                    pos: IVec3::new(0, 0, 0),
                    block: BlockId::Modded(24),
                },
                "block modded:24 at [0, 0, 0] is not supported",
            ),
            (
                CaptureError::AlreadyExists {
                    name: "house".to_owned(),
                    file: "worldgen/prefabs/house.toml".to_owned(),
                },
                "prefab 'house' already exists at worldgen/prefabs/house.toml",
            ),
            (
                CaptureError::SerializeMismatch,
                "captured prefab did not round-trip",
            ),
            (
                CaptureError::WriteFailed {
                    file: "worldgen/prefabs/house.toml".to_owned(),
                    reason: "permission denied".to_owned(),
                },
                "could not write worldgen/prefabs/house.toml: permission denied",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }
}
