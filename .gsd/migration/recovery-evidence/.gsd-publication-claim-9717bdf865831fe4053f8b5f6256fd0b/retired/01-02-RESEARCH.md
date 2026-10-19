# S02: Settings core with TOML persistence — Research

**Date:** 2026-10-19

## Summary

S02 is pure logic: a new `src/settings` module (serde + toml, no GPU, no event loop) that loads, validates, and saves a `Settings` struct — keybinds, mouse sensitivity, FOV, fullscreen, vsync — with per-field fallback so a bad `settings.toml` never blocks launch. The codebase has zero settings infrastructure today: every value is a hardcoded constant (`sensitivity = 0.0022` in main.rs MouseMotion arm, `fov_y_radians: 75°` in Camera::new (src/player/camera.rs), present-mode preference Immediate>Mailbox>Fifo in src/render/context.rs, Escape cursor-grab toggle in main.rs). No serde/toml dependency exists yet — the toml crate must be added (current release line is **1.x** per `cargo search`; `toml::Value` + `from_str`/`to_string` API confirmed via docs). The existing world persistence (src/world/persistence.rs) is a custom binary format — no serde anywhere in Cargo.toml — so S02 introduces the crate's first serde/TOML usage and its first dev-dependency choice (temp test dirs).

Correction to milestone research: tests **do** already exist in the crate (`#[cfg(test)]` mods in src/player/physics.rs and src/player/camera.rs) — `cargo test` is already a working gate; S02 adds settings tests alongside them.

## Requirements (Active) this slice owns/supports

The requirements registry does not yet exist (M001 research noted "settings persistence" and "supported windowing API" as its first entries). This slice is the delivery vehicle for the settings-persistence requirement: round-trip load/save, per-field fallback, duplicate-keybind validation, all `cargo test`-provable without GPU.

## Recommendation

Build `src/settings/` with three files: `mod.rs` (Settings struct, load/save/validate), `keybind.rs` (KeyCode↔stable-string conversion per MEM003: hand-mapped explicit subset, KeyCode Debug spelling, **no** winit serde feature), and inline `#[cfg(test)]` tests. Load via a two-phase parse: `toml::from_str::<toml::Value>` (fails only on syntactically invalid TOML), then **per-field extraction with type/range checks** — this is the only way to get true per-field fallback where a valid field survives an invalid sibling (plain `#[serde(default)]` derives abort the whole struct on any field type error). Save via a serde-derive mirror struct (`Serialize` only) so output TOML is clean and stable; the round-trip test locks the load and save paths together.

**Warnings, not a logging framework:** the crate has no log/tracing dependency at all. Don't add one in this slice. Return warnings as data: `load_with_warnings(path) -> (Settings, Vec<SettingsWarning>)` plus a thin `Settings::load(path)` wrapper. Tests assert warnings; S04 decides how to surface them (eprintln or a real logger — a separate decision).

**Duplicate keybinds:** validate as data too — `validate_keybinds(&Keybinds) -> Vec<KeybindConflict>`. At load: warn, keep values (never block launch). S04's rebind flow consumes the same function to *reject* a duplicate assignment with the duplicate warning. One function, two call sites, testable without UI.

**Range handling:** clamp + warn (fov 30–120, sensitivity 0.0001–0.01 suggested) rather than reject — a bad number must not block launch, and clamping keeps the value usable.

## Implementation Landscape

### File format (proposed)

```toml
[keybinds]
forward = "KeyW"
backward = "KeyS"
left = "KeyA"
right = "KeyD"
jump_or_up = "Space"
crouch_or_down = "ShiftLeft"
sprint = "ControlLeft"

[mouse]
sensitivity = 0.0022

[video]
fov = 75.0
fullscreen = false
vsync = false
```

### Settings struct (proposed API)

