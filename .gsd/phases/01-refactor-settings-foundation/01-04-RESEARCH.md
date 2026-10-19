# S04 Research: In-game settings menu — rebind, adjust, persist

## Summary

S04 is a **wiring and integration slice**: every building block exists and is green — S02's Settings API (load_with_warnings/save/validate_keybinds/key_to_string/parse_key, all pure and unit-tested), S03's UI stack (UiRenderer with glyphon text + px-space quad renderer + color-only UI pass), and S01's migrated ApplicationHandler loop with `cursor_pos` plumbing already in `GameApp`. The new work is (1) a new `src/ui/menu.rs`-style SettingsMenu state machine that lays out rows in physical pixels, hit-tests `cursor_pos` for hover/click, and renders via the existing `set_quads` + glyphon TextArea prepare path; (2) re-routing `main.rs` input through the loaded `Settings` (keybinds, sensitivity, FOV, fullscreen, vsync) with live application; (3) save-on-change persistence so the roadmap's rebind→quit→relaunch proof holds. No new dependencies; the largest unknown (immediate-mode menu layout/hit-testing in px) is pure CPU logic that can be unit-tested without GPU.

## Recommendation

Build the menu as a new pure-logic module (layout + hit-test + state machine, unit-testable), then wire it into `main.rs` in one integration task, then a live-application task (sensitivity/FOV/fullscreen/vsync applied on change + save-on-change). Order tasks: (T1) menu model/layout/hit-test pure logic + unit tests → (T2) keybind routing through Settings + press-to-rebind flow + duplicate warning → (T3) video settings live-apply + fullscreen/vsync + save-on-change → (T4) launch smoke + restart-persistence automation. The interactive rebind proof (press Shift while menu open) is NEEDS-HUMAN; file-based restart persistence and non-blocking-launch can be automated.

## Implementation Landscape

### What exists (do not rebuild)

- **S02 (src/settings/mod.rs, 855 lines)**: `Settings { keybinds: Keybinds(7 actions), mouse_sensitivity, fov_degrees, fullscreen, vsync }`, `load_with_warnings(path) -> (Settings, Vec<SettingsWarning>)`, `Settings::save(path)` (atomic write), `Settings::validate_keybinds() -> Vec<KeybindConflict{actions, key}>`, `DEFAULT_SETTINGS_PATH = "settings.toml"` (cwd-relative). Defaults equal today's hardcoded values, so wiring is behavior-neutral. Clamping happens at load only: sensitivity [0.0001, 0.01], FOV [30, 120] — **the menu must clamp adjustments itself** (setting fov_degrees = 200 in memory then saving is NOT sanitized until reload).
- **S02 (src/settings/keybind.rs)**: `KEY_TABLE: &[(KeyCode, &str)]` subset; `key_to_string`/`parse_key`. A KeyCode outside the subset returns `None` from `key_to_string` and **save() panics** on unmapped keys — the menu's rebind capture MUST reject keys where `key_to_string(code).is_none()` with an "unsupported key" message.
- **S03 (src/ui/)**: `UiRenderer::set_quads(device, &[Quad], w, h)` rebuilds the vertex buffer per frame (empty list = nothing drawn); `StatsOverlay` shows the reusable-Buffer pattern (`set_text` only when text changes — 0.1–1.3 ms per shape); UiPass trait draws all quads then all glyphon text (text on top of panels). main.rs `prepare` call takes an **array of TextAreas** — passing `[stats_area, menu_area]` puts both in the UI pass with no pipeline change.
- **S01 (src/main.rs, 573 lines)**: `GameApp` with `cursor_pos: Option<(f32,f32)>` (physical px, top-left origin, y-down — same space as Quad and glyphon TextArea left/top), `cursor_grabbed: bool`, `set_cursor_grab(window, bool)` helper already exists, `InputState::reset()` exists, KeyboardInput match on `PhysicalKey::Code` with hardcoded KeyCode arms, `MouseInput` handler re-grabs cursor when `!cursor_grabbed` (must be gated on menu-open), `device_event` MouseMotion hardcodes `sensitivity = 0.0022`, `Camera.fov_y_radians` is a plain pub f32 field, `Camera::new` defaults 75°.

### What is missing (S04's deliverables)

