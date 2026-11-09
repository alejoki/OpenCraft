# Worldgen authoring guide

This guide is for a contributor with the repository checked out who wants to
build a structure in game, capture it as a reusable prefab, validate it without
opening a window, and see it assembled into a village in a fresh world.

You will build -> mark -> capture -> validate -> generate. Every command below
runs the shipped binary and reads or writes the paths named here.

## 1. What authored worldgen is

Authored worldgen is repository-tracked content under two directories:

- `worldgen/prefabs/<name>.toml` — one structure prefab.
- `worldgen/villages/<name>.toml` — one village definition that names a hub
  prefab, biome allow-list, sizing knobs, and prefab pools.

The `worldgen/` tree sits beside `config/` at the repository root. It is not a
texture pack or a data-only content pack, it is not installed through the pack
stack, and it is not selected from the Packs menu. It is ordinary repository
content that is read from the process working directory.

A prefab is built from built-in blocks only. Palette values must be names of
built-in blocks; the prefab format deliberately has no mod namespace. Villages
are generated content: they are written into chunks while those chunks are
generated. They are not a new save schema, so the save version does not change,
no prefab or village name is written into a save, and neither enters the pack
content fingerprint or the save content table.

Because villages are generated content, a saved chunk is never retroactively
re-stamped. Existing worlds gain villages only in chunks they have not edited,
and a player edit always wins over generation.

Missing content is valid empty content: with no `worldgen/` tree the game
generates the same terrain as before M015. Broken content is fail-soft at load
time: it produces readable warnings and fewer or no villages, never a world
load failure. The validator described in section 5 is the fail-closed check you
run on purpose.

## 2. Authoring loop in game

Authoring is Creative only.

1. Build the structure with normal Creative controls.
2. Press **M** to place a socket marker on the targeted block face. The marker
   is an authoring aid: it is not in the Creative palette, places no item,
   drops nothing, and can be broken normally.
3. Press **P** with the crosshair on a block to mark corner A. Move and press
   **P** again on a block to mark corner B. Both corners are inclusive, and the
   order does not matter.
4. A name prompt opens, prefilled with a free name (`capture`, then
   `capture_2`, and so on). Type a name, then press **Enter** to save or
   **Esc** to cancel without writing anything.
5. The saved file is `worldgen/prefabs/<name>.toml`.

While the prompt is open it owns the mouse and keyboard. Gameplay does not
move, mine, place, or open a panel behind it. The name filter accepts
`a-z`, `0-9`, `_` and `-`; uppercase is lowercased, other characters are
ignored, and the length is capped at 64 characters. A refused save keeps the
prompt on screen and shows the reason on the same line, so you can fix the name
or the selection and press Enter again.

### Marker geometry rule

A marker becomes a socket whose direction is derived from the four horizontal
neighbours at the marker's own height. Exactly one horizontal axis must be
solid on one side and open on the other:

- solid west, open east -> **east**
- open west, solid east -> **west**
- solid north, open south -> **south**
- open north, solid south -> **north**

A neighbour outside the selected region counts as open and is never read. Zero
qualifying axes or two qualifying axes make the marker ambiguous. A plain
one-block-thick doorway is ambiguous because both horizontal axes are open;
add a threshold step inside the doorway so one side is solid, then capture
again. An ambiguous marker fails the whole capture, names its coordinate, and
writes nothing.

Each marker becomes one origin-relative `{position, direction}` socket and the
marker cell itself is written to the prefab as air. Sockets are sorted by
position when the file is saved.

## 3. Capture normalization and limits

A capture is one inclusive two-corner region, normalized from either press
order. The size is `max - min + 1` on each axis.

- **Size cap:** 32 blocks on each axis. A larger axis rejects the capture.
- **World height:** the selection must sit inside the valid block rows
  `-32..=159`. Rows outside that band reject the capture.
- **Loaded chunks:** every chunk touched by the selection must be loaded.
  The first missing chunk, in chunk `(y, z, x)` order, rejects the capture.
- **Name:** `a-z`, `0-9`, `_` and `-`, at most 64 characters. An existing file
  stem or an existing header-declared `name` is refused, including a
  differently named file that declares the same `name`.
- **Chests:** chest contents are not captured. A loot chest normalizes to a
  plain chest, so a captured chest is an empty chest.
- **Crops:** wheat stages 1, 2 and 3 normalize to wheat stage 0.
- **Everything else:** air and static water are preserved, and every other
  built-in block keeps its identity. Every cell in the region is materialized,
  air included, so the file round-trips exactly.
