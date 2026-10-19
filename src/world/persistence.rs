use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use crate::world::block::BlockId;
use crate::world::chunk::{ChunkPos, CHUNK_VOLUME};

pub const DEFAULT_SAVE_PATH: &str = "saves/default.mcvx";
pub const SAVE_MAGIC: &[u8; 4] = b"MCVX";
pub const SAVE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct CliConfig {
    pub save_path: PathBuf,
    pub seed_override: Option<u32>,
    pub force_new_world: bool,
}

impl CliConfig {
    pub fn from_iter<I, S>(args: I) -> Self
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
    if version != SAVE_VERSION {
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
        },
        chunk_edits,
    })
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

    #[test]
    fn test_cli_args_parsing() {
        let cfg = CliConfig::from_iter(["--seed", "999", "--save", "custom.mcvx", "--new"]);
        assert_eq!(cfg.seed_override, Some(999));
        assert_eq!(cfg.save_path, std::path::PathBuf::from("custom.mcvx"));
        assert!(cfg.force_new_world);
    }
}
