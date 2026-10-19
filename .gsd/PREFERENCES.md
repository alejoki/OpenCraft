---
tool_call_loop_guard:
  repeated_tool:
    exempt_tools:
      - capture_thought
---

# GSD Preferences

The milestone-closeout learnings extraction procedure legitimately requires many
`capture_thought` calls in a single turn (one per extracted Pattern, Lesson, and
Decision — see M001-LEARNINGS.md). Exempt it from the per-tool-name repeated
call cap so learnings persistence is not interrupted mid-batch.
