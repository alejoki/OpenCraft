# Requirements Registry

This file is the explicit capability and coverage contract for the project.
IDs are stable and follow the D022 inventory order. Do not renumber R### records.

## Active

### R001 — Fresh seeded worlds stream deterministic 32-cubed chunks and preserve chunk edits across save and reload
- Class: core-capability
- Status: active
- Description: Fresh seeded worlds stream deterministic 32-cubed chunks and preserve chunk edits across save and reload.
- Why it matters: Deterministic terrain and durable edits underpin every later milestone.
- Source: M001 / D022 item 1
- Primary owning slice: M001
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::terrain; cargo test --locked --lib -- world::manager; cargo test --locked --lib -- world::persistence; bash scripts/launch_smoke.sh

### R002 — Settings load from settings.toml with per-field fallback and atomic writes only on accepted changes
- Class: quality-attribute
- Status: active
- Description: Settings load from settings.toml with per-field fallback and atomic writes only on accepted changes.
- Why it matters: A bad settings file must never block launch or corrupt saved preferences.
- Source: M001 / D022 item 2
- Primary owning slice: M001
- Supporting slices: None
- Validation: cargo test --locked --lib -- settings::; bash scripts/settings_smoke.sh

### R003 — Main menu and world catalog create, list, load, and delete named worlds with inline errors and non-destructive corrupt-save handling
- Class: primary-user-loop
- Status: active
- Description: Main menu and world catalog create, list, load, and delete named worlds with inline errors and non-destructive corrupt-save handling.
- Why it matters: The world lifecycle is the primary menu loop.
- Source: M002 / D022 item 3
- Primary owning slice: M002
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::catalog; bash scripts/menu_smoke.sh

### R004 — Pause, settings, and menu input routing with rebinding and session-preserving return
- Class: primary-user-loop
- Status: active
- Description: Pause, settings, and menu input routing with rebinding and session-preserving return.
- Why it matters: Navigation must never lose the running session.
- Source: M002 / D022 item 4
- Primary owning slice: M002
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::; bash scripts/launch_smoke.sh; bash scripts/menu_smoke.sh; bash scripts/settings_smoke.sh

### R005 — Deterministic 3D caves and coal/iron veins with ores below h-3 and bedrock at wy=-32
- Class: core-capability
- Status: active
- Description: Deterministic 3D caves and coal/iron veins with ores below h-3 and bedrock at wy=-32.
- Why it matters: Cave and ore discovery is the resource-spine foundation.
- Source: M003 / D022 item 5
- Primary owning slice: M003/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::terrain

### R006 — Per-block hardness hold-to-mine with visible progress and reset on release or retarget
- Class: primary-user-loop
- Status: active
- Description: Per-block hardness hold-to-mine with visible progress and reset on release or retarget.
- Why it matters: Mining must take meaningful time and remain responsive.
- Source: M003 / D022 item 6
- Primary owning slice: M003/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- player::mining; cargo test --locked --lib -- player::physics; cargo test --locked --bin opencraft

### R007 — Bedrock is unminable under any hold or tool
- Class: constraint
- Status: active
- Description: Bedrock is unminable under any hold or tool.
- Why it matters: The world floor must remain indestructible.
- Source: M003 / D022 item 7
- Primary owning slice: M003/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- player::mining; cargo test --locked --lib -- world::persistence

### R008 — Nine-slot finite id+count hotbar where mining collects the pre-break block, successful placement consumes one, and failed placement consumes nothing
- Class: primary-user-loop
- Status: active
- Description: Nine-slot finite id+count hotbar where mining collects the pre-break block, successful placement consumes one, and failed placement consumes nothing.
- Why it matters: Mined blocks become finite collected resources.
- Source: M003 / D022 item 8
- Primary owning slice: M003/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- player::inventory; cargo test --locked --bin opencraft

