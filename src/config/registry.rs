//! Static registry of every creator-tunable configuration row.
//!
//! This module is the single source of truth for the M014 config contract:
//! the six files, their curated keys, scalar kinds and ranges, defaults,
//! units, apply modes, and documentation. The loader, the digest, the
//! `--check-config` renderer, and `docs/CONFIG.md` all derive from
//! [`REGISTRY`]; adding a knob means adding one row here.
//!
//! Defaults reference the real module constants (movement, camera, survival,
//! combat, world, mining, render) instead of repeating numeric literals, so a
//! runtime constant and its documented default cannot drift apart.

use crate::player::{bow, camera, combat, eating, melee, physics, survival, tools};
use crate::ui::selection;
use crate::world::{arrow, daycycle, farming, season, session, spawning};

use ConfigFile::{Combat, Mining, Player, Render, Survival, World};

/// One of the six curated TOML files under `config/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConfigFile {
    Player,
    Survival,
    Combat,
    World,
    Mining,
    Render,
}

impl ConfigFile {
    /// Every file in registry and `--check-config` order.
    pub const ALL: [ConfigFile; 6] = [
        ConfigFile::Player,
        ConfigFile::Survival,
        ConfigFile::Combat,
        ConfigFile::World,
        ConfigFile::Mining,
        ConfigFile::Render,
    ];

    /// File name without its directory, e.g. `player.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            ConfigFile::Player => "player.toml",
            ConfigFile::Survival => "survival.toml",
            ConfigFile::Combat => "combat.toml",
            ConfigFile::World => "world.toml",
            ConfigFile::Mining => "mining.toml",
            ConfigFile::Render => "render.toml",
        }
    }

    /// Logical path relative to the working directory, e.g. `config/player.toml`.
    pub const fn rel_path(self) -> &'static str {
        match self {
            ConfigFile::Player => "config/player.toml",
            ConfigFile::Survival => "config/survival.toml",
            ConfigFile::Combat => "config/combat.toml",
            ConfigFile::World => "config/world.toml",
            ConfigFile::Mining => "config/mining.toml",
            ConfigFile::Render => "config/render.toml",
        }
    }
}

/// Scalar kind and inclusive range accepted for one row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConfigKind {
    Float { min: f32, max: f32 },
    Int { min: i64, max: i64 },
    Bool,
}

/// A typed configuration value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConfigValue {
    Float(f32),
    Int(i64),
    Bool(bool),
}

/// How an edited value reaches the running game.
///
/// Every curated M014 row is [`ApplyMode::Live`]; the variant exists so the
/// docs table and future rows can distinguish a restart-only knob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyMode {
    Live,
    Restart,
}

/// One curated configuration row.
#[derive(Clone, Copy, Debug)]
pub struct ConfigRow {
    /// Dotted TOML key inside the file, e.g. `movement.walk_speed`.
    pub key: &'static str,
    /// File that owns the row.
    pub file: ConfigFile,
    /// Scalar kind plus its inclusive accepted range.
    pub kind: ConfigKind,
    /// Value used when the key is absent or invalid.
    pub default: ConfigValue,
    /// Human-readable unit shown in the docs table.
    pub unit: &'static str,
    /// Whether an edit applies live or needs a restart.
    pub apply: ApplyMode,
    /// One-sentence description shown in the docs table.
    pub description: &'static str,
}

const fn float(
    key: &'static str,
    file: ConfigFile,
    default: f32,
    min: f32,
    max: f32,
    unit: &'static str,
    description: &'static str,
) -> ConfigRow {
    ConfigRow {
        key,
        file,
        kind: ConfigKind::Float { min, max },
        default: ConfigValue::Float(default),
        unit,
        apply: ApplyMode::Live,
        description,
    }
}

const fn int(
    key: &'static str,
    file: ConfigFile,
    default: i64,
    min: i64,
    max: i64,
    unit: &'static str,
    description: &'static str,
) -> ConfigRow {
    ConfigRow {
        key,
        file,
        kind: ConfigKind::Int { min, max },
        default: ConfigValue::Int(default),
        unit,
        apply: ApplyMode::Live,
        description,
    }
}

