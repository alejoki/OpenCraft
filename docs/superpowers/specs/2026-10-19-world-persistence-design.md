# World Persistence & Streaming Polish — Design Specification

**Date:** 2026-10-19
**Target Platform:** Native Desktop (Rust, zero additional crate dependencies)
**Primary Goal:** Persist world seed, player state, and sparse per-chunk block edits in a compact, atomic binary save file with sub-millisecond save/load times and zero impact on frame rates.

---

## 1. Goals & Scope

### 1.1 Intended Outcome
Allow players to build and excavate across an infinite seeded world where block edits (including chunk-boundary edits), player coordinates, camera orientation, movement mode, hotbar selection, and render distance automatically persist across chunk unloads and game restarts.

### 1.2 In-Scope Features
1. **Seed-Parameterized Terrain Generation (`src/world/terrain.rs`):**
   - `terrain_height_at_seeded(wx: i32, wz: i32, seed: u32) -> i32` and `generate_chunk_seeded(pos: ChunkPos, seed: u32) -> Chunk`.
   - Default seed `DEFAULT_WORLD_SEED: u32 = 1337`.
2. **CLI Arguments (`src/world/persistence.rs`):**
   - `--save <path>`: Save file path (default: `saves/default.mcvx`).
   - `--seed <u32>`: Seed override for new worlds (or when `--new` is passed).
   - `--new`: Start a fresh world, ignoring any existing save file at `--save` until the next save overwrites it.
3. **Sparse Per-Chunk Edit Deltas & Atomic Binary Save File (`src/world/persistence.rs`):**
   - Store only modified voxels per `ChunkPos` as `(local_index: u16, BlockId)` entries (`local_index = x + (z << 5) + (y << 10)` ∈ `0..32768`).
   - Versioned binary format (`MCVX` v1, Little-Endian) written atomically via `<save_path>.tmp` + `std::fs::rename`.
   - Full validation on load (magic bytes, version, `BlockId::from_u8`, `local_index < 32768`, finite floats).
4. **Delta Replay & Auto-Save Lifecycle (`src/world/manager.rs`, `src/main.rs`):**
   - Replay saved chunk deltas onto newly generated chunks immediately upon background generation completion, before meshing.
   - Record player block edits in `WorldManager::set_block_immediate` into `chunk_edits` and mark `unsaved_changes = true`.
   - Auto-save on window close (`WindowEvent::CloseRequested`), on chunk unload when `unsaved_changes` is true, and on manual quick-save (`F5` key).
5. **Streaming & Input Polish (`src/world/manager.rs`, `src/main.rs`):**
   - Skip `Uniform(BlockId::Air)` chunks in `WorldManager::update_streaming` Step 5 before allocating `PaddedChunk` or consuming the 24-chunk per-frame mesh budget.
   - Reset `InputState` on `WindowEvent::Focused(false)` so movement keys never stick when switching windows.

### 1.3 Out of Scope
- Multi-world GUI menu screen
- Background compression threads (saves are already a few KB)

---

## 2. Data Structures & Binary Format

### 2.1 `BlockId::from_u8` (`src/world/block.rs`)
Add `pub const fn from_u8(v: u8) -> Option<BlockId>` mapping `0..=11` to `BlockId::Air ..= BlockId::Bedrock` and returning `None` for `12..=255`.

### 2.2 `Chunk::apply_edits` (`src/world/chunk.rs`)
Add `pub fn set_by_index(&mut self, idx: u16, block: BlockId)` and `pub fn local_coords_from_index(idx: u16) -> (usize, usize, usize)`:
- `x = (idx as usize) & 31`
- `z = ((idx as usize) >> 5) & 31`
- `y = ((idx as usize) >> 10) & 31`

### 2.3 `WorldSaveData` & `CliConfig` (`src/world/persistence.rs`)
```rust
pub const DEFAULT_WORLD_SEED: u32 = 1337;
pub const DEFAULT_SAVE_PATH: &str = "saves/default.mcvx";

#[derive(Clone, Debug, PartialEq)]
pub struct CliConfig {
    pub save_path: std::path::PathBuf,
    pub seed_override: Option<u32>,
    pub force_new_world: bool,
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

pub type ChunkEditMap = rustc_hash::FxHashMap<ChunkPos, rustc_hash::FxHashMap<u16, BlockId>>;

#[derive(Clone, Debug, PartialEq)]
pub struct WorldSaveData {
    pub seed: u32,
    pub player: SavedPlayerState,
    pub chunk_edits: ChunkEditMap,
}
```