### R009 — SAVE_VERSION 2 hotbar header, v1 migration to an empty hotbar without data loss, and malformed-slot rejection without panic
- Class: continuity
- Status: active
- Description: SAVE_VERSION 2 hotbar header, v1 migration to an empty hotbar without data loss, and malformed-slot rejection without panic.
- Why it matters: Pre-M003 saves must keep loading and corrupt stacks must not crash.
- Source: M003 / D022 item 9
- Primary owning slice: M003/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::persistence; cargo test --locked --bin opencraft

### R010 — Per-slot hotbar counts on every screen and the selected stack name and count in the window title
- Class: primary-user-loop
- Status: active
- Description: Per-slot hotbar counts on every screen and the selected stack name and count in the window title.
- Why it matters: Players need to see finite resources at a glance.
- Source: M003 / D022 item 10
- Primary owning slice: M003/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::hud; cargo test --locked --bin opencraft

### R011 — Package, library, and binary named opencraft with no stale legacy project identity
- Class: launchability
- Status: active
- Description: Package, library, and binary named opencraft with no stale legacy project identity.
- Why it matters: The shipped identity must match OpenCraft.
- Source: M003 / D022 item 11
- Primary owning slice: M003/S04
- Supporting slices: None
- Validation: cargo build --locked; test -x target/debug/opencraft; ! rg -qi 'minecraft[-_ ]?clone' Cargo.toml Cargo.lock src scripts

### R012 — MIT LICENSE and Cargo license = "MIT"
- Class: compliance/security
- Status: active
- Description: MIT LICENSE and Cargo license = "MIT".
- Why it matters: Open-source distribution requires an explicit license.
- Source: M003 / D022 item 12
- Primary owning slice: M003/S04
- Supporting slices: None
- Validation: test -f LICENSE; grep -q '^license = "MIT"' Cargo.toml

### R013 — cargo clippy --all-targets --all-features -- -D warnings is clean with no new allowances
- Class: quality-attribute
- Status: active
- Description: cargo clippy --all-targets --all-features -- -D warnings is clean with no new allowances.
- Why it matters: The hygiene gate enforces a warning-free tree.
- Source: M003 / D022 item 13
- Primary owning slice: M003/S04
- Supporting slices: None
- Validation: cargo clippy --all-targets --all-features -- -D warnings; cargo test --locked

### R014 — Four bounded smoke harnesses pass against target/debug/opencraft with byte-preservation assertions
- Class: operability
- Status: active
- Description: Four bounded smoke harnesses pass against target/debug/opencraft with byte-preservation assertions.
- Why it matters: Launch, menu, and settings must keep working end to end.
- Source: M003 / D022 item 14
- Primary owning slice: M003/S04
- Supporting slices: None
- Validation: bash scripts/launch_smoke.sh; bash scripts/menu_smoke.sh; bash scripts/settings_smoke.sh; bash scripts/pack_smoke.sh

### R015 — The formal requirements registry carries stable RXXX IDs and supports M003 validation tracing
- Class: admin/support
- Status: active
- Description: The formal requirements registry carries stable RXXX IDs and supports M003 and M004 validation tracing.
- Why it matters: Milestone validation must trace criteria to stable IDs.
- Source: M003 / D022 item 15
- Primary owning slice: M003/S04
- Supporting slices: M003/S05, M004/S05
- Validation: bash scripts/requirements_check.sh

### R016 — Texture packs load from packs/<name>/ folders with pack.toml and square PNG layer overrides (16-256 px resampled to 16x16) with per-file fallback and structured warnings that never block launch
- Class: differentiator
- Status: active
- Description: Texture packs load from packs/<name>/ folders with pack.toml and square PNG layer overrides (16-256 px resampled to 16x16) with per-file fallback and structured warnings that never block launch.
- Why it matters: A fork must re-skin the entire game without writing engine code.
- Source: M004 / S01 pack loading core
- Primary owning slice: M004/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- pack::

