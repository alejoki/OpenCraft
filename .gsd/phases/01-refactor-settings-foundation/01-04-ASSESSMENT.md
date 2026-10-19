---
sliceId: S04
uatType: runtime-executable
verdict: PASS
attempt: 1
runId: uat:M001:S04:attempt-1
worktreeRoot: /home/aleksi/minecraft-clone/.gsd-worktrees/M001
date: 2026-10-19T10:55:43.829Z
---

# UAT Result - S04

## Checks

| Check | Mode | Result | Evidence | Notes |
|-------|------|--------|----------|-------|
| cargo build — exit 0, current binary, no warnings on the slice surface | runtime | PASS | gsd_uat_exec:1e4bc9b1-9639-42cd-9183-b5285ce9de94 | exit 0 in 194ms: Finished `dev` profile — clean incremental build, no errors or slice-surface warnings. |
| cargo test — 94/94 pass, 0 failed, 0 ignored (83 lib incl. 24 new src/ui/menu.rs tests + 11 src/main.rs tests) | runtime | PASS | gsd_uat_exec:a6d84314-2fb9-480d-8075-892ed0aaa95f | exit 0: 83 passed lib + 11 passed main + 0 doc tests = 94/94, 0 failed, 0 ignored. |
| cargo clippy — exit 0, zero findings on the slice surface (src/ui/menu.rs, src/ui/mod.rs, src/settings/mod.rs, src/main.rs) | runtime | PASS | gsd_uat_exec:a0b57d05-b28e-414c-b50f-2f82ae4642be | exit 0 with 12 warnings, all inherited mesh/world lints (out of scope per MEM007/MEM017); explicit grep of clippy output for the four slice-surface files returned 'no findings on slice surface'. |
| scripts/launch_smoke.sh — SMOKE PASS: 10 s bounded run, exit 124 (survived), no panic; no stray settings.toml without interaction | runtime | PASS | gsd_uat_exec:04f1262a-1e54-455e-ae05-f9d9107ff535 | SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic — menu wiring did not regress plain launch. |
| scripts/settings_smoke.sh (restart-persistence automatable half) — SMOKE PASS: 10 s bounded run in a temp cwd seeded with settings.toml, exit 124, no panic, settings.toml byte-identical after the run | runtime | PASS | gsd_uat_exec:88c1f1b5-1276-4dc1-aa3a-4fc3269275ca | SMOKE PASS: ran 10s on a seeded settings.toml (jump = ShiftLeft), exit 124, no panic, file byte-identical. |
| Wiring inspection — all integration seams present in src/main.rs: load_with_warnings at startup, input.reset on menu open, apply_gameplay_key routing, evaluate_rebind_candidate capture flow, apply_vsync + set_fullscreen live application, save_settings_or_warn at every accept site + CloseRequested final save | artifact | PASS | gsd_uat_exec:e84fcfa0-ae11-404b-baf0-b42a8608b01c | grep of src/main.rs found all 8 seams: load_with_warnings, input.reset, apply_gameplay_key, evaluate_rebind_candidate, apply_vsync, set_fullscreen, save_settings_or_warn at multiple accept sites (incl. lines 343/481), and WindowEvent::CloseRequested at line 418. (An initial attempt exited 2 from shell quoting of 'input.reset()' parens; retried successfully — not a product failure.) |
| Interactive press-to-rebind: run cargo run, Escape opens menu (cursor releases, movement stops), click Jump row, press ShiftLeft → binding accepted, row shows 'Jump: ShiftLeft', settings.toml updated; duplicate/unmappable keys show warnings; Escape cancels a pending capture | human-follow-up | NEEDS-HUMAN | gsd_uat_exec:a6d84314-2fb9-480d-8075-892ed0aaa95f | Explicitly non-automatable: native wgpu game requiring real keyboard press-to-capture; no synthetic-input automation exists in this surface. Automatable half covered by 24 src/ui/menu.rs unit tests + keybind-map routing press/release tests (cargo test evidence). Human: run `cargo run`, press Escape, click the Jump row, press ShiftLeft. |
| Menu legibility and layout: 12 rows readable on a dark panel over gameplay, hovered row highlighted, layout correct at windowed and fullscreen sizes | human-follow-up | NEEDS-HUMAN | gsd_uat_exec:04f1262a-1e54-455e-ae05-f9d9107ff535 | Explicitly non-automatable: visual judgment of a native wgpu window (no screenshot automation in surface); layout determinism and per-frame reflow are unit-tested. Human: open the menu windowed and fullscreen and judge readability/hover highlight. |
| Live apply visible: FOV clicks visibly widen/narrow the projection (clamped 30–120°), fullscreen toggle switches modes with menu reflow, sensitivity changes look speed, vsync toggles frame pacing where supported | human-follow-up | NEEDS-HUMAN | gsd_uat_exec:88c1f1b5-1276-4dc1-aa3a-4fc3269275ca | Explicitly non-automatable: observing rendered projection change requires human eyes on the native window; wiring proven by grep inspection + smoke run. Human: click FOV rows and toggle fullscreen/vsync while the menu is open. |
| Restart persistence interactively (roadmap proof): rebind jump from Space to Shift, quit, relaunch — Shift jumps, Space does not, menu shows 'Jump: ShiftLeft' | human-follow-up | NEEDS-HUMAN | gsd_uat_exec:88c1f1b5-1276-4dc1-aa3a-4fc3269275ca | Explicitly non-automatable: interactive rebind→quit→relaunch with felt key behavior needs a human. Automatable half proven by settings_smoke.sh (seeded jump=ShiftLeft round-trips byte-identical through a real run). |

## Overall Verdict

PASS - All 6 executable checks re-run and PASS (build, 94/94 tests, clippy clean on slice surface, both smoke scripts, wiring inspection); the 4 human interactive/visual checks are explicitly non-automatable (native wgpu keyboard/visual interaction) and remain NEEDS-HUMAN as documented, with their automatable halves covered by unit tests and smoke evidence.

## Tool Presentation

```json
{
  "blockedTools": [
    {
      "name": "edit",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "write",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "gsd_exec",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "gsd_summary_save",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "gsd_save_gate_result",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "search-the-web",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "WebSearch",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "Bash",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "Write",
      "reason": "forbidden during run-uat"
    },
    {
      "name": "Edit",
      "reason": "forbidden during run-uat"
    }
  ],
  "presentedTools": [
    "gsd_uat_exec",
    "gsd_uat_result_save",
    "gsd_resume",
    "gsd_milestone_status",
    "gsd_journal_query",
    "find",
    "glob",
    "grep",
    "ls",
    "read"
  ],
  "surface": "mcp",
  "toolPresentationPlanId": "run-uat/default-v1"
}
```

## Gate

Aggregate UAT gate saved as pass.

## Manual Validation

One or more checks are marked `NEEDS-HUMAN` and require a person to validate:

- Validate the work here: /home/aleksi/minecraft-clone/.gsd-worktrees/M001
- This milestone runs in a git worktree, so the code lives under the GSD worktrees directory. Open it with: cd "/home/aleksi/minecraft-clone/.gsd-worktrees/M001"
- Follow the UAT checklist at: .gsd/phases/01-refactor-settings-foundation/01-04-UAT.md
<!-- gsd:state-version=192:1 -->
