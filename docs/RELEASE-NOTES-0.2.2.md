# OpenCraft 0.2.2

OpenCraft 0.2.2 is a gameplay-tuning and bug-fix release. The save format is
unchanged, and the release is cut from the tuned `config/` tree now on `main`.

## Highlights

- **Mining has a bare-hand baseline.** `config/mining.toml` gains
  `tools.hand_speed_multiplier` (float, default `1.0`), and the tool-tier
  multipliers are now floats. The shipped tuning is hand `1.0`, wood `1.5`,
  stone `3`, iron `5`, so a matched tool stacks its multiplier on top of the
  hand rate.
- **Movement and combat tuned.** The checked-in config walks at `4.0` blocks/s
  (was `4.6`) and sprints at `7.0` (was `7.2`), with gentler swim rise
  (`swim_vertical_drag = 7.0`, `swim_rise_accel = 30.0`,
  `swim_rise_speed = 3.0`). A wooden sword deals `3` damage (was `4`) and an
  iron sword deals `7` (was `6`); reach stays `3` blocks.
- **New worlds start in Spring.** Day 0 is Spring for every seed, so a fresh
  Survival run begins in the mildest season. Weather still uses the seed, and
  the menu vista keeps its Summer look. Existing saves resume on their stored
  day and season unchanged.
- **Ruins survive Winter.** Ruin surface gates accept the seasonal Snow skin as
  well as Grass, so ruin shells, anchors, and loot chests no longer disappear
  when Winter arrives and reappear in Spring.
- **Passive animals survive Winter.** Pigs and cows can spawn on a lit Snow
  blanket, not only on Grass.
- **Animals move and flee like solid bodies.** Passive animals use the same
  collision-aware mover as hostile chases: they step up one block, walk down
  one block, refuse larger drops, and cannot run through walls. Knockback also
  checks the mob AABB, so a hit animal no longer clips through nearby blocks.
- **Chest screen aligned with the inventory.** The chest now has the same
  storage/hotbar separator and puts the hotbar on the bottom row. Chest and
  player slots draw real textured item icons, including the cursor ghost.
- **Cleaner inventory and crafting slots.** Coloured item background fills are
  gone from inventory, crafting grids, result previews, and chest slots; only
  the texture or icon is drawn. The inventory separator stops at the slot
  column, so it no longer cuts through the top row of the crafting grid.

The config schema is still `1`, now with **86** curated keys. `--check-config`
accepts the tuned tree, and the live multiplier invariant is
`hand <= wood <= stone <= iron`.

## Downloads

| Platform | Archive |
| --- | --- |
| Linux (x86_64, glibc) | `opencraft-0.2.2-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) installer | `opencraft-0.2.2-x86_64-pc-windows-msvc-setup.exe` |
| Windows (x86_64) portable | `opencraft-0.2.2-x86_64-pc-windows-msvc.zip` |

The two archives are portable folders: the `opencraft` binary for that
platform, the shipped `config/` and `worldgen/` trees, and `LICENSE`. Neither
contains `saves/`, `packs/`, or `settings.toml`, so nothing of yours is
overwritten. The Windows setup exe installs the same files for the current
user. These release notes live on the release page rather than inside the
archives.

## Linux

1. Download `opencraft-0.2.2-x86_64-unknown-linux-gnu.tar.gz`.
2. Extract it: `tar xzf opencraft-0.2.2-x86_64-unknown-linux-gnu.tar.gz`.
3. From the extracted directory, run `./opencraft`.

The build needs ALSA, so install `libasound2` (Debian/Ubuntu:
`sudo apt install libasound2`) if the game reports a missing audio library.
The binary is built on the Ubuntu 24.04 GitHub Actions runner, so its glibc
floor is glibc 2.39 — a distribution with an older glibc cannot run it. Build
from source instead on such a system.

## Windows

1. Download `opencraft-0.2.2-x86_64-pc-windows-msvc-setup.exe` and run it.
2. The installer places OpenCraft in `%LOCALAPPDATA%\OpenCraft`, adds a Start
   Menu shortcut, and registers an uninstall entry under
   **Settings > Apps**. It installs for the current user and does not ask for
   administrator approval.
3. Launch OpenCraft from the Start Menu. Settings, saves, and packs stay in
   the install folder next to the game.

The portable `opencraft-0.2.2-x86_64-pc-windows-msvc.zip` is still published:
extract it and run `opencraft.exe` from the extracted folder.

The Windows build links the C runtime statically, so it does not need the
Visual C++ Redistributable. The build is unsigned, so the first launch of the
installer or the game shows a SmartScreen warning. Choose **More info**, then
**Run anyway**.

## Save compatibility

The save format stays **version 9**, exactly as in 0.2.1. Nothing needs to be
converted:

- Version 1-8 saves load through the same reader and are upgraded on the next
  save; every block you edited is kept and untouched terrain is regenerated.
- Version 9 saves round-trip unchanged, including game mode, vitals, chests,
  live mobs, the save-local content table, and the config schema and digest.
- A save written under the 0.2.1 config tree records that tree's digest. On
  first load with the tuned 0.2.2 tree the game prints the one-shot
  `config changed since this world was saved` notice, then writes the new
  digest on the next save. The notice is informational; the world still loads.

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
- Streaming-autosave and rendering behavior are unchanged from 0.2.1; see the
  [0.2.1 release notes](RELEASE-NOTES-0.2.1.md) for the measured autosave
  budget and the 10M-edit stress figure.

## Building from source and further reading

The [README](../README.md) has the prerequisites and the `cargo build --release`
steps. For authoring:

- [Texture and content packs](TEXTURE-PACKS.md)
- [Worldgen prefabs and villages](WORLDGEN.md)
- [Save format and release notes](SAVE-NOTES.md)
- [Creator configuration](CONFIG.md)
