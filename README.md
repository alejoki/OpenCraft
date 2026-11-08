# OpenCraft

OpenCraft is an open-source voxel game engine built from scratch in Rust, inspired by Minecraft. It features a custom rendering engine using `wgpu` for modern, cross-platform graphics.

## ✨ Features

*   **Procedural World Generation**: Infinite, deterministic terrain generated based on a seed.
*   **Efficient Chunk Rendering**: Implements Greedy Meshing to optimize the rendering of voxel chunks, significantly reducing the number of vertices sent to the GPU.
*   **Dynamic Chunk Streaming**: Chunks are loaded, unloaded, and meshed in the background as the player moves through the world.
*   **World Persistence**: Save and load game worlds, including player state, chunk edits, and world seed. A filesystem-based world catalog allows for managing multiple saved games.
*   **Survival and Creative Modes**: New worlds default to Survival. Survival has 20 health, 20 hunger, and 300 air, shown as a hearts/hunger/air HUD; fall, drowning, and starvation damage; a Death screen with Respawn and Quit (respawn at the deterministic spawn with your inventory kept); hold-to-eat apples and bread; hoe-based wheat farming that pauses in Winter; 27-slot chests; and deterministic ruin loot. Creative keeps flying and building, is invulnerable, never hungry, never loses air, is never targeted or damaged by mobs, and one-shots any mob with its drops intact. The mode is chosen at creation and locked; the mob rule follows the *effective* mode, so Edit Background inherits Creative's immunity and one-shot swings while the background world keeps its stored mode.
*   **Mobs and Combat**: Four mobs share the world — Pig and Cow are passive, Zombie and Skeleton are hostile. Passives spawn on lit grass by day and flee when hit; hostiles spawn at night or in caves, with zombies chasing and meleeing while skeletons keep their distance and shoot arrows. Fight with your fists, a sword, or a shortbow, and kills drop straight into your inventory. Spawns respect a 24-block safe radius, a 32-mob global cap, and at most 4 mobs per 32-block region. The Death screen names the cause (Zombie or Skeleton arrow, alongside Fall, Drowning, and Starvation). Live mobs persist in version 9 saves with kind, position, facing, and health. Creative players are never targeted or damaged and one-shot any mob with drops intact. Mob art and audio are pack-overridable.
*   **Mob and Combat Audio**: Nine built-in generated sounds — block break, block place, mine-complete, UI click, mob idle, mob hurt, mob death, bow release, and arrow impact. A texture pack can override any of them per file; a missing file silently keeps the built-in sound and a corrupt one falls back with a menu warning. See the [texture pack authoring guide](docs/TEXTURE-PACKS.md#sounds).
*   **Player Physics and Controls**:
    *   Walk mode with collision detection, gravity, jumping, and sprinting.
    *   Fly mode for free movement and exploration.
    *   Raycasting for block breaking and placing.
*   **Customizable Settings**:
    *   Rebindable key controls for movement and actions.
    *   Adjustable mouse sensitivity, field of view (FOV), render distance, VSync, and fullscreen settings.
    *   Settings are saved to and loaded from a `settings.toml` file.
*   **Texture Packs**: Swap block and mob textures plus menu presentation with folder-based packs — a `pack.toml` plus optional PNGs, no code required. The texture array has 48 layers: 23 authorable block textures (the original 16 plus Farmland, four Wheat stages, Chest, and LootChest) and 13 authorable mob part textures, each with optional per-layer overrides; a pack that supplies only the original 16 block layers falls back per layer to the built-in procedural art. See the [texture pack authoring guide](docs/TEXTURE-PACKS.md).
*   **Data-Only Content Packs**: A `format_version = 2` pack adds blocks, items, food, custom tool tiers and tools, shaped recipes, per-field block patches, recipe toggles, and declarative ore or surface worldgen without writing code. Save format 8 stores content by name, so a removed pack's blocks and items survive as readable placeholders and resolve again when the pack returns; a changed stack reports a non-blocking notice, and `--validate-pack` checks a pack headlessly. See the [content pack authoring guide](docs/TEXTURE-PACKS.md#format-v2).
*   **Creator Tuning (dev builds)**: A curated six-file `config/` tree — `player.toml`, `survival.toml`, `combat.toml`, `world.toml`, `mining.toml`, and `render.toml` — covers movement/camera, survival, combat, world/weather/spawning, mining/farming, and render/UI. Edits apply live in a debug build through a polling watcher, a read-only F6 overlay shows every curated value's source and apply mode, and `--creator-trace` prints stable timestamp-free lines; `--check-config` audits a tree headlessly. Pack block, mob, and built-in item textures reload live, while recipe and other content edits set a clear restart-required boundary and apply on the next load of the same save. Save v9 records the effective config schema and digest so a changed config warns once without blocking the world. Release builds keep config and pack loading but expose none of the creator tools. See the [configuration guide](docs/CONFIG.md).
*   **Customizable Menu Background**: The panorama behind the main menu is its own saved world at `saves/background/background.mcvx`. Choose **Edit Background** on the main menu to build it with the normal controls; the menu resumes your saved vantage point, time of day, and season. A missing or unreadable file falls back to the built-in scenery and never blocks launch.
*   **Save Compatibility**: Loading a world from an older release keeps every block you edited, regenerates the terrain you never touched, and starts its calendar at day 0. Version 9 saves round-trip the game mode, health/hunger/air, every chest's contents, every live mob's kind, position, facing, and health, the save-local content names, and the effective config schema and digest. Version 1-8 saves load and gain the newer tails on the next save, and version 1-5 saves also load as Creative with full vitals. A version 8 save carries no config record, so it loads with no config notice; a changed config or schema shows exactly one non-blocking notice. Malformed mob, content, or config data is rejected rather than skipped. See the [save and release notes](docs/SAVE-NOTES.md).
*   **Comprehensive UI System**:
    *   Main Menu (including Edit Background), New Game, Load Game, and Pause screens.
    *   Chest screen (27 chest slots plus the 36-slot player inventory) and Death screen (Respawn and Quit).
    *   Searchable Creative palette (Creative mode, key **C**): lists every non-placeholder built-in and pack block, item, and tool; filters by name, display name, or namespace; and grants the full authored stack/durability into the hotbar. Placeholders never appear. Pack changes apply on the next world load because the session registry is frozen.
    *   In-game settings menu for live adjustments.
    *   UI logic is decoupled from rendering, built with `glyphon` for text rendering.

## 🐷 Mobs and Combat

Four mobs share the world: **Pig**, **Cow**, **Zombie**, and **Skeleton**. The first two are passive and the last two are hostile. Creative players are never targeted or damaged by them. The spawn and fight rules below describe Survival; ambient spawning itself runs in both modes.

### Where mobs come from

| Rule | Value |
| --- | --- |
| Safe radius | 24 blocks around the player and around the world spawn — no mob spawns inside it |
| Spawn ring | 24-48 blocks from the player |
| Despawn | Past 96 blocks from the player |
| Global cap | 32 live mobs |
| Regional cap | At most 4 mobs per 32-block region, counted across loaded and unloaded chunks |
| Passive spawn | Full sky light on a grass block, during the day |
| Hostile spawn | Sky light 7 or less at any hour, or full sky light at night |
| Daylight burn | A hostile on the lit surface loses 1 health every 0.5 s; cover or water at its feet stops the burn |

Spawn attempts are deterministic for a seed and continue while you play. A mob that is more than 96 blocks away is removed; living mobs inside that radius are saved with the world.

## 🛠️ Built With

*   **Language**: [Rust](https://www.rust-lang.org/)
*   **Graphics**: [wgpu](https://github.com/gfx-rs/wgpu) for modern, cross-platform graphics.
*   **Windowing**: [winit](https://github.com/rust-windowing/winit)
*   **Text Rendering**: [glyphon](https://github.com/grovesNL/glyphon)
*   **Parallelism**: [rayon](https://github.com/rayon-rs/rayon) for background chunk generation and meshing.

## 🚀 Getting Started

### Prerequisites

*   Rust toolchain: You can install it via [rustup](https://rustup.rs/).
*   System dependencies for `wgpu`. See the [wgpu repository](https://github.com/gfx-rs/wgpu#supported-platforms) for platform-specific requirements.

### Building

Clone the repository and build the project in release mode for the best performance.

```bash
git clone https://github.com/alejoki/OpenCraft.git
cd OpenCraft
cargo build --release
```

### Running

To run the game, use `cargo run`. This will launch the game and open the main menu.

```bash
cargo run --release
```

You can also launch directly into a world using command-line flags, bypassing the main menu.

*   `--new`: Force the creation of a new world, even if a save file exists at the specified path.
*   `--seed <NUMBER>`: Specify a seed for world generation.
*   `--save <PATH>`: Set the path for the save file (e.g., `my_world.mcvx`).
*   `--creative` / `--survival`: Select the game mode for a fresh world only. The last flag wins; a loaded world keeps its stored mode.
*   `--demo-combat`: Run the scripted mob/combat capstone — dusk spawn, walking, melee and drops, a zombie fight, a bow shot, a skeleton shot, Creative inertness, save, and a relaunch restore — then exit. This is a development flag, not a player feature.
*   `--demo-save-v9`: Load a v8 save, write a derived v9 file next to it without modifying the input, and print the load, resave, and matching-config proof, then exit. This is a development flag, not a player feature.
*   `--demo-restart`: Run the two-run same-save restart proof. The first run (with `--new`) writes a fixture save; after a recipe edit, a second run on the same save reports the one-time content notice and previews and crafts the edited recipe, then exits. This is a development flag, and it also works in release.
*   `--check-config`: Audit `config/` headlessly and print stable `ok:`/`warning:`/`error:` lines. Debug builds only; a release build rejects it before any startup work.
*   `--creator-trace`: Print one stable, timestamp-free line per applied or rejected config/pack reload. Debug builds only; a release build rejects it before any startup work.

A release build loads `config/` and packs at startup exactly like a debug build, but it contains no watcher, no F6 overlay, and no creator trace output. It rejects `--check-config` with exactly `check-config: dev build only` and `--creator-trace` with exactly `creator-trace: dev build only`, each exiting 1 and creating no settings, saves, or packs. The headless `--demo-save-v9`, `--demo-restart`, and `--validate-pack` seams remain available in release.

For example, to start a new world with seed 4242:

```bash
cargo run --release -- --new --seed 4242 --save "saves/new_world.mcvx"
```

### Demo and smoke scripts

The repository's real-binary checks live under `scripts/`. `scripts/combat_smoke.sh` builds the game and runs the full `--demo-combat` capstone in an isolated temporary directory, then asserts the pinned digest lines, the 317-byte version 9 save witness, and the frame p95 budget. Debug is the default:

```bash
bash scripts/combat_smoke.sh
```

For the release binary, opt in with `SMOKE_PROFILE`:

```bash
SMOKE_PROFILE=release bash scripts/combat_smoke.sh
```

The script proves the assembled seam (spawn, walk, melee, drops, zombie fight, bow shot, skeleton shot, Creative inertness, save, relaunch restore). It is not a player feature.

`scripts/ui_smoke.sh` is the real-binary UI polish proof for M013. It builds the game, runs the headless `--demo-ui` digest seam, and compares its three lines byte-for-byte against pinned 1280x720, 1920x1080, and 3840x2160 contracts for textured item icons, the separated hotbar, table art, tooltip flip, selection fade, and vitals spacing. It then launches a real 10-second world and requires the timeout kill (exit 124) with no panic, proving the production UI pipelines initialize on the event loop:

```bash
bash scripts/ui_smoke.sh
```

For the release binary, opt in with `SMOKE_PROFILE`:

```bash
SMOKE_PROFILE=release bash scripts/ui_smoke.sh
```

`--demo-ui` can also be run directly (`target/debug/opencraft --demo-ui`). It prints the same three pinned `ui_demo:` lines and exits before creating a world, loading a save, or opening a window.

`scripts/modding_smoke.sh` is the real-binary proof for the data-only content packs. It checks the `--validate-pack` exit codes for a valid, broken, and missing pack, runs a bounded launch with a broken pack, and repeats the authoring-doc reader-test. The checked-in test fixtures `tests/fixtures/packs/mymod-ruby` (two blocks, two items, a ruby tier, two tools, two recipes, a disabled shortbow recipe, and two worldgen rules) and `tests/fixtures/packs/ruby-overlay` (a single `stone.png` layer) exercise the same paths a player does:

```bash
bash scripts/modding_smoke.sh
```

For the release binary, opt in with `SMOKE_PROFILE`:

```bash
SMOKE_PROFILE=release bash scripts/modding_smoke.sh
```

The headless `--demo-presentation`, `--demo-crafting`, `--demo-worldgen`, and `--demo-palette` digests replay the authored content through the production seams. See the [content pack authoring guide](docs/TEXTURE-PACKS.md#reference-packs) for the stack order and what each digest covers. `opencraft --demo-village` is the release-facing village digest: it reads only `worldgen/`, prints the catalog fingerprint, the pinned plan (placements, sockets, roads), and identical-cell proofs for shuffled order, one-chunk-at-a-time, empty-catalog and outside-the-bounds generation, then exits 0/1 without opening a window.

`scripts/creator_smoke.sh` is the real-binary proof for the M014 creator surface. In isolated temp cwds it checks `--check-config` on a copied `config/` tree (valid, invalid, and restored), live config apply/reject under `--creator-trace`, an all-domain live pass that applies one edit in each of the six curated files (player, survival, combat, world, mining, render) and matches the final trace digest against a post-run `--check-config`, live block/mob/built-in item texture reload, the restart-required recipe boundary with the two-run `--demo-restart` proof, the v8-to-v9 save migration plus the one-shot config-mismatch notice (one warning, a 446-byte resave with the changed digest, and a silent third load), and release hygiene: rejection of `--check-config` and `--creator-trace`, and a bounded release launch with no creator output and no test packs. It is green in debug and release:

```bash
bash scripts/creator_smoke.sh
SMOKE_PROFILE=release bash scripts/creator_smoke.sh
```

## 🎮 Controls

### General
| Key | Action |
| --- | --- |
| **Escape** | Open/Close Pause Menu (in-game) or back out of other menus. |
| **F** | Toggle Fly/Walk (Creative only). |
| **F5** | Quick save the current world. |

### Movement
| Key | Action |
| --- | --- |
| **W, A, S, D** | Move Forward, Left, Backward, Right. |
| **Space** | Jump (Walk mode) / Ascend (Fly mode). |
| **Left Shift** | Crouch (Walk mode) / Descend (Fly mode). |
| **Left Control** | Sprint. |
| **Mouse** | Look around. |

### Gameplay
| Key | Action |
| --- | --- |
| **Left Click** | Attack a mob under the crosshair within reach; otherwise break the targeted block, and harvest mature wheat. A left press over empty ground still mines. |
| **Right Click** | Use the targeted block (open a chest or crafting table, till with a hoe, plant seeds), else place a block; hold to eat food or draw a selected bow. |
| **Mouse Wheel** | Cycle through hotbar slots. |
| **1-9** | Select hotbar slot. |
| **C** (Creative) | Open the searchable Creative palette. Type to filter by name, id or namespace; click an entry to put a full authored stack in the hotbar. Escape closes. |
| **M** (Creative) | Place a socket marker on the targeted block face (prefab authoring aid; places no item and drops nothing). |
| **P** (Creative) | Select a prefab capture corner on the targeted block: the first press marks corner A, the second marks corner B and opens the name prompt. Type a name, **Enter** saves `worldgen/prefabs/<name>.toml`, **Esc** cancels. While naming, the prompt owns the mouse and keyboard; a rejected save keeps the prompt on screen and shows the reason on the same line. Fixed, non-rebindable authoring key. |
| **Escape** | Open the pause menu (settings are available from there). |

### Survival
| Key | Action |
| --- | --- |
| **Hold Right Click (food)** | Eat an apple or bread from the selected hotbar slot. The progress bar fills over 1.6 s; release or move to cancel. |
| **Right Click (hoe)** | Till the top face of grass or dirt into Farmland. |
| **Craft (wood sword)** | At a crafting table, stack 2 Oak Planks over 1 Stick in a single vertical column. Each swing deals 4 damage. |
| **Craft (stone / iron sword)** | Swap the two planks for 2 Cobblestone (5 damage) or 2 Iron Ore (6 damage), keeping the Stick at the bottom. Swords do not shorten block-breaking time. |
| **Craft (shortbow)** | At a crafting table, stack 3 Sticks and 2 Leather in a bow shape: a Stick in the middle and bottom of the middle column, Leather at the top-right and middle-right, and a Stick at the top-middle. The bow wears out after 59 shots. |
| **Craft (arrows)** | Stack 1 Stick over 1 Coal Ore in a single vertical column; a batch makes 4 Arrows. It is a 1x2 shape, so it also fits the 2x2 player crafting grid. |
| **Hold Right Click (bow)** | With a bow selected, hold Right Click to draw. A full draw takes about 1.0 s; releasing at or after 0.2 s fires one arrow, and releasing earlier cancels without spending an arrow or any bow durability. Each shot spends exactly one arrow and one bow durability. |
| **Fired arrows** | An arrow arcs under gravity, deals 1-4 damage scaled by how long it was drawn, and knocks a mob away from you. An arrow that hits a block sticks there for about 2 s and then vanishes; arrows are never pickups. An empty inventory shows the notice `No arrows`. |
| **Left Click (mob)** | Attack the mob under the crosshair within reach. The hit shows a hurt flash and knocks the mob away; one swing per ~0.6 s, and the same mob ignores further hits for ~0.5 s. A mob in reach takes priority over the block behind it; Left Click always attacks even while a bow is selected. |
| **Hit a pig or cow** | The mob turns and runs away from you for a couple of seconds; a wounded animal keeps its distance instead of standing still. |
| **A zombie spots you** | A Zombie notices a Survival player through line of sight within 16 blocks and chases by walking. It steps up and down one block at a time but never walks off a bigger drop; if it cannot make progress it gives up, idles, and re-targets after a short cooldown. Creative players are never targeted or damaged. |
| **Zombie attack** | Each zombie hit deals 3 damage and knocks you back. After a hit you are invulnerable for 0.5 s and the screen flashes red. The Death screen names the cause: Zombie, Skeleton arrow, Fall, Drowning, or Starvation. |
| **A skeleton spots you** | A Skeleton notices a Survival player through line of sight within 16 blocks and keeps its distance: between 5 and 12 blocks it stands still and shoots, closer than 5 it backs away with the same one-block steps, and beyond 12 blocks it walks in. Creative players are never targeted or shot at. |
| **Skeleton arrows** | A skeleton fires one gravity-affected arrow every 2 s. Each arrow deals 2 damage and knocks you back, and the aim is lobbed so a standing target is hit at range. After a hit you are invulnerable for 0.5 s and the screen flashes red; the Death screen names the cause `Skeleton arrow`. An arrow that hits a block sticks there for about 2 s and then vanishes; arrows are never pickups. |
| **Skeleton drops** | A skeleton drops one bone and two arrows, and rarely (about one kill in sixteen) a full-durability shortbow. |
| **Zombie drops** | A zombie drops one rotten flesh. |
| **Kill a pig or cow** | A lethal hit collapses the animal where it stands, then a moment later its body is gone. A pig drops raw porkchop and leather; a cow drops raw beef and leather. |
| **Mob drops** | Drops go straight into your 36 inventory slots, never onto the ground. If the inventory is full, only the part that does not fit is lost and a short message says so. |
| **Eat raw meat** | Raw porkchop and raw beef restore 3 hunger; rotten flesh restores 1. Hold Right Click with the food selected, as with apples and bread. |
| **Right Click (Wheat Seeds)** | Plant seeds on Farmland. |
| **Left Click (wheat)** | Harvest mature wheat for 1 Wheat and 2 Wheat Seeds; immature wheat returns 1 Wheat Seed. |
| **Right Click (chest)** | Open the chest screen. |
| **Left Click (chest screen)** | Move a whole stack between the chest and your inventory. Right click is inert in the chest screen. |
| **E or Escape (chest screen)** | Close the chest screen. |
| **Respawn / Quit (Death screen)** | Respawn at the deterministic spawn with full vitals and your inventory kept, or Quit to save and return to the menu. |
| **Mob sounds** | An idle call from a nearby mob every few seconds; a hurt sound when a mob takes non-lethal damage; and a death sound when a mob dies. A bow release is heard on a real shot (a tap-cancel is silent) and an arrow impact when an arrow hits a body or a block. |
| **Creative mobs** | Mobs never target or damage a Creative player. Any melee swing (even with no weapon) and any player arrow kills a mob in one hit, and the mob's normal drops still land in your inventory. The rule follows the effective mode, so Edit Background is safe from attacks too. |

## 📂 Project Structure

The codebase is organized into several modules within the `src/` directory:

*   `main.rs`: The main application entry point, handling the event loop, windowing, and state management.
*   `world/`: Contains all logic related to the game world, including chunk management, procedural terrain generation, and persistence (saving/loading).
*   `render/`: The `wgpu`-based rendering engine, including shaders, pipelines, and buffer management.
*   `player/`: Handles player state, physics, camera, and input processing.
*   `mesh/`: Implements the greedy meshing algorithm for creating optimized chunk geometry.
*   `ui/`: Defines the pure-logic state machines and geometry for all UI screens (main menu, settings, etc.).
*   `settings/`: Manages loading, saving, and applying user-configurable settings.
