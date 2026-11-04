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
- Status: superseded
- Superseded by: M007/S01
- Description: Deterministic 3D caves and coal/iron veins with ores below h-3 and bedrock at wy=-32.
- Why it matters: Cave and ore discovery is the resource-spine foundation.
- Source: M003 / D022 item 5
- Primary owning slice: M003/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::terrain
- Note: Deliberately superseded by M007/S01 (four-biome terrain rewrite). What survives and stays pinned by the terrain tests: caves carve Stone only strictly below h-3/h-4, ores stay in their depth bands below h-3, and bedrock stays at wy=-32. Only the implicit single-biome grassland height envelope is replaced, not the cave/ore/bedrock contract.

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

### R021 — Sky-light occlusion: 15-level propagated sky light with per-vertex terrain lighting keeps caves and overhangs genuinely dark at any time of day with no visible chunk-border seams
- Class: core-capability
- Status: active
- Description: Sky-light occlusion: 15-level propagated sky light with per-vertex terrain lighting keeps caves and overhangs genuinely dark at any time of day with no visible chunk-border seams.
- Why it matters: Darkness must come from real occlusion of world geometry, not uniform dimming, so caves read as caves and the world stays honest at every hour.
- Source: M005 / S01 skylight propagation
- Primary owning slice: M005/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::light; cargo test --locked --lib -- world::manager

### R022 — Day/night cycle visuals: a 20-real-minute game-time cycle wheels interpolated sky and fog colors, sun and moon quads, a night star field, and terrain dimming down to the roughly one-third moonlit floor and back, with the main-menu vista pinned at a fixed golden hour
- Class: quality-attribute
- Status: active
- Description: Day/night cycle visuals: a 20-real-minute game-time cycle wheels interpolated sky and fog colors, sun and moon quads, a night star field, and terrain dimming down to the roughly one-third moonlit floor and back, with the main-menu vista pinned at a fixed golden hour.
- Why it matters: The sky must visibly live through a full day — a wheeling cycle sells the world while the moonlit floor keeps night readable before torches exist.
- Source: M005 / S02 day cycle and S03 sky pass
- Primary owning slice: M005/S02
- Supporting slices: M005/S03, M005/S07, M005/S08
- Validation: cargo test --locked --lib -- world::daycycle (incl. NIGHTFALL/pre-dawn keyframe pins and dusk/midnight/dawn sRGB probe; S08 exhaustive step-1 sun-brightness-zero star-visibility sweep + boundary-tick pins + STAR_ANGULAR_SIZE ratio pins); cargo test --locked --lib -- render::sky (incl. square MOON_VERTEX_COUNT geometry pins and S08 kind-gated circular alpha mask WGSL pins); cargo test --locked --bin opencraft

### R023 — Honest game-time clock: it advances only while Playing, freezes in every menu and pause, wraps at cycle end, and persists per world as a SAVE_VERSION 3 time-of-day field with v1/v2 migration to default noon, so Save-and-Quit resumes the exact time of day
- Class: continuity
- Status: active
- Description: Honest game-time clock: it advances only while Playing, freezes in every menu and pause, wraps at cycle end, and persists per world as a SAVE_VERSION 3 time-of-day field with v1/v2 migration to default noon, so Save-and-Quit resumes the exact time of day.
- Why it matters: A paused or closed world must be a frozen world — game time is world state, never wall-clock (D013), and relaunching must resume where the player left off.
- Source: M005 / S02 game-time clock
- Primary owning slice: M005/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::persistence; cargo test --locked --lib -- world::session; cargo test --locked --bin opencraft

### R024 — Audio layer: a fail-soft rodio engine plays discrete synthesized break, place, mining-complete and UI-click sounds, and the Volume row (0-100 percent in 10 percent steps) live-applies, atomically saves, persists across restart, and mutes at 0 percent; a machine with no or blocked audio device launches and plays silently
- Class: core-capability
- Status: active
- Description: Audio layer: a fail-soft rodio engine plays discrete synthesized break, place, mining-complete and UI-click sounds, and the Volume row (0-100 percent in 10 percent steps) live-applies, atomically saves, persists across restart, and mutes at 0 percent; a machine with no or blocked audio device launches and plays silently.
- Why it matters: Sound is a core sensory channel and volume must be a first-class persistent setting; audio failure must never block launch or surface an error state.
- Source: M005 / S04 audio engine
- Primary owning slice: M005/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- audio::; cargo test --locked --lib -- settings::; bash scripts/launch_smoke.sh

### R025 — Pack-loadable sound overrides: pack sound files override the synthesized defaults per slot with never-fail fallback (missing is silent; corrupt, oversized or empty warns in-menu and keeps the default), live apply on Texture Pack cycling, and Default restore of the built-in sounds
- Class: differentiator
- Status: active
- Description: Pack-loadable sound overrides: pack sound files override the synthesized defaults per slot with never-fail fallback (missing is silent; corrupt, oversized or empty warns in-menu and keeps the default), live apply on Texture Pack cycling, and Default restore of the built-in sounds.
- Why it matters: Every sound must be moddable content, not code (D015), and no pack file state may ever block launch.
- Source: M005 / S05 pack sounds
- Primary owning slice: M005/S05
- Supporting slices: M005/S09
- Validation: cargo test --locked --lib -- pack::sound; bash scripts/pack_smoke.sh (incl. S09 scenario-0 doc reader-test: the author-creates-packs/ discoverability wording, the copy-paste four-sound example section, and the tests/fixtures/packs/uat-sound fixture as the documented runnable example)

