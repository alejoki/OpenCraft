use std::fmt;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use crate::player::inventory::{Hotbar, ItemStack};
use crate::world::block::BlockId;
use crate::world::chunk::{ChunkPos, CHUNK_VOLUME};

pub const DEFAULT_SAVE_PATH: &str = "saves/default.mcvx";
pub const SAVE_MAGIC: &[u8; 4] = b"MCVX";
pub const SAVE_VERSION: u32 = 2;
/// First save version that carries the 27-byte hotbar header block.
pub const HOTBAR_HEADER_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct CliConfig {
    pub save_path: PathBuf,
    pub seed_override: Option<u32>,
    pub force_new_world: bool,
}

impl CliConfig {
    pub fn parse_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut save_path = PathBuf::from(DEFAULT_SAVE_PATH);
        let mut seed_override = None;
        let mut force_new_world = false;

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_ref() {
                "--save" => {
                    if let Some(val) = iter.next() {
                        save_path = PathBuf::from(val.as_ref());
                    }
                }
                "--seed" => {
                    if let Some(val) = iter.next() {
                        if let Ok(parsed) = val.as_ref().parse::<u32>() {
                            seed_override = Some(parsed);
                        }
                    }
                }
                "--new" | "--new-world" => {
                    force_new_world = true;
                }
                _ => {}
            }
        }

        Self {
            save_path,
            seed_override,
            force_new_world,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedPlayerState {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub fly_mode: bool,
    pub selected_slot: u8,
    pub render_distance: u16,
    pub hotbar: Hotbar,
}

pub type ChunkEditMap = FxHashMap<ChunkPos, FxHashMap<u16, BlockId>>;

#[derive(Clone, Debug, PartialEq)]
pub struct WorldSaveData {
    pub seed: u32,
    pub player: SavedPlayerState,
    pub chunk_edits: ChunkEditMap,
}

#[derive(Debug)]
pub enum PersistenceError {
    Io(std::io::Error),
    InvalidMagic,
    UnsupportedVersion(u32),
    InvalidBlockId(u8),
    InvalidLocalIndex(u16),
    InvalidPlayerState,
}

impl From<std::io::Error> for PersistenceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistenceError::Io(e) => write!(f, "could not read or write the save file: {e}"),
            PersistenceError::InvalidMagic => write!(f, "not a valid save file (bad magic)"),
            PersistenceError::UnsupportedVersion(v) => {
                write!(f, "unsupported save version {v} (this build reads version {SAVE_VERSION})")
            }
            PersistenceError::InvalidBlockId(b) => write!(f, "unknown block id {b} in save"),
            PersistenceError::InvalidLocalIndex(i) => {
                write!(f, "invalid block index {i} in save")
            }
            PersistenceError::InvalidPlayerState => write!(f, "invalid player state in save"),
        }
    }
}

impl std::error::Error for PersistenceError {}

pub fn save_world_to_bytes(
    out: &mut Vec<u8>,
    data: &WorldSaveData,
) -> Result<(), PersistenceError> {
    out.extend_from_slice(SAVE_MAGIC);
    out.extend_from_slice(&SAVE_VERSION.to_le_bytes());
    out.extend_from_slice(&data.seed.to_le_bytes());
    for c in data.player.position {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&data.player.yaw.to_le_bytes());
    out.extend_from_slice(&data.player.pitch.to_le_bytes());
    out.push(u8::from(data.player.fly_mode));
    out.push(data.player.selected_slot.min(8));
    out.extend_from_slice(&data.player.render_distance.clamp(4, 36).to_le_bytes());

    // v2 hotbar header: 9 slots of `block_id: u8` + `count: u16` LE.
    // `block_id == 0` (Air) is the empty-slot sentinel and carries count 0.
    for stack in data.player.hotbar.stacks() {
        match stack {
            Some(stack) => {
                out.push(stack.block as u8);
                out.extend_from_slice(&stack.count.to_le_bytes());
            }
            None => {
                out.push(0);
                out.extend_from_slice(&0u16.to_le_bytes());
            }
        }
    }

    let mut non_empty_chunks: Vec<(&ChunkPos, &FxHashMap<u16, BlockId>)> = data
        .chunk_edits
        .iter()
        .filter(|(_, edits)| !edits.is_empty())
        .collect();
    non_empty_chunks.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

    out.extend_from_slice(&(non_empty_chunks.len() as u32).to_le_bytes());
    for (pos, edits) in non_empty_chunks {
        out.extend_from_slice(&pos.x.to_le_bytes());
        out.extend_from_slice(&pos.y.to_le_bytes());
        out.extend_from_slice(&pos.z.to_le_bytes());

        let mut sorted_edits: Vec<(u16, BlockId)> =
            edits.iter().map(|(&idx, &b)| (idx, b)).collect();
        sorted_edits.sort_by_key(|&(idx, _)| idx);

        out.extend_from_slice(&(sorted_edits.len() as u32).to_le_bytes());
        for (idx, block) in sorted_edits {
            out.extend_from_slice(&idx.to_le_bytes());
            out.push(block as u8);
        }
    }

    Ok(())
}

