# OpenCraft configuration

`config/` is the creator-facing tuning surface for OpenCraft. It is a plain
directory of six TOML files that sits beside `settings.toml` at the repository
root (or beside the executable when you run from a packaged directory). Edit
these files by hand, run `opencraft --check-config`, then launch the game. The
game never writes, creates, or rewrites anything under `config/`.

Every row in the [registry table](#registry-reference) applies live: its
`Apply` column says `live`. The values here are the same constants the engine
uses by default, so an untouched tree is behavior-identical to a build with no
config files at all.

## The six files

| File | Domain |
| --- | --- |
| `config/player.toml` | Movement, jumping, swimming, flying, and camera near/far planes. |
| `config/survival.toml` | Fall safety, air/drowning, hunger drain, starvation, regeneration, and food values. |
| `config/combat.toml` | Melee damage and reach, swing timing, player damage response, bow charge, and arrows. |
| `config/world.toml` | Day length, seasonal weather chances, mob spawning rules, and wheat growth. |
| `config/mining.toml` | Tool speed multipliers and crafting-table interaction reach. |
| `config/render.toml` | Fog and sky palette response to rain/snow, and selected-item HUD timing. |

Each file is grouped into the nested TOML tables shown in the registry table.
For example, `movement.walk_speed` lives under `[movement]` in
`config/player.toml` as:

```toml
[movement]
walk_speed = 4.6
```

## Loading and validation

All six files are read independently at startup, in this order: player,
survival, combat, world, mining, render. Loading never blocks launch; every
problem becomes a warning or an error that `--check-config` can report.

- **Missing file** — that file's keys keep their registry defaults and produce
  a warning. Missing is never fatal.
- **Unknown key** — reported as a warning and ignored. A typo cannot change
  behavior, but it will not silently disappear either.
- **Wrong type, out-of-range value, or non-finite value** — only that field
  falls back to its default and is reported as an error. Its valid siblings
  still load.
- **Cross-field invariant violation** — the whole owning file falls back to
  defaults and is reported as an error. The error names both participating
  keys. The invariants are:
  - player: `camera.z_near` must be less than `camera.z_far`.
  - combat: `bow.min_charge_secs` must be at most `bow.full_charge_secs`;
    `arrow.speed_min` at most `arrow.speed_max`; `arrow.damage_min` at most
    `arrow.damage_max`.
  - world: `spawning.safe_radius_blocks` < `spawning.spawn_max_radius_blocks` <
    `spawning.despawn_radius_blocks`, and
    `spawning.spawn_region_cap` < `spawning.max_mobs_total`.
  - mining: `tools.wood_speed_multiplier` <= `tools.stone_speed_multiplier` <=
    `tools.iron_speed_multiplier`.
- **File-level failure** — an unreadable file, a file larger than 1 MiB, or
  invalid TOML rejects that whole file (all its keys revert to defaults) and is
  reported as an error.

Diagnostics are ordinary data: the game converts only errors into the warning
notice shown when the settings menu first opens. Missing files and unknown
keys stay silent in-game and appear only in `--check-config` output.

### Live reload (S02 forward note)

The initial S01 contract loads configuration at startup. S02 adds per-file
live reload: when a file changes, the candidate file is parsed as a whole and
committed only if it has no error, so a bad save or partial write cannot take
down the running game. Reload is per-file atomic: one file's rejection keeps
that file's last good values while other files continue to update. The
`--check-config` rules below describe the same parser the reload path uses.

## Checking a config tree

Run `opencraft --check-config` from the directory containing `config/`. It is
a headless check: it opens no window, GPU, save, pack stack, or settings file,
and it never writes anything.

```text
opencraft --check-config
```

Output is deterministic and line-oriented. The six expected files are reported
in the order player, survival, combat, world, mining, render. Within a file,
file-level diagnostics come first, then known-key diagnostics in registry key
order, then unknown keys sorted alphabetically. Warnings for unexpected
`*.toml` files in `config/` come after the six expected files, sorted by file
name.

The stable line shapes are:

```text
ok: config/<file>: <N> values
warning: config/<file>: file is missing; using defaults
warning: config/<file>: <dotted.key>: unknown key
warning: config/<file-name>: not a config file
error: config/<file>: <dotted.key>: <reason>
error: config/<file>: <reason>
config: files=6 missing=<n> warnings=<n> errors=<n> keys=85 schema=1 digest=0x<16 lowercase hex digits>
```

`<N>` is the number of curated values in that file: 16 player, 15 survival, 21
combat, 19 world, 4 mining, 10 render. A file with any error prints its error
lines and no `ok:` line. The final summary line is always last; `warnings` and
`errors` count diagnostic lines, and missing-file and extra-file warnings are
included.

Exit status is honest: **0** when there are no errors, even if files are
missing or keys are unknown; **1** when any error is present. Invalid TOML,
wrong types, out-of-range or non-finite values, oversize files, and cross-field
violations all exit 1.

## Digest

The summary ends with `digest=0x...`, a 64-bit FNV-1a hash over the config
schema version and every effective value in lexicographic key order. It hashes
values, not file bytes. Comments, whitespace, table order, and formatting
changes do not move it; editing a value does. A value that fails validation is
folded in as its default, so the digest describes what the game will actually
use, not what the file intended. Save format v9 records this digest alongside
the schema version so a save can notice that gameplay tuning changed.

## Development and release

Configuration loading works in both development and release builds. The
watcher, tuning overlay, and creator-only flags are development-only; release
rejection for those flags lands in S09. No release build is required to edit
`config/`; use `--check-config` in either build to validate before launching.

## Not in the curated registry

Some values are deliberately excluded because they are persisted save bounds,
performance contracts, or content definitions rather than live tuning:

- Persisted bounds such as `MAX_HEALTH`, `MAX_HUNGER`, `FULL_AIR`, and
  `MAX_AIR`.
- Player dimensions and collision box.
- `DAY_TICKS` and season length.
- Terrain heights, sea level, caves, trees, ruins, worldgen salts, and chunk
  size.
- Ore bands and ore content.
- Block ids, block hardness, and worldgen content definitions.
- `ARROW_CAP` and `MOB_TICK_HZ`.
- Per-kind mob stats.
- Static sky/weather GPU buffers.
- Fonts, HUD layout, and tooltip offsets.

Changing behavior in those areas requires a code change, not a config edit.

## Registry reference

The table below is generated from the same static registry that drives loading,
diagnostics, the digest, and `--check-config`. Do not hand-edit it; update the
registry in code and regenerate the block. Rows are in file order, and each
key's `Range` is the inclusive accepted range.

<!-- BEGIN CONFIG REGISTRY -->
| File | Key | Type | Default | Range | Unit | Apply | Description |
| --- | --- | --- | --- | --- | --- | --- | --- |
| config/player.toml | movement.walk_speed | float | 4.6 | 0.5..=20.0 | blocks/s | live | Horizontal walking speed on land in blocks per second. |
| config/player.toml | movement.sprint_speed | float | 7.2 | 0.5..=40.0 | blocks/s | live | Horizontal sprint speed in blocks per second, also used while crouching. |
| config/player.toml | movement.fly_speed | float | 18.0 | 1.0..=64.0 | blocks/s | live | Flight speed in blocks per second while not sprinting. |
| config/player.toml | movement.fly_sprint_speed | float | 48.0 | 1.0..=256.0 | blocks/s | live | Flight speed in blocks per second while sprinting. |
| config/player.toml | movement.fly_vertical_factor | float | 0.75 | 0.05..=2.0 | multiplier | live | Fraction of flight speed applied to vertical movement. |
| config/player.toml | movement.jump_velocity | float | 9.0 | 0.0..=32.0 | blocks/s | live | Upward launch speed of a standing jump in blocks per second. |
| config/player.toml | movement.gravity | float | 28.0 | 0.1..=128.0 | blocks/s^2 | live | Downward gravity acceleration in blocks per second squared. |
| config/player.toml | movement.terminal_velocity | float | 45.0 | 1.0..=256.0 | blocks/s | live | Maximum downward fall speed in blocks per second. |
| config/player.toml | movement.swim_vertical_drag | float | 6.0 | 0.0..=32.0 | per second | live | Per-second exponential drag on vertical velocity while submerged. |
| config/player.toml | movement.swim_rise_accel | float | 40.0 | 0.0..=256.0 | blocks/s^2 | live | Upward acceleration while jump is held underwater in blocks per second squared. |
| config/player.toml | movement.swim_rise_speed | float | 4.0 | 0.0..=32.0 | blocks/s | live | Maximum upward swimming speed in blocks per second. |
| config/player.toml | movement.swim_max_sink_speed | float | 6.0 | 0.0..=64.0 | blocks/s | live | Maximum downward swimming speed in blocks per second. |
| config/player.toml | movement.swim_walk_speed_mult | float | 0.55 | 0.0..=2.0 | multiplier | live | Horizontal walk-speed multiplier applied while submerged. |
| config/player.toml | movement.swim_sink_accel | float | 28.0 | 0.0..=256.0 | blocks/s^2 | live | Downward acceleration applied underwater in blocks per second squared. |
| config/player.toml | camera.z_near | float | 0.1 | 0.001..=1.0 | blocks | live | Camera near clipping plane in blocks. |
| config/player.toml | camera.z_far | float | 1200.0 | 64.0..=8192.0 | blocks | live | Camera far clipping plane in blocks. |
| config/survival.toml | survival.fall_safe_blocks | float | 3.0 | 0.0..=64.0 | blocks | live | Fall distance that causes no damage in blocks. |
| config/survival.toml | survival.air_drain_per_sec | float | 20.0 | 0.1..=1000.0 | air/s | live | Air supply drained per second while submerged. |
| config/survival.toml | survival.drown_interval_secs | float | 1.0 | 0.05..=60.0 | s | live | Seconds between drowning damage ticks once air is exhausted. |
| config/survival.toml | survival.drown_damage | int | 2 | 1..=20 | health | live | Health lost per drowning damage tick. |
| config/survival.toml | survival.hunger_drain_secs | float | 30.0 | 1.0..=3600.0 | s | live | Seconds of gameplay before one hunger point is drained. |
| config/survival.toml | survival.starve_interval_secs | float | 4.0 | 0.1..=600.0 | s | live | Seconds between starvation damage ticks at zero hunger. |
| config/survival.toml | survival.starve_damage | int | 1 | 1..=20 | health | live | Health lost per starvation damage tick. |
| config/survival.toml | survival.regen_hunger_threshold | int | 18 | 0..=20 | hunger | live | Minimum hunger required for natural health regeneration. |
| config/survival.toml | survival.regen_interval_secs | float | 4.0 | 0.1..=600.0 | s | live | Seconds between natural health regeneration ticks. |
| config/survival.toml | survival.regen_hunger_cost | int | 1 | 0..=20 | hunger | live | Hunger points consumed per natural regeneration tick. |
| config/survival.toml | eating.eat_duration_secs | float | 1.6 | 0.1..=10.0 | s | live | Seconds required to finish eating one food item. |
| config/survival.toml | eating.apple_hunger | int | 4 | 0..=20 | hunger | live | Hunger restored by eating an apple. |
| config/survival.toml | eating.bread_hunger | int | 6 | 0..=20 | hunger | live | Hunger restored by eating bread. |
| config/survival.toml | eating.raw_meat_hunger | int | 3 | 0..=20 | hunger | live | Hunger restored by eating raw meat. |
| config/survival.toml | eating.rotten_flesh_hunger | int | 1 | 0..=20 | hunger | live | Hunger restored by eating rotten flesh. |
| config/combat.toml | melee.fist_damage | int | 1 | 0..=20 | health | live | Melee damage dealt by an empty hand. |
| config/combat.toml | melee.sword_damage_wood | int | 4 | 0..=20 | health | live | Melee damage dealt by a wooden sword. |
| config/combat.toml | melee.sword_damage_stone | int | 5 | 0..=20 | health | live | Melee damage dealt by a stone sword. |
| config/combat.toml | melee.sword_damage_iron | int | 6 | 0..=20 | health | live | Melee damage dealt by an iron sword. |
| config/combat.toml | melee.reach | float | 3.0 | 0.5..=16.0 | blocks | live | Maximum melee attack distance in blocks. |
| config/combat.toml | melee.swing_cooldown_secs | float | 0.6 | 0.0..=10.0 | s | live | Seconds between melee swings. |
| config/combat.toml | player.iframe_secs | float | 0.5 | 0.0..=10.0 | s | live | Invulnerability window after the player takes damage in seconds. |
| config/combat.toml | player.hurt_flash_secs | float | 0.5 | 0.0..=10.0 | s | live | Duration of the red hurt flash on the player in seconds. |
| config/combat.toml | player.knockback_speed | float | 6.0 | 0.0..=64.0 | blocks/s | live | Initial knockback speed applied to the player in blocks per second. |
| config/combat.toml | player.knockback_decay | float | 0.6 | 0.0..=32.0 | per second | live | Per-second decay rate of player knockback velocity. |
| config/combat.toml | bow.min_charge_secs | float | 0.2 | 0.0..=10.0 | s | live | Minimum bow draw time before an arrow can be fired in seconds. |
| config/combat.toml | bow.full_charge_secs | float | 1.0 | 0.01..=10.0 | s | live | Draw time for a fully charged bow shot in seconds. |
| config/combat.toml | arrow.gravity | float | 20.0 | 0.0..=128.0 | blocks/s^2 | live | Downward arrow acceleration in blocks per second squared. |
| config/combat.toml | arrow.speed_min | float | 18.0 | 0.0..=256.0 | blocks/s | live | Minimum arrow launch speed at partial bow charge in blocks per second. |
| config/combat.toml | arrow.speed_max | float | 32.0 | 0.0..=256.0 | blocks/s | live | Arrow launch speed at full bow charge in blocks per second. |
| config/combat.toml | arrow.damage_min | int | 1 | 0..=20 | health | live | Minimum arrow damage at partial bow charge. |
| config/combat.toml | arrow.damage_max | int | 4 | 0..=20 | health | live | Arrow damage at full bow charge. |
| config/combat.toml | arrow.skeleton_speed | float | 28.0 | 0.0..=256.0 | blocks/s | live | Skeleton arrow launch speed in blocks per second. |
| config/combat.toml | arrow.skeleton_damage | int | 2 | 0..=20 | health | live | Damage dealt by a skeleton arrow. |
| config/combat.toml | arrow.lifetime_secs | float | 20.0 | 0.1..=600.0 | s | live | Maximum arrow flight time before it despawns in seconds. |
| config/combat.toml | arrow.embed_secs | float | 2.0 | 0.0..=60.0 | s | live | Seconds a stuck arrow remains embedded before despawning. |
| config/world.toml | clock.day_seconds | float | 1200.0 | 60.0..=86400.0 | s | live | Real seconds in one full day cycle at normal time scale. |
| config/world.toml | weather.winter_snow_percent | int | 40 | 0..=100 | % | live | Percent of winter days with snowfall. |
| config/world.toml | weather.spring_rain_percent | int | 30 | 0..=100 | % | live | Percent of spring days with rain. |
| config/world.toml | weather.summer_rain_percent | int | 20 | 0..=100 | % | live | Percent of summer days with rain. |
| config/world.toml | weather.autumn_rain_percent | int | 35 | 0..=100 | % | live | Percent of autumn days with rain. |
| config/world.toml | spawning.safe_radius_blocks | int | 24 | 1..=512 | blocks | live | Radius around the player where hostile mobs never spawn in blocks. |
| config/world.toml | spawning.spawn_max_radius_blocks | int | 48 | 2..=1024 | blocks | live | Maximum distance from the player at which mobs spawn in blocks. |
| config/world.toml | spawning.despawn_radius_blocks | int | 96 | 3..=127 | blocks | live | Distance beyond which mobs despawn in blocks. |
| config/world.toml | spawning.max_mobs_total | int | 32 | 1..=512 | mobs | live | Maximum number of mobs allowed at once. |
| config/world.toml | spawning.spawn_region_blocks | int | 32 | 4..=256 | blocks | live | Size of each spawn region cell in blocks. |
| config/world.toml | spawning.spawn_region_cap | int | 4 | 1..=512 | mobs | live | Maximum mobs that count toward one spawn region. |
| config/world.toml | spawning.spawn_attempt_secs | float | 0.25 | 0.01..=10.0 | s | live | Seconds between mob spawn attempts. |
| config/world.toml | spawning.despawn_scan_secs | float | 1.0 | 0.01..=60.0 | s | live | Seconds between mob despawn scans. |
| config/world.toml | spawning.burn_interval_secs | float | 0.5 | 0.01..=60.0 | s | live | Seconds between daylight burning checks for undead mobs. |
| config/world.toml | spawning.spawn_min_separation | float | 1.0 | 0.0..=16.0 | blocks | live | Minimum distance between newly spawned mobs in blocks. |
| config/world.toml | spawning.hostile_cave_light | int | 7 | 0..=15 | light | live | Maximum light level where hostile mobs may spawn in caves. |
| config/world.toml | spawning.passive_surface_light | int | 15 | 0..=15 | light | live | Minimum light level where passive mobs may spawn on the surface. |
| config/world.toml | spawning.max_spawn_attempts_per_call | int | 4 | 1..=64 | attempts | live | Maximum spawn positions tried per spawn attempt. |
| config/world.toml | farming.wheat_stage_game_secs | float | 60.0 | 1.0..=3600.0 | s | live | Game seconds between wheat growth stages. |
| config/mining.toml | tools.wood_speed_multiplier | int | 2 | 1..=32 | x | live | Mining speed multiplier for wooden tools. |
| config/mining.toml | tools.stone_speed_multiplier | int | 4 | 1..=32 | x | live | Mining speed multiplier for stone tools. |
| config/mining.toml | tools.iron_speed_multiplier | int | 6 | 1..=32 | x | live | Mining speed multiplier for iron tools. |
| config/mining.toml | interaction.table_reach | float | 8.0 | 1.0..=32.0 | blocks | live | Maximum distance for interacting with a placed crafting table in blocks. |
| config/render.toml | fog.rain_factor | float | 0.75 | 0.0..=4.0 | multiplier | live | Fog range multiplier applied during rain. |
| config/render.toml | fog.snow_factor | float | 0.85 | 0.0..=4.0 | multiplier | live | Fog range multiplier applied during snowfall. |
| config/render.toml | palette.rain_grey_scale | float | 0.7 | 0.0..=4.0 | scale | live | Brightness scale used for the rain sky and fog tint. |
| config/render.toml | palette.snow_grey_scale | float | 1.18 | 0.0..=4.0 | scale | live | Brightness scale used for the snow sky and fog tint. |
| config/render.toml | palette.rain_sky_lerp | float | 0.45 | 0.0..=1.0 | fraction | live | Blend fraction toward the rain grey palette. |
| config/render.toml | palette.snow_sky_lerp | float | 0.55 | 0.0..=1.0 | fraction | live | Blend fraction toward the snow grey palette. |
| config/render.toml | palette.rain_day_factor | float | 0.78 | 0.0..=2.0 | multiplier | live | Daylight multiplier applied during rain. |
| config/render.toml | palette.snow_day_factor | float | 0.92 | 0.0..=2.0 | multiplier | live | Daylight multiplier applied during snowfall. |
| config/render.toml | palette.moonlit_floor | float | 0.34 | 0.0..=1.0 | factor | live | Dimmest daylight factor reached at midnight. |
| config/render.toml | ui.selected_name_secs | float | 2.0 | 0.0..=30.0 | s | live | Seconds the selected item name stays visible after a change. |
<!-- END CONFIG REGISTRY -->
