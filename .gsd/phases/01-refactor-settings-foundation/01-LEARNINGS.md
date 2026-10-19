---
phase: 01
phase_name: refactor-settings-foundation
project: minecraft-clone
generated: 2026-10-19T11:36:16Z
counts:
  decisions: 10
  lessons: 5
  patterns: 10
  surprises: 4
missing_artifacts: []
---

# M001 Learnings: Refactor + Settings Foundation

### Decisions

- **D001 — winit migration target:** Pin winit at 0.30.13 (current stable ApplicationHandler line), not 0.31.0-beta. Shipped exactly: Cargo.lock resolves 0.30.13, event arms moved verbatim.
  - Source: 01-ROADMAP.md/Slices + 01-01-SUMMARY.md/What Happened
- **D002 — text rendering library:** glyphon pinned `=0.5.0` because it pins wgpu ^0.19 exactly; glyphon 0.6+ jumped to wgpu's new numbering (^22/^23) and would force the out-of-scope wgpu upgrade. Shipped: `=0.5.0` in Cargo.toml, UiRenderer on glyphon 0.5.0.
  - Source: 01-DECISIONS.md + 01-03-SUMMARY.md/What Happened
- **D003 — keybind string representation:** Hand-mapped stable strings (KeyCode Debug spelling, e.g. "KeyW") in src/settings/keybind.rs, decoupling settings.toml from winit's serde so future winit bumps cannot break saved configs. Shipped: key_to_string/parse_key.
  - Source: 01-DECISIONS.md + 01-02-SUMMARY.md/What Happened
- **D004 — slice ordering:** S01 (winit migration) and S02 (settings core) first as independent risk-retiring foundations, then S03 (UI) on S01, then S04 (menu) on S02+S03 as the end-to-end integration slice. Shipped in exactly that order; all four complete.
  - Source: 01-ROADMAP.md/Slices
- **D005 — TOML load strategy:** Two-phase per-field-fallback parse (toml::Value tree + per-field extraction) instead of `#[serde(default)]` derive, because serde derive aborts the whole struct on one bad field while the requirement is "a bad settings.toml never blocks launch" with valid fields surviving invalid siblings. Shipped as P001.
  - Source: 01-DECISIONS.md + 01-02-SUMMARY.md/What Happened
- **D006 — settings observability:** Load warnings and keybind conflicts returned as structured data (`load_with_warnings`, `validate_keybinds`), not a logging framework — the crate stays log-free; S04's menu displays the warnings. Shipped: SettingsWarning shown as the menu's first-open message.
  - Source: 01-DECISIONS.md + 01-02-SUMMARY.md/What Happened
- **D007 — UI render-pass seam:** Optional `ui_pass` hook on render_frame: a second color-only begin_render_pass on the existing encoder/view between the world pass and present — least churn, no second surface/encoder, no world-pass changes. Shipped; S04's menu rides the same pass.
  - Source: 01-DECISIONS.md + 01-03-SUMMARY.md/What Happened
- **MEM019 — menu-first keyboard routing:** Utility keys matched before gameplay routes; Escape always toggles the menu even if a gameplay action is rebound onto it (Escape remains a valid rebind target).
  - Source: 01-04-SUMMARY.md/What Happened
- **MEM020/P005 — borrow-split live apply:** fullscreen/FOV applied inside menu_click, vsync present-mode+configure in the MouseInput caller after a pre-call hit-test; save_settings_or_warn as a free fn with disjoint (menu, settings) args to sidestep borrow conflicts.
  - Source: 01-04-SUMMARY.md/What Happened
- **Loaded Settings as single source of truth for video parameters:** startup applies FOV/fullscreen/vsync from settings (not only menu-change triggers), guarded by a default-vs-camera-default test.
  - Source: 01-04-SUMMARY.md/What Happened

### Lessons

- **Agent-shell display gotcha (MEM006):** Headless/agent-shell smoke runs of a GUI game fail until the Wayland socket (WAYLAND_DISPLAY) is attached explicitly — Xwayland `:0` is not what winit picks up on this system. Fix: launch_smoke.sh attaches the Wayland socket; both smoke scripts assert exit 124 + no-panic.
  - Source: 01-01-SUMMARY.md/What Happened
