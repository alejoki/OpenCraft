---
id: M001
title: "Refactor + Settings Foundation"
status: complete
completed_at: 2026-10-19T13:12:59.323Z
key_decisions:
  - D001: pin winit 0.30.13 (stable ApplicationHandler line), never 0.31.0-beta pre-releases
  - D002: pin glyphon =0.5.0 (its wgpu ^0.19 pin; 0.6+ jumped to wgpu ^22/^23 and would force an out-of-scope upgrade)
  - D003: keybinds as hand-mapped stable strings in settings.toml, decoupled from winit serde
  - D004: slice order S01→S02→S03→S04, risk-first with S04 as end-to-end integration
  - D005: two-phase per-field-fallback TOML load (never serde-derive load)
  - D006: warnings/keybind conflicts returned as structured data, no logging dependency
  - D007: optional color-only UI render pass on the existing encoder (shipped as the lifetime-typed UiPass trait instead of a FnMut closure — wgpu 0.19 lifetime constraint, same seam)
  - D008/MEM027: loaded Settings is the single source of truth for video parameters, applied at startup
  - D009/MEM028: atomic save on every accepted change plus save-on-quit
key_files:
  - src/main.rs
  - src/lib.rs
  - src/render/pipeline.rs
  - src/settings/mod.rs
  - src/settings/keybind.rs
  - src/ui/mod.rs
  - src/ui/menu.rs
  - src/ui/text.rs
  - src/ui/quad.rs
  - scripts/launch_smoke.sh
  - scripts/settings_smoke.sh
  - Cargo.toml
  - Cargo.lock
lessons_learned:
  - Agent-shell GUI smokes need the Wayland socket attached explicitly (WAYLAND_DISPLAY), not Xwayland :0 — launch_smoke.sh encodes this (MEM006)
  - Inherited lint scoping: slices must be clippy-clean on their own touched surface and document/defer out-of-scope pre-existing warnings (MEM007/MEM017)
  - Check the shared dependency graph before a platform bump: rwh 0.6.2 shared by winit 0.30.13 and wgpu 0.19.4 made src/render/context.rs and src/lib.rs zero-edit
  - Pure-logic-first module design (settings, quads, menu) made GPU-independent behavior unit-testable and grew the suite 23→94 tests
  - Interactive UAT cannot be fabricated by automation: a milestone-level human session with a durable on-disk artifact (game-written settings.toml with non-default binding) closed the validation gap (MEM026)
  - Rebind validation must check key support before duplicates because save() panics on unmapped KeyCodes
  - wgpu 0.19 RenderPass lifetimes make a FnMut closure UI-pass hook uncompilable with glyphon 0.5.0 borrows — a lifetime-typed trait object is the working pattern
---

# M001: Refactor + Settings Foundation

**Migrated the game to winit 0.30.13's ApplicationHandler loop and delivered a resilient TOML settings layer, a glyphon stats overlay, and an in-game settings menu with press-to-rebind, live apply, and persistent atomic saves.**

## What Happened

M001 delivered the winit 0.30.13 ApplicationHandler migration and the game's first persisted preferences layer in four risk-ordered slices.

**S01 (winit migration)** moved the event loop from the deprecated closure `run()` to `ApplicationHandler` (GameApp/Ready structs, window+GPU init in `resumed()` via `ActiveEventLoop::create_window`, `Arc<Window>` surface for wgpu 0.19). The shared rwh 0.6.2 dependency between winit 0.30.13 and wgpu 0.19.4 meant src/render/context.rs and src/lib.rs needed zero edits. Gameplay parity (movement, jump/crouch/sprint, fly, save/load, grab/release, break/place, hotbar, render-distance, resize, close-save, focus-loss reset) was human-confirmed on 2026-10-19; a bounded 10-second launch smoke asserting exit-124/no-panic was added (scripts/launch_smoke.sh, with the Wayland-socket agent-shell gotcha MEM006 solved along the way).

**S02 (settings core)** built src/settings as pure logic: two-phase per-field-fallback TOML load (a bad settings.toml never blocks launch, valid fields survive invalid siblings, out-of-range values clamp with warnings), a Serialize-only mirror struct with atomic tmp-sibling+rename save, hand-mapped KeyCode↔stable-string conversion (decoupled from winit serde), and duplicate-keybind validation returned as data. 31 settings tests landed.

**S03 (UI foundation)** added glyphon 0.5.0 text rendering, a pixel-space quad API (px→NDC on CPU, unit-tested without GPU), and the optional color-only UI pass on the existing encoder — shipped as the lifetime-typed `UiPass` trait (`Option<&dyn UiPass>`) after the originally-planned `FnMut` closure hook proved uncompilable under wgpu 0.19's RenderPass lifetimes with glyphon borrows (D007 mechanism revision, same seam). The live stats overlay (FPS/frame ms/position/chunks) renders through it.

**S04 (settings menu)** wired everything: pure immediate-mode menu module (24 unit tests: state machine, layout, hit-test, decisions) wired into the migrated loop with Escape toggle, cursor release + InputState reset, menu-first input routing, press-to-rebind for the 7 gameplay keybinds with duplicate/unsupported rejection, live sensitivity/FOV/fullscreen/vsync apply (borrow-split design P005), and atomic save-on-accept plus save-on-quit. Loaded Settings became the single source of truth for video parameters, applied at startup. Human-confirmed interactive UAT produced a durable artifact: a game-written settings.toml with non-default `crouch_or_down = "Tab"`.