### R017 — The main-menu cluster floats over a slowly rotating fixed-seed vista panorama at the configured render distance while Pause shows the paused world and gameplay overlays are suppressed off-Playing
- Class: quality-attribute
- Status: active
- Description: The main-menu cluster floats over a slowly rotating fixed-seed vista panorama at the configured render distance while Pause shows the paused world and gameplay overlays are suppressed off-Playing.
- Why it matters: The front door must present a live 3D world, not a static menu.
- Source: M004 / S02 vista panorama
- Primary owning slice: M004/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::vista; cargo test --locked --bin opencraft

### R018 — The Texture Pack settings row cycles discovered packs with live apply and restart persistence via the [packs] section, and a missing pack falls back to Default with an in-menu warning naming the pack
- Class: primary-user-loop
- Status: active
- Description: The Texture Pack settings row cycles discovered packs with live apply and restart persistence via the [packs] section, and a missing pack falls back to Default with an in-menu warning naming the pack.
- Why it matters: Pack choice must be a first-class, persistent user setting that never blocks launch.
- Source: M004 / S03 texture pack setting
- Primary owning slice: M004/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- settings::; cargo test --locked --bin opencraft; bash scripts/pack_smoke.sh

### R019 — Title branding: a large default text title over the vista, an aspect-preserved pack logo at native size up to 1024 px per side, and a panorama tint quad, with corrupt-logo fallback to the text title and a warning
- Class: differentiator
- Status: active
- Description: Title branding: a large default text title over the vista, an aspect-preserved pack logo at native size up to 1024 px per side, and a panorama tint quad, with corrupt-logo fallback to the text title and a warning.
- Why it matters: A pack must be able to brand the game's front door.
- Source: M004 / S04 title branding
- Primary owning slice: M004/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::logo; cargo test --locked --bin opencraft load_startup_pack

### R020 — Pack authoring documentation and the M004 closure surface: registry rows, criterion trace, and the fourth smoke harness
- Class: admin/support
- Status: active
- Description: Pack authoring documentation and the M004 closure surface: registry rows, criterion trace, and the fourth smoke harness.
- Why it matters: A fresh reader must author a working pack without reading Rust, and the closure gates must stay green.
- Source: M004 / S05 closure
- Primary owning slice: M004/S05
- Supporting slices: None
- Validation: bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh; bash scripts/pack_smoke.sh

## M003 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R005 | S01 | cargo test --locked --lib -- world::terrain |
| 2 | R006, R007 | S02 | cargo test --locked --lib -- player::mining && cargo test --locked --lib -- world::persistence |
| 3 | R008, R009, R010 | S03 | cargo test --locked --lib -- player::inventory && cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- ui::hud |
| 4 | R011, R012, R013, R014 | S04 | cargo build --locked && cargo clippy --all-targets --all-features -- -D warnings && cargo test --locked && bash scripts/launch_smoke.sh && bash scripts/menu_smoke.sh && bash scripts/settings_smoke.sh |
| 5 | R015 | S04, S05 | bash scripts/requirements_check.sh |

## M004 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R016, R018 | S01, S03 | cargo test --locked --lib -- pack:: && cargo test --locked --bin opencraft && bash scripts/pack_smoke.sh |
| 2 | R016 | S01 | cargo test --locked --lib -- pack:: |
| 3 | R018 | S03 | cargo test --locked --lib -- settings:: && bash scripts/pack_smoke.sh |
| 4 | R017 | S02 | cargo test --locked --lib -- world::vista && cargo test --locked --bin opencraft |
| 5 | R016, R017, R018, R019 | S01, S02, S03, S04 | cargo test --locked |
| 6 | R014, R020 | S05 | bash scripts/launch_smoke.sh && bash scripts/menu_smoke.sh && bash scripts/settings_smoke.sh && bash scripts/pack_smoke.sh && bash scripts/requirements_check.sh |
| 7 | R020 | S05 | docs/TEXTURE-PACKS.md quickstart reader-test in bash scripts/pack_smoke.sh |
