# Engine crates

This guide is for a contributor or reviewer who needs to work on the engine
crates without having lived through the M017 partition. After reading it you
should be able to add a neutral engine-crate API without crossing a game
boundary, run every crate gate, and tell which parts of the engine are still
game-side and may be considered for a later move.

The repository has two layers:

- The **game package** `opencraft` at the repository root owns the window,
  gameplay, world and content policy, save/pack/worldgen formats, and every
  player-facing rule.
- The **neutral engine crates** under `crates/` own reusable math, geometry,
  collision, movement, pathfinding and GPU mechanisms. They never depend on the
  game package and never see a game type.

## Workspace layout

| Path | Package | Version | Role |
| --- | --- | --- | --- |
| `Cargo.toml` (root) | `opencraft` | `0.2.2` at capture time | Game binary and library; stays at the repository root. |
| `crates/opencraft-core` | `opencraft-core` | `0.1.0` | Neutral primitives: block query, chunk-coordinate math, AABB/ray geometry, camera math and RGBA downsampling. |
| `crates/opencraft-gfx` | `opencraft-gfx` | `0.1.0` | GPU frame layer over a game-created surface: buffers, meshes, texture arrays, models, sky, weather and frame passes. |
| `crates/opencraft-physics` | `opencraft-physics` | `0.1.0` | AABB collision, raycast/sweep and the movement solver over `BlockQuery`. |
| `crates/opencraft-pathfinding` | `opencraft-pathfinding` | `0.1.0` | Steering primitives plus deterministic nav-grid A* over `BlockQuery`. |
| `crates/engine-reuse-proof` | `engine-reuse-proof` | `0.1.0` | External, device-free consumer of the four engine crates; proves reuse without the game. |

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
└── opencraft-pathfinding ──> opencraft-core

engine-reuse-proof
├── opencraft-core
├── opencraft-gfx
├── opencraft-physics
└── opencraft-pathfinding
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
- `engine-reuse-proof` depends only on the four engine crates plus `glam 0.27`.
  Its manifest has no direct GPU, windowing, serialization or temp-file
  dependency. Its `wgpu` edge is transitive through `opencraft-gfx` and is never
  called: the consumer is device-free.
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
- save, pack and worldgen formats.

Pathfinding A* is proven by crate tests and the external consumer but remains
unwired in mob behavior. Wiring it in is a future gameplay decision, not an
engine-crate detail. `SAVE_VERSION` stays 9 and the `.mcvx`, pack and worldgen
formats are frozen through M017.

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

- `scripts/check_engine_boundaries.sh` covers the four `opencraft-*` crates and
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

## M018 documented-only scope

Storage and chunk streaming, the pack/content registry, and
worldgen/prefab/village extraction remain game-side after M017. M018 may
consider moving them, but this document records intent and gate pressure only:
it defines no traits, no signatures and no format changes for that work.

A future move needs a recorded decision and its own boundary/gate work. It must
not change save, pack or worldgen formats by accident, and it must keep the
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