Validation passed at remediation round 3 (receipt revision 7): all four classes Contract/Integration/Operational/UAT PASS. Closeout verification initially recorded a false-negative: because the milestone branch had already been merged into master (merge 9050c16), the merge-base diff showed only `.gsd` auto-commits and the script aborted before evaluating the self-diff fallback. The re-run established the correct objective evidence — the pre-merge master parent 5b44ab5 → HEAD diff lists the 13 implementation files (Cargo.toml/lock, both smoke scripts, src/lib.rs, src/main.rs, src/render/pipeline.rs, src/settings/*, src/ui/*), and 16 of 28 milestone-scoped commits touch non-.gsd files — and the current tree re-verified green: cargo test 94/94 (83 lib + 11 bin), cargo build 0 warnings. The transient VERIFICATION-FAILED artifact was removed (git history retains it).

Structured learnings were extracted and persisted to the memory store (MEM001–MEM028: D001–D009 decisions, P001–P010 patterns, gotchas including the Wayland-socket smoke setup, rebind validation order, and the UiPass lifetime constraint); the audit trail lives in 01-LEARNINGS.md with surprises kept local. Zero requirements are recorded in the DB, so no requirement transitions were needed.

## Success Criteria Results

- **S01 — winit 0.30 ApplicationHandler loop launches with gameplay parity:** PASS. Launch smoke (bounded 10 s, exit-124/no-panic assertions) passed with no panic (a097f1b4); gameplay parity human-confirmed 2026-10-19 across movement, jump/crouch/sprint, fly, save, grab/release, break/place, hotbar, render-distance, resize, close-save, and focus-loss reset. Build gate clean.
- **S02 — Settings TOML round-trip and resilient loading:** PASS. 94/94 tests green on the current tree (83 lib + 11 bin, gsd_exec 2e0c5474): round-trip load→save→load equality, missing-file→defaults, corrupt-file per-field fallback with valid fields kept, unknown keys ignored, duplicate-keybind rejection.
- **S03 — Live stats overlay + quad rendering + cursor events:** PASS. Color-only second pass on the wgpu 0.19 pipeline via the UiPass trait; CursorMoved/CursorLeft delivery verified through the migrated loop; overlay legibility human-confirmed.
- **S04 — In-game settings menu with rebind/apply/persist:** PASS. Menu opens with cursor release + InputState reset; press-to-rebind with duplicate and unsupported-key rejection; sensitivity/FOV/fullscreen/vsync apply live; seeded settings round-trip smoke byte-identical (2c64b32d); restart persistence human-confirmed with durable artifact (game-written settings.toml with non-default `crouch_or_down = "Tab"`, cebb2d60 corrupt-launch run 10 s no panic).
All four roadmap slices are checked [x] in 01-ROADMAP.md; the DB validation receipt (revision 7, round 3) projects PASS for every class.

## Definition of Done Results

- **All slices complete:** S01 2/2, S02 3/3, S03 3/3, S04 5/5 tasks done (gsd_milestone_status); all slice checkboxes in 01-ROADMAP.md are [x].
- **Summaries exist:** 01-01-SUMMARY.md, 01-02-SUMMARY.md, 01-03-SUMMARY.md, 01-04-SUMMARY.md plus per-task SUMMARies and VERIFY receipts all present under .gsd/phases/01-refactor-settings-foundation/.
- **Integrations work:** DB validation receipt (revision 7, round 3) records all four cross-slice boundaries PASS (S01→S03 loop/cursor delivery, S01→S04 input routing + Escape menu path, S02→S04 settings load/save/video API, S03→S04 UI rendering + hit-test), and current-tree integration evidence stands: cargo build 0 warnings, cargo test 94/94, launch smoke exit-124/no-panic, settings round-trip smoke byte-identical.

## Requirement Outcomes

Not provided.

## Deviations

- D007 mechanism revision: the DECISIONS.md form (a `FnMut(&mut wgpu::RenderPass)` closure hook) does not compile under wgpu 0.19's RenderPass lifetimes with the glyphon 0.5.0 borrow structure; the shipped seam is the lifetime-typed `UiPass` trait (`Option<&dyn UiPass>`), same architecture (single second color-only pass on the existing encoder/view). D007's substance shipped unchanged.
- Merged-before-closeout: the milestone branch was merged into master (9050c16) before the completion write finished, which caused the first closeout verification pass to see only post-merge .gsd auto-commits in the master→HEAD diff and record a false-negative verification failure. The re-run established the correct baseline evidence and passed; the transient 01-VERIFICATION-FAILED.md artifact was removed (git history retains it).
- No other deviations from the roadmap.

## Follow-ups

- Mesh/world/render-texture clippy lint cleanup milestone (13 pre-existing warnings, MEM007/MEM017).
- Wayland vsync=false PresentMode::Immediate fallback is an accepted visual no-op; revisit only if a compositor-specific complaint appears.
- Future settings fields must extend BOTH the phase-2 load lookups and the Serialize-only save mirror, keeping the round-trip test as the lock (P001/P002 patterns).
<!-- gsd:state-version=27:1 -->
