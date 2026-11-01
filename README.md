# OpenCraft

OpenCraft is an open-source voxel game engine built from scratch in Rust, inspired by Minecraft. It features a custom rendering engine using `wgpu` for modern, cross-platform graphics.

## ✨ Features

*   **Procedural World Generation**: Infinite, deterministic terrain generated based on a seed.
*   **Efficient Chunk Rendering**: Implements Greedy Meshing to optimize the rendering of voxel chunks, significantly reducing the number of vertices sent to the GPU.
*   **Dynamic Chunk Streaming**: Chunks are loaded, unloaded, and meshed in the background as the player moves through the world.
*   **World Persistence**: Save and load game worlds, including player state, chunk edits, and world seed. A filesystem-based world catalog allows for managing multiple saved games.
*   **Survival and Creative Modes**: New worlds default to Survival. Survival has 20 health, 20 hunger, and 300 air, shown as a hearts/hunger/air HUD; fall, drowning, and starvation damage; a Death screen with Respawn and Quit (respawn at the deterministic spawn with your inventory kept); hold-to-eat apples and bread; hoe-based wheat farming that pauses in Winter; 27-slot chests; and deterministic ruin loot. Creative keeps flying and building, is invulnerable, never hungry, and never loses air. The mode is chosen at creation and locked.
*   **Player Physics and Controls**:
    *   Walk mode with collision detection, gravity, jumping, and sprinting.
    *   Fly mode for free movement and exploration.
    *   Raycasting for block breaking and placing.
*   **Customizable Settings**:
    *   Rebindable key controls for movement and actions.
    *   Adjustable mouse sensitivity, field of view (FOV), render distance, VSync, and fullscreen settings.
    *   Settings are saved to and loaded from a `settings.toml` file.
*   **Texture Packs**: Swap block and mob textures plus menu presentation with folder-based packs — a `pack.toml` plus optional PNGs, no code required. The texture array has 48 layers: 23 authorable block textures (the original 16 plus Farmland, four Wheat stages, Chest, and LootChest) and 13 authorable mob part textures, each with optional per-layer overrides; a pack that supplies only the original 16 block layers falls back per layer to the built-in procedural art. See the [texture pack authoring guide](docs/TEXTURE-PACKS.md).
*   **Customizable Menu Background**: The panorama behind the main menu is its own saved world at `saves/background/background.mcvx`. Choose **Edit Background** on the main menu to build it with the normal controls; the menu resumes your saved vantage point, time of day, and season. A missing or unreadable file falls back to the built-in scenery and never blocks launch.
*   **Save Compatibility**: Loading a world from an older release keeps every block you edited, regenerates the terrain you never touched, and starts its calendar at day 0. Version 6 saves round-trip the game mode, health/hunger/air, and every chest's contents; version 1-5 saves load as Creative with full vitals. See the [save and release notes](docs/SAVE-NOTES.md).
*   **Comprehensive UI System**:
    *   Main Menu (including Edit Background), New Game, Load Game, and Pause screens.
    *   Chest screen (27 chest slots plus the 36-slot player inventory) and Death screen (Respawn and Quit).
    *   In-game settings menu for live adjustments.
    *   UI logic is decoupled from rendering, built with `glyphon` for text rendering.

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

For example, to start a new world with seed 4242:

```bash
cargo run --release -- --new --seed 4242 --save "saves/new_world.mcvx"
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
| **Left Click** | Break block; harvest mature wheat. |
| **Right Click** | Use the targeted block (open a chest or crafting table, till with a hoe, plant seeds), else place a block; hold to eat food. |
| **Mouse Wheel** | Cycle through hotbar slots. |
| **1-9** | Select hotbar slot. |
| **Escape** | Open the pause menu (settings are available from there). |

### Survival
| Key | Action |
| --- | --- |
| **Hold Right Click (food)** | Eat an apple or bread from the selected hotbar slot. The progress bar fills over 1.6 s; release or move to cancel. |
| **Right Click (hoe)** | Till the top face of grass or dirt into Farmland. |
| **Right Click (Wheat Seeds)** | Plant seeds on Farmland. |
| **Left Click (wheat)** | Harvest mature wheat for 1 Wheat and 2 Wheat Seeds; immature wheat returns 1 Wheat Seed. |
| **Right Click (chest)** | Open the chest screen. |
| **Left Click (chest screen)** | Move a whole stack between the chest and your inventory. Right click is inert in the chest screen. |
| **E or Escape (chest screen)** | Close the chest screen. |
| **Respawn / Quit (Death screen)** | Respawn at the deterministic spawn with full vitals and your inventory kept, or Quit to save and return to the menu. |

## 📂 Project Structure

The codebase is organized into several modules within the `src/` directory:

*   `main.rs`: The main application entry point, handling the event loop, windowing, and state management.
*   `world/`: Contains all logic related to the game world, including chunk management, procedural terrain generation, and persistence (saving/loading).
*   `render/`: The `wgpu`-based rendering engine, including shaders, pipelines, and buffer management.
*   `player/`: Handles player state, physics, camera, and input processing.
*   `mesh/`: Implements the greedy meshing algorithm for creating optimized chunk geometry.
*   `ui/`: Defines the pure-logic state machines and geometry for all UI screens (main menu, settings, etc.).
*   `settings/`: Manages loading, saving, and applying user-configurable settings.
