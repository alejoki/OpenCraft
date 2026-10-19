# M001: Refactor + Settings Foundation — Research

**Date:** 2026-10-19

## Summary

The milestone's two open questions are now answered with hard version facts. First, **winit's current stable line is 0.30.13** (0.31.0-beta.3 exists but is pre-release), and the 0.30 `ApplicationHandler` trait is a near-mechanical reshape of the current closure loop: `Event::WindowEvent` → `window_event()`, `Event::DeviceEvent` → `device_event()`, `Event::AboutToWait` → `about_to_wait()`, with `resumed()` required (fires once after init on desktop) and the window created via `ActiveEventLoop::create_window` (the old `EventLoop::create_window` is deprecated). The `WindowEvent`/`KeyEvent`/`KeyCode` shapes the game consumes are unchanged between 0.29 and 0.30, so the migration risk is structural (state ownership moves into an `ApplicationHandler` struct) rather than semantic.

Second — and most important — **glyphon 0.5.0 pins wgpu `^0.19` exactly**, and winit is only a *dev*-dependency of glyphon, so text rendering composes with both the wgpu-0.19 pin and the winit-0.30 upgrade with zero version conflict. Note the trap: glyphon 0.6+ jumped to wgpu's new numbering (`^22`, `^23`...) and is *incompatible* with wgpu 0.19. Glyphon 0.5 must be the pin, and current online docs describe the 0.12 API — the slice must reference docs.rs/glyphon/0.5.0 and the tag-v0.5.0 examples, not latest.

The codebase side is small and clean: a single ~440-line monolithic `main.rs` hosts the whole event loop and input dispatch; `InputState` (player/physics.rs) is a plain 7-bool struct; settings touchpoints are all hardcoded constants today (sensitivity `0.0022` in main.rs, FOV `75°` in camera.rs, present-mode preference in context.rs, cursor-grab toggle on Escape). There are currently no tests in the crate at all — the settings-layer `cargo test` gate introduces the first test code. No `.gsd/REQUIREMENTS.md` exists yet; this milestone's two "to be recorded" requirements (settings persistence, supported windowing API) are the first entries.

## Recommendation

Proceed as the brief scopes it, with **glyphon 0.5** as the text rendering library (resolving the roadmap-time open question) and **winit 0.30.13** as the migration target. Order slices so the two independent risk-retiring foundations come first and in parallel if possible: (1) the winit ApplicationHandler migration (pure refactor, behavior-preserving, highest integration risk) and (2) the settings core (pure logic, fully cargo-test-able, zero GPU). Then the UI foundation (glyphon 0.5 + a hand-rolled quad renderer proving text-on-pipeline with a trivial HUD overlay), and finally the settings menu slice that wires settings + UI + live-apply + save-on-exit together — its acceptance is inherently the end-to-end restart-persistence check. Represent keybinds in `settings.toml` as hand-mapped stable strings (e.g. `"KeyW"`, the `KeyCode` Debug spelling) via a small explicit conversion module rather than depending on winit's serde feature — the enum is identical across 0.29/0.30, and an explicit mapping decouples the settings file format from any future winit bump.

## Implementation Landscape

### Key Files

- `src/main.rs` — the entire event loop, input dispatch, sensitivity constant (`0.0022`), cursor-grab toggle, save-on-exit, FPS title. This is the file the ApplicationHandler migration rewrites: the closure's captures (world, player, camera, input, ctx, renderer, render_store, cli, timers) become the fields of a `State` struct implementing `ApplicationHandler`. New: a settings-open key must release cursor grab and route input to the menu instead of gameplay.
- `src/player/physics.rs` — `InputState` (7 bools: forward/backward/left/right/jump_or_up/crouch_or_down/sprint). The keybind layer's consumer: a `Keybinds` map resolves `KeyCode` → action → sets these flags. `InputState::reset()` already exists for focus loss and should also fire when the menu opens.
- `src/player/camera.rs` — `fov_y_radians` hardcoded to 75° in `Camera::new`. The FOV setting must flow in here; `Camera::new` needs the FOV (or a setter) and main.rs must pass the loaded setting.
- `src/render/context.rs` — `RenderContext` owns surface config and the present-mode choice (prefers Immediate/Mailbox, falls back to Fifo). The vsync setting overrides this (vsync on → `PresentMode::Fifo`, vsync off → current preference); changing it at runtime requires `surface.configure` again — add a `set_present_mode`-style reconfigure path. `RenderContext::new(window: Arc<Window>)` stays valid in winit 0.30 (`Arc::new(window)` from the new non-Arc `create_window` return, or keep `Window` and hand it over — wgpu 0.19 accepts `Arc<Window>` as a `SurfaceTarget` because both winit lines expose raw-window-handle 0.6).
- `src/render/pipeline.rs` / `src/render/buffer.rs` — existing frame encoding; the UI renders in a **separate color-only render pass after the world pass** (no depth interplay; glyphon `DepthStencilState`-less), added at the `renderer.render_frame` call site in main.rs.
- `src/world/persistence.rs` — existing hand-rolled world save. No changes; the new settings module deliberately does NOT copy this pattern (serde/TOML instead) but can mirror its file-location conventions.
- New `src/settings/` (or `src/settings.rs`) — `Settings` struct, defaults, load with per-field fallback, save, keybind↔action mapping, duplicate-keybind validation, `KeyCode`↔string conversion. Pure logic, `#[cfg(test)]` units, no GPU.
- New `src/ui/` — glyphon 0.5 wrappers (`FontSystem`, `SwashCache`, `TextAtlas`, `TextRenderer`, `Viewport`) plus a minimal quad renderer for menu panels/buttons, and the immediate-mode menu state machine.
- `Cargo.toml` — `winit = "0.30"`, add `serde` (derive), `toml` (0.8+), `glyphon = "0.5"` (pin exact — `=0.5.0` or `~0.5` — to prevent accidental resolution into an incompatible line).

