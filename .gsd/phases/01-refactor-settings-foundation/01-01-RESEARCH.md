# S01: winit 0.30 ApplicationHandler Migration — Research

**Date:** 2026-10-19 · **Verdict:** low-complexity structural refactor; API surface empirically verified by compiling a full probe against the exact target versions.

## Summary

The migration is **structural only**: state moves from a captured closure into an `ApplicationHandler` struct; every `WindowEvent`/`KeyEvent`/`DeviceEvent` shape the game consumes is identical between winit 0.29.15 and 0.30.13. The two real unknowns were settled empirically:

1. **Surface compatibility (highest risk) — RESOLVED, zero change needed.** The project's `Cargo.lock` already resolves winit **0.29.15** and wgpu **0.19.4** — both are raw-window-handle **0.6.2** backports, and winit **0.30.13** also uses rwh **0.6.2** (verified via `cargo tree` on a probe manifest and the project lock). Result: `RenderContext::new(window: Arc<Window>)` and `instance.create_surface(Arc<Window>)` compile **unchanged** with winit 0.30.13. `src/render/context.rs` needs **zero edits** (it only uses `winit::window::Window`, `Arc<Window>`, `dpi::PhysicalSize` — all stable across the bump).
2. **Exact 0.30.13 API surface — verified by a compile probe** (`/tmp/winittest`: minimal `ApplicationHandler` + wgpu 0.19 surface + all event shapes the game uses) which built **with zero warnings and clippy-clean**. Probed and confirmed non-deprecated: `event_loop.run_app(&mut app)`, `ActiveEventLoop::create_window(attrs)`, `Window::default_attributes()`, `EventLoop::set_control_flow(ControlFlow::Poll)` before run, `ActiveEventLoop::exit()`.

Trap avoided: `EventLoop::create_window(...)` before run and the closure `EventLoop::run(|event, elwt|…)` are **deprecated** in 0.30.13 — docs.rs changelog examples show `#[allow(deprecated)]` on the former. Do not use either; create the window inside `resumed()` (this also matches the S01→S03 boundary map: "window creation via `ActiveEventLoop::create_window` returning `Arc<Window>`").

## Files Retrieved

1. `src/main.rs` (lines 1–440, entire file) — the whole event loop, input dispatch, per-frame update/render, world save logic. Everything this slice touches lives here.
2. `Cargo.toml` (lines 1–24) — `winit = "0.29"` → becomes `"0.30"` (resolves to 0.30.13, the final release of the 0.30 line). `Cargo.lock` is committed and must be updated/committed alongside.
3. `src/render/context.rs` (lines 30–110) — `RenderContext::new(Arc<Window>)` + `create_surface(window)` + `resize(PhysicalSize<u32>)`; **no changes required** (verified by probe).
4. `src/lib.rs` (lines 1–4) — module declarations; untouched.

No other file references winit (`rg winit src/` → only `main.rs` and `context.rs`).

## Key Code

Required trait surface (winit 0.30.13, confirmed from docs.rs + probe):

```rust
use winit::application::ApplicationHandler;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

// required: resumed + window_event. provided defaults: device_event,
// about_to_wait, suspended, exiting, memory_warning, new_events, user_event
impl ApplicationHandler for GameApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) { /* create window + GPU once, idempotent guard */ }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) { /* match event exactly as today */ }
    fn device_event(&mut self, _el: &ActiveEventLoop, _did: DeviceId, event: DeviceEvent) { /* MouseMotion → yaw/pitch, only when cursor_grabbed */ }
    fn about_to_wait(&mut self, _el: &ActiveEventLoop) { /* if let Some(w) = &self.window { w.request_redraw(); } */ }
}
```

Entry point / setup (verified warning-free on 0.30.13):

```rust
let event_loop = EventLoop::new().expect("Failed to create event loop");
event_loop.set_control_flow(ControlFlow::Poll); // replaces per-event elwt.set_control_flow
// world + player load stays here in main() (pure logic, no window needed)
let mut app = GameApp::new(cli, world, player); // window: None
let _ = event_loop.run_app(&mut app);           // closure `run` is deprecated
```

## Architecture / Migration Shape

