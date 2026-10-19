---
phase: 01
phase_name: refactor-settings-foundation
project: minecraft-clone
generated: 2026-10-19T13:11:05Z
counts:
  decisions: 5
  lessons: 5
  patterns: 5
  surprises: 3
missing_artifacts: []
---

# M001 Learnings: Refactor + Settings Foundation

### Decisions

- Pin winit to 0.30.13 rather than a 0.31.0 beta so the ApplicationHandler migration uses the stable API while preserving the existing wgpu 0.19 stack.
Source: 01-ROADMAP.md/Slices; 01-01-SUMMARY.md/What Happened

- Pin glyphon to exactly 0.5.0 because it composes with wgpu 0.19; newer glyphon lines would force an unrelated renderer upgrade.
Source: 01-02-RESEARCH.md/Research Findings; 01-03-SUMMARY.md/What Happened

- Store keybinds as explicitly mapped stable strings and load TOML in two phases with per-field fallback instead of relying on serde defaults.
Source: 01-02-SUMMARY.md/What Happened; DECISIONS.md/D003-D005

- Keep UI rendering as a second color-only pass on the existing encoder and surface view, exposed through the lifetime-typed UiPass seam.
Source: 01-03-SUMMARY.md/What Happened; DECISIONS.md/D007

- Make loaded Settings the single source of truth for video and gameplay input, and save every accepted menu change atomically as well as on quit.
Source: 01-04-SUMMARY.md/What Happened; DECISIONS.md/D008-D009

### Lessons

- Moving closure-captured state into GameApp plus a window-dependent Ready state preserves the old event behavior while satisfying winit 0.30's resumed lifecycle.
Source: 01-01-SUMMARY.md/What Happened

- A serde-derived settings struct cannot provide the required per-field recovery when one sibling field is malformed; parsing a toml::Value tree first keeps valid fields usable.
Source: 01-02-SUMMARY.md/What Happened; 01-02-SUMMARY.md/Failure Signal

- wgpu RenderPass lifetimes make a generic closure hook impractical at this seam; a lifetime-typed UiPass trait is the compilable replacement.
Source: 01-03-SUMMARY.md/What Happened; DECISIONS.md/D007

- Full-tree clippy findings were pre-existing in mesh, world, and render texture code, so verification must distinguish the touched surface from inherited warnings.
Source: 01-03-SUMMARY.md/Verification; 01-01-SUMMARY.md/Deviations

- The end-to-end persistence proof requires both an automatable seeded-file smoke and interactive confirmation that an accepted rebind is written by the game.
Source: 01-04-SUMMARY.md/Verification; 01-VALIDATION.md/Operational Verification

### Patterns

- Use an explicit two-phase parse plus structured warnings for resilient configuration loading; never swallow fallback or save errors.
Source: 01-02-SUMMARY.md/Failure Signal

- Use a serialize-only mirror and sibling temporary file followed by rename for crash-safe settings persistence.
Source: 01-02-SUMMARY.md/What Happened

- Use a reusable pixel-space quad API with NDC conversion and a color-only LoadOp::Load pass for immediate-mode UI over the world.
Source: 01-03-SUMMARY.md/What Happened

- Route menu utility keys before gameplay bindings and reset input when focus or cursor ownership changes, preventing stale movement and ensuring Escape remains reliable.
Source: 01-04-SUMMARY.md/What Happened; 01-04-SUMMARY.md/Key Decisions

- Keep a bounded launch smoke and a seeded settings smoke in scripts so lifecycle and persistence regressions are diagnosable without an unbounded game process.
Source: 01-01-SUMMARY.md/What Happened; 01-04-SUMMARY.md/What Happened

### Surprises

- The final branch closeout diff contained only GSD projection metadata because implementation commits were already reachable from the integration branch; source-history evidence was needed to establish that M001 code existed.
Source: 01-VALIDATION.md/Verification Class Compliance; 01-ROADMAP.md/Verification Gate

- The UI pass closure described in the original design had to become a trait-based seam after compiler lifetime constraints surfaced.
Source: 01-03-SUMMARY.md/What Happened; DECISIONS.md/D007

- Wayland may ignore vsync=false when PresentMode::Immediate is unavailable, which is an accepted platform limitation rather than a settings failure.
Source: 01-VALIDATION.md/Success Criteria Checklist; PROJECT.md/Known Limitations