- A menu state machine: closed → open → (row hovered → clicked) → rebinding-capture | adjusting. Immediate-mode: each frame build row rects from constants, hit-test `cursor_pos`, emit Quads + text buffer.
- Keybind routing: replace the 7 hardcoded `KeyCode::KeyW => ...` arms with `if code == settings.keybinds.<action>` comparisons (non-rebindable keys F/F5/digits/brackets/Escape stay hardcoded).
- Live apply: sensitivity (device_event), FOV (camera.fov_y_radians on change), fullscreen (`window.set_fullscreen(Some(Fullscreen::Borderless(None)))/None` — winit 0.30, window is `Arc<Window>`), vsync (set `ctx.config.present_mode` to Fifo/Immediate and `surface.configure` — `ctx.config` is pub; same mechanism as `resize`).
- Load at startup + save on each change; surface `SettingsWarning`s in the menu.
- Menu-open interaction design (Escape currently toggles cursor grab — the natural move is Escape toggles the menu: open ⇒ `set_cursor_grab(false)` + `input.reset()`, close ⇒ re-grab).

### Layout & rendering approach (recommended)

One glyphon `Buffer` per menu instance holding all lines (set_text with `\n`-joined rows, reshaped only when the text changed — same pattern as StatsOverlay), positioned by a single `TextArea { left, top, .. }`. Rows are `LINE_HEIGHT` (22.0) tall starting at a menu origin; each row also gets hover/selection Quads from pure layout math. Colors: white text default, hovered row gets a translucent highlight quad; the whole menu sits on a dark panel quad. All coordinates in physical px — no scale-factor math (MEM012 confirms cursor/quads/ctx.config/glyphon all share px at scale 1).

### Natural task seams

1. **T1 — Menu model + layout + hit-test (pure, no GPU)**: new `src/ui/menu.rs` (or `src/ui/settings_menu.rs`) — row list (7 keybinds + sensitivity + FOV + fullscreen + vsync + close), `row_rect(index) -> Quad geometry`, `hit_test(cursor, rect) -> bool`, state enum (Closed/Open/AwaitingKey{action}/Warning{msg, expiry}), clamped adjust helpers (sensitivity step, FOV step within [min,max]), candidate-duplicate check. Unit tests like S03's quad tests: layout determinism, hit-test in/out/boundary, clamping, duplicate detection, rebind accept/reject transitions.
2. **T2 — Input routing + rebind flow in main.rs**: load settings at `GameApp::new` (store `settings: Settings` + startup warnings), route the 7 keybind arms through `settings.keybinds`, route KeyboardInput to the menu first when open (AwaitingKey captures any KeyCode; Escape cancels capture or closes menu), duplicate reject via `validate_keybinds()` on a candidate clone (or a direct scan for another action holding the key), save on every accepted rebind. Gate the `MouseInput` re-grab logic on `menu.is_open()` so clicks hit-test rows instead.
3. **T3 — Live application + persistence of video settings**: sensitivity from `settings.mouse_sensitivity` in device_event; FOV applied to `camera.fov_y_radians` (radians = fov_degrees.to_radians()); fullscreen via `window.set_fullscreen`; vsync via `ctx.config.present_mode` swap + `surface.configure`; save on every change; also save settings on CloseRequested (crash-safety, one line).
4. **T4 — Verification**: extend/clone `scripts/launch_smoke.sh` (MEM006: it auto-attaches WAYLAND_DISPLAY=wayland-0/XDG_RUNTIME_DIR when shell lacks them; reuse, don't reinvent) — e.g. write a known-good `settings.toml` to the smoke cwd, bounded run, assert process survived (exit 124, no panic) and file still parses. Interactive rebind + visual menu legibility = NEEDS-HUMAN (same disposition as S03's overlay legibility).

## First Proof

**The rebind capture + duplicate-reject state machine against a real keyboard event** is the highest-risk integration (it rides the KeyboardInput match that S01 rewrote). Cheapest early proof: unit tests of the pure menu state machine (T1) prove transitions; then a smoke run with the menu forced-open at startup (temporary const or env for the smoke) proves the menu renders and the UI pass draws it without pipeline errors — that retires the render-path risk before the full wiring lands.

## Verification