### Build Order

1. **Settings core first or in parallel with the winit migration** — it is pure logic with no dependency on the event loop; it unblocks the menu slice and delivers the crate's first tests (`cargo test` proof of round-trip/fallback/duplicate-rejection per the completion contract).
2. **winit migration second or in parallel** — independent of settings; retires the "every input path changes" risk before UI work lands on top of it. Manual smoke check (movement/break-place/raycast unchanged) gates this slice.
3. **UI foundation third** — needs the migrated loop (for cursor-position events, which the current code never consumes — immediate-mode UI requires `WindowEvent::CursorMoved` + `CursorLeft`) and the real wgpu pipeline. Demo with a trivial always-on text overlay (e.g. FPS/pos readout) before any menu logic.
4. **Settings menu last** — wires 1+3: open/close (releases cursor grab, `InputState::reset()`), press-to-rebind with duplicate warning, live-apply sensitivity/FOV/vsync/fullscreen, save on change and on exit. Its acceptance is the end-to-end rebind→quit→relaunch check.

### Verification Approach

- Settings core: `cargo test` — round-trip (load→save→load equality), missing file → defaults, unparseable file → defaults + warning, parseable-but-invalid field → per-field fallback with valid fields kept, unknown keys ignored, duplicate keybind rejected. Use `tempfile`-style temp dirs or an in-test path (tests must not touch a real `settings.toml`).
- Migration: `cargo build` clean on winit 0.30 + manual smoke (WASD/space/shift movement, F fly, F5 save, mouse look, left/right break/place, scroll + digit hotbar, Escape grab toggle, [ ] render distance) — compare against pre-migration behavior; commit the checklist in the slice summary.
- UI: visible overlay text in a live `cargo run` window (GPU-required; cannot be unit-tested).
- Menu: in-game walkthrough — rebind jump Space→ShiftLeft, duplicate warning on conflict, quit, relaunch, verify `settings.toml` content + binding behavior; hand-corrupt `settings.toml` (wrong type one field, garbage another), relaunch, confirm launch + fallback + healed file on next save.

## Don't Hand-Roll

| Problem | Existing Solution | Why Use It |
|---------|------------------|------------|
| TOML parse/serialize + round-trip | `toml` crate (0.8+) | serde derive gives load/save as a struct round-trip; hand-rolled parsing is exactly what the brief rejected |
| Font system / shaping / raster / glyph atlas | glyphon 0.5 (cosmic-text, etagere, lru) | Text rendering is the milestone's declared new territory; glyphon 0.5 is purpose-built for wgpu 0.19 |
| Per-field fallback extraction | `toml::Value`/`Table` per-field `try_into` | Parse to `toml::Table` once, deserialize each field individually with `unwrap_or(default)` — gives per-field fallback for free instead of whole-struct serde failure |
| Keybind string stability | `KeyCode` Debug spelling + explicit match table | Identical enum in 0.29/0.30; a ~150-line explicit conversion is decoupled from winit's serde coverage forever |

## Constraints

- **Hard pin: glyphon must be 0.5.x.** glyphon 0.6+ requires wgpu 22+ (new numbering) — incompatible with the wgpu 0.19 pin. Pin in Cargo.toml (`glyphon = "=0.5.0"` or `~0.5`) so a careless `cargo update` can't jump lines.
- wgpu stays 0.19 all milestone (per decision); compatibility with winit 0.30 holds because both winit 0.29 and 0.30 sit on raw-window-handle 0.6 — no `rwh` version conflict.
- winit target is 0.30.x stable (0.30.13 as of research date); do not take 0.31.0-beta.
- Rust edition 2021; no test infra exists today — the settings slice creates it.
- Settings file beside the game (local path, cwd-adjacent per `cli.save_path` precedent); no global config dirs.
- Immediate-mode UI needs cursor position — the game currently never consumes `CursorMoved` and keeps the cursor grabbed except after Escape; the menu path must release grab and track a logical cursor. (X11/Wayland cursor-grab + `CursorGrabMode::Confined` interplay is why menu-open must explicitly `set_cursor_grab(None)` + `set_cursor_visible(true)` — the helper already exists in main.rs.)

