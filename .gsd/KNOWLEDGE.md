# Project Knowledge

Append-only register of project-specific rules, patterns, and lessons learned.
Agents read this before every unit. Add entries when you discover something worth remembering.

## Rules

| # | Scope | Rule | Why | Added |
|---|-------|------|-----|-------|

## Patterns

| # | Pattern | Where | Notes |
|---|---------|-------|-------|

## Lessons Learned

| # | What Happened | Root Cause | Fix | Scope |
|---|--------------|------------|-----|-------|
| L001 | winit 0.30.13's ApplicationHandler trait methods take `&ActiveEventLoop` (not `&dyn ActiveEventLoop`); `EventLoop::set_control_flow(ControlFlow::Poll)` is called once before `run_app`, window creation moves into `resumed()` via `event_loop.create_window(Window::default_attributes()...)`, and the migrated `GameApp`/`Ready` struct pattern lives in src/main.rs of the M001 worktree (S03/S04 build on this loop; window+GPU stack is behind `Option`, guarded idempotently in resumed()). | — | — | project |
