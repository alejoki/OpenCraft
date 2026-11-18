# Engine crates

This guide is for a contributor or reviewer who needs to work on the engine
crates without having lived through the M017 partition. After reading it you
should be able to add a neutral engine-crate API without crossing a game
boundary, run every crate gate, and tell which parts of the engine are still
game-side and may be considered for a later move.

The repository has two layers:

- The **game package** `opencraft` at the repository root owns the window,
  gameplay, world and content policy, the worldgen format and every
  player-facing rule. It consumes the crate-owned save archive
  (`opencraft-storage`), pack format (`opencraft-content`) and the whole
  terrain-generation pass (`opencraft-worldgen`).
- The **neutral engine crates** under `crates/` own reusable math, geometry,
  collision, movement, pathfinding, GPU mechanisms, save archives, pack
  parsing/validation and world generation. They never depend on the game
  package and never see a game type.

## Workspace layout

| Path | Package | Version | Role |
| --- | --- | --- | --- |
| `Cargo.toml` (root) | `opencraft` | `0.2.2` at capture time | Game binary and library; stays at the repository root. |
| `crates/opencraft-core` | `opencraft-core` | `0.1.0` | Neutral primitives: block query, chunk-coordinate math, AABB/ray geometry, camera math and RGBA downsampling. |
| `crates/opencraft-gfx` | `opencraft-gfx` | `0.1.0` | GPU frame layer over a game-created surface: buffers, meshes, texture arrays, models, sky, weather and frame passes. |
| `crates/opencraft-physics` | `opencraft-physics` | `0.1.0` | AABB collision, raycast/sweep and the movement solver over `BlockQuery`. |
| `crates/opencraft-pathfinding` | `opencraft-pathfinding` | `0.1.0` | Steering primitives plus deterministic nav-grid A* over `BlockQuery`. |
| `crates/opencraft-storage` | `opencraft-storage` | `0.1.0` | Neutral sectioned save archive, canonical chunk-section codec and bounded file I/O; depends on `opencraft-core` only. |
| `crates/opencraft-content` | `opencraft-content` | `0.1.0` | Neutral pack format and validation: `pack.toml` manifest and v2 content parsing, per-entry diagnostics, asset confinement/caps, PNG decode, 16x16 resample, deterministic allocation, patch precedence, placeholders, the FNV digest over raw u16 ids and the deterministic `--validate-pack` renderer. The game's `ContentRegistry` is a thin adapter. Depends on `opencraft-core`, `toml` and `png`. |
| `crates/opencraft-worldgen` | `opencraft-worldgen` | `0.1.0` | Neutral world generation: biome/height/ruin/tree/spawn queries, the loot vocabulary, the prefab codec/catalog/serializer behind a two-method `BlockNameResolver`, the village codec/catalog/fingerprint/plan/chunk-local emission, and the complete terrain-generation pass into a neutral `ChunkBuffer` — 3D cave noise and carving, coal/iron and pack ore veins, surface ruins with loot chests, oak trees, pack surface patches, sky shortcuts, the seasonal surface/leaf policy and village chunk assembly — all over raw `u16` ids. Depends on `opencraft-core`, `opencraft-content`, `toml` and `glam`, with `tempfile` as a dev-dependency only; no storage, GPU, windowing or game edge. |
| `crates/engine-reuse-proof` | `engine-reuse-proof` | `0.1.0` | External, device-free consumer of the engine crates; proves reuse without the game, including a device-free four-chunk terrain proof and a device-free village plan in its deterministic 22-line `ProofReport`. |

Workspace rules:

- `[package]` stays first in the root manifest; the root remains the `opencraft`
  package rather than a virtual workspace.
- Every member has its own `0.1.0` version and `publish = false`. Do not use
  `version.workspace`; the release version guard reads the root package version
  and independent member versions keep it meaningful.
- The workspace uses resolver 2 and one root `Cargo.lock` for all members.
- Plain cargo without `--workspace` selects only the root package. Tests,
  clippy and release gates that must cover members pass `--workspace`
  explicitly.
- `engine-reuse-proof` is a workspace member, not an example and not a
  dev-dependency of the game.

## Dependency DAG and rules

```text
opencraft (game)
├── opencraft-core
├── opencraft-gfx ───────> opencraft-core
├── opencraft-physics ───> opencraft-core
├── opencraft-pathfinding ──> opencraft-core
└── opencraft-storage ───> opencraft-core

opencraft-content ────> opencraft-core + toml + png
opencraft-worldgen ───> opencraft-core + opencraft-content + toml + glam

engine-reuse-proof
├── opencraft-content
├── opencraft-core
├── opencraft-gfx
├── opencraft-physics
├── opencraft-pathfinding
├── opencraft-storage
└── opencraft-worldgen
```

- `opencraft-core` depends on `glam` only. It has no GPU, windowing or game
  dependency.
- `opencraft-gfx` depends on `opencraft-core` plus `wgpu`, `pollster`,
  `bytemuck`, `rustc-hash` and `glam`. It has no direct `winit` or
  `raw-window-handle` dependency. It takes a game-created `wgpu::Instance` and
  `wgpu::Surface<'static>` plus host-supplied specifications, and owns the
  adapter, device, queue, surface configuration and depth attachment.
