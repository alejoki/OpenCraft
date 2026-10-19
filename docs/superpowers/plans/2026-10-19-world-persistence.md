# World Persistence & Streaming Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add zero-dependency binary world persistence (seed, player state, and sparse per-chunk voxel deltas), CLI seed/save flags, auto-save on chunk unload and exit, `F5` quick-save, and streaming/input polish.

**Architecture:** `world::persistence` serializes `WorldSaveData` (`MCVX` v1 binary header + sorted per-chunk `(u16, BlockId)` deltas) atomically via temporary file rename. `world::terrain` is parameterized by `seed: u32`, and `WorldManager` replays saved deltas onto newly generated chunks before meshing while skipping `Uniform(BlockId::Air)` chunks before the 24-chunk per-frame mesh budget. `main.rs` wires CLI arguments, startup load, unload/exit/`F5` saves, and `WindowEvent::Focused(false)` input reset.

**Tech Stack:** Rust (`std::fs`, `std::io::{Read, Write, BufReader, BufWriter}`), `rustc-hash`, `glam`, `winit`, `wgpu` (zero new crate dependencies).

**Spec:** `docs/superpowers/specs/2026-10-19-world-persistence-design.md`

## Global Constraints

- Binary save header magic is `b"MCVX"` (`[u8; 4]`) and `version: u32 = 1` (Little-Endian).
- Default seed: `DEFAULT_WORLD_SEED: u32 = 1337`; default save path: `DEFAULT_SAVE_PATH: &str = "saves/default.mcvx"`.
- Dense voxel linear index is `x + (z << 5) + (y << 10)` ∈ `0..32768` (`u16`).
- Zero new crate dependencies in `Cargo.toml`.

## Review Focus

1. **Corrupted or out-of-bounds binary save data (`local_index >= 32768`, invalid `BlockId`, or `NaN` player position):** `load_world_from_bytes` must validate magic, version, finite player floats, `selected_slot < 9`, `local_index < 32768`, and `BlockId::from_u8` and return `Err(PersistenceError)` instead of panicking.
2. **Empty chunk edit maps (`edit_count == 0`) written to disk:** `save_world_to_bytes` must filter out any chunk entry whose edit map is empty before writing `chunk_count`.
3. **Chunk-boundary block edits (`lx = 0` or `31`) losing neighbor halo accuracy after unload and regeneration:** Saved chunk deltas must be applied to generated chunks immediately upon `gen_rx` receipt before `dirty_mesh_queue` / `build_padded_chunk` runs.
4. **Uniform-Air sky chunks starving the 24-chunk per-frame mesh budget:** `Uniform(BlockId::Air)` chunks in `dirty_mesh_queue` must be marked `meshed_chunks` and removed before `.take(24)` so solid terrain chunks get the full per-frame mesh budget.
5. **Stuck movement keys on window focus loss (`WindowEvent::Focused(false)`):** Losing window focus must reset `InputState` to `InputState::default()`.

---

### Task 1: `BlockId::from_u8`, Index Helpers, Seeded Terrain & Atomic Binary Save/Load

**Files:**
- Modify: `src/world/block.rs`
- Modify: `src/world/chunk.rs`
- Modify: `src/world/terrain.rs`
- Modify: `src/world/mod.rs`
- Create: `src/world/persistence.rs`

**Interfaces:**
- Consumes: `BlockId`, `Chunk`, `ChunkPos`, `CHUNK_SIZE`, `CHUNK_VOLUME`, `linear_index`
- Produces:
  - `BlockId::from_u8(v: u8) -> Option<BlockId>`
  - `world::chunk::local_coords_from_index(idx: u16) -> (usize, usize, usize)`, `Chunk::set_by_index(&mut self, idx: u16, block: BlockId)`
  - `world::terrain::{DEFAULT_WORLD_SEED, terrain_height_at_seeded, generate_chunk_seeded}` (keeping `terrain_height_at` and `generate_chunk` delegating to `DEFAULT_WORLD_SEED`)
  - `world::persistence::{DEFAULT_SAVE_PATH, CliConfig, SavedPlayerState, ChunkEditMap, WorldSaveData, PersistenceError, save_world_to_bytes, load_world_from_bytes, save_world_to_path, load_world_from_path}`

