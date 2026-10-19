# Project Knowledge

Append-only register of project-specific rules, patterns, and lessons learned.
Agents read this before every unit. Add entries when you discover something worth remembering.

## Rules

| # | Scope | Rule | Why | Added |
|---|-------|------|-----|-------|

## Patterns

| # | Pattern | Where | Notes |
|---|---------|-------|-------|
| P001 | Per-field TOML fallback in src/settings/mod.rs requires a two-phase parse: first `toml::from_str::<toml::Value>` into a generic tree, then independent per-field extraction with type/range checks. Plain `#[serde(default)]` derive cannot be used for loading — serde aborts the whole struct on any field type error, losing valid siblings. Add new settings fields by extending the phase-2 lookups (see load_keybind_action / load_clamped_f32 / load_bool helpers). | — | — |

## Lessons Learned

| # | What Happened | Root Cause | Fix | Scope |
|---|--------------|------------|-----|-------|
| L001 | winit 0.30.13's ApplicationHandler trait methods take `&ActiveEventLoop` (not `&dyn ActiveEventLoop`); `EventLoop::set_control_flow(ControlFlow::Poll)` is called once before `run_app`, window creation moves into `resumed()` via `event_loop.create_window(Window::default_attributes()...)`, and the migrated `GameApp`/`Ready` struct pattern lives in src/main.rs of the M001 worktree (S03/S04 build on this loop; window+GPU stack is behind `Option`, guarded idempotently in resumed()). | — | — | project |
| L002 | The workspace has ~12 pre-existing clippy lib warnings (needless_range_loop etc. in src/mesh/greedy.rs, src/mesh/quad.rs, src/world/manager.rs, src/world/terrain.rs) that predate M001-S01; the bin target (src/main.rs) is clippy-clean. Downstream slice verification should not treat these lib lints as a regression — scope clippy checks to the files the slice touched, or record the inherited findings as known limitations. | — | — | project |