- **Modded blocks:** a block that is not built in cannot be represented by the
  prefab format, so it rejects the capture and names its coordinate instead of
  silently becoming air.
- **Origin:** always `[size_x / 2, 0, size_z / 2]` using integer division. The
  y origin stays 0 so a prefab's bottom layer stays on the selection's bottom
  row.

A successful capture writes exactly one file under `worldgen/prefabs/`. It is
serialized and re-parsed before the first byte reaches disk. A capture never
writes a save, config file, or pack file.

## 4. Prefab file reference

A prefab is one TOML document:

```toml
format = 1
name = "small_house"
origin = [2, 0, 2]
size = [5, 4, 5]

layers = [
  [ "ccccc", "ccccc", "ccccc", "ccccc", "ccccc" ],
  [ "L#.#L", "L...L", "L.$.L", "L...L", "L###L" ],
  [ "L#.#L", "L...L", "L...L", "L...L", "L###L" ],
  [ "L###L", "L###L", "L###L", "L###L", "L###L" ],
]

[palette]
"c" = "cobblestone"
"#" = "oak_planks"
"L" = "oak_log"
"$" = "chest"

[[sockets]]
position = [0, 1, -2]
direction = "north"
```

Reading the example:

- `format = 1` is the only accepted schema version.
- `name` is the stable prefab name used by village pools.
- `size = [5, 4, 5]` means 5 cells on x, 4 on y, 5 on z.
- `origin = [2, 0, 2]` is an in-grid pivot. Each component must be smaller
  than its `size` component. The pivot cell is the placement anchor; a
  prefab's coordinates are grid cells `0..size`.
- `layers` is the block grid, and it must appear **before** `[palette]`.
  TOML scopes every key after a table header to that table, so a `layers` key
  written after `[palette]` would be read as a palette entry.
- The outer `layers` array is y from bottom to top. Inside one layer there is
  one string per z row. Each character is one x cell along `+x`. `.` means air.
- The first layer is the floor, the next two layers are the walls, and the last
  layer is the roof. The `$` in row `z = 2` is the chest at grid
  `[2, 1, 2]`.
- `[palette]` maps one printable ASCII character, other than `.`, to one
  built-in block name. Palette values cannot be air.
- `[[sockets]]` is repeated once per socket. `position` is origin-relative, so
  it may be negative: this one is grid `[2, 1, 0]`, which is the north face at
  road level. `direction` is `north` (`-Z`), `south` (`+Z`), `east` (`+X`), or
  `west` (`-X`). Socket positions are unique and are sorted when serialized.
- Every grid cell must be present, including air. A wrong layer count, row
  count, or row length is rejected with the offending layer or row.

The other checked-in reference files are `plains_hub` (7x4x7 with four
sockets) and `wheat_farm` (a 7x2x7 field with one north socket). Use them as
syntax and sizing references.

## 5. Validating content

Validate prefabs without opening a window:

```bash
opencraft --validate-worldgen
opencraft --validate-worldgen path/to/content
```

The optional directory defaults to `worldgen`. The command is release-facing
and windowless: it does not start a world, window, GPU, audio engine, settings
file, or save.

Output is deterministic and filename-ordered:

```text
ok: prefab plains_hub
ok: prefab small_house
ok: prefab wheat_farm
validate-worldgen: 3 prefabs, 0 errors, 0 warnings
```

- One `ok:` line per accepted file, or one `error:` line per rejected file.
- The summary line is always
  `validate-worldgen: N prefabs, E errors, W warnings`. The warning count is
  currently always 0 and is reserved for future passes.
- Exit code 0 means no errors. Exit code 1 means at least one file was rejected.
- A missing or unreadable tree is valid empty content: the zero summary and
  exit 0.
- Only `.toml` regular files directly under `<root>/prefabs/` are read.
  Non-`.toml` files, directories, and nested directories are ignored.
- File names, not header names, determine order, so a duplicate header name
  keeps the sorted-first file and rejects the later one.
- Diagnostics always use the logical `worldgen/prefabs/<file>` label, however
  the root was spelled on the command line.

The stable per-file reasons include `invalid TOML`,
`size 33x4x5 exceeds cap 32x32x32`,
`duplicate prefab name '<name>'`,
`palette value for 'c' is unknown block 'moon_rock'`,
`palette key '.' is reserved for air`,
`socket position [x, y, z] is outside the prefab`,
`socket direction 'up' is unknown`,
`duplicate socket at [x, y, z]`, `layer 1 has 3 rows, expected 4`, and
`row 1/2 has length 4, expected 5`.

Each prefab file is capped at 1 MiB. A larger file is rejected whole.
`--validate-worldgen` audits prefabs; a village definition is checked when the
catalog loads (see section 6).