### R026 — M005 closure surface: registry rows R021-R026, the M005 criterion trace, four green smoke harnesses, and recorded human UAT for the human-sensory criteria
- Class: admin/support
- Status: active
- Description: M005 closure surface: registry rows R021-R026, the M005 criterion trace, four green smoke harnesses, and recorded human UAT for the human-sensory criteria.
- Why it matters: Milestone validation must trace every M005 criterion to stable registry IDs and the sensory outcomes need recorded human judgment.
- Source: M005 / S06 closure
- Primary owning slice: M005/S06
- Supporting slices: M005/S09
- Validation: bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh (S09 records the UAT-H5 re-test protocol against the tests/fixtures/packs/uat-sound fixture)

### R027 — Four-biome terrain with sea-level ocean basins and worldgen-only water
- Class: core-capability
- Status: active
- Description: One seeded world generates four distinguishable biomes (plains, forest, mountains, oceans) from a single height function, with ocean basins filled with water below a named sea level, and water exists only as generating world content that is never persisted or editable.
- Why it matters: The world must change across space as well as time, and water must stay a generating feature rather than saved world state.
- Source: M007 / S01 terrain rewrite
- Primary owning slice: M007/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::terrain; cargo test --locked --lib -- world::block; cargo test --locked --lib -- world::manager; cargo test --locked --release --lib -- --ignored streaming_budget --nocapture

### R028 — SAVE_VERSION 5 persisted calendar and the New Game season-length row
- Class: continuity
- Status: active
- Description: Saves carry day_index and season_length_days in a SAVE_VERSION 5 tail with v1-v4 migration defaults (day 0, one-day seasons), the clock advances only while Playing, and New Game exposes a bounded season-length cycler.
- Why it matters: Seasons must resume exactly across relaunch while older saves keep loading without data loss.
- Source: M007 / S02 season clock
- Primary owning slice: M007/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::persistence; cargo test --locked --lib -- world::season; cargo test --locked --lib -- world::daycycle; bash scripts/season_smoke.sh

### R029 — Deliberate swim mechanics: idle sink, Jump rise, no surface lock
- Class: core-capability
- Status: active
- Description: Water applies drag and gravity to an idle submerged body so it sinks away from the surface; holding Jump/Space applies a bounded rise back up, with no automatic surface lock. Water stays passable-but-never-walkable at every mobility consumer, and the body may stand only on the solid seabed (D087).
- Why it matters: The human sensory pass rejected the old buoyant auto-float (walking onto water only wobbled at the surface; only fly mode could submerge), so crossings must be a deliberate swim: sink when idle, rise with Jump, and traverse generated oceans through the production physics seams. Mechanics only; fall damage and drowning are honestly deferred to M008.
- Source: M007 / S03 swim physics (reworked by D087 in M007/S08-T07)
- Primary owning slice: M007/S03
- Supporting slices: M007/S08
- Validation: cargo test --locked --lib -- swim; cargo test --locked --lib -- player::physics; cargo test --locked --lib -- ocean_crossing; cargo test --locked --bins -- swim

### R030 — Translucent water render pass and underwater fog shift
- Class: quality-attribute
- Status: active
- Description: Water surfaces render in a separate alpha-blended depth-write-off pass after opaque geometry and before overlays, and the fog palette shifts to an underwater range when the camera eye submerges.
- Why it matters: The ocean surface must read as translucent at correct depth order while the opaque path stays untouched and fail-soft.
- Source: M007 / S04 translucent water
- Primary owning slice: M007/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- render::buffer; cargo test --locked --lib -- render::pipeline; bash scripts/water_smoke.sh

### R031 — Seasonal worldgen surfaces and the bounded rollover wave
- Class: core-capability
- Status: active
- Description: Worldgen surfaces are pure functions of position, seed, and season (winter snow blanket on land, seasonal oak leaves only: Spring/Summer/Autumn canopy, Winter bare over persistent trunks and branches), and a season change arrives through a bounded swap-on-arrival regeneration wave that never removes chunks or writes seasonal block edits.
- Why it matters: Seasons must visibly change the world without a one-frame full-world remesh or persisted seasonal state, and tree lifecycles must stay believable: only leaves are seasonal, while the deterministic trunk and branch skeleton renders in every season and is never clipped at a chunk edge.
- Source: M007 / S05 seasonal worldgen (tree lifecycle corrected by D088 in M007/S08-T08; leaf shed narrowed to Winter in quick task 5)
- Primary owning slice: M007/S05
- Supporting slices: M007/S08
- Validation: cargo test --locked --lib -- world::terrain; cargo test --locked --lib -- world::manager; bash scripts/season_smoke.sh

### R032 — Weather states and precipitation presentation
- Class: quality-attribute
- Status: active
- Description: Pure weather derivation yields Clear, Rain, and Snowfall per season and day, with rain in warm seasons, snowfall only in winter, and matching sky, fog, and precipitation presentation that is deterministic for a given seed and identity-clean when clear.
- Why it matters: Weather is a core sensory channel of the seasonal year and must stay a presentation derivation, never saved state or block interaction.
- Source: M007 / S06 weather
- Primary owning slice: M007/S06
- Supporting slices: None
- Validation: cargo test --locked --lib -- render::weather; cargo test --locked --lib -- world::daycycle; cargo test --locked --bins -- weather; bash scripts/weather_smoke.sh

### R033 — Deterministic surface ruins with the present-but-empty loot seam
- Class: core-capability
- Status: superseded
- Superseded by: M008/S06
- Description: Small surface ruins generate deterministically per seed on grass plains and forest columns, are cleared from spawn footprints, and expose a named loot seam that is present but deliberately empty in M007.
- Why it matters: Structures make exploration pay off structurally, and the empty seam lets M008 add survival loot without a worldgen signature change.
- Source: M007 / S07 surface ruins
- Primary owning slice: M007/S07
- Supporting slices: None
- Validation: cargo test --locked --lib -- ruin; cargo test --locked --lib -- ruin_loot
- Note: Deliberately superseded by M008/S06 (deterministic ruin LootChests): the surface-ruin shell, spawn-footprint clearing, named loot seam, and seed-determinism contract all survive, while the empty M007 loot seam is now filled.

