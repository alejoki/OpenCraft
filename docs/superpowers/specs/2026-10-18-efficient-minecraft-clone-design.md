# Ultra-Efficient Rust Minecraft Clone — Design Specification

**Date:** 2026-10-18
**Target Platform:** Native Desktop (Linux/Cross-platform, Rust + `winit` + `wgpu` on Vulkan/GL)
**Primary Optimization Goal:** Maximum FPS, large render distances (16–32+ chunk radius, 32³ chunks), and minimal RAM/VRAM usage.

---

## 1. Goals & Scope

### 1.1 Intended Outcome
Build a playable, high-performance voxel sandbox MVP in Rust that achieves hundreds of FPS at large view distances while keeping system RAM and GPU VRAM footprints minimal.

### 1.2 In-Scope Features (MVP)
- **Compact 32×32×32 Voxel Chunks:** 1-byte `BlockId` storage with single-byte uniform-chunk compression for empty (Air) and solid (deep underground) chunks.
- **Multithreaded Terrain Streaming:** Deterministic 2D/3D noise terrain generation with heightmap biomes (plains, hills, mountains, beaches/water level, trees, snow caps) on background `rayon` worker threads.
- **64-bit Binary Greedy Meshing + Ambient Occlusion:** Bitwise face culling (`col_mask & !(col_mask >> 1)`) and 2D greedy quad merging with per-vertex 4-level ambient occlusion.
- **8-Byte-Per-Quad GPU Pipeline (`wgpu`):** Instanced quad expansion in WGSL where each merged quad takes only 8 bytes (`[u32; 2]`) in VRAM and expands to 6 vertices in the vertex shader with automatic AO diagonal flipping and repeating UVs.
- **Frustum & Normal-Bucket Culling:** Per-chunk 6-plane frustum culling plus per-direction (`±X, ±Y, ±Z`) face bucket culling to skip back-facing chunk geometry on the CPU before issuing draw calls.
- **Procedural Pixel-Art Texture Array:** Startup-generated 16×16 `Texture2DArray` with mipmaps for all block faces (zero external asset files required).
- **First-Person Player & Voxel Interaction:**
  - Walk, sprint, jump with gravity and swept/axis-separated AABB collision (`0.6 × 1.8 × 0.6` m).
  - Toggleable fast free-fly mode (`F` key) for inspecting and benchmarking large render distances.
  - Amanatides-Woo 3D DDA voxel raycasting (`max_distance = 8.0` blocks) with 3D wireframe target box, instant block breaking (Left Click) and block placement (Right Click, guarded against player AABB intersection).
  - 9-slot block selection hotbar (`1–9` keys and mouse scroll wheel) + center crosshair + live performance telemetry in window title (FPS, frame time ms, visible/loaded chunks, VRAM quad buffer usage, player coordinates, render distance).

### 1.3 Out of Scope (MVP)
- Mobs, pathfinding, and combat AI
- Crafting, smelting, and multi-screen inventory UI
- Cellular fluid flow or redstone simulation
- Multiplayer networking

---

## 2. Module Architecture

```text
src/
├── main.rs          # Application entry point, winit event loop, frame timing, title telemetry
├── world/
│   ├── mod.rs       # Re-exports world types
│   ├── block.rs     # BlockId enum/u8, face texture layer mapping, opacity queries
│   ├── chunk.rs     # Chunk (32×32×32), ChunkStorage (Uniform vs Dense), coordinate math
│   ├── terrain.rs   # Deterministic noise terrain generator (height, biomes, trees)
│   └── manager.rs   # WorldManager: active chunk map, spiral loading, rayon task dispatch
├── mesh/
│   ├── mod.rs       # Re-exports meshing types
│   ├── quad.rs      # PackedQuad ([u32; 2], 8 bytes), FaceDir (0..6), ChunkMesh buckets
│   └── greedy.rs    # PaddedChunk (34³) snapshot & 64-bit Binary Greedy Mesher with AO
├── render/
│   ├── mod.rs       # Re-exports renderer
│   ├── context.rs   # wgpu Instance, Adapter, Device, Queue, Surface, Depth texture
│   ├── texture.rs   # Procedural 16×16 pixel-art Texture2DArray & mipmap generator
│   ├── buffer.rs    # Per-chunk GPU buffer allocation, frustum + face-bucket draw submission
│   └── pipeline.rs  # Voxel WGSL pipeline, wireframe target box pipeline, 2D HUD/hotbar pass
└── player/
    ├── mod.rs       # Re-exports player controller
    ├── camera.rs    # First-person yaw/pitch, view-projection matrix, 6 frustum planes
    ├── physics.rs   # Player state, AABB voxel collision, gravity/jump + fly movement
    └── raycast.rs   # 3D DDA raycast returning hit block position, normal, and distance
```