- [ ] **Step 1: Write failing tests in `src/world/persistence.rs` and `src/world/terrain.rs`**

```rust
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
        edits.insert(ChunkPos::new(9, 1, 9), rustc_hash::FxHashMap::default()); // empty map should be omitted

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

        // Corrupt block_id (last byte) to invalid value 99
        let last = valid.len() - 1;
        valid[last] = 99;
        assert!(matches!(
            load_world_from_bytes(&valid),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        // Corrupt local_index to 40000 (>= 32768)
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib world::persistence::tests`
Expected: FAIL

- [ ] **Step 3: Implement `BlockId::from_u8`, `Chunk::set_by_index`, `generate_chunk_seeded`, and `src/world/persistence.rs`**

Implement `BlockId::from_u8(v: u8) -> Option<BlockId>` in `src/world/block.rs`, `local_coords_from_index(idx: u16) -> (usize, usize, usize)` and `Chunk::set_by_index(&mut self, idx: u16, block: BlockId)` in `src/world/chunk.rs`, `terrain_height_at_seeded` / `generate_chunk_seeded` in `src/world/terrain.rs`, and the binary reader/writer + `CliConfig::from_iter` in `src/world/persistence.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/world/
git commit -m "feat(world): add seeded terrain generation and atomic MCVX binary persistence"
```

---

### Task 2: `WorldManager` Delta Replay, Persistent Block Edits & Uniform-Air Streaming Fast Path

**Files:**
- Modify: `src/world/manager.rs`

**Interfaces:**
- Consumes: `ChunkEditMap`, `DEFAULT_WORLD_SEED`, `generate_chunk_seeded`, `linear_index`, `Chunk::set_by_index`
- Produces:
  - `WorldManager::with_seed_and_edits(render_distance: i32, seed: u32, chunk_edits: ChunkEditMap) -> Self`
  - `WorldManager::seed(&self) -> u32`, `WorldManager::chunk_edits(&self) -> &ChunkEditMap`
  - `WorldManager::has_unsaved_changes(&self) -> bool`, `WorldManager::mark_saved(&mut self)`
  - `WorldManager::dirty_mesh_queue_len(&self) -> usize`

- [ ] **Step 1: Write failing tests in `src/world/manager.rs`**

```rust
#[test]
fn test_chunk_edits_persist_across_unload_and_regeneration() {
    let mut mgr = WorldManager::with_seed_and_edits(4, 1337, ChunkEditMap::default());
    let c0 = ChunkPos::new(0, 1, 0);
    let c_neg_x = ChunkPos::new(-1, 1, 0);
    for dx in -2..=1 {
        for dy in 0..=2 {
            for dz in -1..=1 {
                let p = ChunkPos::new(dx, dy, dz);
                mgr.insert_chunk_for_test(p, generate_chunk_seeded(p, 1337));
            }
        }
    }

    // Place a Brick block at wx = 0, wy = 40, wz = 5 (local x = 0 of chunk (0, 1, 0))
    mgr.set_block_immediate(0, 40, 5, BlockId::Brick);
    assert!(mgr.has_unsaved_changes());
    assert_eq!(mgr.get_block(0, 40, 5), BlockId::Brick);

    // Move player far away (chunk 100, 1, 100) to force unloading chunk (0, 1, 0)
    let (_, unloaded) = mgr.update_streaming(ChunkPos::new(100, 1, 100));
    assert!(unloaded.contains(&c0));
    assert_eq!(mgr.get_block(0, 40, 5), BlockId::Air); // unloaded

    // Move back to (0, 1, 0) and pump streaming until (0, 1, 0) and (-1, 1, 0) regenerate
    for _ in 0..40 {
        mgr.update_streaming(c0);
        if mgr.get_block(0, 40, 5) == BlockId::Brick {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(mgr.get_block(0, 40, 5), BlockId::Brick);
    let _ = c_neg_x;
}

#[test]
fn test_uniform_air_chunks_do_not_consume_mesh_budget() {
    let mut mgr = WorldManager::new(4);
    // Insert 30 uniform Air chunks at cy = 4 and 1 solid Stone chunk at (0, 0, 0) with neighbors
    for dx in -2..=2 {
        for dz in -2..=2 {
            for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                let pos = ChunkPos::new(dx, cy, dz);
                let block = if cy == 0 && dx == 0 && dz == 0 {
                    BlockId::Stone
                } else {
                    BlockId::Air
                };
                mgr.insert_chunk_for_test(pos, Chunk::new_uniform(block));
            }
        }
    }
    mgr.queue_all_loaded_for_test();
    assert!(mgr.dirty_mesh_queue_len() > 25);

    // Single update_streaming call should clear all Uniform(Air) chunks immediately AND dispatch the solid chunk
    mgr.update_streaming(ChunkPos::new(0, 0, 0));
    assert_eq!(mgr.dirty_mesh_queue_len(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib world::manager::tests`
