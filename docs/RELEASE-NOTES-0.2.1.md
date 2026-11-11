# OpenCraft 0.2.1

OpenCraft 0.2.1 is a patch re-cut of the 0.2.0 hardening release with **no new
gameplay**. It re-cuts the 0.2.0 release from the milestone branch with three
remediation fixes that landed after that tag, and for the first time documents
the measured streaming-autosave budget:

- The Skeleton combat demo is deterministic again: the Skeleton settles on the
  ground before the scripted strike, its pose is pinned, and scripted-demo
  mouse-look is muted so the demo does not depend on the host cursor or frame
  timing.
- Content-pack paths that are not regular files — a directory, FIFO, socket or
  device named `*.mcvx` — are rejected with a typed error before any bytes are
  read, instead of hanging or failing obscurely.
- The reserved placeholder item icon layer (27) is now an opaque grey checker,
  so a count-one placeholder item is visible on the hotbar and in the
  inventory.

Save format, world content, and gameplay behavior are unchanged from 0.2.0.

## Downloads

| Platform | Archive |
| --- | --- |
| Linux (x86_64, glibc) | `opencraft-0.2.1-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) | `opencraft-0.2.1-x86_64-pc-windows-msvc.zip` |

Each archive is a portable folder: the `opencraft` binary for that platform,
the shipped `config/` and `worldgen/` trees, and `LICENSE`. It contains no
`saves/`, `packs/`, or `settings.toml`, so nothing of yours is overwritten.
These release notes live on the release page rather than inside the archive.

## Measured performance

Streaming autosave encodes and writes on an off-thread worker; unloading chunks
no longer stalls the frame. On the ~1M-edit world from the release-profile
budget test (`tests/autosave_budget.rs`), with 2,000 chunks and 30 dispatches
writing 120,965,580 bytes, the frame-thread work is p50 **2.816 ms**, p95
**4.530 ms**, max **4.596 ms** against the **16.6 ms** per-frame budget, with
worker p95 **44.897 ms**.

The ~10M-edit stress case (20,000 chunks, 5 dispatches, 201,600,930 bytes
written) reports frame p95 **36.642 ms** (p50 35.839 ms) and worker p95
**380.73 ms** (380.726 ms). That case is reported, not gated, because at that
scale the snapshot clone still runs on the frame thread, so a frame can exceed
one frame budget; it is a stress figure, not a promise.

The Playing frame also reuses one retained `Vec<MobVertex>` mob vertex buffer
(`build_mob_batch_into`) instead of allocating a fresh vertex vector every
frame. The allocating wrappers remain for tests and the demo, but the real
frame path no longer allocates per frame.

## Linux

1. Download `opencraft-0.2.1-x86_64-unknown-linux-gnu.tar.gz`.
2. Extract it: `tar xzf opencraft-0.2.1-x86_64-unknown-linux-gnu.tar.gz`.
3. From the extracted directory, run `./opencraft`.

The build needs ALSA, so install `libasound2` (Debian/Ubuntu:
`sudo apt install libasound2`) if the game reports a missing audio library.
The binary is built on the Ubuntu 24.04 GitHub Actions runner, so its glibc
floor is glibc 2.39 — a distribution with an older glibc cannot run it. Build
from source instead on such a system.

## Windows

1. Download `opencraft-0.2.1-x86_64-pc-windows-msvc.zip`.
2. Extract it, then run `opencraft.exe` from the extracted folder.

The build is unsigned, so the first launch shows a SmartScreen warning. Choose
**More info**, then **Run anyway**.

## Save compatibility

The save format stays **version 9**, exactly as in 0.1.0. Nothing needs to be
converted:

- Version 1-8 saves load through the same reader and are upgraded on the next
  save; every block you edited is kept and untouched terrain is regenerated.
- Version 9 saves round-trip unchanged, including game mode, vitals, chests,
  live mobs, the save-local content table, and the config schema and digest.

Make a copy of your world before the first load with a newer build anyway: an
upgraded file cannot be opened by an older release. See
[docs/SAVE-NOTES.md](SAVE-NOTES.md) for the full save reference.

## First run and file layout

`config/` and `worldgen/` ship next to the binary inside the archive and are
read-only to the game. `saves/` and `settings.toml` are created in the
current working directory when you first save or change a setting, so run the
game from a directory you can write to. `packs/` is not created for you — make
it in the same working directory and put pack folders inside it if you want to
use texture or content packs.

## Known caveats

- The Windows binary is unsigned: SmartScreen will warn on first launch.
- There is no installer, package, or auto-updater; both archives are portable
  folders you extract and run.
- There is no multiplayer or network play.
- The Linux binary requires glibc 2.39 or newer (the Ubuntu 24.04 build floor).
- Linux audio requires ALSA (`libasound2`); otherwise the game fails to start
  with a missing-library error.
- The 10M-edit autosave case is a reported stress figure, not a gated budget;
  at that scale a frame can take longer than one 16.6 ms frame.

## Building from source and further reading

The [README](../README.md) has the prerequisites and the `cargo build --release`
steps. For authoring:

- [Texture and content packs](TEXTURE-PACKS.md)
- [Worldgen prefabs and villages](WORLDGEN.md)
- [Save format and release notes](SAVE-NOTES.md)