fn read_u8<R: Read>(r: &mut R) -> Result<u8, PersistenceError> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16, PersistenceError> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_i32<R: Read>(r: &mut R) -> Result<i32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_le_bytes(b))
}

fn read_f32<R: Read>(r: &mut R) -> Result<f32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    let v = f32::from_le_bytes(b);
    if !v.is_finite() {
        return Err(PersistenceError::InvalidPlayerState);
    }
    Ok(v)
}

pub fn load_world_from_bytes(bytes: &[u8]) -> Result<WorldSaveData, PersistenceError> {
    let mut cur = Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic)?;
    if &magic != SAVE_MAGIC {
        return Err(PersistenceError::InvalidMagic);
    }

    let version = read_u32(&mut cur)?;
    if !(1..=SAVE_VERSION).contains(&version) {
        return Err(PersistenceError::UnsupportedVersion(version));
    }

    let seed = read_u32(&mut cur)?;
    let px = read_f32(&mut cur)?;
    let py = read_f32(&mut cur)?;
    let pz = read_f32(&mut cur)?;
    let yaw = read_f32(&mut cur)?;
    let pitch = read_f32(&mut cur)?;
    let fly_raw = read_u8(&mut cur)?;
    let selected_slot = read_u8(&mut cur)?;
    let render_distance = read_u16(&mut cur)?;

    if selected_slot >= 9 || !(4..=36).contains(&render_distance) {
        return Err(PersistenceError::InvalidPlayerState);
    }

    // v2 added the 27-byte hotbar block between render_distance and
    // chunk_count. v1 files predate it and migrate to an empty hotbar.
    let hotbar = if version >= HOTBAR_HEADER_VERSION {
        read_hotbar(&mut cur)?
    } else {
        Hotbar::empty()
    };

    let chunk_count = read_u32(&mut cur)?;
    if chunk_count > 1_000_000 {
        return Err(PersistenceError::InvalidPlayerState);
    }

    let mut chunk_edits: ChunkEditMap = FxHashMap::default();
    for _ in 0..chunk_count {
        let cx = read_i32(&mut cur)?;
        let cy = read_i32(&mut cur)?;
        let cz = read_i32(&mut cur)?;
        let edit_count = read_u32(&mut cur)?;
        if edit_count == 0 || edit_count as usize > CHUNK_VOLUME {
            return Err(PersistenceError::InvalidPlayerState);
        }

        let mut edits = FxHashMap::default();
        for _ in 0..edit_count {
            let local_idx = read_u16(&mut cur)?;
            if local_idx as usize >= CHUNK_VOLUME {
                return Err(PersistenceError::InvalidLocalIndex(local_idx));
            }
            let block_raw = read_u8(&mut cur)?;
            let Some(block) = BlockId::from_u8(block_raw) else {
                return Err(PersistenceError::InvalidBlockId(block_raw));
            };
            edits.insert(local_idx, block);
        }
        chunk_edits.insert(ChunkPos::new(cx, cy, cz), edits);
    }

    Ok(WorldSaveData {
        seed,
        player: SavedPlayerState {
            position: [px, py, pz],
            yaw,
            pitch,
            fly_mode: fly_raw != 0,
            selected_slot,
            render_distance,
            hotbar,
        },
        chunk_edits,
    })
}