Each non-rendering module (`world`, `mesh`, `player`) is pure Rust data and math with zero dependency on `wgpu` or `winit`, allowing deterministic unit tests via `cargo test`.

---

## 3. Data Layout & Formats

### 3.1 Block Types (`src/world/block.rs`)
`BlockId` is a `#[repr(u8)]` enum:
- `0`: `Air` (transparent, non-solid)
- `1`: `Grass` (top: grass_top, bottom: dirt, sides: grass_side)
- `2`: `Dirt`
- `3`: `Stone`
- `4`: `Cobblestone`
- `5`: `Sand`
- `6`: `OakLog` (top/bottom: log_top, sides: log_side)
- `7`: `OakLeaves`
- `8`: `OakPlanks`
- `9`: `Brick`
- `10`: `Snow`
- `11`: `Bedrock`

### 3.2 Chunk Storage (`src/world/chunk.rs`)
- Dimensions: `CHUNK_SIZE = 32` (`32 × 32 × 32 = 32,768` voxels).
- Linear index formula within dense array: `idx = x + (z << 5) + (y << 10)` where `x, y, z ∈ 0..32`.
- Storage representation:
  ```rust
  pub enum ChunkStorage {
      Uniform(BlockId),
      Dense(Box<[BlockId; 32768]>),
  }
  ```
- Setting a voxel in a `Uniform(b)` chunk with a block `!= b` automatically promotes it to `Dense`. After edits or generation, if all 32,768 voxels match, `try_compress()` demotes `Dense` back to `Uniform(b)`.

### 3.3 Packed Quad Format (`src/mesh/quad.rs`)
Each greedy-merged quad is represented as `#[repr(C)] pub struct PackedQuad(pub [u32; 2]);` (8 bytes total):

- **`word0: u32`**:
  - Bits `0..5` (5 bits): `x` ∈ `0..31`
  - Bits `5..10` (5 bits): `y` ∈ `0..31`
  - Bits `10..15` (5 bits): `z` ∈ `0..31`
  - Bits `15..20` (5 bits): `w - 1` ∈ `0..31` (represents quad width `1..32`)
  - Bits `20..25` (5 bits): `h - 1` ∈ `0..31` (represents quad height `1..32`)
  - Bits `25..28` (3 bits): `face_dir` ∈ `0..5` (`0 = +X, 1 = -X, 2 = +Y, 3 = -Y, 4 = +Z, 5 = -Z`)
  - Bits `28..32` (4 bits): unused / reserved (`0`)
- **`word1: u32`**:
  - Bits `0..8` (8 bits): `tex_layer` ∈ `0..255` (layer index in `Texture2DArray`)
  - Bits `8..16` (8 bits): `ao_packed` — four 2-bit AO values (`0` = darkest/3 neighbors, `3` = brightest/0 neighbors) for corners `v00` (bits `8..10`), `v10` (bits `10..12`), `v01` (bits `12..14`), `v11` (bits `14..16`).
  - Bits `16..32` (16 bits): reserved (`0`).

---

## 4. Algorithms & Pipelines