### R034 — Streaming budget proof and the M007 closure surface
- Class: admin/support
- Status: active
- Description: The headless release harness proves per-frame streaming dispatch at p95 at most 8 ms and max at most 16 ms while crossing at least 32 chunks at default render distance, a bounded release run reports p95 frame time at most 16.6 ms, the real binary round-trips a mid-winter save and swims an ocean, and the registry carries the M007 rows and criterion trace with save and release notes.
- Why it matters: Milestone closure needs measured operational evidence and a traceable capability contract, not prose claims.
- Source: M007 / S08 closure
- Primary owning slice: M007/S08
- Supporting slices: M007/S01, M007/S03, M007/S05, M007/S06, M007/S07
- Validation: cargo test --locked --release --lib -- --ignored streaming_budget --nocapture; bash scripts/swim_smoke.sh; bash scripts/winter_smoke.sh; bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh

### R035 — Persisted per-world Survival and Creative modes
- Class: core-capability
- Status: active
- Description: New Game defaults to Survival and offers Creative; a fresh command-line world defaults to Creative and accepts --creative/--survival last-wins; a loaded world keeps its stored mode, the mode is locked after creation, Creative flies and is invulnerable/never hungry, and Edit Background uses effective Creative without rewriting the stored mode.
- Why it matters: Mode drives damage, hunger, flight, and background isolation, and must not silently change old worlds or leak Survival rules into the menu background.
- Source: M008 / S01 modes
- Primary owning slice: M008/S01
- Supporting slices: M008/S04
- Validation: cargo test --locked --lib -- player::survival; cargo test --locked --lib -- ui::screens; cargo test --locked --bin opencraft; bash scripts/survival_smoke.sh

### R036 — Survival vitals and environmental damage
- Class: core-capability
- Status: active
- Description: Persisted health, hunger, and air drive fall, drowning, and starvation damage, hunger drain, and regeneration only at high hunger; Creative is immune, never hungry, and never loses air.
- Why it matters: These rules are the survival loop's core state machine and must be deterministic and headless-testable.
- Source: M008 / S04 survival loop
- Primary owning slice: M008/S04
- Supporting slices: M008/S01, M008/S05
- Validation: cargo test --locked --lib -- player::survival; cargo test --locked --lib -- player::physics; cargo test --locked --lib -- world::session; bash scripts/survival_smoke.sh

### R037 — Death and respawn with inventory kept
- Class: primary-user-loop
- Status: active
- Description: The Death screen offers Respawn and Quit; Respawn returns to the deterministic spawn with full vitals and the inventory kept; a health-zero save opens the Death screen; Quit saves and returns to the menu.
- Why it matters: Death needs a readable endpoint that does not destroy progression.
- Source: M008 / S04 death and respawn
- Primary owning slice: M008/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::lifecycle; cargo test --locked --lib -- world::session; cargo test --locked --bin opencraft; bash scripts/survival_smoke.sh

### R038 — Survival HUD and hold-to-eat food
- Class: primary-user-loop
- Status: active
- Description: The HUD shows hearts, hunger, and air; oak leaves deterministically drop apples; holding right-click for 1.6 s eats an apple or bread with a progress bar, cancels on release or movement, refuses at full hunger, and restores the item's hunger value.
- Why it matters: Visible vitals and food make the survival loop playable and give the human UAT its primary sensory subject.
- Source: M008 / S04 HUD and S05 food
- Primary owning slice: M008/S05
- Supporting slices: M008/S04
- Validation: cargo test --locked --lib -- ui::hud; cargo test --locked --lib -- player::eating; cargo test --locked --lib -- world::loot; cargo test --locked --bin opencraft

### R039 — 27-slot chests with lossless break preflight
- Class: core-capability
- Status: active
- Description: A Chest crafts from eight planks, places, opens a 27-slot screen with the 36-slot inventory, moves stacks through the cursor, persists contents per position, and breaks only when all contents plus one Chest item fit in the inventory; a refusal leaves the chest and inventory unchanged.
- Why it matters: Storage must be usable and lossless, including under a full inventory.
- Source: M008 / S03 chests
- Primary owning slice: M008/S03
- Supporting slices: M008/S06
- Validation: cargo test --locked --lib -- player::container; cargo test --locked --lib -- ui::chest; cargo test --locked --lib -- world::persistence; cargo test --locked --bin opencraft; bash scripts/content_smoke.sh

### R040 — Deterministic ruin LootChests
- Class: core-capability
- Status: active
- Description: A complete surface ruin contains one 27-slot LootChest whose contents are deterministic from seed and ruin and capacity-bound; a touched-empty record persists without re-rolling; breaking the LootChest yields one ordinary Chest item and its preflighted contents.
- Why it matters: Exploration must pay off deterministically and without duplication or re-rolls.
- Source: M008 / S06 ruins and loot
- Primary owning slice: M008/S06
- Supporting slices: M008/S03
- Validation: cargo test --locked --lib -- ruin; cargo test --locked --lib -- ruin_loot; cargo test --locked --lib -- world::loot; cargo test --locked --lib -- world::session; bash scripts/content_smoke.sh