/// Reads and strictly validates the v2 hotbar header block.
///
/// A zero block id is the empty-slot sentinel and must pair with count 0.
/// Any other id must decode, be carryable (not Air/Bedrock), and carry a
/// count within `1..=MAX_STACK_COUNT`. Short reads surface as `Io`.
fn read_hotbar<R: Read>(r: &mut R) -> Result<Hotbar, PersistenceError> {
    let mut slots: [Option<ItemStack>; crate::player::inventory::HOTBAR_SLOTS] =
        [None; crate::player::inventory::HOTBAR_SLOTS];

    for slot in slots.iter_mut() {
        let block_raw = read_u8(r)?;
        let count = read_u16(r)?;

        if block_raw == BlockId::Air as u8 {
            if count != 0 {
                return Err(PersistenceError::InvalidPlayerState);
            }
            continue;
        }

        let Some(block) = BlockId::from_u8(block_raw) else {
            return Err(PersistenceError::InvalidBlockId(block_raw));
        };
        if matches!(block, BlockId::Air | BlockId::Bedrock) {
            return Err(PersistenceError::InvalidPlayerState);
        }
        let Some(stack) = ItemStack::new(block, count) else {
            return Err(PersistenceError::InvalidPlayerState);
        };
        *slot = Some(stack);
    }

    Ok(Hotbar::from_slots(slots))
}

