---
id: S04
parent: M001
milestone: M001
provides:
  - In-game settings menu over the gameplay view (open via Escape, cursor release + InputState reset, close re-grabs)
  - Press-to-rebind for the 7 gameplay keybinds with duplicate-conflict and unsupported-key rejection shown as in-menu messages
  - Live-adjustable mouse sensitivity / FOV / fullscreen / vsync applied to the running pipeline and camera
  - settings.toml persistence on every accepted change plus save-on-quit, atomic and crash-safe; restart persistence proven by settings_smoke.sh
  - The milestone's user-facing settings capability and its end-to-end restart-persistence automatable proof
requires:
  - slice: S01
    provides: the migrated ApplicationHandler loop, Arc<Window>, and input routing (Escape, KeyboardInput, MouseInput, MouseMotion)
  - slice: S02
    provides: the Settings API (load_with_warnings with per-field fallback, atomic save, validate_keybinds, key_to_string, clamp constants now pub)
  - slice: S03
    provides: the src/ui module (glyphon stack, quad px-space API, color-only UI pass, cursor_pos plumbing)
affects:
  []
key_files:
  - src/ui/menu.rs
  - src/ui/mod.rs
  - src/settings/mod.rs
  - src/main.rs
  - scripts/settings_smoke.sh
key_decisions:
  - MEM019: menu-first keyboard routing with utility keys matched before gameplay routes — Escape always toggles the menu even if a gameplay action is rebound onto it (Escape is a valid rebind target)
  - P005/MEM020 borrow-split live apply: fullscreen/FOV inside menu_click, vsync present-mode+configure in the MouseInput caller after a pre-call hit-test; save_settings_or_warn as a free fn with disjoint (menu, settings) args
  - Loaded Settings is the single source of truth for video parameters: startup applies FOV/fullscreen/vsync from settings, not just menu-change triggers (guarded by default_settings_fov_matches_camera_default_fov)
  - Save-on-every-accepted-change (atomic tmp-sibling + rename, crash-safe) plus one final save on CloseRequested; save errors surface as in-menu messages, never swallowed, except the exiting-process CloseRequested save which ignores errors by design (documented in code)
patterns_established:
  - Pure immediate-mode menu module: state machine + layout + hit-test + decisions as pure CPU logic with caller-owned tick counter for message expiry (no clock in the module), unit-tested before wiring
  - Menu-first input routing: while the menu is open every KeyboardInput/MouseInput routes to the menu and returns before gameplay handling — capture candidates can never reach InputState
  - Per-frame layout derivation from ctx.config.width/height (no cached rects) so any Resized/fullscreen reflow needs no invalidation path
  - Change-gated save: one atomic tmp-sibling+rename write per accepted human-paced change, never per frame; save errors surfaced as in-menu text
observability_surfaces:
  - none
drill_down_paths:
  []
duration: ""
verification_result: passed
completed_at: 2026-10-19T10:54:01.843Z
blocker_discovered: false
---

# S04: In-game settings menu: rebind, adjust, persist

**A pure-logic settings menu (src/ui/menu.rs) wired into the running game: Escape opens it with cursor release + InputState reset, gameplay keys route through the loaded Settings keybind map with press-to-rebind (duplicate/unsupported rejection, atomic save-on-accept), and sensitivity/FOV/fullscreen/vsync apply live and persist to settings.toml on every accepted change — seeded-file restart persistence proven by smoke, interactive rebind is NEEDS-HUMAN UAT.**

## What Happened

Five tasks delivered the slice. T01 added the pure-logic menu state machine in a new src/ui/menu.rs (873 lines): MenuState (Closed/Open/AwaitingKey + tick-counter message expiry), a 12-row MenuRow model (7 keybinds, sensitivity, FOV, fullscreen, vsync, close), deterministic px row layout derived from surface dims, half-open hit_test, clamped adjust helpers (in-memory Settings stay unclamped; clamp before save), evaluate_rebind_candidate (RejectUnsupported before duplicate check because save() panics on unmapped KeyCodes; RejectDuplicate names the conflicting action, consistent with validate_keybinds), and format_warnings — 24 pure unit tests, module imports only winit KeyCode + crate types.

