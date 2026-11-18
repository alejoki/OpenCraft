# OpenCraft 0.3.0

OpenCraft 0.3.0 is a save-breaking release: the world file moves to the
sectioned **version 10** archive. Make a copy of every world you care about
before the first load with this build, because an upgraded file cannot be
opened by an older release. Version 1-9 saves load through the same reader and
are upgraded on the next save.

## Highlights

- **Save version 10 is a sectioned archive.** Each logical group has its own
  body and the archive directory locates it by id instead of parsing one flat
  payload. The content table is sorted and kind-tagged, each edited chunk gets
  a compact palette section, immature crops get their own sorted section, and
  the content metadata and config record move into fixed sections 1-9. A
  malformed, truncated, non-canonical or over-cap file still fails closed with
  a typed error, and the 256 MiB cap and temp-file-plus-rename atomic write are
  unchanged.
- **Version 1-9 worlds migrate forward.** They load through the frozen legacy
  and flat decoders; every block you edited is kept, unedited terrain is
  regenerated from the seed, and the next save writes the version 10 layout.
  The migration is byte-identical on the measured 10M-edit fixture, with zero
  unresolved content names.
- **Cold load is much faster on large worlds.** On the release fixture with
  10,000,000 edits across 20,000 chunks, the recorded v9 storage+session p50
  was 319.630 ms and the release target was half that, 159.815 ms. The
  measured same-run comparison is 350.182 ms for v9 and **51.520 ms for v10**
  p50, a 6.797x speedup and 32.2% of the target. Both directions report the
  same edit set and the same checksum, `0xd3ef88bdc7ae0ebc`. The first-area
  and full-verify guardrails also passed (95.638 ms against a 164.987 ms limit,
  and 52.287 ms against a 164.813 ms limit).
- **Three neutral engine crates.** `opencraft-storage`,
  `opencraft-content` and `opencraft-worldgen` now own the save archive,
  pack parsing/allocation/digest and the whole terrain-generation pass over
  raw `u16` ids. The game keeps its window, gameplay policy and adapters. The
  external device-free `engine-reuse-proof` consumer proves the archival codec,
  pack catalog, four-chunk terrain pass and a village plan without a GPU,
  window, filesystem, clock or thread; its deterministic report is 22 lines.
- **No gameplay change.** Movement, combat, crafting, survival, worldgen
  content and the config schema are the same as 0.2.2. Pack and worldgen
  formats are byte-identical: v10 changes only `.mcvx`. A* pathfinding remains
  unwired.
- **Budgets stay green.** Autosave frame p95 is 0.095 ms against the 16.6 ms
  gate, village streaming p95 is 2.488 ms with a 3.651 ms max against 8/16 ms,
  and the windowed village frame p95 is 7.289 ms against 16.6 ms.

## Downloads

| Platform | Archive |
| --- | --- |
| Linux (x86_64, glibc) | `opencraft-0.3.0-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) installer | `opencraft-0.3.0-x86_64-pc-windows-msvc-setup.exe` |
| Windows (x86_64) portable | `opencraft-0.3.0-x86_64-pc-windows-msvc.zip` |

The two archives are portable folders: the `opencraft` binary for that
platform, the shipped `config/` and `worldgen/` trees, and `LICENSE`. Neither
contains `saves/`, `packs/`, or `settings.toml`, so nothing of yours is
overwritten. The Windows setup exe installs the same files for the current
user. These release notes live on the release page rather than inside the
archives.

## Linux

1. Download `opencraft-0.3.0-x86_64-unknown-linux-gnu.tar.gz`.
2. Extract it: `tar xzf opencraft-0.3.0-x86_64-unknown-linux-gnu.tar.gz`.
3. From the extracted directory, run `./opencraft`.

The build needs ALSA, so install `libasound2` (Debian/Ubuntu:
`sudo apt install libasound2`) if the game reports a missing audio library.
The binary is built on the Ubuntu 24.04 GitHub Actions runner, so its glibc
floor is glibc 2.39 — a distribution with an older glibc cannot run it. Build
from source instead on such a system.

## Windows

1. Download `opencraft-0.3.0-x86_64-pc-windows-msvc-setup.exe` and run it.
2. The installer places OpenCraft in `%LOCALAPPDATA%\OpenCraft`, adds a Start
   Menu shortcut, and registers an uninstall entry under
   **Settings > Apps**. It installs for the current user and does not ask for
   administrator approval.
3. Launch OpenCraft from the Start Menu. Settings, saves, and packs stay in
   the install folder next to the game.

The portable `opencraft-0.3.0-x86_64-pc-windows-msvc.zip` is still published:
extract it and run `opencraft.exe` from the extracted folder.

The Windows build links the C runtime statically, so it does not need the
Visual C++ Redistributable. The build is unsigned, so the first launch of the
installer or the game shows a SmartScreen warning. Choose **More info**, then
**Run anyway**.

## Save compatibility

**Copy your worlds before loading them with this build.** The save format is
now **version 10**, and an upgraded file cannot be read by 0.2.x or any older
release.

- Version 1-9 saves load through the same reader. Every block you edited is
  kept, unedited terrain is regenerated, and the calendar/default-vitals rules
  for old saves are unchanged.
- The file is upgraded only when the world is next saved. Loading a world does
  not rewrite it by itself.
- Version 8 and version 10 files carry the save-local content table by name;
  well-formed names missing from the current pack stack remain deterministic
  placeholders instead of blocking the load.
- A version 9 or 10 file also carries the config schema and effective-value
  digest. A changed config or schema shows exactly one non-blocking notice
  after the world is open and playable.
- Malformed, truncated, wrong-magic, unknown-codec, non-canonical-directory,
  unknown-kind and over-cap input fails closed with a named error.

See [docs/SAVE-NOTES.md](SAVE-NOTES.md) for the full version 10 wire layout,
the migration rules and the measured cold-load evidence.

## First run and file layout

`config/` and `worldgen/` ship next to the binary inside the archive and are
read-only to the game. `saves/` and `settings.toml` are created in the
current working directory when you first save or change a setting, so run the
game from a directory you can write to. `packs/` is not created for you — make
it in the same working directory and put pack folders inside it if you want to
use texture or content packs.

## Known caveats

- The Windows binary is unsigned: SmartScreen will warn on first launch.
- The installer has no update path, and there is no package manager or
  auto-updater; the portable archives are extracted and run in place.
- There is no multiplayer or network play.
- The Linux binary requires glibc 2.39 or newer (the Ubuntu 24.04 build floor).
- Linux audio requires ALSA (`libasound2`); otherwise the game fails to start
  with a missing-library error.
- Version 10 is forward-only. Keep a copy of an old world if you might return
  to a 0.2.x build; copying it back after an upgrade is the only way to open
  it in an older release.

## Building from source and further reading

The [README](../README.md) has the prerequisites and the `cargo build --release`
steps. For authoring:

- [Texture and content packs](TEXTURE-PACKS.md)
- [Worldgen prefabs and villages](WORLDGEN.md)
- [Save format and release notes](SAVE-NOTES.md)
- [Creator configuration](CONFIG.md)