## Common Pitfalls

- **Using current glyphon docs for glyphon 0.5 code** — the API moved between 0.5 and 0.12 (e.g. `TextRenderer::new`/`prepare` signatures and `TextAtlas` construction take `&mut Device/&mut Queue/&Viewport` in the 0.5 era, not the newer shapes). Slice must use docs.rs/glyphon/0.5.0 and the tag-v0.5.0 example as the source of truth.
- **`FontSystem::new()` scans system fonts at first use** — slow-ish, nondeterministic across machines, and fails oddly on font-less environments. Prefer bundling one font binary (e.g. a small open font in `assets/`) via cosmic-text's explicit font loading so the menu renders identically everywhere.
- **TextAtlas must be created with the surface's sRGB format** (`ctx.config.format`) or text renders wrong/panics; `Viewport` must be updated on resize alongside the existing `ctx.resize`.
- **Whole-struct `toml::from_str::<Settings>` with `#[serde(default)]` is NOT per-field fallback** — a single wrong-typed field fails the entire deserialize, nuking all settings. The per-field contract requires the `toml::Table` + per-field `try_into` approach (or an equivalent two-stage split).
- **Deprecation traps in the 0.30 migration** — `EventLoop::create_window` and `run()` (closure) are deprecated in 0.30; using them "works" but fails the "no deprecated 0.29 closure API remains" acceptance. Create the window in `resumed()` via `ActiveEventLoop::create_window`, run with `run_app`. Also: on desktop `resumed` fires once after init and can fire redundantly (e.g. on some platforms after suspend) — guard window creation with an `Option<Window>`/already-created check.
- **Escape is currently the grab-toggle; it cannot also be the menu-open key without a mode distinction** — decide the menu-open UX explicitly (e.g. Escape releases grab first, opens menu when already released; or a dedicated key) and make "gameplay vs menu" an explicit input-routing mode in the State struct so keybinds never leak into gameplay while the menu is open.
- **winit 0.30's `run_app` returns `Result<(), EventLoopError>`** (0.29's `run` was `-> !` on desktop) — the `main` tail and `.expect("Event loop error")` pattern change shape slightly.

## Open Risks

- Glyphon 0.5 is 2.5 years old by now; it works with wgpu 0.19 by construction, but the cosmic-text 0.10 era had known fontdb/ shaping edge cases. Risk is low for a simple menu (short ASCII labels) but is the reason to bundle a known-good font instead of relying on system fonts.
- The ApplicationHandler refactor's real risk is behavioral drift in input routing (the manual smoke check is the designed mitigation — it cannot be automated per the brief's "cannot be simulated" note).
- Vsync semantics on Linux/Wayland: `PresentMode::Immediate` is often unsupported; the current fallback logic already handles this, and the vsync setting should route through the same capability check rather than blindly setting Fifo/Immediate.

## Skills Discovered

| Technology | Skill | Status |
|------------|-------|--------|
| wgpu / winit / Rust text rendering | (searched via `npx skills find`) | none found — only unrelated Bevy/image-gen/business skills matched |

## Sources

- winit `ApplicationHandler` trait definition, `run_app`, and 0.29→0.30 migration examples (source: [docs.rs winit changelog](https://docs.rs/winit/latest/winit/changelog/index.html), [ApplicationHandler](https://docs.rs/winit/latest/winit/application/trait.ApplicationHandler.html))
- winit current stable = 0.30.13; 0.31.0-beta.3 pre-release; `serde` feature exists on the winit crate (source: [crates.io API — winit](https://crates.io/api/v1/crates/winit))
- glyphon 0.5.0 depends on wgpu `^0.19`, cosmic-text `^0.10`; winit only a dev-dependency; glyphon 0.6+ requires wgpu `^22`+ (source: [crates.io API — glyphon 0.5.0 deps](https://crates.io/api/v1/crates/glyphon/0.5.0/dependencies), [glyphon 0.6.0 deps](https://crates.io/api/v1/crates/glyphon/0.6.0/dependencies), [docs.rs glyphon 0.5.0](https://docs.rs/glyphon/0.5.0/glyphon/struct.TextRenderer.html))
- Candidate requirements surfaced (no REQUIREMENTS.md exists yet): (1) core-capability — settings persistence with per-field fallback and always-launch guarantee; (2) constraint/launchability — event loop on supported winit ApplicationHandler API. Duplicate-keybind warning and restart persistence are acceptance details of (1), advisory vs. separate requirements — left to the requirements-recording step, not silently expanded here.
<!-- gsd:state-version=17:1 -->