- `cargo build` clean on slice-touched files (S03's bar: 0 warnings on the slice surface; 13 inherited mesh/world/texture clippy warnings are out of scope per MEM007/MEM017).
- `cargo test`: existing 59 + new pure menu-logic tests (layout, hit-test boundaries, clamping, duplicate rejection, rebind state transitions, warnings formatting).
- `cargo clippy`: exit 0, zero findings on `src/ui/menu.rs` + `src/main.rs` surface.
- `scripts/launch_smoke.sh` (or a variant): bounded 10 s run, exit 124, no "panicked" in output; ideally with a pre-seeded settings.toml.
- Restart-persistence (automatable half): pre-write `settings.toml` with `jump_or_up = "ShiftLeft"`, launch smoke, confirm file survives and still parses; the interactive half (actually pressing Shift in the menu) is NEEDS-HUMAN UAT.

## Constraints & Gotchas

- **MEM014**: glyphon render must go through the `UiPass` trait (`draw_ui<'pass>`), not a closure — already implemented on UiRenderer; multiple TextAreas are passed as a slice to `text_renderer.prepare(...)`, drawn in array order (menu area after stats area so menu text draws on top where panels overlap visually — they shouldn't overlap, but order is free).
- **MEM016**: match cursor events with `WindowEvent::CursorLeft { .. }` (device_id field) — already correct in main.rs; don't regress.
- **Keybind narrowing is intentional** (documented in S02's Keybinds doc comment): today `ShiftLeft|ShiftRight` both crouch; after S04 only the bound key works. Planner should not "fix" this; it's the documented decision.
- **Save panics on unmapped KeyCodes** (S02 by design, test-guarded): rebind capture must filter via `key_to_string(code).is_some()`.
- **In-memory settings are unclamped**: only load clamps (sensitivity/FOV). Menu adjustments must clamp to the same ranges [0.0001, 0.01] / [30.0, 120.0] before save — consider reusing the constants (they're private; either make them pub or duplicate the values in the menu module — prefer making them pub to keep one source of truth).
- **Menu-open vs cursor re-grab**: `WindowEvent::MouseInput` currently re-grabs the cursor whenever `!cursor_grabbed` — when the menu is open (cursor released), clicks must route to the menu instead, or the first menu click hides the pointer and breaks the menu. This is the most likely behavioral regression of the slice.
- **vsync toggle on Wayland**: `PresentMode::Immediate` is often unavailable (current code falls back to Mailbox/Fifo); flipping `vsync=true → Fifo` is always safe, but `vsync=false` may be a visual no-op if the compositor only supports Fifo. Acceptable; document, don't fight the compositor.
- **Fullscreen triggers Resized**: `window.set_fullscreen` delivers Resized → existing `ctx.resize` + stats overlay re-shape path handles it; the menu layout should derive from `ctx.config.width/height` per frame (immediate-mode) so it reflows automatically.
- **settings.toml is cwd-relative** (`DEFAULT_SETTINGS_PATH`): launched via `cargo run` from the crate root it lands next to Cargo.toml; `/settings.toml` is already gitignored (S02). Smoke scripts must use a controlled cwd so they don't clobber a real file — follow `launch_smoke.sh`'s /tmp discipline.
- **Crash-safe ordering**: save immediately on each accepted change (atomic rename already), not deferred to exit — a later panic can't lose the player's rebind.
- **Warnings surfacing (MEM009)**: startup `SettingsWarning`s are data; show them as initial menu text lines (e.g. first open) rather than a logging framework — the crate is log-free by convention (D006).
- **KeyboardInput routing order**: when the menu is open (especially AwaitingKey), gameplay keys must NOT also set InputState — route to menu first and `return`, or gate the gameplay match on `!menu_open`. Reset InputState on open (roadmap requirement) and on close re-grab.

## Skills Discovered

- `npx skills find` is blocked by this unit's tools policy (planning-dispatch, read-only bash); no external skill discovery performed. All technologies (winit 0.30, wgpu 0.19, glyphon 0.5, serde/toml) are already established locally with prior-slice patterns and memory notes — no new core technology requires a skill.
- The loaded `observability` skill is not directly applicable (this is a player-facing interactive UI, not an unattended process); its one relevant echo is S02's design: warnings/conflicts already surface as structured data for the menu to display, and save failures return `io::Error` to the caller rather than being swallowed — keep that explicit-failure style in the menu wiring (e.g. surface a save error in the menu instead of ignoring it).
