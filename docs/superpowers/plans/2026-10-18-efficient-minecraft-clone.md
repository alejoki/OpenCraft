# Ultra-Efficient Rust Minecraft Clone Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native Rust (`winit` + `wgpu`) Minecraft clone with 32³ uniform/dense chunks, 64-bit binary greedy meshing, 8-byte instanced GPU quads, frustum/normal-bucket culling, multithreaded streaming, and playable sandbox physics/editing.

**Architecture:** Pure-data `world`, `mesh`, and `player` modules handle 32³ chunk compression, deterministic terrain generation, bitwise greedy meshing with ambient occlusion, AABB collision, and 3D DDA raycasting with zero GPU dependencies. A thin `render` module uploads 8-byte `PackedQuad` buffers to `wgpu`, culls chunks by frustum and face-normal buckets, expands quads in WGSL via vertex pulling, and draws procedural pixel-art block textures and HUD overlays.

**Tech Stack:** Rust (`edition = "2021"`), `winit = "0.29"`, `wgpu = "0.19"`, `pollster = "0.3"`, `bytemuck = { version = "1.14", features = ["derive"] }`, `glam = "0.27"`, `rayon = "1.10"`, `crossbeam-channel = "0.5"`, `rustc-hash = "1.1"`.

**Spec:** `docs/superpowers/specs/2026-10-18-efficient-minecraft-clone-design.md`

## Global Constraints

- `CHUNK_SIZE: usize = 32` (`CHUNK_VOLUME: usize = 32_768`; `PADDED_SIZE: usize = 34`).
- Dense chunk linear index: `idx = x + (z << 5) + (y << 10)` for `x, y, z ∈ 0..32`.
- `PackedQuad` is `#[repr(C)] pub struct PackedQuad(pub [u32; 2])` (exactly 8 bytes).
- World vertical range: `MIN_CHUNK_Y: i32 = -1`, `MAX_CHUNK_Y: i32 = 4` (`y ∈ -32..160`).
- `world`, `mesh`, and `player` modules must not depend on `wgpu` or `winit` so `cargo test` runs headlessly.

## Review Focus

1. **Negative world coordinate division/modulo off-by-one:** `world_to_chunk_and_local(-1, -32, -33)` must use Euclidean division (`div_euclid(32)`, `rem_euclid(32)`) so `-1` maps to chunk `-1`, local `31` rather than chunk `0`.
2. **Chunk boundary face culling across neighbor halos:** A solid voxel at local `x = 31` adjacent to a solid voxel at neighbor `x = 0` (`PaddedChunk` coordinate `x = 33`) must have its `+X` face culled so chunk borders never produce internal seams.
3. **Cross-chunk block edits at `local = 0` or `31` leaving stale neighbor meshes:** Mutating a voxel on a chunk face boundary (`0` or `31`) must return the neighbor `ChunkPos` in the dirty list so adjacent chunk faces update on the same frame.
4. **DDA raycast along axis-aligned rays (`dir.x == 0.0` and `dir.z == 0.0`):** Raycasting straight down `(0.0, -1.0, 0.0)` must guard `1.0 / dir` with `f32::INFINITY` and never produce `NaN` step distances.
5. **Placing a block overlapping the player's own AABB:** Block placement must check `Player::aabb().intersects_block(bx, by, bz)` and reject placement when the target voxel overlaps the player.

---