T02 wired it into main.rs: startup load_with_warnings (never blocks launch; warnings stored and shown as the menu's first-open message), Escape toggles the menu (open releases cursor grab + InputState::reset, close re-grabs), MouseInput gated menu-first so the legacy re-grab block can never steal a menu click, rows hit-tested via hovered_row, and the menu (panel + hover-highlight quads + 12 text lines + capture hint + transient message) rides the existing color-only UI pass with change-gated text shaping; the message tick is bounded at ~60/s so ControlFlow::Poll cannot flash warnings away.

T03 replaced the 7 hardcoded gameplay KeyCode arms with routing through the loaded keybind map via a pure apply_gameplay_key helper (press/release both route; defaults equal the old hardcoded values so the swap is behavior-neutral), and delivered press-to-rebind: any pressed key in AwaitingKey goes through evaluate_rebind_candidate — Accept applies, saves atomically immediately, finishes capture; rejections show in-menu warnings; Escape cancels capture or closes. Routing order is utility-keys-first so Escape always toggles the menu even if a gameplay action is rebound onto it (MEM019).

T04 made loaded Settings the live source of truth: device_event MouseMotion uses settings.mouse_sensitivity (hardcoded 0.0022 gone — absence-checked), FOV clicks clamp and write camera.fov_y_radians live (also applied at startup), fullscreen toggles via window.set_fullscreen with per-frame layout reflow surviving Resized, vsync toggles ctx.config.present_mode + surface.configure (Wayland Immediate-unavailable documented as accepted no-op), every accepted change saves immediately, plus a final save on CloseRequested. The borrow-unfriendly rows are split per MEM020/P005.

T05 added scripts/settings_smoke.sh: bounded 10 s run in a temp cwd seeded with jump_or_up="ShiftLeft" settings.toml, asserting exit 124, no panic text, and byte-identical file survival (the game must not rewrite an unchanged settings file) — the automatable half of the roadmap restart-persistence proof — and ran the full gate suite green.

Integration closure: T01's pure machine is consumed by T02's wiring, fed by T03's routing/capture flow, made live by T04's source-of-truth changes, and proven wired end-to-end by T05's smokes — all in the single crate, no downstream slices yet.

## Verification

All five slice gates green via gsd_exec evidence: (1) cargo build — exit 0, no warnings on the slice surface [gsd_exec 54060fc6]; (2) cargo test — 94/94 passed, 0 failed, 0 ignored (83 lib incl. 24 new menu tests + 11 src/main.rs menu/routing tests) [gsd_exec 03185386]; (3) cargo clippy — exit 0, zero findings on src/ui/menu.rs, src/ui/mod.rs, src/settings/mod.rs, src/main.rs; remaining findings are inherited mesh/world lints out of scope per MEM007/MEM017 [gsd_exec 03185386]; (4) bash scripts/launch_smoke.sh — SMOKE PASS, 10 s bounded run, exit 124, no panic, no stray settings.toml in cwd [gsd_exec 60eec96f]; (5) bash scripts/settings_smoke.sh — SMOKE PASS, seeded settings.toml (jump_or_up=ShiftLeft) survived a bounded run byte-identical, exit 124, no panic [gsd_exec e122c732]. Wiring seams confirmed by inspection: startup load_with_warnings, input.reset() on menu open, apply_gameplay_key routing, evaluate_rebind_candidate capture, apply_vsync/set_fullscreen live application, save at every accept site + CloseRequested [gsd_exec 430fa96e]. Interactive press-to-rebind / legibility / live-apply / interactive restart persistence are NEEDS-HUMAN UAT (recorded in uatContent for run-uat).

## Requirements Advanced

None.

## Requirements Validated

None.

## New Requirements Surfaced

None.

## Requirements Invalidated or Re-scoped

None.

## Operational Readiness

None.

## Deviations

None.

## Known Limitations

"Interactive press-to-rebind, menu legibility, visible live-apply, and interactive restart persistence remain NEEDS-HUMAN UAT (documented in the UAT human-checks section; the automatable halves are proven by 24 menu unit tests + settings_smoke.sh). Wayland with vsync=false may fall back (PresentMode::Immediate unavailable) — accepted visual no-op, documented in apply_vsync. ShiftRight/ControlRight no longer crouch/sprint — intentional narrowing documented in S02's Keybinds rationale. Duplicate bindings in a hand-edited settings.toml load with a warning (never block launch) and resolve first-match in declaration order."

## Follow-ups

None.

## Files Created/Modified

None.
<!-- gsd:state-version=192:1 -->