/// The complete curated registry, grouped in [`ConfigFile::ALL`] order.
///
/// Every key is unique and every default lies inside its declared range.
pub const REGISTRY: &[ConfigRow] = &[
    // -----------------------------------------------------------------
    // config/player.toml (16)
    // -----------------------------------------------------------------
    float(
        "movement.walk_speed",
        Player,
        physics::WALK_SPEED,
        0.5,
        20.0,
        "blocks/s",
        "Horizontal walking speed on land in blocks per second.",
    ),
    float(
        "movement.sprint_speed",
        Player,
        physics::SPRINT_SPEED,
        0.5,
        40.0,
        "blocks/s",
        "Horizontal sprint speed in blocks per second, also used while crouching.",
    ),
    float(
        "movement.fly_speed",
        Player,
        physics::FLY_SPEED,
        1.0,
        64.0,
        "blocks/s",
        "Flight speed in blocks per second while not sprinting.",
    ),
    float(
        "movement.fly_sprint_speed",
        Player,
        physics::FLY_SPRINT_SPEED,
        1.0,
        256.0,
        "blocks/s",
        "Flight speed in blocks per second while sprinting.",
    ),
    float(
        "movement.fly_vertical_factor",
        Player,
        physics::FLY_VERTICAL_FACTOR,
        0.05,
        2.0,
        "multiplier",
        "Fraction of flight speed applied to vertical movement.",
    ),
    float(
        "movement.jump_velocity",
        Player,
        physics::JUMP_VELOCITY,
        0.0,
        32.0,
        "blocks/s",
        "Upward launch speed of a standing jump in blocks per second.",
    ),
    float(
        "movement.gravity",
        Player,
        physics::GRAVITY,
        0.1,
        128.0,
        "blocks/s^2",
        "Downward gravity acceleration in blocks per second squared.",
    ),
    float(
        "movement.terminal_velocity",
        Player,
        physics::TERMINAL_VELOCITY,
        1.0,
        256.0,
        "blocks/s",
        "Maximum downward fall speed in blocks per second.",
    ),
    float(
        "movement.swim_vertical_drag",
        Player,
        physics::SWIM_VERTICAL_DRAG,
        0.0,
        32.0,
        "per second",
        "Per-second exponential drag on vertical velocity while submerged.",
    ),
    float(
        "movement.swim_rise_accel",
        Player,
        physics::SWIM_RISE_ACCEL,
        0.0,
        256.0,
        "blocks/s^2",
        "Upward acceleration while jump is held underwater in blocks per second squared.",
    ),
    float(
        "movement.swim_rise_speed",
        Player,
        physics::SWIM_RISE_SPEED,
        0.0,
        32.0,
        "blocks/s",
        "Maximum upward swimming speed in blocks per second.",
    ),
    float(
        "movement.swim_max_sink_speed",
        Player,
        physics::SWIM_MAX_SINK_SPEED,
        0.0,
        64.0,
        "blocks/s",
        "Maximum downward swimming speed in blocks per second.",
    ),
    float(
        "movement.swim_walk_speed_mult",
        Player,
        physics::SWIM_WALK_SPEED_MULT,
        0.0,
        2.0,
        "multiplier",
        "Horizontal walk-speed multiplier applied while submerged.",
    ),
    float(
        "movement.swim_sink_accel",
        Player,
        physics::SWIM_SINK_ACCEL,
        0.0,
        256.0,
        "blocks/s^2",
        "Downward acceleration applied underwater in blocks per second squared.",
    ),
    float(
        "camera.z_near",
        Player,
        camera::DEFAULT_Z_NEAR,
        0.001,
        1.0,
        "blocks",
        "Camera near clipping plane in blocks.",
    ),
    float(
        "camera.z_far",
        Player,
        camera::DEFAULT_Z_FAR,
        64.0,
        8192.0,
        "blocks",
        "Camera far clipping plane in blocks.",
    ),
    // -----------------------------------------------------------------
    // config/survival.toml (15)
    // -----------------------------------------------------------------
    float(
        "survival.fall_safe_blocks",
        Survival,
        survival::FALL_SAFE_BLOCKS,
        0.0,
        64.0,
        "blocks",
        "Fall distance that causes no damage in blocks.",
    ),
    float(
        "survival.air_drain_per_sec",
        Survival,
        survival::AIR_DRAIN_PER_SEC,
        0.1,
        1000.0,
        "air/s",
        "Air supply drained per second while submerged.",
    ),
    float(
        "survival.drown_interval_secs",
        Survival,
        survival::DROWN_INTERVAL_SECS,
        0.05,
        60.0,
        "s",
        "Seconds between drowning damage ticks once air is exhausted.",
    ),
    int(
        "survival.drown_damage",
        Survival,
        survival::DROWN_DAMAGE as i64,
        1,
        20,
        "health",
        "Health lost per drowning damage tick.",
    ),
    float(
        "survival.hunger_drain_secs",
        Survival,
        survival::HUNGER_DRAIN_SECS,
        1.0,
        3600.0,
        "s",
        "Seconds of gameplay before one hunger point is drained.",
    ),
    float(
        "survival.starve_interval_secs",
        Survival,
        survival::STARVE_INTERVAL_SECS,
        0.1,
        600.0,
        "s",
        "Seconds between starvation damage ticks at zero hunger.",
    ),
    int(
        "survival.starve_damage",
        Survival,
        survival::STARVE_DAMAGE as i64,
        1,
        20,
        "health",
        "Health lost per starvation damage tick.",
    ),
    int(
        "survival.regen_hunger_threshold",
        Survival,
        survival::REGEN_HUNGER_THRESHOLD as i64,
        0,
        20,
        "hunger",
        "Minimum hunger required for natural health regeneration.",
    ),
    float(
        "survival.regen_interval_secs",
        Survival,
        survival::REGEN_INTERVAL_SECS,
        0.1,
        600.0,
        "s",
        "Seconds between natural health regeneration ticks.",
    ),
    int(
        "survival.regen_hunger_cost",
        Survival,
        survival::REGEN_HUNGER_COST as i64,
        0,
        20,
        "hunger",
        "Hunger points consumed per natural regeneration tick.",
    ),
    float(
        "eating.eat_duration_secs",
        Survival,
        eating::EAT_DURATION_SECS,
        0.1,
        10.0,
        "s",
        "Seconds required to finish eating one food item.",
    ),
    int(
        "eating.apple_hunger",
        Survival,
        eating::APPLE_HUNGER as i64,
        0,
        20,
        "hunger",
        "Hunger restored by eating an apple.",
    ),
    int(
        "eating.bread_hunger",
        Survival,
        eating::BREAD_HUNGER as i64,
        0,
        20,
        "hunger",
        "Hunger restored by eating bread.",
    ),
    int(
        "eating.raw_meat_hunger",
        Survival,
        eating::RAW_MEAT_HUNGER as i64,
        0,
        20,
        "hunger",
        "Hunger restored by eating raw meat.",
    ),
    int(
        "eating.rotten_flesh_hunger",
        Survival,
        eating::ROTTEN_FLESH_HUNGER as i64,
        0,
        20,
        "hunger",
        "Hunger restored by eating rotten flesh.",
    ),
    // -----------------------------------------------------------------
    // config/combat.toml (21)
    // -----------------------------------------------------------------
    int(
        "melee.fist_damage",
        Combat,
        melee::FIST_DAMAGE as i64,
        0,
        20,
        "health",
        "Melee damage dealt by an empty hand.",
    ),
    int(
        "melee.sword_damage_wood",
        Combat,
        melee::SWORD_DAMAGE_WOOD as i64,
        0,
        20,
        "health",
        "Melee damage dealt by a wooden sword.",
    ),
    int(
        "melee.sword_damage_stone",
        Combat,
        melee::SWORD_DAMAGE_STONE as i64,
        0,
        20,
        "health",
        "Melee damage dealt by a stone sword.",
    ),
    int(
        "melee.sword_damage_iron",
        Combat,
        melee::SWORD_DAMAGE_IRON as i64,
        0,
        20,
        "health",
        "Melee damage dealt by an iron sword.",
    ),
    float(
        "melee.reach",
        Combat,
        melee::MELEE_REACH,
        0.5,
        16.0,
        "blocks",
        "Maximum melee attack distance in blocks.",
    ),
    float(
        "melee.swing_cooldown_secs",
        Combat,
        melee::SWING_COOLDOWN_SECS,
        0.0,
        10.0,
        "s",
        "Seconds between melee swings.",
    ),
    float(
        "player.iframe_secs",
        Combat,
        combat::PLAYER_IFRAME_SECS,
        0.0,
        10.0,
        "s",
        "Invulnerability window after the player takes damage in seconds.",
    ),
    float(
        "player.hurt_flash_secs",
        Combat,
        combat::PLAYER_HURT_FLASH_SECS,
        0.0,
        10.0,
        "s",
        "Duration of the red hurt flash on the player in seconds.",
    ),
    float(
        "player.knockback_speed",
        Combat,
        combat::PLAYER_KNOCKBACK_SPEED,
        0.0,
        64.0,
        "blocks/s",
        "Initial knockback speed applied to the player in blocks per second.",
    ),
    float(
        "player.knockback_decay",
        Combat,
        combat::PLAYER_KNOCKBACK_DECAY,
        0.0,
        32.0,
        "per second",
        "Per-second decay rate of player knockback velocity.",
    ),
    float(
        "bow.min_charge_secs",
        Combat,
        bow::BOW_MIN_CHARGE_SECS,
        0.0,
        10.0,
        "s",
        "Minimum bow draw time before an arrow can be fired in seconds.",
    ),
    float(
        "bow.full_charge_secs",
        Combat,
        bow::BOW_FULL_CHARGE_SECS,
        0.01,
        10.0,
        "s",
        "Draw time for a fully charged bow shot in seconds.",
    ),
    float(
        "arrow.gravity",
        Combat,
        arrow::ARROW_GRAVITY,
        0.0,
        128.0,
        "blocks/s^2",
        "Downward arrow acceleration in blocks per second squared.",
    ),
    float(
        "arrow.speed_min",
        Combat,
        arrow::ARROW_SPEED_MIN,
        0.0,
        256.0,
        "blocks/s",
        "Minimum arrow launch speed at partial bow charge in blocks per second.",
    ),
    float(
        "arrow.speed_max",
        Combat,
        arrow::ARROW_SPEED_MAX,
        0.0,
        256.0,
        "blocks/s",
        "Arrow launch speed at full bow charge in blocks per second.",
    ),
    int(
        "arrow.damage_min",
        Combat,
        arrow::ARROW_DAMAGE_MIN as i64,
        0,
        20,
        "health",
        "Minimum arrow damage at partial bow charge.",
    ),
    int(
        "arrow.damage_max",
        Combat,
        arrow::ARROW_DAMAGE_MAX as i64,
        0,
        20,
        "health",
        "Arrow damage at full bow charge.",
    ),
    float(
        "arrow.skeleton_speed",
        Combat,
        arrow::ARROW_SPEED_SKELETON,
        0.0,
        256.0,
        "blocks/s",
        "Skeleton arrow launch speed in blocks per second.",
    ),
    int(
        "arrow.skeleton_damage",
        Combat,
        arrow::ARROW_DAMAGE_SKELETON as i64,
        0,
        20,
        "health",
        "Damage dealt by a skeleton arrow.",
    ),
    float(
        "arrow.lifetime_secs",
        Combat,
        arrow::ARROW_LIFETIME_SECS,
        0.1,
        600.0,
        "s",
        "Maximum arrow flight time before it despawns in seconds.",
    ),
    float(
        "arrow.embed_secs",
        Combat,
        arrow::ARROW_EMBED_SECS,
        0.0,
        60.0,
        "s",
        "Seconds a stuck arrow remains embedded before despawning.",
    ),
    // -----------------------------------------------------------------
    // config/world.toml (19)
    // -----------------------------------------------------------------
    float(
        "clock.day_seconds",
        World,
        daycycle::DAY_SECONDS,
        60.0,
        86400.0,
        "s",
        "Real seconds in one full day cycle at normal time scale.",
    ),
    int(
        "weather.winter_snow_percent",
        World,
        season::WINTER_SNOW_PERCENT as i64,
        0,
        100,
        "%",
        "Percent of winter days with snowfall.",
    ),
    int(
        "weather.spring_rain_percent",
        World,
        season::SPRING_RAIN_PERCENT as i64,
        0,
        100,
        "%",
        "Percent of spring days with rain.",
    ),
    int(
        "weather.summer_rain_percent",
        World,
        season::SUMMER_RAIN_PERCENT as i64,
        0,
        100,
        "%",
        "Percent of summer days with rain.",
    ),
    int(
        "weather.autumn_rain_percent",
        World,
        season::AUTUMN_RAIN_PERCENT as i64,
        0,
        100,
        "%",
        "Percent of autumn days with rain.",
    ),
    int(
        "spawning.safe_radius_blocks",
        World,
        spawning::SAFE_RADIUS_BLOCKS as i64,
        1,
        512,
        "blocks",
        "Radius around the player where hostile mobs never spawn in blocks.",
    ),
    int(
        "spawning.spawn_max_radius_blocks",
        World,
        spawning::SPAWN_MAX_RADIUS_BLOCKS as i64,
        2,
        1024,
        "blocks",
        "Maximum distance from the player at which mobs spawn in blocks.",
    ),
    int(
        "spawning.despawn_radius_blocks",
        World,
        spawning::DESPAWN_RADIUS_BLOCKS as i64,
        3,
        127,
        "blocks",
        "Distance beyond which mobs despawn in blocks.",
    ),
    int(
        "spawning.max_mobs_total",
        World,
        spawning::MAX_MOBS_TOTAL as i64,
        1,
        512,
        "mobs",
        "Maximum number of mobs allowed at once.",
    ),
    int(
        "spawning.spawn_region_blocks",
        World,
        spawning::SPAWN_REGION_BLOCKS as i64,
        4,
        256,
        "blocks",
        "Size of each spawn region cell in blocks.",
    ),
    int(
        "spawning.spawn_region_cap",
        World,
        spawning::SPAWN_REGION_CAP as i64,
        1,
        512,
        "mobs",
        "Maximum mobs that count toward one spawn region.",
    ),
    float(
        "spawning.spawn_attempt_secs",
        World,
        spawning::SPAWN_ATTEMPT_SECS,
        0.01,
        10.0,
        "s",
        "Seconds between mob spawn attempts.",
    ),
    float(
        "spawning.despawn_scan_secs",
        World,
        spawning::DESPAWN_SCAN_SECS,
        0.01,
        60.0,
        "s",
        "Seconds between mob despawn scans.",
    ),
    float(
        "spawning.burn_interval_secs",
        World,
        spawning::BURN_INTERVAL_SECS,
        0.01,
        60.0,
        "s",
        "Seconds between daylight burning checks for undead mobs.",
    ),
    float(
        "spawning.spawn_min_separation",
        World,
        spawning::SPAWN_MIN_SEPARATION,
        0.0,
        16.0,
        "blocks",
        "Minimum distance between newly spawned mobs in blocks.",
    ),
    int(
        "spawning.hostile_cave_light",
        World,
        spawning::HOSTILE_CAVE_LIGHT as i64,
        0,
        15,
        "light",
        "Maximum light level where hostile mobs may spawn in caves.",
    ),
    int(
        "spawning.passive_surface_light",
        World,
        spawning::PASSIVE_SURFACE_LIGHT as i64,
        0,
        15,
        "light",
        "Minimum light level where passive mobs may spawn on the surface.",
    ),
    int(
        "spawning.max_spawn_attempts_per_call",
        World,
        spawning::MAX_SPAWN_ATTEMPTS_PER_CALL as i64,
        1,
        64,
        "attempts",
        "Maximum spawn positions tried per spawn attempt.",
    ),
    float(
        "farming.wheat_stage_game_secs",
        World,
        farming::WHEAT_STAGE_GAME_SECS,
        1.0,
        3600.0,
        "s",
        "Game seconds between wheat growth stages.",
    ),
    // -----------------------------------------------------------------
    // config/mining.toml (4)
    // -----------------------------------------------------------------
    int(
        "tools.wood_speed_multiplier",
        Mining,
        tools::WOOD_SPEED_MULTIPLIER as i64,
        1,
        32,
        "x",
        "Mining speed multiplier for wooden tools.",
    ),
    int(
        "tools.stone_speed_multiplier",
        Mining,
        tools::STONE_SPEED_MULTIPLIER as i64,
        1,
        32,
        "x",
        "Mining speed multiplier for stone tools.",
    ),
    int(
        "tools.iron_speed_multiplier",
        Mining,
        tools::IRON_SPEED_MULTIPLIER as i64,
        1,
        32,
        "x",
        "Mining speed multiplier for iron tools.",
    ),
    float(
        "interaction.table_reach",
        Mining,
        session::TABLE_INTERACT_REACH,
        1.0,
        32.0,
        "blocks",
        "Maximum distance for interacting with a placed crafting table in blocks.",
    ),
    // -----------------------------------------------------------------
    // config/render.toml (10)
    // -----------------------------------------------------------------
    float(
        "fog.rain_factor",
        Render,
        daycycle::RAIN_FOG_RANGE_FACTOR,
        0.0,
        4.0,
        "multiplier",
        "Fog range multiplier applied during rain.",
    ),
    float(
        "fog.snow_factor",
        Render,
        daycycle::SNOW_FOG_RANGE_FACTOR,
        0.0,
        4.0,
        "multiplier",
        "Fog range multiplier applied during snowfall.",
    ),
    float(
        "palette.rain_grey_scale",
        Render,
        daycycle::RAIN_GREY_SCALE,
        0.0,
        4.0,
        "scale",
        "Brightness scale used for the rain sky and fog tint.",
    ),
    float(
        "palette.snow_grey_scale",
        Render,
        daycycle::SNOW_GREY_SCALE,
        0.0,
        4.0,
        "scale",
        "Brightness scale used for the snow sky and fog tint.",
    ),
    float(
        "palette.rain_sky_lerp",
        Render,
        daycycle::RAIN_SKY_LERP,
        0.0,
        1.0,
        "fraction",
        "Blend fraction toward the rain grey palette.",
    ),
    float(
        "palette.snow_sky_lerp",
        Render,
        daycycle::SNOW_SKY_LERP,
        0.0,
        1.0,
        "fraction",
        "Blend fraction toward the snow grey palette.",
    ),
    float(
        "palette.rain_day_factor",
        Render,
        daycycle::RAIN_DAY_FACTOR,
        0.0,
        2.0,
        "multiplier",
        "Daylight multiplier applied during rain.",
    ),
    float(
        "palette.snow_day_factor",
        Render,
        daycycle::SNOW_DAY_FACTOR,
        0.0,
        2.0,
        "multiplier",
        "Daylight multiplier applied during snowfall.",
    ),
    float(
        "palette.moonlit_floor",
        Render,
        daycycle::MOONLIT_FLOOR,
        0.0,
        1.0,
        "factor",
        "Dimmest daylight factor reached at midnight.",
    ),
    float(
        "ui.selected_name_secs",
        Render,
        selection::SELECTED_NAME_SECS,
        0.0,
        30.0,
        "s",
        "Seconds the selected item name stays visible after a change.",
    ),
];