## 6. Village definitions

A village definition is one TOML document under `worldgen/villages/`. The
checked-in reference is:

```toml
format = 1
name = "plains"
biomes = ["plains", "forest"]
hub = "plains_hub"
spacing = 4
min_prefabs = 10
max_prefabs = 20
max_slope = 6

[pool]
house = ["small_house"]
farm = ["wheat_farm"]
well = []
junction = ["plains_hub"]
```

Fields:

- `format = 1`.
- `name` follows the same `a-z0-9_-`, 64-character rule as a prefab name and
  must be unique across the village files.
- `biomes` is a non-empty list. M015 supports `plains` and `forest`; any other
  biome name is rejected.
- `hub` must name a prefab that exists in the catalog.
- `spacing` is an integer in `1..=16` and sets the road length between linked
  prefabs.
- `min_prefabs` and `max_prefabs` are integers in `10..=20`, and the minimum may
  not exceed the maximum.
- `max_slope` is an integer in `0..=16`, the allowed difference between the
  highest and lowest sampled terrain column under one placement.
- `[pool]` lists catalog prefab names for `house`, `farm`, `well` and
  `junction` roles. Each is optional. An absent or empty `junction` defaults to
  the hub. At least one of `house`, `farm`, or `well` must be non-empty.

Every used prefab — the hub plus every kept pool entry — must have at least one
socket, and every socket must sit at grid `y = 1`,
`origin[1] + position[1] == 1`. That is the road-alignment invariant: the
socket cell is where a road meets the prefab.

A pool entry that names an unknown prefab is dropped with one warning while the
rest of the definition survives. That is the reduced-village path. A
structural problem instead rejects the whole definition with one reason.

### Adding a fresh capture to a village

After `P` capture writes `worldgen/prefabs/cottage.toml`:

1. Validate the prefab:
   ```bash
   opencraft --validate-worldgen
   ```
2. Add its name to a pool in `worldgen/villages/plains.toml`:
   ```toml
   [pool]
   house = ["small_house", "cottage"]
   ```
3. Load the catalog headlessly and look for warnings:
   ```bash
   opencraft --demo-village
   ```
   A broken village file prints
   `village_demo: warning worldgen/villages/<file>: <reason>`, and an unknown
   pool name is dropped. A clean catalog prints `diagnostics=0`.
4. Start a fresh world and travel into a village cell. Existing saved chunks
   are not retrofitted; use a new world or unedited terrain.

A village file is also capped at 1 MiB. A larger file is rejected whole.

## 7. How a village is chosen and built

Village placement is deterministic from the world seed. Moving the same seed
and the same content produces the same villages.

- The world is divided into 256-block cells. Each cell has one hash-derived
  anchor, inset from the cell edges so neighbouring villages cannot collide.
- A salted 1-in-4 gate decides whether a cell is eligible at all.
- The anchor column must be above the beach shelf and below the mountain stone
  band. Plains and forest are the only accepted biomes; ocean, beach and
  mountain sites are rejected.
- A site is rejected if its footprint or roads cross a ruin anchor, or if the
  whole village bounds sit within the 64-block spawn exclusion radius.
- The definition's hub is the root. The planner walks four wings in a fixed
  order (north, east, south, west), tries straight roads first and, for leaf
  connections, L-shaped roads.
- A child prefab is rotated by one of the four quarter turns so one of its
  sockets faces the parent socket. An unmatched side socket simply stops that
  branch; it never leaves a road ending at nothing.
- The base grade is sampled across the whole footprint. The highest sampled
  terrain becomes the prefab's bottom layer, and dirt foundations fill the gap
  below it. A placement whose sampled slope exceeds `max_slope` is rejected.
- Roads are cobblestone surfaces with dirt foundations, and two air cells are
  cleared above each road cell. A road that cannot reach the child grade,
  crosses a placement, or leaves the allowed band is rejected as a whole
  connection rather than committed as an orphan.
- The planner targets the definition's 10-20 prefab band and stops at
  `max_prefabs`. If terrain or socket matching rejects slots, it builds the
  subset that fits instead of failing the chunk.
- Village assembly is the very last pass over a generated chunk. It runs after
  pack surface patches, ruins, trees and raw terrain, so a village cell is
  never clipped or overwritten by an earlier pass.
- Generation is season-independent, and the terrain output with no village
  catalog installed is byte-identical to the pre-M015 output.

## 8. Playing and verifying

Existing worlds keep their saved chunks. Villages appear only in unedited
chunks that generate after the content is installed, and player edits always
win over anything generation would place.

