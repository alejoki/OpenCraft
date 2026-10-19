# M001: New milestone M001 (working draft — in-progress discussion)

## Decisions captured so far

1. **winit upgrade:** Upgrade winit 0.29 → current ApplicationHandler API as part of the M001 refactor (not stay on deprecated closure API).
2. **Config format:** TOML via serde (`settings.toml`), struct round-trip load/save. First serde use in the project.
3. **Keybind UI:** Press-to-rebind menu — list actions, click/press to rebind, basic duplicate warning.
4. **M001 core deliverable:** Refactor + settings foundation. winit migration + TOML settings + keybind menu. NOT refactor-only, NOT adding new gameplay features.
5. **Settings scope:** Input + video — keybinds, mouse sensitivity, FOV, fullscreen/vsync. No gameplay-affecting settings (render distance etc.) this milestone.
6. **wgpu:** Stays at 0.19 — winit-only upgrade; the two are decoupled, smaller risk surface.
7. **Menu UI approach:** Hand-rolled immediate-mode UI against the existing wgpu pipeline (new src/ui/ module: quad renderer + text via glyphon/cosmic-text). NOT egui, NOT keyboard-only menu.
8. **Bad config behavior:** Per-field fallback — invalid fields reset to defaults, valid fields kept, warning logged, game always launches; file rewritten on next save. NOT all-or-nothing .bak, NOT fail-loud exit.
9. **Acceptance bar:** Automated cargo tests for settings load/save/fallback round-trips + manual in-game smoke check (rebind persists across restart, no gameplay regressions). NOT manual-only, NOT headless harness.

## Still open for depth summary

- Why-this-milestone framing and user-visible outcome wording
- Completion class / integration+operational completeness details
- Error-handling strategy beyond config fallback
- Risks/unknowns list, constraints, integration points

## Captured Question Round — 2026-10-19T06:47:04.257Z

### Depth Check

Did I capture the depth right?

- **Yes, you got it (Recommended)** — The full picture — vision, scope, architecture, error handling, quality bar — matches what we discussed; proceed to write the M001 context file.
- **Not quite — let me clarify** — Something is off or missing — you'll clarify before anything is written.

**Selected:** Yes, you got it (Recommended)
<!-- gsd:state-version=17:1 -->