### Task 1: Project Scaffolding, Block Types & Compact 32³ Chunk Storage

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/world/mod.rs`
- Create: `src/world/block.rs`
- Create: `src/world/chunk.rs`

**Interfaces:**
- Consumes: none
- Produces:
  - `world::block::BlockId` (`#[repr(u8)]` enum: `Air = 0, Grass = 1, Dirt = 2, Stone = 3, Cobblestone = 4, Sand = 5, OakLog = 6, OakLeaves = 7, OakPlanks = 8, Brick = 9, Snow = 10, Bedrock = 11`)
  - `BlockId::is_solid(self) -> bool`, `BlockId::face_tex_layer(self, face_dir: u8) -> u8`, `BlockId::HOTBAR: [BlockId; 9]`, `BlockId::name(self) -> &'static str`
  - `world::chunk::{CHUNK_SIZE, CHUNK_VOLUME, ChunkPos, ChunkStorage, Chunk, world_to_chunk_and_local}`
  - `Chunk::new_uniform(block: BlockId) -> Self`, `Chunk::get(&self, x: usize, y: usize, z: usize) -> BlockId`, `Chunk::set(&mut self, x: usize, y: usize, z: usize, block: BlockId)`, `Chunk::try_compress(&mut self) -> bool`, `Chunk::is_uniform(&self) -> Option<BlockId>`

- [ ] **Step 1: Scaffold `Cargo.toml`, `src/lib.rs`, `src/main.rs`, and `src/world/` stubs with failing tests in `src/world/chunk.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    #[test]
    fn test_negative_world_coordinates() {
        assert_eq!(world_to_chunk_and_local(0, 32, -1), (ChunkPos::new(0, 1, -1), (0, 0, 31)));
        assert_eq!(world_to_chunk_and_local(-32, -33, 63), (ChunkPos::new(-1, -2, 1), (0, 31, 31)));
    }

    #[test]
    fn test_chunk_uniform_promotion_and_compression() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        assert_eq!(chunk.is_uniform(), Some(BlockId::Air));
        chunk.set(3, 4, 5, BlockId::Stone);
        assert_eq!(chunk.is_uniform(), None);
        assert_eq!(chunk.get(3, 4, 5), BlockId::Stone);
        assert_eq!(chunk.get(0, 0, 0), BlockId::Air);
        chunk.set(3, 4, 5, BlockId::Air);
        assert!(chunk.try_compress());
        assert_eq!(chunk.is_uniform(), Some(BlockId::Air));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib world::chunk::tests`
Expected: FAIL (`todo!()` or unimplemented functions)

- [ ] **Step 3: Implement `BlockId` in `src/world/block.rs` and `Chunk` / `world_to_chunk_and_local` in `src/world/chunk.rs`**