These are the shipped headless proofs:

```bash
# Reads only worldgen/. Prints the catalog fingerprint, a pinned plan, and
# identical-cell proofs. It writes nothing and opens no window. Exit 0/1.
opencraft --demo-village

# Drives the real two-corner capture state machine in an isolated world.
# Writes <dir>/prefabs/capture.toml, re-parses it, validates the tree, then
# writes <dir>/villages/capture_demo.toml and proves socket capture plus a
# village/road placement from the captured prefab. Exit 0/1; a bare switch
# exits 2 with "error: --demo-capture requires a directory".
opencraft --demo-capture /tmp/authoring-demo
```

Use a scratch directory for `--demo-capture`; its only writes are below that
directory. Reusing the directory is supported: the next run picks a free prefab
name and rewrites the demo village definition.

The isolated real-binary checks are:

```bash
# Debug is the default.
bash scripts/village_smoke.sh

# Release binary.
SMOKE_PROFILE=release bash scripts/village_smoke.sh
```

Both run in temporary working directories with a copied `worldgen/` tree and
leave the repository untouched. The script builds the game and pins the
validator's clean, broken, oversize, duplicate and missing-tree cases; capture
success plus idempotence and failure legs; the deterministic `--demo-village`
digest; and the reduced-village degradation path.

The release streaming and frame budget harnesses are ignored tests, because
they must measure the release binary on an idle desktop session:

```bash
cargo test --locked --release --lib -- --ignored streaming_budget_villages --nocapture
cargo test --locked --release --test village_frame_budget -- --ignored --nocapture
```

The reference content is:

- `worldgen/prefabs/plains_hub.toml` — 7x4x7 hub with four sockets.
- `worldgen/prefabs/small_house.toml` — 5x4x5 house with one north socket and a
  chest.
- `worldgen/prefabs/wheat_farm.toml` — 7x2x7 field with one north socket.
- `worldgen/villages/plains.toml` — the plains/forest definition above.

### Degradation contract

Corrupt or missing content produces one readable warning per bad file and no or
fewer villages; it never blocks a world load. A bad pool entry drops that entry
while the definition survives. A capture failure keeps the prompt and the
session alive, writes nothing, and shows the reason on the prompt line.

## 9. Troubleshooting

| Reason text | Cause | Fix |
| --- | --- | --- |
| `chunk (x, y, z) is not loaded` | The selection reaches a chunk that is not resident. | Move closer, wait for streaming, or raise render distance so every covered chunk loads; then mark the corners again. |
| `selection is AxBxC, exceeds prefab cap 32x32x32` | At least one selection axis is longer than 32 blocks. | Shrink the region or split the build into multiple prefabs. |
| `selection is outside the world height (-32..=159)` | The selection reaches above or below the valid block rows. | Move the build into the `-32..=159` band. |
| `socket marker at [x, y, z] is ambiguous` | The marker has zero or two qualifying horizontal axes. A plain one-block-thick doorway has this problem. | Add a threshold step inside the doorway, or enclose the marker so exactly one horizontal side is solid, then capture again. |
| `prefab '<name>' already exists at worldgen/prefabs/<file>` | The name is already used by a file stem or a header-declared name. | Type a different name. The prompt prefill already suggests a free one. |
| `invalid prefab name: ...` | The name is empty, too long, or uses a character outside `a-z0-9_-`. | Use only lowercase letters, digits, `_` and `-`, at most 64 characters. |
| `duplicate prefab name '<name>'` | Two different files under `worldgen/prefabs/` declare the same `name`. | Rename one file or change its header `name`; the sorted-first file wins and the later one is rejected. |
| `prefab '<name>' socket <n> is not at grid y=1` | A village pool names a prefab whose socket is above or below road level. | Capture the prefab with the marker at the road-level opening, or edit the socket so `origin[1] + position[1] == 1`. |
| `pool '<key>' names unknown prefab '<name>'` | A village pool names a prefab that is missing or failed validation, so it is not in the catalog. | Fix the spelling or add the prefab, then run `opencraft --validate-worldgen` and `opencraft --demo-village` and confirm `diagnostics=0`. |
| `worldgen/prefabs/<file>: file is larger than 1 MiB` (or `worldgen/villages/<file>: ...`) | A prefab or village file exceeds the 1 MiB per-file cap. | Trim the content or split the structure; oversized files are rejected whole. |
| `worldgen/villages/<file>: invalid TOML` | A village definition has a syntax or schema error; other files still load. | Fix the file and rerun `opencraft --demo-village`; the warning names the logical path and the stable reason. |