**Single-file refactor of `src/main.rs`** (plus `Cargo.toml`/`Cargo.lock`). Natural seams:

1. **`GameApp` struct** absorbs every closure-captured local: `cli`, `world`, `player`, `camera`, `input`, `cursor_grabbed`, `last_frame`, `title_timer`, `last_saved_badge`, `frame_count`, plus window-dependent state created in `resumed()`: `window: Option<Arc<Window>>`, `ctx: Option<RenderContext>` (or a nested `Option<Ready>` holding ctx + `renderer: VoxelRenderer` + `render_store: ChunkRenderStore` — renderer's constructor takes `&ctx`, so they init together).
2. **`resumed()`** (fires once after init on desktop; guard idempotent against refires): `event_loop.create_window(Window::default_attributes().with_title(…).with_inner_size(PhysicalSize::new(1280, 720)))` → `Arc::new` → `RenderContext::new` → `VoxelRenderer::new(&ctx)` → `ChunkRenderStore::new` → camera from player → `set_cursor_grab(&window, true)`.
3. **`window_event()`** — the existing `WindowEvent` match moves verbatim; `elwt.exit()` → `event_loop.exit()`. All arms keep byte-identical logic, including sensitivity `0.0022` (that constant is S02/S04's touchpoint — preserve, don't refactor). `RedrawRequested` body unchanged (per-frame update, streaming, render, title). `MouseInput` "re-grab on click when ungrabbed" flow preserved.
4. **`device_event()`** — `DeviceEvent::MouseMotion { delta }` arm verbatim.
5. **`about_to_wait()`** — `window.request_redraw()` guarded on `Option` window.
6. Free helpers `set_cursor_grab` and `save_current_world` stay as free fns in `main.rs`, unchanged.

Rejected alternative: initializing GPU state in `main()` via `EventLoop::create_window` — deprecated in 0.30.13, and it fights the S01→S03 boundary contract.

## Environment Notes

- **Clippy was missing from the stable toolchain** (slice gate requires `cargo clippy`). Fixed during research: `rustup component add clippy` ran successfully (clippy 0.1.99 now available). Executors should not hit this.
- Linux dev box: winit builds the full Wayland/X11 stack; probe compiled in ~37s cold. The game runs on this machine (existing title-screenshot history).

## Verification

1. `cargo build` — must be **warning-free** (deprecation warnings would reveal use of removed 0.29 patterns; probe proves a warning-free target exists).
2. `cargo clippy` — clean (now installed).
3. `cargo run` manual smoke checklist (roadmap acceptance — no automated tests cover the loop, and none should be added in this slice): WASD movement, Space jump, Shift crouch, Ctrl sprint, F fly toggle, F5 save + "[Saved]" title badge, Escape cursor grab/release toggle (click-to-regrab works), LMB break / RMB place (player-AABB placement rejection intact), hotbar Digit1–9 + mouse wheel, `[`/`]` render-distance change, window title FPS/chunk/quad readout updates, resize keeps rendering, close (X) saves world and exits, out-of-focus resets movement keys.
4. `Cargo.toml`/`Cargo.lock` updated with winit 0.30 and lock delta committed.

## Risks / Constraints

- **Behavior parity is the acceptance bar.** Every event arm must be a verbatim move; the only sanctioned semantic change is *where* state lives. Any accidental change (e.g. dropping the `!event.repeat` guards, the `Focused(false) → input.reset()` arm, or the ungrabbed-click regrab) breaks parity silently.
- `resumed()` can fire multiple times on some platforms (macOS reactivation, Android) — the idempotent guard is mandatory, not optional.
- `ControlFlow::Poll` + request_redraw-every-about_to_wait keeps the current busy-frame behavior; setting it once pre-run (not per-event) is equivalent and cleaner.
- S03 will additionally need `CursorMoved`/`CursorLeft` routed through `window_event()` — trivially available once this migration lands (they're ordinary `WindowEvent` variants; pre-migration loop just ignored them via `_ => {}`).

## Skills Discovered

None installed — winit 0.30 API knowledge was fully covered by docs.rs (`/websites/rs_winit_winit` via Context7) plus empirical compile probes; no winit-specific skill is needed for this slice.
<!-- gsd:state-version=43:1 -->
