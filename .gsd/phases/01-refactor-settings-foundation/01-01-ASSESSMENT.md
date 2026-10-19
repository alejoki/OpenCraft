---
sliceId: S01
uatType: runtime-executable
verdict: PASS
attempt: 1
runId: uat:M001:S01:attempt-1
worktreeRoot: /home/aleksi/minecraft-clone/.gsd-worktrees/M001
date: 2026-10-19T07:44:59.581Z
---

# UAT Result - S01

## Checks

| Check | Mode | Result | Evidence | Notes |
|-------|------|--------|----------|-------|
| bash scripts/launch_smoke.sh → exit 0, stdout 'SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic'; exit 124 proves the process survived the full 10s on the ApplicationHandler loop without exiting early or panicking | runtime | PASS | gsd_uat_exec:63dfb1d3-cc00-4c44-b674-922b70a6fa9d | exit=0 duration=10044ms; stdout: 'SMOKE PASS: ran 10s on the migrated loop, exit 124, no panic' |
| Manifest probe → winit = "0.30" in Cargo.toml, version = "0.30.13" in Cargo.lock, no winit = "0.29" remaining | artifact | PASS | gsd_uat_exec:49133b9d-119a-4ccc-a698-4ee8cf9547a5 | Probe output: toml_winit_030=True lock_version=0.30.13 no_029=True → MANIFEST_OK (exit 0). First attempt 681be09d failed on harness arg misuse (SyntaxError), retried successfully. |
| cargo build produces warning-free build | runtime | PASS | gsd_uat_exec:abc690cf-788d-453d-89d3-0efda52b23c8 | exit=0; 'Finished dev profile' with zero warnings → BUILD_CLEAN |
| cargo clippy --bin minecraft-clone → zero findings on the migration surface src/main.rs (12 lib warnings in untouched files are pre-existing per MEM007) | runtime | PASS | gsd_uat_exec:0990d267-0519-411a-b18d-4b9a6af735f8 | Log inspection of the captured clippy output (0990d267, exit 0): total_warning_sites=12, src/main.rs_sites=0; all 12 sites are in src/mesh/greedy.rs, src/mesh/quad.rs, src/world/manager.rs, src/world/persistence.rs, src/world/terrain.rs — pre-existing lib lints per MEM007/L002. Migration surface src/main.rs is clippy-clean. |
| Manual gameplay-parity checklist: WASD/Space/Shift/Ctrl movement, F fly toggle, F5 save + badge, Escape cursor grab/release + click-to-regrab, LMB break / RMB place with AABB rejection, hotbar Digit1-9 + wheel, [ ] render-distance, live FPS/chunk/quad title readout, resize/close behavior, out-of-focus key reset | human-follow-up | NEEDS-HUMAN | - | Explicitly non-automatable: the UAT file itself designates this checklist as NEEDS-HUMAN ('cannot be automated without a human at the keyboard'). Requires hands-on interactive gameplay; runtime-executable mode cannot exercise keyboard/mouse gameplay. Edge cases (resumed() idempotency, ungrabbed-click regrab, close/OOM save-before-exit, focus-loss key reset) are covered by design in the migrated event arms. |

## Overall Verdict

PASS - All four automated checks passed (10s smoke exit 124 on the migrated loop, manifest winit 0.30.13, warning-free build, clippy clean on src/main.rs with only pre-existing lib lints); the manual gameplay-parity checklist is explicitly non-automatable and remains NEEDS-HUMAN.

## Tool Presentation

```json
{
  "aliases": [
    {
      "canonical": "bash",
      "requested": "Bash"
    },
    {
      "canonical": "write",
      "requested": "Write"
    },
    {
      "canonical": "edit",
      "requested": "Edit"
    },
    {
      "canonical": "search-the-web",
      "requested": "WebSearch"
    }
  ],
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
  "fallbackToolsUsed": [],
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
- Follow the UAT checklist at: .gsd/phases/01-refactor-settings-foundation/01-01-UAT.md
<!-- gsd:state-version=61:1 -->