### R041 — Hoe and wheat farming with Winter pause
- Class: core-capability
- Status: active
- Description: Wood/Stone/Iron hoes till grass or dirt into Farmland with durability; WheatSeeds plant on Farmland and advance through four Wheat stages on game time; Winter pauses growth; mature harvest yields Wheat and seeds, immature harvest returns a seed; Bread crafts from wheat; crop stages persist as chunk edits while growth progress does not.
- Why it matters: The farm chain is the durable food source and must survive save/reload and chunk unload/regeneration.
- Source: M008 / S07 farming
- Primary owning slice: M008/S07
- Supporting slices: M008/S05, M008/S06
- Validation: cargo test --locked --lib -- world::farming; cargo test --locked --lib -- world::manager; cargo test --locked --lib -- player::inventory; cargo test --locked --bin opencraft; bash scripts/content_smoke.sh; cargo test --release --locked --lib -- world::session; cargo test --release --locked --bin opencraft; SMOKE_PROFILE=release bash scripts/content_smoke.sh

### R042 — 32-layer texture and pack extension
- Class: differentiator
- Status: active
- Description: The texture array has 32 layers with procedural Farmland, four Wheat stages, Chest, and LootChest art; 23 layer names are authorable; layers 23-31 are reserved; existing 16-layer packs fall back per layer, and optional new-layer overrides work.
- Why it matters: New blocks must read distinctly and remain moddable without breaking old packs.
- Source: M008 / S02 texture packs
- Primary owning slice: M008/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- render::texture; cargo test --locked --lib -- pack::; bash scripts/pack_smoke.sh

### R043 — Save v6 fail-closed continuity
- Class: continuity
- Status: active
- Description: Save v6 round-trips mode, health, hunger, air, and sorted 27-slot chest records; v1-v5 saves load as Creative with full vitals and no chests; unknown block, item, mode, and chest data and water are rejected in both directions.
- Why it matters: Old worlds must load and corrupt or newer saves must fail closed rather than corrupt a world.
- Source: M008 / S01 save v6 and S03 chest persistence
- Primary owning slice: M008/S01
- Supporting slices: M008/S03
- Validation: cargo test --locked --lib -- world::persistence; cargo test --locked --lib -- world::session; bash scripts/survival_smoke.sh; bash scripts/content_smoke.sh

### R044 — M008 closure surface
- Class: admin/support
- Status: active
- Description: README and SAVE-NOTES document the M008 survival layer; the registry carries the M008 rows and criterion trace; the full gate and all 12 real-binary smokes pass; and a recorded human windowed UAT with screenshots covers the HUD, chest screen, four wheat stages, hold-to-eat progress, and Death screen.
- Why it matters: Closure must trace every criterion, and the sensory outcomes need recorded human judgment rather than inferred claims.
- Source: M008 / S09 closure
- Primary owning slice: M008/S09
- Supporting slices: M008/S08
- Validation: bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh; cargo fmt --all -- --check; cargo clippy --all-targets -- -D warnings; cargo test --locked; bash scripts/survival_smoke.sh; bash scripts/content_smoke.sh; cargo test --release --locked; SMOKE_PROFILE=release bash scripts/survival_smoke.sh; SMOKE_PROFILE=release bash scripts/content_smoke.sh; recorded human windowed UAT

### R045 — Batched four-mob box-model rendering and animation
- Class: core-capability
- Status: active
- Description: Pig, Cow, Zombie, and Skeleton render through a batched four-mob box-model path with procedural walk, hurt, and death animation and thirteen pack-overridable mob texture layers.
- Why it matters: Mobs must be visible as distinct creatures whose movement and damage are readable in the world.
- Source: M009 / S01 rendering
- Primary owning slice: M009/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- render::mob; cargo test --locked --lib -- world::mob; bash scripts/pack_smoke.sh

### R046 — Save v7 mob persistence and migration
- Class: continuity
- Status: active
- Description: Save format v7 persists each mob's kind, feet position, yaw, and health, validates those records fail-closed, and migrates v1-v6 saves with zero mobs.
- Why it matters: A world must restore its mobs exactly and must never load a corrupt mob record into play.
- Source: M009 / S02 persistence
- Primary owning slice: M009/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::persistence; cargo test --locked --lib -- world::session; bash scripts/combat_smoke.sh

### R047 — Item vocabulary and player melee
- Class: core-capability
- Status: active
- Description: The player fights with fists plus wood, stone, and iron swords at 4/5/6 damage, a ~0.6 s cooldown, 0.5 s invulnerability frames, and knockback, with attack-before-mining input priority.
- Why it matters: Melee is the baseline combat loop, and its timing and priority rules decide whether fighting feels responsive and fair.
- Source: M009 / S03 melee
- Primary owning slice: M009/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- player::melee; cargo test --locked --lib -- player::inventory; bash scripts/combat_smoke.sh

### R048 — Passive flee/death lifecycle and deterministic drops
- Class: core-capability
- Status: active
- Description: Passive mobs flee when hurt and run a death lifecycle, and Pig/Cow drops land deterministically through the best-effort inventory path with an overflow-only notice.
- Why it matters: Hunting passives must produce predictable rewards without silently losing items.
- Source: M009 / S04 passive lifecycle and loot
- Primary owning slice: M009/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::mob_loot; cargo test --locked --lib -- player::inventory; bash scripts/combat_smoke.sh

### R049 — Light-and-clock mob spawning
- Class: core-capability
- Status: active
- Description: Mobs spawn from light and clock rules with a ~24-block safe radius, a global cap of 32 plus at most 4 per region, despawn beyond 96 blocks, and 0.5 s daylight burning.
- Why it matters: Spawn rules keep the world populated without ambushing the player at home or overrunning the frame budget.
- Source: M009 / S05 spawning
- Primary owning slice: M009/S05
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::spawning; cargo test --locked --lib -- world::daycycle; bash scripts/combat_smoke.sh

### R050 — Hostile steering AI, zombie melee, and damage feedback
- Class: core-capability
- Status: active
- Description: Hostiles chase by line of sight with one-block step-up, cliff guarding, and stuck re-aggro; zombies melee the player, damage shows feedback, and the ephemeral death cause renders on the Death screen.
- Why it matters: Hostiles must pursue believably, and the player must be able to read damage and the reason for death.
- Source: M009 / S06 hostile AI and combat
- Primary owning slice: M009/S06
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::mob_ai; cargo test --locked --lib -- player::combat; bash scripts/combat_smoke.sh

