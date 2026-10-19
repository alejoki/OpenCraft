# M001: Refactor + Settings Foundation

**Gathered:** 2026-10-19
**Status:** Ready for planning

## Project Description

A foundations milestone for the Rust/wgpu minecraft-clone that modernizes the windowing layer and adds a persistent settings system. The winit 0.29 event loop migrates to the current `ApplicationHandler` API (wgpu stays at 0.19 — the two are decoupled, so one breaking migration at a time). A new `settings.toml` (TOML via serde, the project's first serde use) manages input + video settings: keybinds, mouse sensitivity, FOV, fullscreen/vsync. A hand-rolled immediate-mode settings/keybind menu (new `src/ui/` module) lets the player press-to-rebind keys with a basic duplicate warning.

## Why This Milestone

The event-loop refactor is happening anyway, so it should land on a supported API instead of winit's deprecated closure API (upstream features land on 0.30+ only). And the game currently has no way to persist player preferences — every launch starts from hardcoded defaults. This milestone fixes both: a supported windowing base plus the first player-facing persistence feature.

## User-Visible Outcome

### When this milestone is complete, the user can:

- Open the settings/keybind menu in-game, click an action, press a key to rebind it (with a duplicate warning), and after quitting and relaunching the binding is still in effect
- Adjust mouse sensitivity, FOV, and fullscreen/vsync from the menu, and those values survive restarts via `settings.toml`

### Entry point / environment

- Entry point: `cargo run` — desktop game window (existing binary; settings menu reachable in-game)
- Environment: local dev / desktop (user's machine, GPU-required)
- Live dependencies involved: none (local filesystem only — `settings.toml` next to the game)

## Completion Class

- Contract complete means: `cargo test` proves the settings layer — load→save→load round-trip equality, missing file → defaults, corrupt/invalid file → per-field fallback with valid fields kept, duplicate keybind rejected; the crate builds clean on winit 0.30+ ApplicationHandler.
- Integration complete means: the migrated event loop drives the real window — input events (keyboard, mouse, pointer) reach gameplay through the new API, and the in-game menu renders, captures key presses, and writes through to `settings.toml` in a live wgpu context.
- Operational complete means: rebinding a key, quitting the process, and relaunching shows the persisted binding; a hand-corrupted `settings.toml` never blocks game launch.

## Final Integrated Acceptance

To call this milestone complete, we must prove:

- Rebind a key in the menu (e.g. jump from Space to Shift), quit the process, relaunch — the new binding works in-game and appears in the menu; the old binding triggers the duplicate warning if assigned elsewhere
- Corrupt `settings.toml` by hand (invalid type in one field, garbage value in another), launch the game — it starts normally, invalid fields fall back to defaults, valid fields survive, and the next save rewrites a clean file
- Movement, block break/place, and raycast behave exactly as before the winit migration (manual smoke check) — the refactor introduces no gameplay regressions
- What cannot be simulated: the ApplicationHandler migration's effect on real window/input events cannot be fully proven by unit tests — it must be exercised against a real window and a real restart of the process

## Architectural Decisions

### Upgrade winit during the M001 refactor

**Decision:** Migrate the event loop from winit 0.29's deprecated closure API to the current `ApplicationHandler` trait API as part of M001.

**Rationale:** The event-loop code is being restructured anyway; building on a deprecated API means future winit features land on 0.30+ only and the migration debt grows.

**Alternatives Considered:**

- Stay on 0.29 — zero migration risk now, but the refactor is built on an API upstream no longer supports

### TOML via serde for settings

**Decision:** Store settings in a human-readable `settings.toml`, loaded/saved as a struct round-trip through serde.

**Rationale:** Standard Rust ecosystem choice, human-editable, and settings load/save becomes a simple struct round-trip instead of hand-rolled parsing.

**Alternatives Considered:**

- Hand-rolled key=value — consistent with the project's hand-rolled persistence style, but parsing/validation/round-trip code is written and maintained by hand

### Hand-rolled immediate-mode UI for the menu

**Decision:** Build a small immediate-mode UI directly against the existing wgpu pipeline — a new `src/ui/` module with a quad renderer and text rendering (glyphon or cosmic-text, chosen at roadmap time). No heavyweight UI dependency.

**Rationale:** No new heavyweight dependency, full control, and it matches the project's hand-rolled style. Text rendering is the main new territory.

**Alternatives Considered:**

- egui / egui-wgpu / egui-winit — full widget set out of the box, but three new dependencies, render-pass wiring, and a stylistic departure from the hand-rolled codebase
- Keyboard-only text menu — least code, but weaker UX and mouse users must still use the keyboard for settings

### winit-only upgrade; wgpu stays at 0.19

**Decision:** Upgrade winit to current (0.30+ ApplicationHandler) but keep wgpu at 0.19 in M001.

**Rationale:** winit and wgpu are decoupled; each migration is independently testable, and bundling two breaking migrations makes regressions harder to isolate.

**Alternatives Considered:**

- Upgrade both together — one-time churn, but two breaking migrations in one milestone mix risk

### Press-to-rebind keybind menu with duplicate warning

**Decision:** The menu lists actions; the player selects one, presses a key to bind it, and a basic duplicate warning fires if the key is already used.

**Rationale:** Simple to build, covers the real use case; full conflict detection/multi-bind/reset-to-default is more UI work than M001 needs.

**Alternatives Considered:**

- Config-file-only rebind — cheapest, weakest UX
- Full editor + conflicts — conflict detection, multi-bind actions, per-action reset; deferred

### Per-field fallback on bad config

**Decision:** If `settings.toml` is missing, corrupt, or contains invalid values, invalid fields fall back to defaults individually, valid fields are kept, a warning is logged, and the game always launches; the next save rewrites a clean file.

**Rationale:** One typo should never nuke every setting or block launch; the game must always start.

**Alternatives Considered:**

- All-or-nothing (move to `.bak`, fresh defaults) — simplest, but one typo loses every setting
- Fail loudly (error + exit) — never hides problems, but blocks launch on a trivial typo

## Error Handling Strategy

The guiding rule: **the game always launches.** Settings load never returns a fatal error — every failure mode resolves to defaults: missing file → all defaults; unparseable file → all defaults with a logged warning; parseable file with invalid individual fields → per-field fallback (invalid field → default, valid fields kept). Problems are surfaced via logged warnings (never silent), and the on-disk file is healed to a clean state on the next save. Duplicate keybind attempts in the menu are rejected with an in-menu warning, not a crash. Event-loop migration errors surface as ordinary Rust errors at startup (`Result` from init), not panics.

## Risks and Unknowns

- Text rendering is new territory — the codebase has no text/UI drawing today; picking and integrating glyphon vs cosmic-text is the main technical unknown (resolved at roadmap time)
- ApplicationHandler migration touches every input path — keyboard, mouse, pointer, and redraw all change; gameplay regressions are the primary risk, mitigated by the manual smoke check
- Key code representation across winit versions — migrated keycodes must map cleanly to stable `settings.toml` string values so config files survive future winit bumps

## Existing Codebase / Prior Art

- `src/main.rs` — hosts the current winit 0.29 closure-based event loop; the primary file the ApplicationHandler migration rewrites
- `src/player/` (`camera.rs`, `physics.rs`, `raycast.rs`) — consumes input events; the migration must keep their inputs flowing unchanged
- `src/render/` (`context.rs`, `pipeline.rs`) — the wgpu context the new `src/ui/` module will render alongside; wgpu stays at 0.19
- `src/world/persistence.rs` — existing hand-rolled persistence pattern; the settings layer deliberately departs from it (serde/TOML)

## Relevant Requirements

- (to be recorded) Settings persistence — this milestone introduces the first persisted player preferences
- (to be recorded) Supported windowing API — this milestone retires the deprecated winit closure API

## Scope

### In Scope

- winit 0.29 → 0.30+ ApplicationHandler migration (wgpu stays 0.19)
- `settings.toml` via serde: load/save round-trip, defaults, per-field fallback
- New `src/ui/` module: quad renderer + text rendering (glyphon or cosmic-text)
- Settings/keybind menu: press-to-rebind with duplicate warning; sensitivity, FOV, fullscreen/vsync controls
- Automated tests for the settings layer (round-trip, missing file, corrupt file, duplicate keybind)
- Manual smoke check: no gameplay regressions after migration

### Out of Scope / Non-Goals

- wgpu upgrade (stays 0.19)
- Gameplay-affecting settings (render distance, world options)
- Full keybind editor: conflict detection, multi-bind actions, per-action reset-to-default
- Any new gameplay features
- Headless event-loop test harness

## Technical Constraints

- Rust, existing crate layout; wgpu pinned at 0.19 for this milestone
- First serde use in the project — introduces `serde` + `toml` (or `toml_edit`-compatible) dependencies
- Settings file lives beside the game (local filesystem); no global config paths this milestone
- Keybind values in `settings.toml` stored as stable string forms of winit keycodes

## Integration Points

- winit event loop → gameplay input paths — the ApplicationHandler migration re-routes all keyboard/mouse/pointer/redraw events to the existing player/world systems
- Settings layer ↔ `settings.toml` on disk — load at startup, save on change
- `src/ui/` ↔ existing wgpu render context — menu rendered in the same window/pipeline, on top of the game
- Keybind settings ↔ input handling — rebound keys take effect immediately in gameplay

## Testing Requirements

Automated (`cargo test`, pure logic — no GPU required): settings round-trip (load → save → load equal); missing file → defaults; corrupt file → per-field fallback with valid fields kept; unknown keys ignored; duplicate keybind rejected. Build gate: `cargo build` and `cargo test` clean on winit 0.30+. Manual (in-game): menu opens/closes; rebind a key, restart, binding persisted; duplicate warning fires; sensitivity/FOV/fullscreen apply and persist; movement, break/place, raycast unchanged vs pre-migration behavior.

## Acceptance Criteria

- Event loop runs on winit 0.30+ `ApplicationHandler`; no deprecated 0.29 closure API remains
- Game launches with no `settings.toml` present; defaults written on first save
- Rebind in menu → quit → relaunch: binding persisted and functional
- Duplicate keybind attempt rejected with visible warning
- Sensitivity, FOV, fullscreen/vsync persist across restarts
- Hand-corrupted `settings.toml` never blocks launch; invalid fields fall back individually, valid fields survive, next save heals the file
- Settings round-trip, fallback, and duplicate-rejection behaviors covered by passing `cargo test`
- Manual smoke check: movement, block break/place, raycast unchanged

## Open Questions

- Text rendering library: glyphon vs cosmic-text (vs raw) — current thinking: decide at roadmap/slice planning time when the `src/ui/` slice is sized
- Exact winit target version (latest stable 0.30.x line) — resolved when the migration slice is planned
<!-- gsd:state-version=27:1 -->
