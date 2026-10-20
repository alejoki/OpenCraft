# OpenCraft

OpenCraft is an open-source voxel game engine built from scratch in Rust, inspired by Minecraft. It features a custom rendering engine using `wgpu` for modern, cross-platform graphics.

## ✨ Features

*   **Procedural World Generation**: Infinite, deterministic terrain generated based on a seed.
*   **Efficient Chunk Rendering**: Implements Greedy Meshing to optimize the rendering of voxel chunks, significantly reducing the number of vertices sent to the GPU.
*   **Dynamic Chunk Streaming**: Chunks are loaded, unloaded, and meshed in the background as the player moves through the world.
*   **World Persistence**: Save and load game worlds, including player state, chunk edits, and world seed. A filesystem-based world catalog allows for managing multiple saved games.
*   **Player Physics and Controls**:
    *   Walk mode with collision detection, gravity, jumping, and sprinting.
    *   Fly mode for free movement and exploration.
    *   Raycasting for block breaking and placing.
*   **Customizable Settings**:
    *   Rebindable key controls for movement and actions.
    *   Adjustable mouse sensitivity, field of view (FOV), VSync, and fullscreen settings.
    *   Settings are saved to and loaded from a `settings.toml` file.
*   **Comprehensive UI System**:
    *   Main Menu, New Game, Load Game, and Pause screens.
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

For example, to start a new world with seed 4242:

```bash
cargo run --release -- --new --seed 4242 --save "saves/new_world.mcvx"
```

## 🎮 Controls

### General
| Key | Action |
| --- | --- |
| **Escape** | Open/Close Pause Menu (in-game) or back out of other menus. |
| **F** | Toggle Fly/Walk mode. |
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
| **Left Click** | Break block. |
| **Right Click** | Place block. |
| **Mouse Wheel** | Cycle through hotbar slots. |
| **1-9** | Select hotbar slot. |
| **[** / **]** | Decrease / Increase render distance. |

## 📂 Project Structure

The codebase is organized into several modules within the `src/` directory:

*   `main.rs`: The main application entry point, handling the event loop, windowing, and state management.
*   `world/`: Contains all logic related to the game world, including chunk management, procedural terrain generation, and persistence (saving/loading).
*   `render/`: The `wgpu`-based rendering engine, including shaders, pipelines, and buffer management.
*   `player/`: Handles player state, physics, camera, and input processing.
*   `mesh/`: Implements the greedy meshing algorithm for creating optimized chunk geometry.
*   `ui/`: Defines the pure-logic state machines and geometry for all UI screens (main menu, settings, etc.).
*   `settings/`: Manages loading, saving, and applying user-configurable settings.