- `opencraft-physics` and `opencraft-pathfinding` depend on `opencraft-core`
  plus `glam`. They query blocks only through `BlockQuery -> BlockKind`.
- `opencraft-storage` depends on `opencraft-core` only. It has no game, GPU or
  windowing dependency and no `opencraft-content`, `opencraft-worldgen`, `toml`
  or `png` edge.
- `opencraft-content` depends on `opencraft-core`, `toml` and `png`, with
  `tempfile` as a dev-dependency only. It has no game, GPU, windowing, audio,
  storage or worldgen edge. Its file I/O is the bounded pack reader
  (`read_manifest_text`, `read_asset_bytes`, `decode_png_rgba`), which caps
  every read and rejects non-regular files.
- `opencraft-worldgen` depends on `opencraft-core`, `opencraft-content`, `toml`
  and `glam`, with `tempfile` as a dev-dependency only. It owns neutral
  biome/height/ruin/tree/spawn queries, the loot vocabulary, the prefab
  codec/catalog/serializer behind a two-method `BlockNameResolver`, the
  village codec/catalog/fingerprint/plan/chunk-local emission, and the whole
  terrain-generation pass over a neutral raw-`u16` `ChunkBuffer`: 3D cave
  noise and carving, coal/iron and pack ore veins, surface ruins with loot
  chests, oak trees, pack surface patches, sky shortcuts, the seasonal
  surface/leaf policy and village chunk assembly. It never imports a game
  `BlockId`/`ItemId`/`ContentRegistry`/`Chunk`/`WorldSession`, and it must not
  depend on `opencraft-storage`, `opencraft-gfx`, a windowing crate or the game.
- `engine-reuse-proof` depends only on the seven engine crates plus `glam 0.27`.
  Its manifest has no direct GPU, windowing, serialization or temp-file
  dependency. Its `wgpu` edge is transitive through `opencraft-gfx` and is never
  called: the consumer is device-free. Its `ProofReport` is 22 deterministic
  lines. The penultimate line reports the device-free four-chunk terrain proof
  (uniform/dense chunks, non-air cells and an FNV-1a digest over raw `u16`
  cells), and the last plans a village from embedded prefab/village TOML with
  no filesystem, clock, thread or GPU.
- No crate depends on the `opencraft` package, directly or transitively. The
  boundary gate enforces this with `cargo tree`.

New external dependencies are not added without a recorded decision. The
engine crates are intentionally small; adding a dependency for convenience
widens the public reuse surface.

## Neutral-API contract and game-side invariants

Physics and steering share one query contract: `BlockQuery -> BlockKind`. The
game implements it as `BlockIdQuery` over its own `BlockId` world, so no
`BlockId` enters a crate. `BlockKind` is the neutral solid/liquid/air
classification the crates actually branch on.

The gfx seam is host data in, mechanism inside:

- `TextureArraySpec` is data-only: label, size, layers and mip levels. The host
  creates the `wgpu::Texture` and drives uploads.
- `SkyParams` carries the per-frame day factor, fog and clear colors, the
  precomputed celestial uniform and the neutral precipitation value. The host's
  `daycycle` math produces it.
- `FrameView` carries the camera, render distance, highlight targets, selected
  slot and precomputed hotbar NDC rectangles.
- `SkyGeometry` carries the static sky billboard geometry supplied once by the
  host.

No `BlockId`, `ContentRegistry`, `Player`, `WorldSession` or `Mob` enters a
crate. These stay game-side:

- content identity and art: `TEX_SIZE`, `NUM_TEX_LAYERS`,
  `BLOCK_ARRAY_LAYERS`, `MIP_LEVELS`, mob layer bases, `LAYER_FILES`,
  procedural art and texture-pack resolution;
- `daycycle` celestial math;
- hotbar layout and the NDC rectangle computation;
- mob and arrow scene assembly, including model placement and animation;
- save-session policy, content-registry policy and worldgen formats.

Pathfinding A* is proven by crate tests and the external consumer but remains
unwired in mob behavior. Wiring it in is a future gameplay decision, not an
engine-crate detail. In M017, `SAVE_VERSION` stayed 9 and the `.mcvx`, pack and
worldgen formats were frozen through that milestone; M018/S02 has since moved
`.mcvx` to v10 through `opencraft-storage` (see the M018 scope below).

## Gate commands

Run these from the repository root:

```bash
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo fmt --all -- --check
bash scripts/check_engine_boundaries.sh
cargo test -p engine-reuse-proof --locked
cargo run -p engine-reuse-proof --locked
cargo metadata --locked --no-deps --format-version 1
cargo tree --locked -p <crate> --edges normal,build,dev
```

- `scripts/check_engine_boundaries.sh` covers every `opencraft-*` crate and
  `engine-reuse-proof`. It checks manifests for `publish = false` and
  `version.workspace`, checks dependency trees for a game-package edge and for
  forbidden GPU/windowing dependencies, runs an unconditional synthetic-tree
  self-test, and prints one `PASS:` line per crate plus the aggregate line.