### R051 — Shortbow and shared arrow physics
- Class: core-capability
- Status: active
- Description: The shortbow supports charge, release, and cancel, and shares gravity-affected arrow physics with block embedding and entity hits.
- Why it matters: Ranged combat is the player's answer to skeletons, and shared physics keeps every arrow consistent.
- Source: M009 / S07 bow and arrows
- Primary owning slice: M009/S07
- Supporting slices: None
- Validation: cargo test --locked --lib -- player::bow; cargo test --locked --lib -- world::arrow; bash scripts/combat_smoke.sh

### R052 — Skeleton ranged AI and skeleton drops
- Class: core-capability
- Status: active
- Description: Skeletons use ranged AI and drop bones, arrows, and a rare bow, and a skeleton arrow records the `Skeleton arrow` death cause.
- Why it matters: Skeleton encounters need a distinct threat and reward identity that the death screen can explain.
- Source: M009 / S08 skeleton combat
- Primary owning slice: M009/S08
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::mob_ai; cargo test --locked --lib -- world::mob_loot; bash scripts/combat_smoke.sh

### R053 — Creative inertness and mob/combat audio
- Class: integration
- Status: active
- Description: Creative mode stays inert at both authority sites, and nine mob/combat audio slots play with per-file texture-pack fallback.
- Why it matters: Creative must not fight or take damage, and combat needs audible feedback that pack authors can override.
- Source: M009 / S09 creative and audio
- Primary owning slice: M009/S09
- Supporting slices: None
- Validation: cargo test --locked --lib -- creative_; cargo test --locked --lib -- world::mob_audio; bash scripts/pack_smoke.sh; bash scripts/combat_smoke.sh

### R054 — M009 closure surface
- Class: admin/support
- Status: active
- Description: M009 closes with the combat smoke proving the full real-binary capstone in debug and release, the registry and criterion trace carrying R045-R053, the docs updated, and the recorded human UAT prepared.
- Why it matters: Closure must be reproducible on the assembled tree, and the sensory outcomes need recorded human judgment rather than inferred claims.
- Source: M009 / S10 closure
- Primary owning slice: M009/S10
- Supporting slices: M009/S01, M009/S02, M009/S03, M009/S04, M009/S05, M009/S06, M009/S07, M009/S08, M009/S09
- Validation: bash scripts/combat_smoke.sh; SMOKE_PROFILE=release bash scripts/combat_smoke.sh; bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh

### R055 — Textured item icons for every carryable stack
- Class: core-capability
- Status: active
- Description: Every carryable item draws an icon in the 36 inventory slots, both crafting grids and result previews, the cursor ghost, and the nine hotbar slots: blocks as textured three-face cubes sampled from the active pack, materials and tools as deterministic procedural sprites, and empty slots with no icon.
- Why it matters: A stack must be recognizable at a glance in both the inventory and the hotbar, and the icon must follow the active texture pack.
- Source: M013 / S01, S02 item icon renderer
- Primary owning slice: M013/S01
- Supporting slices: M013/S02
- Validation: cargo test --locked --lib -- ui::item_icon; bash scripts/ui_smoke.sh

### R056 — Inventory hotbar as a separated bottom row
- Class: primary-user-loop
- Status: active
- Description: The inventory panel draws the nine hotbar slots as a visually separated bottom row below the three storage rows, with slot indices 0..8 still the hotbar and every hit-test, drag, split, and stack-transfer semantic unchanged.
- Why it matters: Players must read storage and hotbar as distinct groups without relearning the inventory.
- Source: M013 / S03 inventory layout
- Primary owning slice: M013/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::inventory; bash scripts/ui_smoke.sh

### R057 — Dedicated pack-overridable crafting-table art
- Class: integration
- Status: active
- Description: The crafting table has dedicated top and side textures (authorable layers 23 and 24) visibly different from oak planks, and packs can override them with crafting_table_top.png and crafting_table_side.png while missing files fall back silently and corrupt files warn and fall back.
- Why it matters: The crafting table must read as a distinct workstation and remain authorable without breaking existing packs.
- Source: M013 / S04 crafting-table textures
- Primary owning slice: M013/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- render::texture; cargo test --locked --lib -- world::block; cargo test --locked --lib -- pack::; bash scripts/pack_smoke.sh

### R058 — Clamped item tooltips on inventory hover
- Class: primary-user-loop
- Status: active
- Description: Hovering an occupied inventory slot, crafting cell, result slot, or cursor stack shows a tooltip with the item name and count/durability detail; the tooltip stays inside the surface, flips away from edges, and disappears when the hover ends.
- Why it matters: Players need to identify stacks and tool condition before moving or using them.
- Source: M013 / S05 inventory tooltips
- Primary owning slice: M013/S05
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::inventory; cargo test --locked --bin opencraft -- tooltip; bash scripts/ui_smoke.sh

### R059 — Transient selected-hotbar-item name
- Class: primary-user-loop
- Status: active
- Description: Selecting a hotbar slot shows the item name above the hotbar on a dark strip, fades out after about two seconds, restarts on a new selection or stack change, and draws nothing for an empty slot or while a panel owns the pointer.
- Why it matters: The player needs quick confirmation of the active item without opening the inventory.
- Source: M013 / S05 selected-item name
- Primary owning slice: M013/S05
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::selection; cargo test --locked --bin opencraft -- selected_name; bash scripts/ui_smoke.sh

