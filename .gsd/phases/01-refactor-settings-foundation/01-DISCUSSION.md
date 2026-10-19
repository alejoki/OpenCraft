# M001 Discussion Log

## Exchange — 2026-10-19T06:22:56.080Z

### winit version

Upgrade winit to the current API during the M001 refactor, or stay on 0.29?

- **Upgrade winit now (Recommended)** — Rewrite the event loop onto the current ApplicationHandler API during the M001 refactor — we're restructuring that code anyway, so we avoid building on a deprecated API.
- **Stay on 0.29** — Keep the deprecated closure API; zero migration risk now, but the refactor is built on an API upstream no longer supports and any future winit feature lands on 0.30+ only.

**Selected:** Upgrade winit now (Recommended)

### Config format

How should the settings config file be formatted?

- **TOML via serde (Recommended)** — Human-readable TOML file, standard Rust ecosystem choice, first serde use in the project. Settings load/save becomes a struct round-trip.
- **Hand-rolled key=value** — Consistent with the project's hand-rolled persistence style; no new deps, but parsing/validation/round-trip code is written and maintained by hand.

**Selected:** TOML via serde (Recommended)

### Keybind UI

How deep should keybind remapping go in M001?

- **Press-to-rebind menu (Recommended)** — Menu lists actions; click one, press a key to bind it. Basic duplicate warning. Simple to build, covers the real use case.
- **Config-file-only rebind** — Menu shows current bindings as static list; changing them means editing settings.toml by hand. Cheapest, weakest UX.
- **Full editor + conflicts** — Press-to-rebind plus conflict detection, multi-bind actions, reset-to-default per action. More UI work in M001.

**Selected:** Press-to-rebind menu (Recommended)
---

## Exchange — 2026-10-19T06:28:56.462Z

### Depth Check

Did I capture the depth right?

- **Yes, you got it (Recommended)** — The full picture — vision, scope, architecture, error handling, quality bar — matches what we discussed; proceed to requirements and roadmap.
- **Not quite — let me clarify** — Something is off or missing — you'll clarify before we write anything.

**Selected:** Yes, you got it (Recommended)
---

## Exchange — 2026-10-19T06:41:18.182Z

### M001 goal

What is M001's core deliverable — what should be true when it ships?

- **Refactor + settings (Recommended)** — Migrate the event loop to winit's ApplicationHandler, add TOML settings persistence, and ship the press-to-rebind keybind menu. Player-visible outcome: settings survive restarts and keys can be rebound in-game.
- **Refactor only** — Just the winit migration and internal cleanup; settings/keybinds move to a later milestone.
- **Refactor + gameplay too** — Everything in 'Refactor + settings', plus one new gameplay feature on top (e.g. block interaction or world-gen improvement).

**Selected:** Refactor + settings (Recommended)

### Settings scope

Which settings should M001's config system actually manage?

- **Input + video (Recommended)** — Keybinds, mouse sensitivity, FOV, fullscreen/vsync toggle — the settings a player actually reaches for first.
- **Keybinds only** — Only key remapping persists in M001; sensitivity/FOV/video settings come later.
- **Broad set** — Also include gameplay-affecting settings like render distance or world options.

**Selected:** Input + video (Recommended)

### wgpu upgrade

Cargo.toml shows wgpu 0.19 is also aging. Should M001 bump it too, or winit only?

- **winit only (Recommended)** — winit and wgpu are decoupled — upgrade winit now, keep wgpu 0.19. Smaller risk surface per migration.
- **Both together** — Modernize both at once — more churn now, but you don't touch migration again later.

**Selected:** winit only (Recommended)
---

## Exchange — 2026-10-19T06:46:37.266Z

### Menu UI

There's no UI layer in the codebase yet — how should the settings/keybind menu be built?

- **Hand-rolled wgpu UI (Recommended)** — Small immediate-mode UI written directly against the existing wgpu pipeline — panels, buttons, key-capture prompts. No new dependency; matches the project's hand-rolled style.
- **egui integration** — Integrate egui/egui-wgpu for menus — full widget set and text rendering out of the box, but a significant new dependency and render-pass integration.
- **Keyboard-only menu** — No mouse UI — menu is a keyboard-driven list (arrows to move, Enter to select, press key to rebind), rendered with minimal text drawing.

**Selected:** Hand-rolled wgpu UI (Recommended)

### Bad config

If settings.toml is missing, corrupt, or has invalid values — what should happen?

- **Per-field fallback (Recommended)** — Parse failures or unknown keys fall back to defaults for just those fields, valid values are kept, and the corrected file is rewritten on next save. Game always launches.
- **All-or-nothing** — Any parse error discards the whole file and starts from defaults (original moved to settings.toml.bak). Simpler code, but one typo nukes all settings.
- **Fail loudly** — Show an error and exit — loudest failure, forces the user to fix the file, but the game never silently changes their config.

**Selected:** Per-field fallback (Recommended)

### Acceptance

What proof level should M001 acceptance require?

- **Tests + manual smoke (Recommended)** — Automated unit tests for settings load/save/fallback round-trips (pure logic, runs in CI), plus a manual in-game smoke check for menu, rebinding, and no gameplay regressions.
- **Manual only** — No automated tests this milestone — everything verified by playing the game. Fastest, weakest regression safety.
- **Tests + harness** — Automated tests plus a headless integration harness that exercises the event loop and settings lifecycle without a window.

**Selected:** Tests + manual smoke (Recommended)
---

## Exchange — 2026-10-19T06:47:04.257Z

### Depth Check

Did I capture the depth right?

- **Yes, you got it (Recommended)** — The full picture — vision, scope, architecture, error handling, quality bar — matches what we discussed; proceed to write the M001 context file.
- **Not quite — let me clarify** — Something is off or missing — you'll clarify before anything is written.

**Selected:** Yes, you got it (Recommended)
---

<!-- gsd:state-version=43:1 -->