/// Look up a registry row by dotted key.
pub fn index_of(key: &str) -> Option<usize> {
    REGISTRY.iter().position(|row| row.key == key)
}

const fn kind_name(kind: ConfigKind) -> &'static str {
    match kind {
        ConfigKind::Float { .. } => "float",
        ConfigKind::Int { .. } => "int",
        ConfigKind::Bool => "bool",
    }
}

const fn apply_name(apply: ApplyMode) -> &'static str {
    match apply {
        ApplyMode::Live => "live",
        ApplyMode::Restart => "restart",
    }
}

/// Format an `f32` with at least one decimal place, matching the spelling
/// creators see in the checked-in TOML files.
fn format_f32(value: f32) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

fn format_default(value: ConfigValue) -> String {
    match value {
        ConfigValue::Float(value) => format_f32(value),
        ConfigValue::Int(value) => value.to_string(),
        ConfigValue::Bool(value) => value.to_string(),
    }
}

fn format_range(kind: ConfigKind) -> String {
    match kind {
        ConfigKind::Float { min, max } => {
            format!("{}..={}", format_f32(min), format_f32(max))
        }
        ConfigKind::Int { min, max } => format!("{min}..={max}"),
        ConfigKind::Bool => "-".to_string(),
    }
}

/// Deterministic markdown table of the whole registry.
///
/// The header and separator are fixed; one row follows per [`REGISTRY`] row in
/// registry order. `docs/CONFIG.md` embeds this exact output between its
/// registry markers.
pub fn docs_table() -> String {
    let mut table = String::new();
    table.push_str("| File | Key | Type | Default | Range | Unit | Apply | Description |\n");
    table.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for row in REGISTRY {
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            row.file.rel_path(),
            row.key,
            kind_name(row.kind),
            format_default(row.default),
            format_range(row.kind),
            row.unit,
            apply_name(row.apply),
            row.description,
        ));
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str) -> &'static ConfigRow {
        &REGISTRY[index_of(key).expect("registry key must exist")]
    }

    #[test]
    fn registry_has_eighty_five_rows() {
        assert_eq!(REGISTRY.len(), 85);
    }

    #[test]
    fn registry_keys_are_unique_and_dotted() {
        let mut keys: Vec<&str> = REGISTRY.iter().map(|row| row.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), REGISTRY.len(), "registry keys must be unique");
        for row in REGISTRY {
            assert!(
                row.key.contains('.'),
                "registry key {:?} must be dotted",
                row.key
            );
            assert!(
                !row.key.contains('|') && !row.key.contains(char::is_whitespace),
                "registry key {:?} must be a clean dotted path",
                row.key
            );
        }
    }

    #[test]
    fn registry_has_expected_rows_per_file() {
        let expected = [
            (ConfigFile::Player, 16),
            (ConfigFile::Survival, 15),
            (ConfigFile::Combat, 21),
            (ConfigFile::World, 19),
            (ConfigFile::Mining, 4),
            (ConfigFile::Render, 10),
        ];
        let mut total = 0;
        for (file, count) in expected {
            let actual = REGISTRY.iter().filter(|row| row.file == file).count();
            assert_eq!(actual, count, "{} row count", file.name());
            total += actual;
        }
        assert_eq!(total, 85);
    }

    #[test]
    fn registry_is_grouped_in_config_file_all_order() {
        let mut last = 0;
        for row in REGISTRY {
            let position = ConfigFile::ALL
                .iter()
                .position(|file| *file == row.file)
                .expect("every row file must be in ConfigFile::ALL");
            assert!(
                position >= last,
                "row {:?} breaks ConfigFile::ALL ordering",
                row.key
            );
            last = position;
        }
    }

    #[test]
    fn every_default_matches_its_kind_and_range() {
        for row in REGISTRY {
            match (row.kind, row.default) {
                (ConfigKind::Float { min, max }, ConfigValue::Float(value)) => {
                    assert!(value.is_finite(), "{} default must be finite", row.key);
                    assert!(
                        value >= min && value <= max,
                        "{} default {value} outside {min}..={max}",
                        row.key
                    );
                }
                (ConfigKind::Int { min, max }, ConfigValue::Int(value)) => {
                    assert!(
                        value >= min && value <= max,
                        "{} default {value} outside {min}..={max}",
                        row.key
                    );
                }
                (ConfigKind::Bool, ConfigValue::Bool(_)) => {}
                (kind, value) => panic!("{} declares kind {kind:?} but default {value:?}", row.key),
            }
        }
    }

    #[test]
    fn every_row_applies_live() {
        for row in REGISTRY {
            assert_eq!(
                row.apply,
                ApplyMode::Live,
                "{} must be live in M014",
                row.key
            );
        }
    }

    #[test]
    fn descriptions_are_one_sentence_without_pipes() {
        for row in REGISTRY {
            let description = row.description;
            assert!(!description.is_empty(), "{} needs a description", row.key);
            assert!(
                !description.contains('|'),
                "{} description must not contain a pipe",
                row.key
            );
            assert!(
                !description.contains('\n'),
                "{} description must be one line",
                row.key
            );
            assert!(
                description.ends_with('.'),
                "{} description must be a sentence",
                row.key
            );
            assert!(
                !row.unit.is_empty(),
                "{} needs a unit in the docs table",
                row.key
            );
        }
    }

    #[test]
    fn movement_defaults_equal_named_constants() {
        assert_eq!(
            row("movement.walk_speed").default,
            ConfigValue::Float(physics::WALK_SPEED)
        );
        assert_eq!(
            row("movement.sprint_speed").default,
            ConfigValue::Float(physics::SPRINT_SPEED)
        );
        assert_eq!(
            row("movement.fly_speed").default,
            ConfigValue::Float(physics::FLY_SPEED)
        );
        assert_eq!(
            row("movement.fly_sprint_speed").default,
            ConfigValue::Float(physics::FLY_SPRINT_SPEED)
        );
        assert_eq!(
            row("movement.fly_vertical_factor").default,
            ConfigValue::Float(physics::FLY_VERTICAL_FACTOR)
        );
        assert_eq!(
            row("movement.jump_velocity").default,
            ConfigValue::Float(physics::JUMP_VELOCITY)
        );
        assert_eq!(
            row("movement.gravity").default,
            ConfigValue::Float(physics::GRAVITY)
        );
        assert_eq!(
            row("movement.terminal_velocity").default,
            ConfigValue::Float(physics::TERMINAL_VELOCITY)
        );
        assert_eq!(
            row("movement.swim_vertical_drag").default,
            ConfigValue::Float(physics::SWIM_VERTICAL_DRAG)
        );
        assert_eq!(
            row("movement.swim_rise_accel").default,
            ConfigValue::Float(physics::SWIM_RISE_ACCEL)
        );
        assert_eq!(
            row("movement.swim_rise_speed").default,
            ConfigValue::Float(physics::SWIM_RISE_SPEED)
        );
        assert_eq!(
            row("movement.swim_max_sink_speed").default,
            ConfigValue::Float(physics::SWIM_MAX_SINK_SPEED)
        );
        assert_eq!(
            row("movement.swim_walk_speed_mult").default,
            ConfigValue::Float(physics::SWIM_WALK_SPEED_MULT)
        );
        assert_eq!(
            row("movement.swim_sink_accel").default,
            ConfigValue::Float(physics::SWIM_SINK_ACCEL)
        );
    }

    #[test]
    fn camera_defaults_equal_named_constants() {
        assert_eq!(
            row("camera.z_near").default,
            ConfigValue::Float(camera::DEFAULT_Z_NEAR)
        );
        assert_eq!(
            row("camera.z_far").default,
            ConfigValue::Float(camera::DEFAULT_Z_FAR)
        );
        let camera = camera::Camera::new(glam::Vec3::ZERO);
        assert_eq!(camera.z_near, camera::DEFAULT_Z_NEAR);
        assert_eq!(camera.z_far, camera::DEFAULT_Z_FAR);
    }

    #[test]
    fn docs_table_has_header_separator_and_one_row_per_entry() {
        let table = docs_table();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), REGISTRY.len() + 2);
        assert_eq!(
            lines[0],
            "| File | Key | Type | Default | Range | Unit | Apply | Description |"
        );
        assert_eq!(
            lines[1],
            "| --- | --- | --- | --- | --- | --- | --- | --- |"
        );
        for (line, row) in lines[2..].iter().zip(REGISTRY) {
            let columns: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(columns.len(), 10, "row {line:?} must have 8 columns");
            assert_eq!(columns[1], row.file.rel_path());
            assert_eq!(columns[2], row.key);
            assert_eq!(columns[3], kind_name(row.kind));
            assert_eq!(columns[4], format_default(row.default));
            assert_eq!(columns[5], format_range(row.kind));
            assert_eq!(columns[6], row.unit);
            assert_eq!(columns[7], apply_name(row.apply));
            assert_eq!(columns[8], row.description);
        }
    }

    #[test]
    fn index_of_finds_known_keys_and_rejects_unknown_ones() {
        assert_eq!(index_of("movement.walk_speed"), Some(0));
        assert_eq!(index_of("camera.z_far"), Some(15));
        assert_eq!(index_of("ui.selected_name_secs"), Some(84));
        assert_eq!(index_of("movement.not_a_key"), None);
        assert_eq!(index_of(""), None);
    }

    #[test]
    #[ignore = "prints the docs/CONFIG.md registry block for a manual paste"]
    fn print_docs_table() {
        print!("{}", docs_table());
    }
}