pub fn save_world_to_path(path: &Path, data: &WorldSaveData) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut bytes = Vec::with_capacity(1024);
    save_world_to_bytes(&mut bytes, data)?;

    let tmp_path = path.with_extension("mcvx.tmp");
    {
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn load_world_from_path(path: &Path) -> Result<WorldSaveData, PersistenceError> {
    let bytes = fs::read(path)?;
    load_world_from_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::MAX_STACK_COUNT;
    use crate::world::block::BlockId;
    use crate::world::chunk::{linear_index, ChunkPos};

    #[test]
    fn test_save_and_load_roundtrip() {
        let mut edits = ChunkEditMap::default();
        let mut c0 = rustc_hash::FxHashMap::default();
        c0.insert(linear_index(0, 5, 31) as u16, BlockId::Air);
        c0.insert(linear_index(12, 10, 4) as u16, BlockId::Brick);
        edits.insert(ChunkPos::new(-2, 0, 3), c0);
        edits.insert(ChunkPos::new(9, 1, 9), rustc_hash::FxHashMap::default());

        let data = WorldSaveData {
            seed: 424242,
            player: SavedPlayerState {
                position: [-12.5, 64.25, 108.0],
                yaw: 1.25,
                pitch: -0.4,
                fly_mode: false,
                selected_slot: 4,
                render_distance: 20,
                hotbar: Hotbar::empty(),
            },
            chunk_edits: edits,
        };

        let tmp_path = std::env::temp_dir().join("mcvx_test_roundtrip.mcvx");
        save_world_to_path(&tmp_path, &data).expect("save should succeed");
        let loaded = load_world_from_path(&tmp_path).expect("load should succeed");
        let _ = std::fs::remove_file(&tmp_path);

        assert_eq!(loaded.seed, 424242);
        assert_eq!(loaded.player, data.player);
        assert_eq!(loaded.chunk_edits.len(), 1);
        assert_eq!(
            loaded.chunk_edits[&ChunkPos::new(-2, 0, 3)][&(linear_index(12, 10, 4) as u16)],
            BlockId::Brick
        );
    }

    #[test]
    fn test_save_roundtrips_additive_ore_block_ids() {
        let coal_index = linear_index(1, 2, 3) as u16;
        let iron_index = linear_index(4, 5, 6) as u16;
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(coal_index, BlockId::CoalOre);
        chunk.insert(iron_index, BlockId::IronOre);
        let mut edits = ChunkEditMap::default();
        edits.insert(ChunkPos::new(-1, 2, 3), chunk);

        let data = WorldSaveData {
            seed: 1337,
            player: SavedPlayerState {
                position: [1.0, 2.0, 3.0],
                yaw: 0.0,
                pitch: 0.0,
                fly_mode: false,
                selected_slot: 0,
                render_distance: 16,
                hotbar: Hotbar::empty(),
            },
            chunk_edits: edits,
        };

        assert_eq!(SAVE_VERSION, 2);
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");
        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");

        let loaded_chunk = loaded
            .chunk_edits
            .get(&ChunkPos::new(-1, 2, 3))
            .expect("edited chunk must round-trip");
        assert_eq!(loaded_chunk[&coal_index], BlockId::CoalOre);
        assert_eq!(loaded_chunk[&iron_index], BlockId::IronOre);
        assert_eq!(loaded.seed, data.seed);
    }

    #[test]
    fn test_corrupted_save_rejected() {
        assert!(matches!(
            load_world_from_bytes(b"NOPE0000"),
            Err(PersistenceError::InvalidMagic | PersistenceError::Io(_))
        ));

        let mut valid = Vec::new();
        let mut edits = ChunkEditMap::default();
        let mut c0 = rustc_hash::FxHashMap::default();
        c0.insert(10, BlockId::Stone);
        edits.insert(ChunkPos::new(0, 0, 0), c0);
        let data = WorldSaveData {
            seed: 1,
            player: SavedPlayerState {
                position: [0.0, 50.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                fly_mode: true,
                selected_slot: 0,
                render_distance: 16,
                hotbar: Hotbar::empty(),
            },
            chunk_edits: edits,
        };
        save_world_to_bytes(&mut valid, &data).unwrap();

        let last = valid.len() - 1;
        valid[last] = 99;
        assert!(matches!(
            load_world_from_bytes(&valid),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        valid[last] = BlockId::Stone as u8;
        let idx_bytes = 40000u16.to_le_bytes();
        valid[last - 2] = idx_bytes[0];
        valid[last - 1] = idx_bytes[1];
        assert!(matches!(
            load_world_from_bytes(&valid),
            Err(PersistenceError::InvalidLocalIndex(40000))
        ));
    }

    fn hotbar_sample() -> Hotbar {
        Hotbar::from_slots([
            Some(ItemStack::new(BlockId::CoalOre, 3).unwrap()),
            Some(ItemStack::new(BlockId::IronOre, 2).unwrap()),
            None,
            None,
            Some(ItemStack::new(BlockId::Stone, MAX_STACK_COUNT).unwrap()),
            None,
            None,
            None,
            Some(ItemStack::new(BlockId::Brick, 1).unwrap()),
        ])
    }

    fn sample_v2_data(hotbar: Hotbar) -> WorldSaveData {
        WorldSaveData {
            seed: 2024,
            player: SavedPlayerState {
                position: [1.0, 64.0, -2.0],
                yaw: 0.5,
                pitch: -0.25,
                fly_mode: true,
                selected_slot: 4,
                render_distance: 16,
                hotbar,
            },
            chunk_edits: ChunkEditMap::default(),
        }
    }

    #[test]
    fn test_v2_hotbar_roundtrips_exactly() {
        let hotbar = hotbar_sample();
        let data = sample_v2_data(hotbar.clone());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");

        // Header layout: 36 bytes through render_distance, then 9 * 3
        // hotbar bytes, then chunk_count. No edits means 4 more bytes.
        assert_eq!(bytes.len(), 36 + 27 + 4);
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());

        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.player, data.player);
        assert_eq!(loaded.player.selected_slot, 4);
        for slot in 0..crate::player::inventory::HOTBAR_SLOTS {
            assert_eq!(
                loaded.player.hotbar.stack(slot),
                hotbar.stack(slot),
                "slot {slot} must round-trip"
            );
        }
    }

    #[test]
    fn test_v1_save_migrates_to_empty_hotbar() {
        // Hand-build the old v1 layout (no 27-byte hotbar block).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SAVE_MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&777u32.to_le_bytes());
        for c in [-4.5f32, 70.25, 9.0] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&1.5f32.to_le_bytes()); // yaw
        bytes.extend_from_slice(&(-0.25f32).to_le_bytes()); // pitch
        bytes.push(1); // fly_mode
        bytes.push(6); // selected_slot
        bytes.extend_from_slice(&18u16.to_le_bytes()); // render_distance

        // One chunk with two edits.
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2i32.to_le_bytes());
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.extend_from_slice(&3i32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        let coal_index = linear_index(1, 2, 3) as u16;
        bytes.extend_from_slice(&coal_index.to_le_bytes());
        bytes.push(BlockId::CoalOre as u8);
        let iron_index = linear_index(4, 5, 6) as u16;
        bytes.extend_from_slice(&iron_index.to_le_bytes());
        bytes.push(BlockId::IronOre as u8);

        let loaded = load_world_from_bytes(&bytes).expect("v1 must migrate, not fail");

        assert_eq!(loaded.seed, 777);
        assert_eq!(loaded.player.position, [-4.5, 70.25, 9.0]);
        assert_eq!(loaded.player.yaw, 1.5);
        assert_eq!(loaded.player.pitch, -0.25);
        assert!(loaded.player.fly_mode);
        assert_eq!(loaded.player.selected_slot, 6);
        assert_eq!(loaded.player.render_distance, 18);
        assert_eq!(loaded.player.hotbar, Hotbar::empty());

        let chunk = loaded
            .chunk_edits
            .get(&ChunkPos::new(2, -1, 3))
            .expect("v1 chunk edits must survive migration");
        assert_eq!(chunk[&coal_index], BlockId::CoalOre);
        assert_eq!(chunk[&iron_index], BlockId::IronOre);
    }

    /// Byte offset of the first hotbar slot in a v2 save.
    const V2_HOTBAR_OFFSET: usize = 36;

    fn set_v2_slot(bytes: &mut [u8], slot: usize, block: u8, count: u16) {
        let off = V2_HOTBAR_OFFSET + slot * 3;
        bytes[off] = block;
        bytes[off + 1..off + 3].copy_from_slice(&count.to_le_bytes());
    }

    fn valid_v2_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &sample_v2_data(Hotbar::empty())).unwrap();
        bytes
    }

    #[test]
    fn test_v2_hotbar_rejects_invalid_slots() {
        // Empty sentinel must carry count 0.
        let mut bytes = valid_v2_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Air as u8, 5);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // A non-empty block id may not carry count 0.
        let mut bytes = valid_v2_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Grass as u8, 0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // Bedrock cannot be carried.
        let mut bytes = valid_v2_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Bedrock as u8, 1);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // Unknown ids surface as InvalidBlockId.
        let mut bytes = valid_v2_bytes();
        set_v2_slot(&mut bytes, 0, 99, 1);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        // Counts above MAX_STACK_COUNT are invalid.
        let mut bytes = valid_v2_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::CoalOre as u8, MAX_STACK_COUNT + 1);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    #[test]
    fn test_v2_hotbar_truncated_and_unsupported_version() {
        let bytes = valid_v2_bytes();
        // Cut mid-hotbar (after slot 1's block byte) so the count read hits EOF.
        let truncated = &bytes[..V2_HOTBAR_OFFSET + 4];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));

        let mut future = valid_v2_bytes();
        future[4..8].copy_from_slice(&3u32.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&future),
            Err(PersistenceError::UnsupportedVersion(3))
        ));
    }

    #[test]
    fn test_cli_args_parsing() {
        let cfg = CliConfig::parse_args(["--seed", "999", "--save", "custom.mcvx", "--new"]);
        assert_eq!(cfg.seed_override, Some(999));
        assert_eq!(cfg.save_path, std::path::PathBuf::from("custom.mcvx"));
        assert!(cfg.force_new_world);
    }

    #[test]
    fn test_cli_args_ignore_malformed_and_incomplete_flags() {
        // A non-numeric seed is ignored; "--save" still consumes the following
        // token as its value, so "--new" never becomes a flag.
        let cfg = CliConfig::parse_args(["--seed", "not-a-number", "--save", "--new"]);
        assert_eq!(cfg.seed_override, None);
        assert_eq!(cfg.save_path, std::path::PathBuf::from("--new"));
        assert!(!cfg.force_new_world);

        // A flag with no following value keeps the default path and seed.
        let incomplete = CliConfig::parse_args(["--seed"]);
        assert_eq!(incomplete.seed_override, None);
        assert_eq!(
            incomplete.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );

        // A seed that overflows u32 is rejected rather than truncated.
        let overflow = CliConfig::parse_args(["--seed", "4294967296"]);
        assert_eq!(overflow.seed_override, None);
    }
}