### 4.1 Binary Greedy Meshing with AO (`src/mesh/greedy.rs`)
1. **Input:** A `PaddedChunk` (`34 × 34 × 34` blocks) containing the `32³` chunk at local coordinates `1..=32` and a 1-voxel halo at `0` and `33` copied from the 6 neighboring chunks.
2. **Early Exit:** If the `PaddedChunk` is entirely Air or entirely solid with no exposed faces inside `1..=32`, return an empty `ChunkMesh` immediately.
3. **Bitwise Face Culling:**
   - For each of the 3 axes (`X, Y, Z`) and each row `(u, v) ∈ 1..=32 × 1..=32`, construct a `u64` solidity bitmask along the axis (`bits 0..=33`).
   - Positive face mask (`+X, +Y, +Z`): `(col_mask & !(col_mask >> 1)) >> 1 & 0xFFFF_FFFF` (selects bits `1..=32` whose neighbor at `+1` is Air).
   - Negative face mask (`-X, -Y, -Z`): `(col_mask & !(col_mask << 1)) >> 1 & 0xFFFF_FFFF` (selects bits `1..=32` whose neighbor at `-1` is Air).
4. **AO Calculation & 2D Greedy Merge:**
   - For each active bit in the face mask, compute the 8-bit `ao_packed` value from the 8 neighbor voxels in the adjacent air slice using standard Minecraft corner AO (`if side1 && side2 { 0 } else { 3 - (side1 + side2 + corner) }`).
   - Combine `(tex_layer as u16) | ((ao_packed as u16) << 8)` into a 16-bit key per visible face cell in the 32×32 slice.
   - Sweep across the 32×32 slice using bitmasks: merge contiguous cells in `u` (width `w`) and then extend down in `v` (height `h`) wherever rows share the exact same 16-bit `(tex_layer, ao_packed)` key, clearing merged bits from the row bitmask.
   - Emit one `PackedQuad` into the corresponding `FaceDir` bucket (`0..6`).

### 4.2 GPU Rendering & Culling (`src/render/`)
1. **Chunk GPU Upload:**
   - Each non-empty `ChunkMesh` concatenates the 6 `FaceDir` quad vectors into a single `wgpu::Buffer` (`BufferUsages::VERTEX`) and records `[Range<u32>; 6]` instance ranges plus the chunk's world-space origin `[i32; 3]`.
   - Vertex Buffer Slot 0 (`VertexStepMode::Instance`) holds `PackedQuad` (`[u32; 2]`, 8 bytes), drawn with `instances = bucket_range.start .. bucket_range.end`.
   - Each `GpuChunk` caches a 16-byte uniform buffer and `BindGroup` containing `chunk_origin: [f32; 4]` created once at mesh upload time. Rendering a visible chunk binds its cached bind group (`set_bind_group(1, &chunk.bind_group, &[])`), binds its quad vertex buffer (`set_vertex_buffer(0, chunk.buffer.slice(..))`), and issues `draw(0..6, range)` calls for the visible face buckets.
2. **Frustum & Normal-Bucket Culling:**
   - Extract 6 frustum planes from `view_proj` matrix each frame.
   - Test each `GpuChunk`'s world AABB (`[origin, origin + 32.0]`) against the 6 planes.
   - For each chunk inside the frustum, check camera position relative to the chunk AABB:
     - Draw `+X` bucket only if `camera.x > chunk_min.x`
     - Draw `-X` bucket only if `camera.x < chunk_max.x`
     - Draw `+Y` bucket only if `camera.y > chunk_min.y`
     - Draw `-Y` bucket only if `camera.y < chunk_max.y`
     - Draw `+Z` bucket only if `camera.z > chunk_min.z`
     - Draw `-Z` bucket only if `camera.z < chunk_max.z`
3. **WGSL Vertex & Fragment Shader:**
   - Unpacks `word0` and `word1`.
   - Selects corner `(u_off, v_off)` for `@builtin(vertex_index)` ∈ `0..6`, flipping the quad's triangle diagonal when `ao00 + ao11 > ao10 + ao01` so ambient occlusion interpolates smoothly without anisotropy artifacts.
   - Computes world position, view distance, directional sun lighting (`+Y = 1.0`, `±X = 0.8`, `±Z = 0.7`, `-Y = 0.5`), AO multiplier (`0.45 .. 1.0`), and smooth distance fog blending into the sky color near the render distance boundary.

