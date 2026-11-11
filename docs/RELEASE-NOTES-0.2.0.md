# OpenCraft 0.2.0

OpenCraft 0.2.0 is a hardening release with **no new gameplay**. It makes the
game safer with untrusted input and cheaper on the main thread: every pack
input and every save read is now bounded before it is allocated or parsed,
streaming autosave runs encode and write on an off-thread worker so unloading
chunks no longer stalls the frame, world names that Windows reserves are
rejected instead of failing at save time, a typo'd `settings.toml` is preserved
as a readable `settings.toml.bak` instead of being overwritten, and the
demo/validation seams plus command adapters moved out of `main.rs` into library
modules so the binary entry point is half its former size. Save format, world
content, and gameplay behavior are unchanged.

## Downloads

| Platform | Archive |
| --- | --- |
| Linux (x86_64, glibc) | `opencraft-0.2.0-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) | `opencraft-0.2.0-x86_64-pc-windows-msvc.zip` |

Each archive is a portable folder: the `opencraft` binary for that platform,
the shipped `config/` and `worldgen/` trees, and `LICENSE`. It contains no
`saves/`, `packs/`, or `settings.toml`, so nothing of yours is overwritten.
These release notes live on the release page rather than inside the archive.

## Linux

1. Download `opencraft-0.2.0-x86_64-unknown-linux-gnu.tar.gz`.
2. Extract it: `tar xzf opencraft-0.2.0-x86_64-unknown-linux-gnu.tar.gz`.
3. From the extracted directory, run `./opencraft`.

The build needs ALSA, so install `libasound2` (Debian/Ubuntu:
`sudo apt install libasound2`) if the game reports a missing audio library.
The binary is built on the Ubuntu 24.04 GitHub Actions runner, so its glibc
floor is glibc 2.39 — a distribution with an older glibc cannot run it. Build
from source instead on such a system.

## Windows

1. Download `opencraft-0.2.0-x86_64-pc-windows-msvc.zip`.
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

## Building from source and further reading

The [README](../README.md) has the prerequisites and the `cargo build --release`
steps. For authoring:

- [Texture and content packs](TEXTURE-PACKS.md)
- [Worldgen prefabs and villages](WORLDGEN.md)
- [Save format and release notes](SAVE-NOTES.md)