### R060 — Resolution-safe vitals HUD scaling
- Class: quality-attribute
- Status: active
- Description: Vitals icons, gaps, and rows scale continuously from the measured hotbar width so hearts and hunger never overlap at 1280x720, 1920x1080, and 3840x2160, stay above the hotbar and inside the surface, and keep the air row on its own line.
- Why it matters: HUD legibility must survive the full range of supported resolutions without resolution-specific breakpoints.
- Source: M013 / S06 vitals scaling
- Primary owning slice: M013/S06
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::hud::tests::vitals; cargo test --locked --bin opencraft -- playing_hud_quads; bash scripts/ui_smoke.sh

### R061 — M013 closure surface
- Class: admin/support
- Status: active
- Description: M013 closes with a real-binary scripts/ui_smoke.sh proving the pinned UI digests and bounded launch in debug and release, the registry and criterion trace carrying R055-R060, the docs updated, and the recorded 720p/1080p human UAT prepared.
- Why it matters: Closure must be reproducible on the assembled tree, and the sensory outcomes need recorded human judgment rather than inferred claims.
- Source: M013 / S07 closure
- Primary owning slice: M013/S07
- Supporting slices: M013/S01, M013/S02, M013/S03, M013/S04, M013/S05, M013/S06
- Validation: bash scripts/ui_smoke.sh; SMOKE_PROFILE=release bash scripts/ui_smoke.sh; bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh

### R062 — Frozen built-in identity and dense u16 block storage
- Class: constraint
- Status: active
- Description: Built-in block discriminants 0..=22, material vocabulary 0..=10, tiers, the 22 built-in recipe ids, texture layers 0..=47 and icon layers 0..=31 stay frozen, and chunks store blocks densely in two bytes so `BlockId::to_raw`/`from_raw` round-trips built-ins plus modded indices.
- Why it matters: Every pack, save and pinned surface depends on stable discriminants and two-byte block storage; moving either silently corrupts saved worlds and golden digests.
- Source: M010 / S01 identity and storage spine
- Primary owning slice: M010/S01
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::chunk; cargo test --locked --lib -- world::block; cargo test --locked --lib -- world::terrain; cargo test --locked --lib -- mesh::greedy; bash scripts/presentation_smoke.sh

### R063 — Pack format v2, ordered stack merge and --validate-pack
- Class: core-capability
- Status: active
- Description: A v1 texture-only pack still loads unchanged while a format v2 pack declares blocks, items, tiers, tools, recipes, block/recipe patches and worldgen; entries fall back individually with warnings, the ordered stack merges per authored file and field with later packs winning, ids are deterministic namespaced names, and `--validate-pack` prints per-entry results and exits 0 for a clean pack and nonzero for broken or missing packs without opening a window, GPU or save.
- Why it matters: Content packs are only authorable and stackable if parsing, merge precedence and a strict offline validator agree on the same rules.
- Source: M010 / S02 pack format and validation
- Primary owning slice: M010/S02
- Supporting slices: None
- Validation: cargo test --locked --lib -- pack::v2; cargo test --locked --lib -- pack::spec; cargo test --locked --lib -- pack::validate; bash scripts/validate_pack_smoke.sh; bash scripts/pack_smoke.sh

### R064 — Save v8 content table, v1-v7 migration, placeholders and changed-stack notice
- Class: continuity
- Status: active
- Description: v8 writes a sorted content-name table and round-trips byte-stably, v1-v7 saves migrate, writes fail closed before the first output byte, unknown but well-formed names degrade to solid unbreakable drop-nothing blocks and checker-iconed movable unusable items that survive re-save (a degraded pack tool re-saves its tool kind), and restoring the pack resolves the same names while a changed stack shows a non-blocking digest notice.
- Why it matters: Pack content must never be lost or silently remapped when the stack changes between sessions.
- Source: M010 / S03 save v8 and placeholders
- Primary owning slice: M010/S03
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::persistence; cargo test --locked --lib -- world::session; cargo test --locked --bin opencraft -- modding_acceptance_placeholder

### R065 — Registry-backed modded blocks, items and patchable built-ins
- Class: core-capability
- Status: active
- Description: Authored block faces, generic item and tool icons, authored hardness, drop and stack limits flow through production mining, placement and rendering; built-in block fields are patchable by id without moving discriminants and built-in layers 0..=47 stay byte-identical.
- Why it matters: New content is only real if the gameplay and render paths read it through the same registry the save and palette do.
- Source: M010 / S04 blocks and items in the world
- Primary owning slice: M010/S04
- Supporting slices: None
- Validation: cargo test --locked --lib -- content::faces; cargo test --locked --lib -- content::registry; cargo test --locked --lib -- render::texture; bash scripts/pack_smoke.sh; bash scripts/ui_smoke.sh

### R066 — Registry-shaped recipes, custom tool tiers and authored food
- Class: core-capability
- Status: active
- Description: Shaped recipes work at 2x2 and 4x4, disabled built-ins vanish from previews and results, recipes with missing or placeholder ingredients never match, authored tiers change break duration, durability and melee damage through the existing paths, and authored food restores its nutrition through hold-to-eat.
- Why it matters: Recipes, tools and food are the progression surfaces a content pack must be able to change without new code.
- Source: M010 / S05 recipes, tools and food
- Primary owning slice: M010/S05
- Supporting slices: None
- Validation: cargo test --locked --lib -- content::registry; cargo test --locked --lib -- player::tools; cargo test --locked --lib -- player::eating; bash scripts/crafting_smoke.sh

### R067 — Declarative ore and surface-patch worldgen
- Class: core-capability
- Status: active
- Description: Pack ore veins and surface patches generate deterministically from `(seed, pos, rule)` with independent streams in authored y ranges and biomes, an empty rule set produces byte-identical chunks to pre-M010, and coal/iron distribution is unchanged.
- Why it matters: Generated content is the only way a pack changes terrain, and it must not perturb built-in terrain for existing seeds.
- Source: M010 / S06 worldgen
- Primary owning slice: M010/S06
- Supporting slices: None
- Validation: cargo test --locked --lib -- world::terrain; cargo test --locked --lib -- world::chunk; bash scripts/worldgen_smoke.sh