Expected: FAIL

- [ ] **Step 3: Implement delta tracking/replay and Uniform-Air mesh fast path in `src/world/manager.rs`**

- Add `world_seed: u32`, `chunk_edits: ChunkEditMap`, and `unsaved_changes: bool` to `WorldManager`.
- In `update_streaming` Step 1, when `(pos, mut chunk)` arrives from `gen_rx`, replay `self.chunk_edits.get(&pos)` via `chunk.set_by_index(idx, block)` and `chunk.try_compress()` before inserting `chunk` into `self.chunks`.
- In `set_block_immediate`, record `self.chunk_edits.entry(cpos).or_default().insert(linear_index(lx, ly, lz) as u16, block)` and `self.unsaved_changes = true`.
- In `update_streaming` Step 5, retain only non-`Uniform(BlockId::Air)` chunks in `dirty_mesh_queue` (inserting skipped uniform-Air chunks directly into `self.meshed_chunks`) before `.take(24)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib world::manager::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/world/manager.rs
git commit -m "feat(world): replay persistent chunk deltas on load and skip uniform-air meshing"
```

---

### Task 3: Startup Save/Load, CLI Flags, `F5` Quick-Save, Auto-Save on Unload/Exit & Focus-Loss Input Reset

**Files:**
- Modify: `src/player/physics.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `CliConfig`, `WorldSaveData`, `SavedPlayerState`, `save_world_to_path`, `load_world_from_path`, `WorldManager::with_seed_and_edits`, `terrain_height_at_seeded`, `InputState::reset`
- Produces:
  - `InputState::reset(&mut self)` in `src/player/physics.rs`
  - Full save/load lifecycle and `WindowEvent::Focused(false)` handling in `src/main.rs`

- [ ] **Step 1: Write failing test for `InputState::reset` on focus loss in `src/player/physics.rs`**

```rust
#[test]
fn test_focus_loss_resets_input_state() {
    let mut input = InputState {
        forward: true,
        backward: true,
        left: true,
        right: true,
        jump_or_up: true,
        crouch_or_down: true,
        sprint: true,
    };
    input.reset();
    assert!(!input.forward && !input.backward && !input.left && !input.right);
    assert!(!input.jump_or_up && !input.crouch_or_down && !input.sprint);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib player::physics::tests::test_focus_loss_resets_input_state`
Expected: FAIL

- [ ] **Step 3: Implement `InputState::reset` in `src/player/physics.rs` and persistence/focus hooks in `src/main.rs`**

- Add `pub fn reset(&mut self) { *self = Self::default(); }` to `InputState` in `src/player/physics.rs`.
- In `src/main.rs`:
  - Parse `CliConfig::from_iter(std::env::args().skip(1))`.
  - Load existing `WorldSaveData` if `!cli.force_new_world && cli.save_path.exists()`, restoring `seed`, `Player`, and `WorldManager::with_seed_and_edits`; otherwise initialize with `cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED)`.
  - Add helper `save_current_world(&cli.save_path, &mut world, &player)` called on `WindowEvent::CloseRequested`, on `KeyCode::F5`, and when `!unloaded.is_empty() && world.has_unsaved_changes()`.
  - Handle `WindowEvent::Focused(false)` by calling `input.reset()`.

- [ ] **Step 4: Run `cargo test` and `cargo build --release` to verify all tests and release binary**

Run: `cargo test && cargo build --release`
Expected: PASS (all unit tests pass and release binary compiles cleanly)

- [ ] **Step 5: Commit**

```bash
git add src/player/physics.rs src/main.rs
git commit -m "feat(app): wire automatic save/load, CLI flags, F5 quick-save, and focus-loss input reset"
```