```rust
pub struct Settings {
    pub keybinds: Keybinds,       // 7 KeyCodes: forward, backward, left, right,
                                  // jump_or_up, crouch_or_down, sprint
    pub mouse_sensitivity: f32,  // default 0.0022 (matches main.rs today)
    pub fov_degrees: f32,        // default 75.0 (matches camera.rs today)
    pub fullscreen: bool,        // default false
    pub vsync: bool,             // default false (current ctx.rs prefers Immediate)
}

impl Settings {
    pub fn load(path: &Path) -> Settings;
    pub fn load_with_warnings(path: &Path) -> (Settings, Vec<SettingsWarning>);
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error>; // atomic: tmp+rename like world save
    pub fn validate_keybinds(&self) -> Vec<KeybindConflict>;
}

// keybind.rs
pub fn key_to_string(code: KeyCode) -> &'static str; // "KeyW", "Space", "ShiftLeft"
pub fn parse_key(s: &str) -> Option<KeyCode>;
```

### Cargo.toml additions

- `serde = { version = "1", features = ["derive"] }` (Serialize derive for the save mirror struct)
- `toml = "1"` (current line is 1.1.8; `Value`/`from_str`/`to_string` confirmed)
- Optional dev-dependency `tempfile` for round-trip tests, or a hand-rolled unique-tmp-path helper (std::env::temp_dir + process id + counter). Either is fine; tempfile is the pragmatic standard.
- `settings.toml` should be added to `.gitignore` (world saves at `/saves` already are; a settings file written to the repo root during tests/manual runs would dirty git status).

### Key facts and constraints for the planner

1. **S02 must not depend on S01.** `winit::keyboard::KeyCode` is a plain enum, identical across winit 0.29/0.30, usable in lib code without any event loop — the settings module compiles and `cargo test` passes against winit 0.29 today and after S01's bump. Roadmap already marks `depends:[]` — keep it that way in task ordering.
2. **Semantic narrowing flag:** today main.rs treats `ShiftLeft | ShiftRight` as equivalent crouch and `ControlLeft | ControlRight` as equivalent sprint. The settings model is one KeyCode per action; once S04 wires it, the un-bound twin stops working. Acceptable (document it) — supporting key pairs would complicate the file format for little value.
3. **f32 vs f64:** toml floats are f64; Settings stores f32. Round-trip equality must be asserted on the parsed f32 values; toml's shortest-roundtrip float formatting preserves f32 precision fine.
4. **Supported keybind subset:** the explicit conversion table only needs keys a player could plausibly bind: letters KeyA–KeyZ, digits Digit0–9, Space, Shift/Control/Alt Left+Right, Tab, Enter, Escape, Backquote, arrows, F1–F12, maybe keypad. An unrecognized string in the file → warning + that field's default (per-field fallback again).
5. **Atomic save:** reuse the tmp-file + rename pattern from src/world/persistence.rs so a crash mid-save can't corrupt settings.
6. **S04 (not S02) wires the touchpoints** — sensitivity in main.rs MouseMotion, FOV in Camera construction, vsync→present-mode selection in RenderContext, fullscreen via window. S02's job is that defaults equal today's hardcoded values so the S04 swap is behavior-neutral. RenderContext currently hardcodes the preference *before* surface capabilities are known — S04 will need the choice deferred; that's S03/S04 territory, not S02's.
7. **Observability skill note:** warnings are decision-point data (which field fell back and why) — keep each warning structured enough for S04 to display it (field name + reason string), but return-by-value; no log framework this slice.

## Don't Hand-Roll

- TOML parsing/serialization — use the `toml` crate (two-phase Value load + serde-derive save mirror).
- Temp directories in tests — `tempfile` dev-dependency if desired (a tiny unique-path helper also works; avoid fixed filenames that break parallel test runs).

## Sources

- Cargo.toml, src/main.rs, src/player/physics.rs, src/player/camera.rs, src/render/context.rs, src/world/persistence.rs (read in full or in relevant sections)
- toml crate docs (docs.rs, Value/from_str API) + `cargo search toml` (current 1.1.8 line)
- MEM003 (keybind string representation decision), MEM001–MEM004 (M001 architecture decisions)
<!-- gsd:state-version=24:1 -->