### 2.4 Binary Layout (`MCVX` Version 1, Little-Endian)
- `0..4`: `b"MCVX"` (`[u8; 4]`)
- `4..8`: `version: u32 = 1`
- `8..12`: `seed: u32`
- `12..24`: `player.position: [f32; 3]` (`x, y, z`)
- `24..28`: `player.yaw: f32`
- `28..32`: `player.pitch: f32`
- `32..33`: `player.fly_mode: u8` (`0` = Walk, `1` = Fly)
- `33..34`: `player.selected_slot: u8` (`0..=8`)
- `34..36`: `player.render_distance: u16` (`4..=36`)
- `36..40`: `chunk_count: u32`
- For each of `chunk_count` chunks (sorted deterministically by `(pos.x, pos.y, pos.z)` on write):
  - `pos.x: i32`, `pos.y: i32`, `pos.z: i32` (12 bytes)
  - `edit_count: u32` (`1..=32768`, 4 bytes)
  - For each of `edit_count` edits (sorted by `local_index` on write):
    - `local_index: u16` (`0..32768`, 2 bytes)
    - `block_id: u8` (`0..=11`, 1 byte)

---

## 3. Algorithms & Data Flow

1. **Startup Flow (`main.rs`):**
   - Parse `CliConfig::from_iter(std::env::args().skip(1))`.
   - If `!cli.force_new_world && cli.save_path.exists()`, call `load_world_from_path(&cli.save_path)`.
   - On success, restore `Player` and create `WorldManager::with_seed_and_edits(save.player.render_distance as i32, save.seed, save.chunk_edits)`.
   - If the save file does not exist (or `--new` was passed), use `seed = cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED)`, spawn at `y = terrain_height_at_seeded(0, 0, seed) + 4`, and create `WorldManager::with_seed_and_edits(16, seed, FxHashMap::default())`.
2. **Streaming & Delta Replay (`WorldManager`):**
   - Background terrain worker runs `generate_chunk_seeded(pos, seed)`.
   - When `gen_rx` delivers `(pos, mut chunk)` on the main thread, `WorldManager::apply_chunk_edits(pos, &mut chunk)` applies any saved `(local_idx, block_id)` entries for `pos` and calls `chunk.try_compress()` before inserting `chunk` into `self.chunks` and queueing neighbor meshes.
   - In `update_streaming` Step 5, before calling `build_padded_chunk(pos)` or counting `pos` toward the 24-chunk per-frame mesh budget, check if `self.chunks.get(&pos).and_then(|c| c.is_uniform()) == Some(BlockId::Air)`. If so, remove from `dirty_mesh_queue`, insert into `meshed_chunks`, and continue without allocating `PaddedChunk`.
3. **Editing & Saving (`WorldManager` & `main.rs`):**
   - `set_block_immediate(wx, wy, wz, block)` records `(linear_index(lx, ly, lz) as u16, block)` in `self.chunk_edits.entry(cpos).or_default()` and sets `self.unsaved_changes = true`.
   - When `update_streaming` unloads chunks (`!unloaded.is_empty()`) and `world.has_unsaved_changes()` is `true`, `main.rs` saves the world and calls `world.mark_saved()`.
   - Pressing `F5` or closing the window saves the world immediately.

---

## 4. Error Handling & Testing Strategy

### 4.1 Error Handling
- `PersistenceError` enum covers `Io(std::io::Error)`, `InvalidMagic`, `UnsupportedVersion(u32)`, `InvalidBlockId(u8)`, `InvalidLocalIndex(u16)`, and `InvalidPlayerState`.
- Atomic write creates parent directories (`std::fs::create_dir_all`), writes to `<save_path>.tmp`, calls `sync_all()`, and renames to `<save_path>`.

### 4.2 Automated Tests (`cargo test`)
1. `test_save_and_load_roundtrip`: Verifies seed, player state, negative chunk coordinates, and block edits round-trip identically through disk.
2. `test_corrupted_save_rejected`: Verifies bad magic, bad version, invalid block ID (`99`), and out-of-range voxel index (`40000`) return `Err`.
3. `test_cli_args_parsing`: Verifies `--seed`, `--save`, and `--new` flags parse accurately.
4. `test_chunk_edits_persist_across_unload_and_regeneration`: Edits a block inside a chunk and at a chunk border (`lx = 0`), unloads the chunks, regenerates them via `update_streaming`, and verifies the edits and boundary face culling are preserved.
5. `test_uniform_air_chunks_do_not_consume_mesh_budget`: Verifies `Uniform(BlockId::Air)` chunks in `dirty_mesh_queue` are marked meshed immediately without starving solid terrain chunks.