- `scripts/ci_gate.sh` gates the latest GitHub Actions run for the current HEAD,
  waiting for the run and failing unless the Linux and Windows jobs pass. It is
  the final operational gate, not a local substitute for the commands above.
- `cargo tree` is the dependency proof of record: a crate that should be
  neutral must show only its allowed edges.

## M017 evidence at closeout

These are capture-time figures from the M017 closeout tree, not permanent
promises. Re-run the commands to refresh them.

| Signal | Capture-time result |
| --- | --- |
| `opencraft-core` | 22 unit tests + 1 doctest |
| `opencraft-gfx` | 70 tests |
| `opencraft-physics` | 20 tests |
| `opencraft-pathfinding` | 15 unit + 7 `nav_contract` + 3 doctests |
| `engine-reuse-proof` | 22 tests (1 lib, 6 gfx, 10 motion, 5 physics) and the deterministic 15-line `ProofReport` |
| Workspace | 31 `test result: ok` blocks, 2657 passed / 0 failed / 6 ignored (2679 including the explicit consumer step) |
| Boundary gate | Seven `PASS:` lines |
| Streaming budget | p95 <= 8 ms, max <= 16 ms |
| Autosave budget | frame p95 <= 16.6 ms |
| Village frame budget | frame p95 <= 16.6 ms |

The four release budget harnesses (two streaming, autosave and village frame)
are re-reported against their existing budgets. A capture-time figure moving is
not automatically a failure, but a budget crossing is a regression until proven
environmental.

## M018 member and format scope

The three M018 members now exist in the workspace. `opencraft-storage`'s
neutral archive container, canonical chunk-section codec and bounded file I/O
surface landed in S01. Save v10 is now live through that crate: the S02
sectioned writer/reader, sorted kind-tagged content table, per-chunk palette
sections, crop section, eager content-table resolution and forward migration
from v1-v9 are implemented, and the game's save I/O routes through the neutral
bounded file helpers. `opencraft-content` now owns the pack format: the S04
slice moved the manifest/v2 parsers, per-entry diagnostics, asset confinement
and caps, PNG decode, 16x16 resample and the deterministic `--validate-pack`
renderer into the crate, and the game consumes them through historical
module-path re-exports. S05 moved deterministic allocation, patch precedence,
placeholder allocation, the FNV-1a content digest and the changed-stack notice
into `opencraft_content::catalog` over neutral raw `u16` ids, leaving the game's
`ContentRegistry` as a thin adapter. S06 moved the neutral worldgen surface into
`opencraft-worldgen`: the biome/height/ruin/tree/spawn queries, the loot
vocabulary, the prefab codec/catalog/serializer behind a two-method
`BlockNameResolver`, and the village codec/catalog/fingerprint/plan/chunk-local
emission over raw `u16` ids. S07 then moved the whole terrain-generation pass:
3D cave noise and carving, coal/iron and pack ore veins, surface ruins with
loot chests, oak-tree emission, pack surface patches, sky shortcuts, the
seasonal surface/leaf policy and final village chunk assembly all run in the
crate against a neutral raw-`u16` `ChunkBuffer`. The game keeps `Chunk`, the
move-only `chunk_from_buffer` adapter, the `WorldGenContext` wrapper,
`Season`/clock policy, capture and all UI, and reaches the crate through
`src/world/terrain.rs`; `src/world/village.rs` places villages through the same
buffer boundary, and `src/world/{prefab,loot}.rs` remain re-export shims. Test
ownership follows that boundary: the crate holds the 53-test generation oracle
(all moved terrain tests plus the pinned baseline), while the game keeps the
mesh integration test and the adapter tests in `src/world/terrain.rs` (dense
buffer move, season mirror, default wrapper, authored rule/context order and
the pinned FNV triple run through the adapter). S08 owns the final closure.

The pack and worldgen formats stay frozen at their current versions. `.mcvx`
now writes v10 through `opencraft-storage`; any future save-format change needs
a recorded decision and its own boundary/gate work. The remaining extractions
must not change pack or worldgen formats by accident, and must keep the
neutral-API rules above intact.

## Failure and recovery

- A boundary `FAIL` means a crate reached for the game package, a GPU or
  windowing dependency, or a manifest guard was weakened. Fix the dependency,
  not the check.
- A requirements-checker `FAIL` means the registry and its executable checker
  drifted (record count, ownership, trace rows or validation commands).
  Restore lockstep; do not loosen the checker.
- A performance budget overrun means fix the regression or revert the change.
  Never re-pin a digest, witness or threshold without a decision and recorded
  evidence.

## Requirement and decision references

R089-R092 in `REQUIREMENTS-REGISTRY.md` define the workspace/boundary contract,
the external reuse proof, the no-regression closure surface and the scope
guardrails. D292-D309 in `.gsd/DECISIONS.md` record the layout, dependency DAG,
carve strategy, neutral seams, test-migration oracle and consumer proof shape.