### 4.3 World Streaming & Worker Pool (`src/world/manager.rs`)
- Vertical world bounds: `chunk_y ∈ -1..=4` (`y ∈ -32..160`, 6 vertical chunks per column).
- Horizontal streaming: Spiral order around the player's `(chunk_x, chunk_z)` within `render_distance` (default `16` chunks = `512` blocks radius; adjustable via `[` and `]` keys between `4` and `36`).
- Two background task types dispatched via `rayon` and returned over `crossbeam_channel`:
  1. **Chunk Generation:** Generates `32³` terrain chunk and sends `(ChunkPos, Chunk)` to main thread.
  2. **Chunk Meshing:** Once a chunk and its 6 neighbors exist in `WorldManager`, snapshots a `PaddedChunk` (`34³`) and dispatches binary greedy meshing to `rayon`, returning `(ChunkPos, ChunkMesh)`.
- **Frame Budget:** Main thread processes up to 32 generated chunks and uploads up to 24 completed meshes per frame so frame times stay ultra-smooth during rapid movement.
- **Player Block Edits:** When the player breaks or places a block, `WorldManager::set_block` mutates the chunk immediately on the main thread and synchronously re-meshes the affected chunk (plus any neighbor if the edit was on `0` or `31`) in ~100 µs for zero-latency visual feedback.

---

## 5. Controls & User Interface

| Input | Action |
|-------|--------|
| `W / A / S / D` | Move forward / left / backward / right |
| `Mouse Move` | Look around (yaw / pitch) |
| `Space` | Jump (Walking mode) / Ascend (Fly mode) |
| `Left Shift` | Sprint (Walking mode) / Descend (Fly mode) |
| `Left Ctrl` | Fast speed boost in Fly mode |
| `F` | Toggle between Walking (gravity + collision) and Fly mode |
| `Left Mouse Button` | Break targeted block |
| `Right Mouse Button` | Place selected block on targeted block face |
| `1–9` or `Scroll Wheel` | Select block in 9-slot hotbar |
| `[` / `]` | Decrease / Increase chunk render distance (`4..=36`) |
| `Escape` | Toggle mouse cursor grab |

---

## 6. Error Handling & Testing Strategy

### 6.1 Error Handling
- Window resize / zero-dimension surface events safely skip rendering until non-zero dimensions are restored and reconfigure the `wgpu::Surface` and depth buffer.
- `wgpu::SurfaceError::Lost` or `Outdated` triggers an automatic surface reconfiguration; `OutOfMemory` logs and exits cleanly.

### 6.2 Automated Verification (`cargo test`)
1. **`world::chunk` tests:**
   - Verify `world_to_chunk_and_local` handles positive, zero, and negative coordinates accurately.
   - Verify `Chunk` starts `Uniform(BlockId::Air)`, promotes to `Dense` on single block edit, and compresses back to `Uniform` when filled uniformly.
2. **`mesh::greedy` tests:**
   - Single isolated solid block at `(5, 5, 5)` produces exactly 6 quads (1 per `FaceDir`, each `w=1, h=1`).
   - Completely solid `32×32×32` chunk surrounded by Air produces **exactly 6 quads total** (1 per `FaceDir`, each merged to `w=32, h=32`), proving 99.9% quad reduction over naive meshing (6 vs 6,144 quads).
   - Two adjacent blocks of the same type merge into a single `2×1` quad; two adjacent blocks of different types produce two `1×1` quads with their interior shared face culled.
3. **`player::raycast` & `player::physics` tests:**
   - Raycast along `-Y` hits ground block at expected coordinates and returns `+Y` placement normal.
   - Player AABB resting on ground does not fall through solid blocks and resolves horizontal wall collisions cleanly.
