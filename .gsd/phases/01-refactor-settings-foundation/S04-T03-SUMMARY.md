---
id: S04-T03
parent: S04
milestone: M001
key_files:
  - src/main.rs
key_decisions:
  - Gameplay rebind routes placed after the non-rebindable utility-key arms so Escape always toggles the menu even if a gameplay action is rebound onto it (Escape is a valid rebind target)
  - Save-on-accept reuses save_settings_or_warn (atomic tmp+rename write, io error surfaced as in-menu text, never deferred or swallowed)
duration:
verification_result: mixed
completed_at:
blocker_discovered: false
---

# S04-T03: Replaced the 7 hardcoded gameplay KeyCode arms with Settings keybind-map routing (press/release semantics preserved, intentional shift narrowing kept) and wired menu-first keyboard routing with press-to-rebind: unsupported/duplicate rejection, save-on-accept with in-menu io-error surfacing, Escape cancel/close

**Replaced the 7 hardcoded gameplay KeyCode arms with Settings keybind-map routing (press/release semantics preserved, intentional shift narrowing kept) and wired menu-first keyboard routing with press-to-rebind: unsupported/duplicate rejection, save-on-accept with in-menu io-error surfacing, Escape cancel/close**

## What Happened

Swapped the 7 hardcoded gameplay KeyCode arms in src/main.rs window_event for routing through the loaded Settings keybind map, and delivered the press-to-rebind flow while the menu is open.

Routing (menu closed): the 7 rebindable actions (forward/backward/left/right, jump_or_up, crouch_or_down, sprint) now route through a new pure helper `apply_gameplay_key(&mut InputState, &Keybinds, KeyCode, pressed) -> bool` called from a single trailing `_` match arm. Press sets the InputState flag, release clears it — both transitions route through the same map, preserving pre-rebind press/release semantics exactly (defaults equal the old hardcoded values, so the swap is behavior-neutral — confirmed by the launch smoke). The old ShiftLeft|ShiftRight / ControlLeft|ControlRight pairs are intentionally narrowed to only the bound crouch/sprint key per the documented S02 Keybinds rationale.

Routing (menu open): every KeyboardInput routes to the menu FIRST and returns before the gameplay match — gameplay keys can never touch InputState while the menu is open, especially in AwaitingKey where any pressed KeyCode is a capture candidate. In AwaitingKey, the pressed key goes through T01's `evaluate_rebind_candidate` (unsupported filter first, then duplicate check): Accept applies `action.set_key`, calls `save_settings_or_warn` immediately (atomic tmp+rename write — save on every accepted rebind, never deferred; an io::Error surfaces as in-menu text instead of being swallowed), then `finish_capture` per the menu.rs caller contract; the menu text composes per frame from live settings so the new binding shows immediately. RejectDuplicate / RejectUnsupported show T01's `rebind_rejection_message` (conflicting-action naming matches KeybindConflict data). Escape cancels a pending capture first, otherwise closes the menu and re-grabs the cursor (InputState::reset on menu open and cursor re-grab on close were already wired by T02 and preserved).

## Failure Modes

External dependencies and their failure paths:
- settings.toml write on every accepted rebind (`Settings::save` via `save_settings_or_warn`): failure path is any io::Error (disk full, permission denied, settings dir deleted mid-run). Handling: the error text surfaces as an in-menu message (visible, not swallowed, not fatal — the rebind stays in memory; the next accepted change retries). The write is atomic (create_dir_all + .toml.tmp + sync_all + rename), so a crash mid-save leaves the previous settings.toml intact — crash-safe, and a later panic cannot lose the rebind because the save is never deferred.
- Unsupported-key filter: `Settings::save` PANICS on keys without a `key_to_string` mapping, so `evaluate_rebind_candidate`'s RejectUnsupported runs before any accept — the panic path is unreachable from the rebind flow (covered by T01 tests).
- Windowing cursor-grab API on menu close: handled by the pre-existing T02 `set_cursor_grab` fallback chain (Locked → Confined → None, results ignored by design).
- No network dependencies; the only subprocess is the verification smoke script itself.

## Load Profile

## Negative Tests

- src/ui/menu.rs (T01 suite, the rebind decision surface): unsupported-key rejection (save-would-panic guard), duplicate rejection naming the conflicting action, idempotent re-accept of the key already bound to the same action.
- src/main.rs new tests (routing surface, this task):
  - `gameplay_keys_route_through_the_loaded_keybind_map_press_and_release` — all 7 default bindings press AND release route through the map.
  - `rebound_keys_take_effect_and_the_replaced_key_goes_dead` — jump rebound to ArrowUp: new key jumps, Space is unbound, an unbound press must not clear/disturb the flag, old-key release must not resurrect it.
  - `only_the_bound_crouch_and_sprint_keys_trigger` — ShiftRight/ControlRight no longer crouch/sprint (intentional narrowing), InputState untouched.
  - `non_gameplay_keys_report_not_bound_and_leave_input_untouched` — Q/Z/F/F5/Escape report not-bound with zero InputState mutation.
  - `duplicate_bindings_in_a_hand_edited_file_resolve_first_match` — a hand-edited duplicate (backward=KeyW) resolves deterministically to forward (declaration order), not last-write-wins.

## Verification

cargo build green; cargo test green (10/10 in src/main.rs incl. 5 new routing tests; full crate suite green); clippy clean on the bin surface src/main.rs (zero findings referencing src/main.rs; the 12 lib warnings are inherited src/mesh/* + src/world/* lints out of scope per MEM007 — confirmed both by the plain clippy run linting the bin and by the -D warnings run locating all 12 in lib files); launch smoke passes (game boots and survives 10s on the event loop with routing active — the defaults-equivalence guarantee). First cargo test run failed on an incorrect assertion in my own new test (an unbound Space press correctly leaves the ArrowUp-pressed flag set — I had asserted it cleared); fixed the test assertion, not the routing, and re-ran green. Interactive proof (rebind jump to ShiftLeft in-game, quit, relaunch, binding persists and is shown) is the slice-level NEEDS-HUMAN UAT item owned by the orchestrator, not this task.

## Verification Evidence

| # | Command | Exit Code | Verdict | Duration |
|---|---------|-----------|---------|----------|
| 1 | `cargo build` | 0 | pass | 1314ms |
| 2 | `cargo test` | 101 | fail — new test rebound_keys_take_effect_and_the_replaced_key_goes_dead had a wrong assertion (unbound press correctly leaves a set flag); routing was correct, test fixed | 982ms |
| 3 | `cargo test` | 0 | pass — 10 passed, 0 failed (5 new routing tests green) | 979ms |
| 4 | `cargo clippy --lib --bins` | 0 | pass — bin (src/main.rs) linted with zero findings; 12 warnings inherited from src/mesh/* + src/world/* out of scope per MEM007 (also confirmed via the -D warnings run: all 12 errors located in lib files, none in src/main.rs) | 648ms |
| 5 | `bash scripts/launch_smoke.sh` | 0 | pass — SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic (defaults-equivalence guarantee holds with routing active) | 10046ms |

## Deviations

None.

## Known Issues

None.

## Files Created/Modified

- `src/main.rs`
<!-- gsd:state-version=192:1 -->
