---
id: S04-T02
parent: S04
milestone: M001
key_files:
  - (none)
key_decisions:
  - (none)
duration:
verification_result: passed
completed_at:
blocker_discovered: false
---

# S04-T02: Wired the T01 menu state machine into main.rs: startup settings load with in-menu warnings, Escape open/close with cursor release + InputState reset, menu-first MouseInput gating with row hit-testing, and menu panel/hover/text rendering riding the existing UI pass

**Wired the T01 menu state machine into main.rs: startup settings load with in-menu warnings, Escape open/close with cursor release + InputState reset, menu-first MouseInput gating with row hit-testing, and menu panel/hover/text rendering riding the existing UI pass**

## What Happened

Wired the S04-T01 pure menu state machine into the running game in src/main.rs, exactly per the task plan's 6 Do items:

1. Startup settings load: `GameApp::new` calls `settings::load_with_warnings(Path::new(DEFAULT_SETTINGS_PATH))` — never blocks launch (missing/corrupt settings.toml → defaults + `Vec<SettingsWarning>`, both stored on GameApp as `settings` + `startup_warnings`).
2. Escape now toggles the menu (replacing the old raw cursor-grab toggle): open ⇒ `set_cursor_grab(false)` + `self.cursor_grabbed = false` + `self.input.reset()` (roadmap requirement); close ⇒ re-grab. Escape in `AwaitingKey` cancels the capture first (menu stays open) — a small forward-compatibility choice matching the final UX T03 refines.
3. MouseInput gating: the menu-open check runs BEFORE the legacy click-to-re-grab block, so the re-grab can never steal a menu click; when open, clicks route through `GameApp::menu_click` → `menu::hovered_row` hit-test (keybind row → `begin_rebind`; sensitivity/FOV rows → left=up/right=down via T01 clamped helpers, mandatory since in-memory Settings are unclamped; fullscreen/vsync → toggle; Close row → close + re-grab; empty space or `None` cursor → no-op).
4. Rendering rides the existing UI pass with no pipeline change (MEM014/P004): when open, the quad list grows to stats panel + dark menu panel (`menu::panel_rect`) + translucent hover highlight (`menu::hovered_row` → `row_rect`), all derived per frame from `ctx.config.width/height` so a Resized/fullscreen reflow needs no cached rects; menu text (12 row lines "label: value" + capture hint + transient message) shapes into a second `StatsOverlay` buffer with the same change-gating (set_text only when the composed string changed) and its TextArea is passed AFTER the stats area in the same `prepare` areas array.
5. Startup `SettingsWarning`s surface as the menu's initial message on first open (MEM009/D006: data in-menu, never logged); accepted value-row changes save atomically immediately and a save `io::Error` surfaces as an in-menu message via `save_settings_or_warn` instead of being swallowed.
6. Warning expiry: the T01 pure tick counter is decremented per frame but bounded at ~60 ticks/s (`MENU_TICK_INTERVAL = 1/60` via an `Instant` timer) — ControlFlow::Poll can run far above 60 fps and MESSAGE_TICKS is calibrated as ~10 s at 60 Hz, so unbounded per-frame ticking would flash warnings away in under a second.

New unit tests (5, in src/main.rs `#[cfg(test)]`, pure — no GPU/events): full row listing with live values, changed-settings display, awaiting-key capture hint + message-line append, startup-warnings single-line join composition, and the Left/Right/other click-to-direction mapping.

Key decision: `compose_menu_text` is a free function taking `(&Settings, &SettingsMenu)` and `menu_click` copies surface dims + clones the `Arc<Window>` before the `&mut self` call — whole-`self` borrows conflict with the live `gpu`/`window` field borrows from the `window_event` destructuring (captured as MEM018/P005).

## Failure Modes (Q5)

External dependencies and their failure paths:
- settings.toml read (startup): missing file, unreadable, or syntactically invalid → `load_with_warnings` returns defaults + structured per-field warnings; launch never blocks (verified by launch_smoke running with no settings.toml present). Covered by existing settings/mod.rs tests (missing-file, invalid-TOML, wrong-type, out-of-range fallbacks).
- settings.toml write (on accepted menu changes): `io::Error` (permissions, disk full) → surfaced as an in-menu message via `save_settings_or_warn` ("could not save settings: {err}"), not swallowed; the atomic tmp-sibling+rename write (P002) prevents partial-file corruption.
- Cursor position unknown (`CursorLeft`, click without prior CursorMoved): `menu_click` early-returns on `None` cursor — no click action, no gameplay raycast leak.
- Click outside the panel: `hovered_row` returns None → no-op; the gameplay raycast and re-grab paths are unreachable while the menu is open (gate placed before both).
- glyphon prepare failure (atlas full): existing behavior — UI pass skipped for that frame.
- Startup warnings: shown once on first open, expire via the bounded tick — cannot stick forever.

## Load Profile (Q6)

Per-frame menu cost while open is O(1) in surface content: ≤3 extra quads (18 vertices) rebuilt from pure CPU math, a 12-rect hit-test scan, and a text re-shape that only runs when the composed string actually changes (StatsOverlay change-gating) — the expensive path (glyphon reshape) never runs per frame. The message tick is capped at ~60/s. Nothing grows with session length or interaction count; at 10x expected load (more opens/closes, more hover movement) nothing saturates — this is a single-window CPU menu. The only pool-like resource is the glyph atlas, guarded by the existing prepare-error frame skip.

## Negative Tests (Q7)

- src/main.rs (new): `startup_warnings_format_into_a_single_message_line` (multi-warning display formatting), `adjust_direction_maps_left_up_right_down_and_nothing_else` (Middle/Back buttons rejected as no-ops), `menu_text_lists_every_row_with_its_live_value` (asserts exactly ROW_COUNT lines — no capture/message lines leak in when merely open), `awaiting_key_row_shows_capture_hint_and_message_line_appends` (capture hint replaces the bound-key value; message appends as final line).
- src/ui/menu.rs (T01, wired by this task): hit-test in/out/half-open boundaries, sensitivity/FOV clamping both directions + boundary acceptance + pin-at-bound, duplicate-key and unsupported-key rebind rejection, out-of-range row indices, narrow-surface layout clamping.
- src/settings/mod.rs: missing-file/invalid-TOML/wrong-type/out-of-range load fallbacks, save→load round-trip.
- scripts/launch_smoke.sh: bounded run with no settings.toml in cwd — proves the missing-file load path compiles into and survives a real launch (exit 124, no panic), and confirms no stray settings.toml write without interaction.

## Verification

cargo build clean (exit 0); cargo test fully green (lib suite + 5 new pure unit tests in src/main.rs covering menu text composition, capture hints, warning formatting, and click-direction mapping); cargo clippy exit 0 with ZERO findings attributable to src/main.rs (verified by grepping the full clippy output — the 12 remaining warnings are the inherited mesh/texture lib warnings, out of scope per MEM007/MEM017); bash scripts/launch_smoke.sh prints SMOKE PASS (10s bounded run, exit 124, no panic — the game launches with settings loading and menu code paths compiled in; no settings.toml written without menu interaction, confirmed after the run). Interactive menu open/close/rebind remains NEEDS-HUMAN UAT per the slice proof plan (recorded in T05's scope).

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | passed | 1048ms |
| 2 | `cargo test` | 0 | passed | 315ms |
| 3 | `cargo clippy` | 0 | passed | 228ms |
| 4 | `bash scripts/launch_smoke.sh` | 0 | passed | 10045ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

None.
<!-- gsd:state-version=160:1 -->