Implement `idx = x + (z << 5) + (y << 10)`, `div_euclid(32)` / `rem_euclid(32)` in `world_to_chunk_and_local(wx: i32, wy: i32, wz: i32) -> (ChunkPos, (usize, usize, usize))`, and texture layer indices (`0..14`) for `BlockId::face_tex_layer(self, face_dir: u8) -> u8` (`face_dir`: `0=+X, 1=-X, 2=+Y, 3=-Y, 4=+Z, 5=-Z`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib world::chunk::tests`
Expected: PASS (2 tests passed)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/
git commit -m "feat(world): add BlockId and compact 32x32x32 Chunk storage"
```

---

### Task 2: Deterministic Terrain Generator & 64-Bit Binary Greedy Mesher with AO

**Files:**
- Create: `src/world/terrain.rs`
- Create: `src/mesh/mod.rs`
- Create: `src/mesh/quad.rs`
- Create: `src/mesh/greedy.rs`
- Modify: `src/world/mod.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `BlockId`, `Chunk`, `ChunkPos`, `CHUNK_SIZE`
- Produces:
  - `world::terrain::generate_chunk(pos: ChunkPos) -> Chunk`
  - `mesh::quad::{FaceDir, PackedQuad, ChunkMesh}`:
    - `PackedQuad::new(x: u32, y: u32, z: u32, w: u32, h: u32, face_dir: u32, tex_layer: u8, ao_packed: u8) -> Self`
    - `PackedQuad::unpack(self) -> (u32, u32, u32, u32, u32, u32, u8, u8)`
    - `ChunkMesh { pub buckets: [Vec<PackedQuad>; 6] }` with `ChunkMesh::is_empty(&self) -> bool`, `ChunkMesh::total_quads(&self) -> usize`
  - `mesh::greedy::{PaddedChunk, mesh_chunk}`:
    - `PaddedChunk::from_center_only(chunk: &Chunk) -> Self`
    - `PaddedChunk::set(&mut self, px: usize, py: usize, pz: usize, block: BlockId)` (where `px, py, pz ∈ 0..34`)
    - `mesh_chunk(padded: &PaddedChunk) -> ChunkMesh`

- [ ] **Step 1: Write failing tests in `src/mesh/greedy.rs` and `src/world/terrain.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;
    use crate::world::chunk::Chunk;

    #[test]
    fn test_single_voxel_produces_six_unit_quads() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(5, 6, 7, BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 6);
        for dir in 0..6 {
            assert_eq!(mesh.buckets[dir].len(), 1);
            let (_, _, _, w, h, fdir, _, _) = mesh.buckets[dir][0].unpack();
            assert_eq!((w, h, fdir), (1, 1, dir as u32));
        }
    }

    #[test]
    fn test_full_solid_chunk_greedy_merges_to_six_quads() {
        let chunk = Chunk::new_uniform(BlockId::Stone);
        let padded = PaddedChunk::from_center_only(&chunk);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.total_quads(), 6);
        for dir in 0..6 {
            assert_eq!(mesh.buckets[dir].len(), 1);
            let (_, _, _, w, h, _, _, _) = mesh.buckets[dir][0].unpack();
            assert_eq!((w, h), (32, 32));
        }
    }

    #[test]
    fn test_halo_culls_chunk_boundary_face() {
        let mut chunk = Chunk::new_uniform(BlockId::Air);
        chunk.set(31, 10, 10, BlockId::Stone);
        let mut padded = PaddedChunk::from_center_only(&chunk);
        // Set +X neighbor halo at px = 33, py = 11, pz = 11 (corresponding to local 31, 10, 10 + 1 in X)
        padded.set(33, 11, 11, BlockId::Stone);
        let mesh = mesh_chunk(&padded);
        assert_eq!(mesh.buckets[0].len(), 0); // +X face culled by neighbor halo
        assert_eq!(mesh.total_quads(), 5);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib mesh::greedy::tests`
Expected: FAIL

- [ ] **Step 3: Implement `PackedQuad` in `src/mesh/quad.rs`, `mesh_chunk` in `src/mesh/greedy.rs`, and `generate_chunk` in `src/world/terrain.rs`**

In `mesh_chunk`, for each face direction `0..6` (`+X, -X, +Y, -Y, +Z, -Z`):
- Build 64-bit solid bitmasks along the axis (`0..34`) and compute visible face masks using `(col_mask & !(col_mask >> 1)) >> 1 & 0xFFFF_FFFF` for positive faces and `(col_mask & !(col_mask << 1)) >> 1 & 0xFFFF_FFFF` for negative faces.
- For each visible face at `(u, v) ∈ 0..32 × 0..32`, compute the 4-corner 8-bit `ao_packed` from the 8 neighbor voxels in the adjacent air slice (`ao_corner(s1, s2, c) = if s1 && s2 { 0 } else { 3 - (s1 as u8 + s2 as u8 + c as u8) }`) and combine with `tex_layer` into a `u16` key.
- Greedily expand width `w` along `u` and height `h` along `v` where cells are visible and share the identical `u16` `(tex_layer, ao_packed)` key, then emit `PackedQuad::new(x, y, z, w, h, face_dir, tex_layer, ao_packed)`.
- In `src/world/terrain.rs`, implement deterministic value/gradient FBM 2D noise heightmap (`y ∈ 8..96`), bedrock at `y == -32`, stone below `height - 4`, dirt/sand/grass/snow surface by elevation, and procedural oak trees (`OakLog` + `OakLeaves`) on grass plains.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/
git commit -m "feat(mesh,terrain): add 64-bit binary greedy mesher with AO and terrain generator"
```

---

### Task 3: World Streaming Manager & Background Worker Pipeline

**Files:**
- Create: `src/world/manager.rs`
- Modify: `src/world/mod.rs`

**Interfaces:**
- Consumes: `BlockId`, `Chunk`, `ChunkPos`, `world_to_chunk_and_local`, `generate_chunk`, `PaddedChunk`, `mesh_chunk`, `ChunkMesh`
- Produces:
  - `world::manager::WorldManager`:
    - `WorldManager::new(render_distance: i32) -> Self`
    - `WorldManager::get_block(&self, wx: i32, wy: i32, wz: i32) -> BlockId`
    - `WorldManager::set_block_immediate(&mut self, wx: i32, wy: i32, wz: i32, block: BlockId) -> Vec<(ChunkPos, ChunkMesh)>`
    - `WorldManager::build_padded_chunk(&self, pos: ChunkPos) -> Option<PaddedChunk>`
    - `WorldManager::update_streaming(&mut self, center_chunk: ChunkPos) -> (Vec<(ChunkPos, ChunkMesh)>, Vec<ChunkPos>)` (returns newly completed meshes and unloaded chunk positions)
    - `WorldManager::loaded_chunk_count(&self) -> usize`, `WorldManager::render_distance(&self) -> i32`, `WorldManager::set_render_distance(&mut self, rd: i32)`

- [ ] **Step 1: Write failing tests in `src/world/manager.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    #[test]
    fn test_boundary_block_edit_marks_neighbor_dirty() {
        let mut mgr = WorldManager::new(2);
        mgr.insert_chunk_for_test(ChunkPos::new(0, 0, 0), Chunk::new_uniform(BlockId::Stone));
        mgr.insert_chunk_for_test(ChunkPos::new(-1, 0, 0), Chunk::new_uniform(BlockId::Stone));
        for dir in [
            ChunkPos::new(1, 0, 0), ChunkPos::new(0, 1, 0), ChunkPos::new(0, -1, 0),
            ChunkPos::new(0, 0, 1), ChunkPos::new(0, 0, -1),
            ChunkPos::new(-2, 0, 0), ChunkPos::new(-1, 1, 0), ChunkPos::new(-1, -1, 0),
            ChunkPos::new(-1, 0, 1), ChunkPos::new(-1, 0, -1),
        ] {
            mgr.insert_chunk_for_test(dir, Chunk::new_uniform(BlockId::Air));
        }

        // Breaking block at wx = 0 (local x = 0 of chunk 0,0,0) borders chunk (-1,0,0) at local x = 31
        let remeshed = mgr.set_block_immediate(0, 5, 5, BlockId::Air);
        let positions: Vec<ChunkPos> = remeshed.iter().map(|(p, _)| *p).collect();
        assert!(positions.contains(&ChunkPos::new(0, 0, 0)));
        assert!(positions.contains(&ChunkPos::new(-1, 0, 0)));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib world::manager::tests`
Expected: FAIL

- [ ] **Step 3: Implement `WorldManager` in `src/world/manager.rs`**

Use `rustc_hash::{FxHashMap, FxHashSet}` for active chunks and pending generation/meshing sets, and `crossbeam_channel::unbounded` + `rayon::spawn` for background chunk generation and meshing. Ensure `set_block_immediate` updates the target chunk and synchronously re-meshes it plus any neighbor chunk when `lx == 0`, `lx == 31`, `ly == 0`, `ly == 31`, `lz == 0`, or `lz == 31`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib world::manager::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/world/
git commit -m "feat(world): add multithreaded WorldManager streaming and immediate border remeshing"
```

---

### Task 4: First-Person Camera, Frustum Culling, DDA Voxel Raycast & Player Physics

**Files:**
- Create: `src/player/mod.rs`
- Create: `src/player/camera.rs`
- Create: `src/player/raycast.rs`
- Create: `src/player/physics.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `BlockId`, `ChunkPos`, `CHUNK_SIZE`
- Produces:
  - `player::camera::{Camera, Frustum}`:
    - `Camera::view_proj(&self, aspect: f32) -> glam::Mat4`
    - `Frustum::from_view_proj(vp: &glam::Mat4) -> Self`
    - `Frustum::intersects_aabb(&self, min: glam::Vec3, max: glam::Vec3) -> bool`
    - `visible_face_mask(camera_pos: glam::Vec3, chunk_min: glam::Vec3, chunk_max: glam::Vec3) -> u8`
  - `player::raycast::{RaycastHit, raycast_voxels}`:
    - `raycast_voxels<F: Fn(i32, i32, i32) -> BlockId>(origin: glam::Vec3, dir: glam::Vec3, max_dist: f32, get_block: F) -> Option<RaycastHit>`
    - `RaycastHit { pub block_pos: glam::IVec3, pub adjacent_pos: glam::IVec3, pub normal: glam::IVec3, pub distance: f32 }`
  - `player::physics::{Aabb, Player, InputState}`:
    - `Aabb::intersects_block(&self, bx: i32, by: i32, bz: i32) -> bool`
    - `Player::aabb(&self) -> Aabb`, `Player::eye_position(&self) -> glam::Vec3`
    - `Player::update<F: Fn(i32, i32, i32) -> BlockId>(&mut self, dt: f32, input: &InputState, get_block: F)`

- [ ] **Step 1: Write failing tests in `src/player/raycast.rs` and `src/player/physics.rs`**

```rust
#[test]
fn test_raycast_axis_aligned_zero_components() {
    let hit = raycast_voxels(
        glam::Vec3::new(2.5, 5.2, 2.5),
        glam::Vec3::new(0.0, -1.0, 0.0),
        8.0,
        |x, y, z| if x == 2 && y == 3 && z == 2 { BlockId::Stone } else { BlockId::Air },
    )
    .expect("should hit block below");
    assert_eq!(hit.block_pos, glam::IVec3::new(2, 3, 2));
    assert_eq!(hit.adjacent_pos, glam::IVec3::new(2, 4, 2));
    assert_eq!(hit.normal, glam::IVec3::new(0, 1, 0));
}

#[test]
fn test_cannot_place_block_overlapping_player_aabb() {
    let player = Player::new(glam::Vec3::new(0.5, 10.0, 0.5));
    let aabb = player.aabb();
    assert!(aabb.intersects_block(0, 10, 0));
    assert!(aabb.intersects_block(0, 11, 0));
    assert!(!aabb.intersects_block(0, 9, 0));
    assert!(!aabb.intersects_block(2, 10, 0));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib player::`
Expected: FAIL

- [ ] **Step 3: Implement `camera.rs`, `raycast.rs`, and `physics.rs`**

- In `camera.rs`: Gribb-Hartmann 6-plane frustum extraction from `Mat4` (with `wgpu` `[0, 1]` depth clip space) and `visible_face_mask` checking `camera_pos` against `chunk_min` / `chunk_max` for each of the 6 `FaceDir` bits (`0..6`).
- In `raycast.rs`: Amanatides-Woo 3D DDA stepping with `if dir[i].abs() < 1e-8 { f32::INFINITY } else { (1.0 / dir[i]).abs() }`.
- In `physics.rs`: Player dimensions `width = 0.6`, `height = 1.8`, `eye_height = 1.62`. Support both walking mode (gravity `-28.0 m/s²`, jump velocity `9.0 m/s`, independent Y then X then Z swept/stepped voxel AABB resolution) and fly mode (`F` toggle).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS (all unit tests in `world`, `mesh`, and `player` pass)

- [ ] **Step 5: Commit**

```bash
git add src/player/ src/lib.rs
git commit -m "feat(player): add camera frustum culling, DDA raycaster, and AABB voxel physics"
```

---

### Task 5: `wgpu` Rendering Pipeline, Procedural Texture Array, HUD & Event Loop

**Files:**
- Create: `src/render/mod.rs`
- Create: `src/render/context.rs`
- Create: `src/render/texture.rs`
- Create: `src/render/buffer.rs`
- Create: `src/render/pipeline.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `WorldManager`, `ChunkPos`, `ChunkMesh`, `PackedQuad`, `BlockId`, `Camera`, `Frustum`, `visible_face_mask`, `Player`, `InputState`, `raycast_voxels`
- Produces:
  - `render::texture::create_block_texture_array(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler)`
  - `render::buffer::{GpuChunk, ChunkRenderStore}` with `upload_mesh`, `remove_chunk`, `total_vram_bytes`
  - `render::pipeline::VoxelRenderer` with WGSL voxel shader (instanced 8-byte quad expansion + AO diagonal flip + distance fog) and line/UI overlay pass (3D target block wireframe, center crosshair, and 9-slot block hotbar)
  - `src/main.rs` full `winit` interactive game loop

- [ ] **Step 1: Write unit test for procedural texture generation and WGSL shader source validation in `src/render/texture.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_procedural_texture_layers_non_empty() {
        let layers = generate_all_texture_layers_rgba();
        assert_eq!(layers.len(), 14);
        for layer in &layers {
            assert_eq!(layer.len(), 16 * 16 * 4);
            // Verify alpha is opaque (255) for all solid block textures
            assert!(layer.chunks_exact(4).all(|px| px[3] == 255));
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib render::texture::tests`
Expected: FAIL

- [ ] **Step 3: Implement `src/render/texture.rs`, `src/render/context.rs`, `src/render/buffer.rs`, `src/render/pipeline.rs`, and `src/main.rs`**

- `texture.rs`: Generate 14 distinct 16×16 RGBA pixel-art textures (grass top, grass side, dirt, stone, cobblestone, sand, oak log top, oak log side, oak leaves, oak planks, brick, snow, bedrock, snow side) with mip levels down to 1×1.
- `buffer.rs` & `pipeline.rs`:
  - Voxel shader binds `Group 0` (camera `view_proj`, `camera_pos`, `fog_color`, `fog_start_end`, plus `Texture2DArray` & `Sampler`) and `Group 1` (per-chunk 16-byte uniform `chunk_origin: [f32; 4]`).
  - Vertex buffer slot 0 (`VertexStepMode::Instance`, stride 8 bytes, `Uint32x2`) feeds `PackedQuad`. Draw call `render_pass.draw(0..6, bucket_range)` expands the 2 triangles in WGSL, flipping the diagonal when `ao00 + ao11 > ao10 + ao01`.
  - Overlay pipeline draws the 12-edge wireframe cube around the currently targeted block (`RaycastHit`), the center crosshair, and the 9-slot block hotbar at the bottom of the screen indicating the active block slot.
- `main.rs`: Connect `winit` window/device events (`WASD`, `Space`, `Shift`, `Ctrl`, `F` fly toggle, `1..9` / scroll wheel hotbar, `[` / `]` render distance, Left-click break, Right-click place with `player.aabb().intersects_block` guard, `Esc` cursor grab) and update the window title once per 0.25s with live `FPS | frame ms | Chunks (visible/loaded) | VRAM KB | Pos | Block | RD`.

- [ ] **Step 4: Run `cargo test` and `cargo build --release` to verify compilation and tests**

Run: `cargo test && cargo build --release`
Expected: PASS with 0 errors and release binary produced at `target/release/minecraft-clone`

- [ ] **Step 5: Commit**

```bash
git add src/ Cargo.toml Cargo.lock
git commit -m "feat(render,app): add wgpu instanced quad renderer, procedural textures, HUD, and game loop"
```