- **Inherited-lint scoping convention (MEM007/MEM017/L002):** Full-workspace clippy carries 13 pre-existing lib warnings in src/mesh/*, src/world/*, src/render/texture.rs that predate the milestone; slices satisfy "clippy clean" on their own touched surface and must not refactor out-of-scope files — document and defer instead.
  - Source: 01-01-SUMMARY.md/Deviations + 01-03-SUMMARY.md/Verification
- **Rebind validation order:** evaluate_rebind_candidate checks RejectUnsupported before the duplicate check because save() panics on unmapped KeyCodes — validating key support first prevents a panic path at accept time.
  - Source: 01-04-SUMMARY.md/What Happened
- **Human-in-the-loop halves of UAT:** Interactive evidence (legibility, press-to-rebind, visible live-apply, gameplay parity, restart persistence) cannot be fabricated by automation; deferring it to a milestone-level interactive session with a durable on-disk artifact (game-written settings.toml with a non-default binding) closed the validation gap cleanly.
  - Source: 01-VALIDATION.md/Verdict Rationale
- **Zero-edit collateral from the winit bump:** rwh 0.6.2 is shared by winit 0.30.13 and wgpu 0.19.4, so src/render/context.rs and src/lib.rs needed zero edits during the migration — checking the shared dependency graph first avoided unnecessary churn.
  - Source: 01-01-SUMMARY.md/What Happened

### Patterns

- **P001 — two-phase per-field TOML fallback parse:** Parse to `toml::Value` first (syntactic failure → all defaults), then extract each field with type/range checks so a valid field survives an invalid sibling and out-of-range values clamp with warnings.
  - Source: 01-02-SUMMARY.md/What Happened
- **P002 — Serialize-only mirror struct + atomic save:** Load deliberately bypasses serde (hand-checked), save goes through a private Serialize-only mirror struct written atomically (tmp sibling + rename); the round-trip test locks both halves together.
  - Source: 01-02-SUMMARY.md/What Happened
- **Optional UI render pass seam:** Second color-only begin_render_pass (no depth attachment, LoadOp::Load, same encoder/view) between the world pass and present — a general recipe for overlaying UI on an existing single-pass pipeline without churn.
  - Source: 01-03-SUMMARY.md/What Happened
- **Pixel-space quad drawing API:** quad_to_vertices/quads_to_vertices convert physical-pixel top-left-origin quads to NDC on the CPU (y flip, 6 verts/quad) as pure functions — UI geometry stays unit-testable without a GPU.
  - Source: 01-03-SUMMARY.md/What Happened
- **Change-gated text shaping:** Re-shape glyphon buffers only when the text string changes (overlay at a 0.25 s cadence; set_text ~0.1–1.3 ms), never per frame.
  - Source: 01-03-SUMMARY.md/What Happened
- **Pure immediate-mode menu module:** State machine + layout derivation + hit-test + decisions as pure CPU logic with a caller-owned tick counter for message expiry (no clock inside the module), unit-tested before wiring (24 tests).
  - Source: 01-04-SUMMARY.md/What Happened
- **Menu-first input routing:** While a menu is open, every KeyboardInput/MouseInput routes to the menu and returns before gameplay handling so capture candidates can never leak into InputState.
  - Source: 01-04-SUMMARY.md/What Happened
- **Per-frame layout derivation:** Menu layout derived from ctx.config.width/height each frame with no cached rects, so Resized/fullscreen reflow needs no invalidation path.
  - Source: 01-04-SUMMARY.md/What Happened
- **Change-gated atomic persistence:** One atomic tmp-sibling+rename write per accepted human-paced change (never per frame), plus one final save on CloseRequested; save errors surface as in-menu messages, never swallowed.
  - Source: 01-04-SUMMARY.md/What Happened
- **Bounded smoke harnesses:** timeout-killed GUI runs asserting exit 124 + no-panic text in captured output (launch) plus byte-identical seeded-file survival (settings) — reusable, cheap, CI-friendly proof the game actually runs.
  - Source: 01-01-SUMMARY.md/What Happened + 01-04-SUMMARY.md/What Happened

### Capture Status Note (final)

- All durable Decisions, Lessons, and Patterns from this milestone are persisted to the memory store: MEM001–MEM028 (D001–D007 as MEM001–MEM004/MEM008/MEM009/MEM013, P001–P010, L002/L007 and related gotchas, MEM019–MEM020 routing/borrow patterns, MEM026 human-in-the-loop UAT convention, MEM027 loaded-Settings-as-video-source-of-truth decision, MEM028 save-on-accept + save-on-quit decision). The two decisions previously blocked by the tool-call loop guard were persisted in the closeout retry turn (MEM027, MEM028). Surprises remain only in this file, not in the memory store.

### Surprises

- The winit 0.29→0.30 migration required zero edits in src/render/context.rs and src/lib.rs — the shared rwh 0.6.2 dependency meant the surface was smaller than feared.
  - Source: 01-01-SUMMARY.md/What Happened
- The pre-existing clippy lib warnings (12, later refined to 13) only became visible to the milestone because the migration forced whole-crate rebuilds; they predate the project's GSD history.
  - Source: 01-01-SUMMARY.md/Deviations + 01-03-SUMMARY.md/Verification
- Milestone validation round 1 flagged needs-attention solely for missing *human* evidence — every automated check passed on the first pass; the only gap was interactive UAT that automation structurally cannot provide.
  - Source: 01-VALIDATION.md/Verdict Rationale
- The test suite grew 23 → 54 → 59 → 94 across the milestone as each slice's pure-logic-first design (settings, quads, menu) made behavior unit-testable without a GPU.
  - Source: 01-02-SUMMARY.md/What Happened + 01-03-SUMMARY.md/Verification + 01-04-SUMMARY.md/Verification