### R068 — Searchable Creative palette with authored grants and placeholder exclusion
- Class: primary-user-loop
- Status: active
- Description: A fresh Creative world opens a searchable palette listing every non-placeholder built-in and pack block and item, filters case-insensitively by id, display name and namespace, places authored stacks into the hotbar, and never lists placeholders while the closed-state digests stay pinned.
- Why it matters: Pack content that cannot be found in-game is not discoverable, and placeholders in the palette would offer unusable stacks.
- Source: M010 / S07 Creative palette
- Primary owning slice: M010/S07
- Supporting slices: None
- Validation: cargo test --locked --lib -- ui::palette; cargo test --locked --bin opencraft -- palette; bash scripts/palette_smoke.sh

### R069 — Reference packs, end-to-end acceptance and modding smoke
- Class: integration
- Status: active
- Description: The checked-in `tests/fixtures/packs/mymod-ruby` content pack plus texture-only `tests/fixtures/packs/ruby-overlay` drive one headless acceptance test through generate, mine, craft, eat, place/use, save and reload, a placeholder removal/re-save/restore test preserves every reference block and item, and `scripts/modding_smoke.sh` proves validate-pack exit codes, the docs reader-test and bounded launches in debug and release.
- Why it matters: The pack surface must be proven on the shipped example content, not only on synthetic fixtures.
- Source: M010 / S08 reference packs and acceptance
- Primary owning slice: M010/S08
- Supporting slices: None
- Validation: cargo test --locked --bin opencraft -- modding_acceptance; bash scripts/modding_smoke.sh; SMOKE_PROFILE=release bash scripts/modding_smoke.sh

### R070 — M010 closure surface
- Class: admin/support
- Status: active
- Description: M010 closes with completed pack-author, README and save docs plus a prepared human UAT runbook, every smoke green in debug and release, fmt/clippy/lib/bin/streaming gates passing, and the registry plus checker/selftest carrying R062-R069 and the 11-row M010 criterion trace.
- Why it matters: Closure must be reproducible on the assembled tree, and the sensory outcomes need recorded human judgment rather than inferred claims.
- Source: M010 / S08 closure
- Primary owning slice: M010/S08
- Supporting slices: M010/S01, M010/S02, M010/S03, M010/S04, M010/S05, M010/S06, M010/S07
- Validation: bash scripts/modding_smoke.sh; SMOKE_PROFILE=release bash scripts/modding_smoke.sh; cargo fmt --all -- --check; cargo clippy --locked --all-targets -- -D warnings; cargo test --locked; bash scripts/requirements_check.sh; bash scripts/requirements_check_selftest.sh

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

## M005 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R022 | S02, S03 | cargo test --locked --lib -- world::daycycle && cargo test --locked --lib -- render::sky && bounded --time-scale 60 full-cycle run |
| 2 | R021 | S01 | cargo test --locked --lib -- world::light && cargo test --locked --lib -- world::manager |
| 3 | R023 | S02 | cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- world::session && cargo test --locked --bin opencraft |
| 4 | R024 | S04 | cargo test --locked --lib -- audio:: && cargo test --locked --lib -- settings:: && cargo test --locked --bin opencraft |
| 5 | R025 | S05 | cargo test --locked --lib -- pack::sound && bash scripts/pack_smoke.sh |
| 6 | R024 | S04 | injected no-device unit proof && bash scripts/launch_smoke.sh |
| 7 | R022 | S02, S03 | cargo test --locked --lib -- world::daycycle && vista golden-hour wiring pins |
| 8 | R026 | S06 | bash scripts/requirements_check.sh && bash scripts/requirements_check_selftest.sh && recorded human UAT |

## M007 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R028, R031, R032 | S02, S05, S06 | cargo test --locked --lib -- world::terrain && cargo test --locked --lib -- world::daycycle && bash scripts/weather_smoke.sh |
| 2 | R027 | S01 | cargo test --locked --lib -- world::terrain && cargo test --locked --lib -- world::block |
| 3 | R029 | S03 | cargo test --locked --lib -- swim && cargo test --locked --lib -- player::physics |
| 4 | R034 | S08 | cargo test --locked --release --lib -- --ignored streaming_budget --nocapture && bash scripts/swim_smoke.sh |
| 5 | R028 | S02 | cargo test --locked --lib -- world::persistence && bash scripts/season_smoke.sh |
| 6 | R033 | S07 | cargo test --locked --lib -- ruin && cargo test --locked --lib -- ruin_loot |
| 7 | R034 | S08 | cargo test --locked --lib -- world::manager && bash scripts/winter_smoke.sh && grep -q SAVE-NOTES README.md |

## M008 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R036, R037, R038 | S01, S04, S05 | cargo test --locked --lib -- player::survival && cargo test --locked --lib -- ui::hud && cargo test --locked --lib -- player::eating && bash scripts/survival_smoke.sh |
| 2 | R041 | S02, S05, S06, S07 | cargo test --locked --lib -- world::farming && cargo test --locked --lib -- world::manager && bash scripts/content_smoke.sh && cargo test --release --locked --lib -- world::session && SMOKE_PROFILE=release bash scripts/content_smoke.sh |
| 3 | R039, R040 | S03, S06 | cargo test --locked --lib -- player::container && cargo test --locked --lib -- ruin_loot && bash scripts/content_smoke.sh |
| 4 | R035, R043 | S01, S03 | cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- world::session && bash scripts/survival_smoke.sh |
| 5 | R042 | S02 | cargo test --locked --lib -- render::texture && bash scripts/pack_smoke.sh |
| 6 | R043 | S01, S03 | cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- world::session |
| 7 | R044 | S08, S09 | cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --locked && bash scripts/requirements_check.sh && bash scripts/survival_smoke.sh && bash scripts/content_smoke.sh && cargo test --release --locked && bash scripts/requirements_check_selftest.sh && SMOKE_PROFILE=release bash scripts/survival_smoke.sh && SMOKE_PROFILE=release bash scripts/content_smoke.sh |
| 8 | R044 | S04, S05, S06, S07, S09 | recorded human windowed UAT with screenshots |

## M009 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R045, R048, R049 | S01, S04, S05 | cargo test --locked --lib -- render::mob && cargo test --locked --lib -- world::mob_loot && cargo test --locked --lib -- world::spawning && bash scripts/combat_smoke.sh |
| 2 | R049, R050 | S05, S06 | cargo test --locked --lib -- world::spawning && cargo test --locked --lib -- world::mob_ai && bash scripts/combat_smoke.sh |
| 3 | R047, R051 | S03, S07 | cargo test --locked --lib -- player::melee && cargo test --locked --lib -- player::bow && bash scripts/combat_smoke.sh |
| 4 | R048, R051, R052 | S04, S07, S08 | cargo test --locked --lib -- world::mob_loot && cargo test --locked --lib -- world::arrow && cargo test --locked --lib -- world::mob_ai && bash scripts/combat_smoke.sh |
| 5 | R046 | S02 | cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- world::session && bash scripts/combat_smoke.sh |
| 6 | R053 | S09 | cargo test --locked --lib -- creative_ && cargo test --locked --lib -- world::mob_audio && bash scripts/pack_smoke.sh && bash scripts/combat_smoke.sh |
| 7 | R050 | S06 | cargo test --locked --lib -- world::mob_ai && cargo test --locked --lib -- player::combat && bash scripts/combat_smoke.sh |
| 8 | R045, R053 | S01, S09 | cargo test --locked --lib -- render::mob && cargo test --locked --lib -- world::mob_audio && bash scripts/pack_smoke.sh && bash scripts/combat_smoke.sh |
| 9 | R054 | S01, S02, S03, S04, S05, S06, S07, S08, S09, S10 | bash scripts/combat_smoke.sh && SMOKE_PROFILE=release bash scripts/combat_smoke.sh && bash scripts/requirements_check.sh && bash scripts/requirements_check_selftest.sh |

## M013 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R055 | S01, S02 | cargo test --locked --lib -- ui::item_icon && bash scripts/ui_smoke.sh |
| 2 | R056 | S03 | cargo test --locked --lib -- ui::inventory && bash scripts/ui_smoke.sh |
| 3 | R057 | S04 | cargo test --locked --lib -- render::texture && cargo test --locked --lib -- world::block && cargo test --locked --lib -- pack:: && bash scripts/pack_smoke.sh |
| 4 | R058 | S05 | cargo test --locked --lib -- ui::inventory && cargo test --locked --bin opencraft -- tooltip && bash scripts/ui_smoke.sh |
| 5 | R059 | S05 | cargo test --locked --lib -- ui::selection && cargo test --locked --bin opencraft -- selected_name && bash scripts/ui_smoke.sh |
| 6 | R060 | S06 | cargo test --locked --lib -- ui::hud::tests::vitals && cargo test --locked --bin opencraft -- playing_hud_quads && bash scripts/ui_smoke.sh |
| 7 | R061 | S01, S02, S03, S04, S05, S06, S07 | cargo test --locked && cargo fmt --all -- --check && cargo clippy --locked --all-targets -- -D warnings && bash scripts/ui_smoke.sh && SMOKE_PROFILE=release bash scripts/ui_smoke.sh && bash scripts/requirements_check.sh && bash scripts/requirements_check_selftest.sh |
| 8 | R061 | S01, S02, S03, S04, S05, S06, S07 | recorded human UAT with 1280x720 and 1920x1080 screenshots |

## M010 Criterion Trace

| Criterion | Requirements | Slices | Evidence |
| --- | --- | --- | --- |
| 1 | R062, R063 | S01, S02 | cargo test --locked --lib -- pack::v2 && cargo test --locked --lib -- content::registry && bash scripts/validate_pack_smoke.sh |
| 2 | R065 | S04 | cargo test --locked --lib -- content::faces && cargo test --locked --lib -- content::registry && bash scripts/pack_smoke.sh && bash scripts/ui_smoke.sh |
| 3 | R066 | S05 | cargo test --locked --lib -- player::tools && cargo test --locked --lib -- player::eating && bash scripts/crafting_smoke.sh |
| 4 | R067 | S06 | cargo test --locked --lib -- world::terrain && bash scripts/worldgen_smoke.sh |
| 5 | R066 | S05 | cargo test --locked --lib -- content::registry && bash scripts/crafting_smoke.sh |
| 6 | R068 | S07 | cargo test --locked --lib -- ui::palette && bash scripts/palette_smoke.sh |
| 7 | R064 | S03 | cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- world::session |
| 8 | R064, R068 | S03, S07 | cargo test --locked --lib -- world::persistence && cargo test --locked --lib -- ui::palette |
| 9 | R064 | S03 | cargo test --locked --lib -- world::persistence && cargo test --locked --bin opencraft -- modding_acceptance_placeholder |
| 10 | R063, R069 | S02, S08 | bash scripts/validate_pack_smoke.sh && bash scripts/modding_smoke.sh |
| 11 | R062, R063, R064, R065, R066, R067, R068, R069 | S01, S02, S03, S04, S05, S06, S07, S08 | cargo fmt --all -- --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked && bash scripts/modding_smoke.sh && bash scripts/requirements_check.sh && bash scripts/requirements_check_selftest.sh |
