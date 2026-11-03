use std::fmt;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use glam::IVec3;
use rustc_hash::FxHashMap;

use crate::content::{names, ContentRegistry, PackIdentity};
use crate::player::container::Container;
use crate::player::inventory::{
    ChestSlots, CraftingGrid, GridKind, Hotbar, Inventory, Item, ItemError, ItemId, ItemStack,
    Material, ToolKind, ToolTier, CHEST_SLOTS, HOTBAR_SLOTS, INVENTORY_SLOTS, MAX_GRID_SLOTS,
};
use crate::player::survival::{GameMode, SurvivalState};
use crate::world::block::BlockId;
use crate::world::chunk::{ChunkPos, CHUNK_VOLUME};
use crate::world::daycycle::{DAY_TICKS, DEFAULT_TIME_OF_DAY};
use crate::world::mob::{Mob, MobKind};
use crate::world::season::{DEFAULT_SEASON_LENGTH_DAYS, MAX_SEASON_LENGTH_DAYS};

pub const DEFAULT_SAVE_PATH: &str = "saves/default.mcvx";
/// Save path of the main-menu background world (M012).
///
/// Deliberately a subdirectory of the saves tree: [`crate::world::catalog_fs`]
/// lists only top-level `saves/*.mcvx` files, so the background can never
/// appear in the Load Game screen or be deleted from it.
pub const DEFAULT_BACKGROUND_SAVE_PATH: &str = "saves/background/background.mcvx";
pub const SAVE_MAGIC: &[u8; 4] = b"MCVX";
pub const SAVE_VERSION: u32 = 8;
/// First save version that carries the sorted save-local content string table,
/// `u16` name indices for every persisted content reference, and the
/// pack-identity + digest tail.
pub const CONTENT_TABLE_VERSION: u32 = 8;
/// Version the frozen test-only legacy encoder emits.
#[cfg(test)]
const LEGACY_WRITE_VERSION: u32 = 7;

/// Upper bound on saved content names. A forged count must never drive an
/// unbounded decode pass; the value leaves room for the built-in vocabulary
/// plus the 208 block-layer / 224 icon caps and then some.
pub const MAX_SAVE_CONTENT_NAMES: usize = 1024;
/// Upper bound on persisted pack identities in the v8 tail.
pub const MAX_SAVED_PACKS: usize = 1024;
/// Longest content name accepted on the save wire. Shared with the pack
/// parser so a valid pack can never author an unsavable id.
pub const MAX_SAVE_CONTENT_NAME_LEN: usize = names::MAX_CONTENT_NAME_LEN;
/// First save version that carries the survival tail: game mode, the
/// health/hunger/air triple, and the sorted per-position chest records.
pub const SURVIVAL_VERSION: u32 = 6;
/// First save version that carries the append-only mob tail: a `u32` count
/// followed by one fixed-size [`SAVED_MOB_LEN`] record per live mob.
pub const MOB_VERSION: u32 = 7;
/// The save version that introduced the current item vocabulary. New
/// identities take the next free discriminant and are gated to this version.
pub const ITEM_VOCAB_VERSION: u32 = MOB_VERSION;
/// Upper bound on persisted mob records. This is a defensive wire bound (a
/// forged count must never drive an unbounded decode pass), deliberately
/// unrelated to any future gameplay spawn cap.
pub const MAX_SAVED_MOBS: usize = 16_384;
/// Fixed on-disk size of one mob record: `kind: u8` + feet `x/y/z: f32` +
/// `yaw: f32` + `health: u16 LE`.
pub const SAVED_MOB_LEN: usize = 19;
/// Upper bound on persisted chest records. Mirrors the chunk-count bound so a
/// forged count can never request an unbounded decode pass.
pub const MAX_SAVED_CHESTS: usize = 1_000_000;
/// First save version that carries the 27-byte hotbar header block.
pub const HOTBAR_HEADER_VERSION: u32 = 2;
/// First save version that carries the time-of-day `u32`.
pub const TIME_OF_DAY_VERSION: u32 = 3;
/// First save version that carries the inventory extension after the chunk
/// section (full 9 + 27 slots, cursor, and unfinished crafting grids).
pub const INVENTORY_EXTENSION_VERSION: u32 = 4;
/// First save version that carries the world calendar after the inventory
/// extension: `day_index` then `season_length_days`, both `u32` LE. Older
/// saves migrate to day 0 and [`DEFAULT_SEASON_LENGTH_DAYS`].
pub const SEASON_VERSION: u32 = 5;

// Stable wire discriminants for the version-4 inventory extension. These are
// part of the on-disk format; changing the in-memory enum order without
// updating these constants would silently corrupt saves.
const ITEM_TAG_EMPTY: u8 = 0;
const ITEM_TAG_BLOCK: u8 = 1;
const ITEM_TAG_MATERIAL: u8 = 2;
const ITEM_TAG_TOOL: u8 = 3;
/// v8 generic registry item record: `u16 name_index + u16 count`.
const ITEM_TAG_GENERIC: u8 = 4;
/// v8 pack tool record: `u16 name_index + u16 remaining_durability`.
///
/// Pack tools are v8-only content: the legacy v1-v7 reader and writer keep
/// rejecting the widened identity instead of truncating its index.
const ITEM_TAG_MOD_TOOL: u8 = 5;

const MATERIAL_STICK: u8 = 0;
// Append-only: new material identities take the next unused discriminant.
const MATERIAL_APPLE: u8 = 1;
const MATERIAL_WHEAT: u8 = 2;
const MATERIAL_WHEAT_SEEDS: u8 = 3;
const MATERIAL_BREAD: u8 = 4;
// Append-only: the melee/drop vocabulary (M009) takes the next free tags.
const MATERIAL_LEATHER: u8 = 5;
const MATERIAL_RAW_PORKCHOP: u8 = 6;
const MATERIAL_RAW_BEEF: u8 = 7;
const MATERIAL_ROTTEN_FLESH: u8 = 8;
const MATERIAL_BONE: u8 = 9;
const MATERIAL_ARROW: u8 = 10;
const TOOL_KIND_PICKAXE: u8 = 0;
const TOOL_KIND_AXE: u8 = 1;
const TOOL_KIND_SHOVEL: u8 = 2;
const TOOL_KIND_HOE: u8 = 3;
const TOOL_KIND_SWORD: u8 = 4;
const TOOL_KIND_BOW: u8 = 5;
const TOOL_TIER_WOOD: u8 = 0;
const TOOL_TIER_STONE: u8 = 1;
const TOOL_TIER_IRON: u8 = 2;

const GRID_KIND_PLAYER: u8 = 0;
const GRID_KIND_TABLE: u8 = 1;

// v6 game-mode wire tags. Explicit and append-only like every other tag.
const GAME_MODE_SURVIVAL: u8 = 0;
const GAME_MODE_CREATIVE: u8 = 1;

fn game_mode_tag(mode: GameMode) -> u8 {
    match mode {
        GameMode::Survival => GAME_MODE_SURVIVAL,
        GameMode::Creative => GAME_MODE_CREATIVE,
    }
}

fn game_mode_from_tag(tag: u8) -> Option<GameMode> {
    match tag {
        GAME_MODE_SURVIVAL => Some(GameMode::Survival),
        GAME_MODE_CREATIVE => Some(GameMode::Creative),
        _ => None,
    }
}

/// Default acceleration scale for the game clock: 1.0 is the real 20-real-minute
/// cycle. `--time-scale` overrides it (a test seam for observing a full cycle in
/// a bounded run, MEM146).
pub const DEFAULT_TIME_SCALE: f32 = 1.0;

/// Default number of day cycles a bounded accelerated demo runs before saving
/// and exiting: exactly one, which is the behavior every pre-S06 accelerated
/// run already had (and what `scripts/season_smoke.sh` asserts when it checks
/// `day_index == 1` after one accelerated run).
pub const DEFAULT_DEMO_DAYS: u32 = 1;

/// Upper bound on `--demo-days`, mirroring [`MAX_SEASON_LENGTH_DAYS`]: a demo
/// spanning more than a full year (4 seasons of 365 days) is never useful for
/// a bounded smoke run, and the bound keeps the tick budget trivially in range.
pub const MAX_DEMO_DAYS: u32 = 365;

#[derive(Clone, Debug, PartialEq)]
pub struct CliConfig {
    pub save_path: PathBuf,
    pub seed_override: Option<u32>,
    pub force_new_world: bool,
    /// Clock acceleration multiplier from `--time-scale` (app-level setting,
    /// not a world intent — it never affects `cli_has_world_intent`). A value
    /// above 1.0 is the MEM146 test seam for observing a full cycle in a
    /// bounded run: the accelerated run saves and exits cleanly (status 0)
    /// after exactly one day cycle, so completion is a health signal rather
    /// than an external timeout kill.
    pub time_scale: f32,
    /// Season length in game days from `--season-length` (a fresh-world
    /// parameter like `time_scale`, so it never affects
    /// `cli_has_world_intent`). Only the fresh-world branch of startup
    /// consumes it: loading an existing save must restore the persisted
    /// calendar instead of a launch flag. Values are clamped to
    /// `1..=MAX_SEASON_LENGTH_DAYS`; zero and non-numeric input keep the
    /// default [`DEFAULT_SEASON_LENGTH_DAYS`].
    pub season_length: u32,
    /// Number of whole day cycles a bounded accelerated run observes before
    /// saving and exiting, from `--demo-days` (S06-T05). A test-seam launch
    /// parameter like `time_scale`/`season_length`, so it never affects
    /// `cli_has_world_intent`. Values are clamped to
    /// `1..=MAX_DEMO_DAYS`; missing, zero, and non-numeric input keep the
    /// default [`DEFAULT_DEMO_DAYS`] (one cycle — the long-standing bounded
    /// demo contract that `scripts/season_smoke.sh` depends on).
    pub demo_days: u32,
    /// Whether the bounded scripted ocean-crossing demo (`--demo-swim`, S08-T02)
    /// is requested. A bare launch switch like `--new`: it takes no value, is
    /// deliberately NOT a world intent (so it never affects
    /// `cli_has_world_intent`), and is never persisted. The fresh-world startup
    /// branch owns its effect (teleporting the player to the pinned route
    /// start); a loaded save is never teleported.
    pub demo_swim: bool,
    /// Whether the bounded scripted survival demo (`--demo-survival`, S08-T02)
    /// is requested. Same bare-launch-switch contract as `demo_swim`: it takes
    /// no value, is deliberately NOT a world intent (so it never affects
    /// `cli_has_world_intent`), and is never persisted. Only the fresh startup
    /// branch owns its effect, and the flag suppresses the accelerated-demo
    /// watch so the survival digest is never pre-empted by a `--time-scale`
    /// auto-shutdown.
    pub demo_survival: bool,
    /// Whether the bounded scripted ruin/farm/chest content demo
    /// (`--demo-content`, S08-T03) is requested. Same bare-switch contract as
    /// `demo_swim`/`demo_survival`: no value, never a world intent, never
    /// persisted, fresh startup only, and exempt from the accelerated-demo
    /// watch.
    pub demo_content: bool,
    /// Whether the bounded scripted combat demo (`--demo-combat`, M009/S01-T05)
    /// is requested. Same bare-switch contract as `demo_swim`/`demo_survival`:
    /// no value, never a world intent, never persisted, fresh startup only,
    /// and exempt from the accelerated-demo watch.
    pub demo_combat: bool,
    /// Whether the headless UI digest (`--demo-ui`, M013/S07-T03) is requested.
    /// Same bare-switch contract as the other demo seams: it takes no value,
    /// is deliberately NOT a world intent (so it never affects
    /// `cli_has_world_intent`), and is never persisted. `main` consumes it
    /// before `startup_session`, so it never reaches the event loop, a world,
    /// a save, a settings file, a window, or the GPU.
    pub demo_ui: bool,
    /// Whether the headless presentation digest (`--demo-presentation`,
    /// M010/S04-T05) is requested. Same bare-switch contract as `demo_ui`: it
    /// takes no value, is deliberately NOT a world intent (so it never affects
    /// `cli_has_world_intent`), and is never persisted. `main` consumes it
    /// before `StartupContent::load`, so the digest never opens a window, GPU,
    /// audio engine, event loop or settings write. The digest's only write is
    /// the fixture save at the demo's `--save` path (or a temp file).
    pub demo_presentation: bool,
    /// Pack folder requested by `--validate-pack <name>` (M010/S02-T04).
    /// `main` consumes it before `startup_session`, so validation never opens
    /// a window, GPU, world, save or settings file. A switch with no following
    /// token stores `Some(String::new())` so the typo fails closed instead of
    /// launching the game. Like `demo_ui` it is a launch switch, never a world
    /// intent (it never affects `cli_has_world_intent`) and is never persisted.
    pub validate_pack: Option<String>,
    /// Game mode requested by `--creative` / `--survival` (S08-T01 T05).
    /// Defaults to [`GameMode::Creative`] so every pre-M008 CLI demo, smoke,
    /// and physics test keeps its flying default. A fresh-world parameter like
    /// `time_scale`: it never affects `cli_has_world_intent`, and only the
    /// fresh startup branch consumes it — a loaded save always restores the
    /// persisted mode instead of a launch flag. Bare flags, last one wins.
    pub mode: GameMode,
}

impl CliConfig {
    pub fn parse_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut save_path = PathBuf::from(DEFAULT_SAVE_PATH);
        let mut seed_override = None;
        let mut force_new_world = false;
        let mut time_scale = DEFAULT_TIME_SCALE;
        let mut season_length = DEFAULT_SEASON_LENGTH_DAYS;
        let mut demo_days = DEFAULT_DEMO_DAYS;
        let mut demo_swim = false;
        let mut demo_survival = false;
        let mut demo_content = false;
        let mut demo_combat = false;
        let mut demo_ui = false;
        let mut demo_presentation = false;
        let mut validate_pack = None;
        let mut mode = GameMode::Creative;

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_ref() {
                "--save" => {
                    if let Some(val) = iter.next() {
                        save_path = PathBuf::from(val.as_ref());
                    }
                }
                "--seed" => {
                    if let Some(val) = iter.next() {
                        if let Ok(parsed) = val.as_ref().parse::<u32>() {
                            seed_override = Some(parsed);
                        }
                    }
                }
                "--time-scale" => {
                    // Lenient like the other value flags: a missing, non-numeric,
                    // or non-positive value keeps the default scale.
                    if let Some(val) = iter.next() {
                        if let Ok(parsed) = val.as_ref().parse::<f32>() {
                            if parsed.is_finite() && parsed > 0.0 {
                                time_scale = parsed;
                            }
                        }
                    }
                }
                "--season-length" => {
                    // Lenient like `--time-scale`: a missing, non-numeric, or
                    // zero value keeps the default length. A value above the
                    // persisted bound is clamped rather than rejected, since
                    // `season_of` is defensive and persistence clamps the
                    // same range on load.
                    if let Some(val) = iter.next() {
                        if let Ok(parsed) = val.as_ref().parse::<u32>() {
                            if parsed > 0 {
                                season_length = parsed.min(MAX_SEASON_LENGTH_DAYS);
                            }
                        }
                    }
                }
                "--demo-days" => {
                    // Lenient like `--season-length`: a missing, non-numeric, or
                    // zero value keeps the one-cycle default, and an absurd
                    // value clamps to the bounded maximum instead of running
                    // an effectively unbounded demo.
                    if let Some(val) = iter.next() {
                        if let Ok(parsed) = val.as_ref().parse::<u32>() {
                            if parsed > 0 {
                                demo_days = parsed.min(MAX_DEMO_DAYS);
                            }
                        }
                    }
                }
                "--demo-swim" => {
                    // Bare switch (no value to consume), mirroring `--new`:
                    // a stray following token stays an independent argument.
                    demo_swim = true;
                }
                "--demo-survival" => {
                    // Same bare-switch rule as `--demo-swim`.
                    demo_survival = true;
                }
                "--demo-content" => {
                    // Same bare-switch rule as `--demo-swim`.
                    demo_content = true;
                }
                "--demo-combat" => {
                    // Same bare-switch rule as `--demo-swim`.
                    demo_combat = true;
                }
                "--demo-ui" => {
                    // Same bare-switch rule as `--demo-swim`. `main`
                    // short-circuits on this before any world startup.
                    demo_ui = true;
                }
                "--demo-presentation" => {
                    // Same bare-switch rule as `--demo-ui`. `main`
                    // short-circuits on this before `StartupContent::load`.
                    demo_presentation = true;
                }
                "--validate-pack" => {
                    // Value switch: consume the next token. A missing value
                    // stores an empty name so the request fails closed in
                    // `main` instead of falling through to a normal launch.
                    validate_pack = Some(
                        iter.next()
                            .map(|val| val.as_ref().to_string())
                            .unwrap_or_default(),
                    );
                }
                // Bare mode switches, last one wins (plain assignment order).
                // They consume no value and are not world intents, so a
                // launch with only a mode flag still opens the main menu.
                "--creative" => mode = GameMode::Creative,
                "--survival" => mode = GameMode::Survival,
                "--new" | "--new-world" => {
                    force_new_world = true;
                }
                _ => {}
            }
        }

        Self {
            save_path,
            seed_override,
            force_new_world,
            time_scale,
            season_length,
            demo_days,
            demo_swim,
            demo_survival,
            demo_content,
            demo_combat,
            demo_ui,
            demo_presentation,
            validate_pack,
            mode,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedPlayerState {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub fly_mode: bool,
    pub selected_slot: u8,
    pub render_distance: u16,
    /// The full validated inventory aggregate: nine hotbar slots, 27 main
    /// slots, and the optional cursor stack.
    pub inventory: Inventory,
    /// Unfinished player-inventory crafting grid (2x2 active cells).
    pub player_grid: CraftingGrid,
    /// Unfinished crafting-table grid (4x4 active cells).
    pub table_grid: CraftingGrid,
    /// Persisted vitals triple. Only reachable through the checked
    /// [`SurvivalState`] constructors, so decode can reject out-of-range wire
    /// values before they enter the domain.
    pub survival: SurvivalState,
}

impl SavedPlayerState {
    /// Legacy block-only hotbar projection used by the v2/v3 wire header and
    /// by pre-inventory consumers. Non-block hotbar items (tools) are not
    /// representable here and project to empty slots; the v4 extension keeps
    /// their authoritative state.
    pub fn legacy_hotbar(&self) -> Hotbar {
        legacy_hotbar_projection(&self.inventory)
    }
}

fn legacy_hotbar_projection(inventory: &Inventory) -> Hotbar {
    let mut slots = [None; HOTBAR_SLOTS];
    for (slot, projected) in slots.iter_mut().enumerate() {
        *projected = inventory.hotbar_stack(slot);
    }
    Hotbar::from_slots(slots)
}

pub type ChunkEditMap = FxHashMap<ChunkPos, FxHashMap<u16, BlockId>>;

/// Persisted world chests keyed by their absolute block position. Every chest
/// owns exactly [`CHEST_SLOTS`] validated item slots, including an all-empty
/// chest (which is written as-is rather than dropped).
pub type ChestMap = FxHashMap<IVec3, Container>;

/// One persisted mob record (save version 7).
///
/// Deliberately a flat wire value rather than `Mob`: [`Mob`] carries six
/// ephemeral runtime fields (home anchor, velocity, AI state, hurt timer,
/// cooldown, walk phase) that a codec never writes, so a `Vec<Mob>` inside
/// [`WorldSaveData`] could never round-trip its derived `PartialEq`. The
/// save stores exactly what a relaunch must preserve: kind, feet position,
/// facing, and remaining health.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavedMob {
    pub kind: MobKind,
    /// Feet position in world space.
    pub position: [f32; 3],
    pub yaw: f32,
    pub health: u16,
}

impl From<&Mob> for SavedMob {
    fn from(mob: &Mob) -> Self {
        SavedMob {
            kind: mob.kind,
            position: mob.position.to_array(),
            yaw: mob.yaw,
            health: mob.health,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorldSaveData {
    pub seed: u32,
    /// Game time of day (0 = dawn, 6,000 = noon, 12,000 = dusk, 18,000 =
    /// midnight), persisted per world rather than derived from the wall
    /// clock. World state, so it sits next to `seed` rather than in
    /// `player`.
    pub time_of_day: u32,
    /// Whole game days elapsed since world creation, counted at the dawn
    /// boundary by [`crate::world::daycycle::GameClock`]. World state, so it
    /// sits next to `time_of_day`.
    pub day_index: u32,
    /// Length of one season in game days; drives the pure
    /// [`crate::world::season`] calendar. World state, so it sits next to
    /// `day_index`.
    pub season_length_days: u32,
    /// Persisted game mode. Kept separate from `player.fly_mode`: the writer
    /// canonicalises the legacy fly byte through [`GameMode::allows_fly`], so
    /// a Survival save never carries a flying player.
    pub mode: GameMode,
    pub player: SavedPlayerState,
    /// Persisted chest records, written sorted by `(x, y, z)` for a stable
    /// byte layout independent of hash-map iteration order.
    pub chests: ChestMap,
    /// Persisted live mobs (save version 7), written sorted by kind tag then
    /// position so the tail is independent of the session's list order.
    pub mobs: Vec<SavedMob>,
    pub chunk_edits: ChunkEditMap,
}

/// Persisted v8 content metadata: the pack stack that produced the save and
/// the FNV-1a registry digest at write time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveContentMeta {
    pub packs: Vec<PackIdentity>,
    pub digest: u64,
}

/// Result of a registry-aware load.
///
/// `registry` is the caller's base registry cloned and augmented (in table
/// order) with a deterministic placeholder definition for every saved name the
/// base could not resolve. `unresolved` counts exactly those appended
/// placeholders. `saved_content` is `None` for v1-v7 payloads, which carry no
/// pack identity or digest.
#[derive(Clone, Debug)]
pub struct LoadedWorld {
    pub data: WorldSaveData,
    pub registry: ContentRegistry,
    pub saved_content: Option<SaveContentMeta>,
    pub unresolved: usize,
}

impl LoadedWorld {
    /// A readable, non-blocking note describing how the registry that loaded
    /// this world differs from the one that saved it.
    ///
    /// `None` for v1-v7 payloads (they carry no pack identity or digest) and
    /// for a v8 save loaded against the exact same content stack.
    pub fn content_notice(&self) -> Option<String> {
        let saved = self.saved_content.as_ref()?;
        crate::content::content_mismatch_note(
            &saved.packs,
            saved.digest,
            &self.registry,
            self.unresolved,
        )
    }
}

#[derive(Debug)]
pub enum PersistenceError {
    Io(std::io::Error),
    InvalidMagic,
    UnsupportedVersion(u32),
    InvalidBlockId(u16),
    InvalidLocalIndex(u16),
    InvalidPlayerState,
    /// Version-4 inventory extension carried an unknown item tag.
    InvalidItemTag(u8),
    /// Version-4 inventory extension carried an unknown material discriminant.
    InvalidMaterial(u8),
    /// Version-4 inventory extension carried an unknown tool kind.
    InvalidToolKind(u8),
    /// Version-4 inventory extension carried an unknown tool tier.
    InvalidToolTier(u8),
    /// Version-4 crafting-grid record carried an unknown grid kind.
    InvalidGridKind(u8),
    /// A decoded item failed domain validation (count, block, durability).
    InvalidItem(ItemError),
    /// Crafting-grid state was inconsistent with its persisted kind/position.
    InvalidCraftingState,
    /// Version-6 survival tail carried an unknown game-mode tag.
    InvalidGameMode(u8),
    /// Version-6 vitals were outside the checked [`SurvivalState`] bounds.
    InvalidVitals {
        health: u16,
        hunger: u16,
        air: u16,
    },
    /// Version-6 chest section was malformed (unbounded count, duplicate
    /// position, or internally inconsistent record).
    InvalidChestState,
    /// Version-7 mob tail carried an unknown [`MobKind`] tag.
    InvalidMobKind(u8),
    /// Version-7 mob section was malformed (unbounded count, non-finite
    /// position or facing, health above the kind maximum, or duplicate
    /// `(kind, position)` record).
    InvalidMobState,
    /// A registry-allocated pack item has no encoding in a legacy save.
    UnsupportedItem,
    /// The v8 content string table was malformed: unbounded count, a
    /// zero/oversized name length, invalid UTF-8, duplicate or unsorted
    /// names, or an out-of-range name index.
    InvalidContentTable,
    /// A content name was malformed, reserved, or used under the wrong kind
    /// (for example a pack item name persisted as a block).
    InvalidContentName(String),
    /// A registry-allocated item or pack-tool index is not present in the
    /// registry that is naming or loading it.
    UnknownItemIndex(u16),
    /// The v8 pack-identity + digest tail was malformed (unbounded pack
    /// count, oversized/duplicate folder, or trailing bytes).
    InvalidContentMeta,
}

impl From<std::io::Error> for PersistenceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistenceError::Io(e) => write!(f, "could not read or write the save file: {e}"),
            PersistenceError::InvalidMagic => write!(f, "not a valid save file (bad magic)"),
            PersistenceError::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported save version {v} (this build reads version {SAVE_VERSION})"
                )
            }
            PersistenceError::InvalidBlockId(b) => write!(f, "unknown block id {b} in save"),
            PersistenceError::InvalidLocalIndex(i) => {
                write!(f, "invalid block index {i} in save")
            }
            PersistenceError::InvalidPlayerState => write!(f, "invalid player state in save"),
            PersistenceError::InvalidItemTag(tag) => {
                write!(f, "unknown item tag {tag} in save")
            }
            PersistenceError::InvalidMaterial(id) => {
                write!(f, "unknown material id {id} in save")
            }
            PersistenceError::InvalidToolKind(kind) => {
                write!(f, "unknown tool kind {kind} in save")
            }
            PersistenceError::InvalidToolTier(tier) => {
                write!(f, "unknown tool tier {tier} in save")
            }
            PersistenceError::InvalidGridKind(kind) => {
                write!(f, "unknown crafting grid kind {kind} in save")
            }
            PersistenceError::InvalidItem(e) => write!(f, "invalid item in save: {e:?}"),
            PersistenceError::InvalidCraftingState => {
                write!(f, "invalid crafting state in save")
            }
            PersistenceError::InvalidGameMode(tag) => {
                write!(f, "unknown game mode {tag} in save")
            }
            PersistenceError::InvalidVitals {
                health,
                hunger,
                air,
            } => write!(
                f,
                "invalid vitals in save (health {health}, hunger {hunger}, air {air})"
            ),
            PersistenceError::InvalidChestState => {
                write!(f, "invalid chest state in save")
            }
            PersistenceError::InvalidMobKind(tag) => {
                write!(f, "unknown mob kind {tag} in save")
            }
            PersistenceError::InvalidMobState => write!(f, "invalid mob state in save"),
            PersistenceError::UnsupportedItem => {
                write!(
                    f,
                    "a registry-allocated item cannot be written to a legacy save"
                )
            }
            PersistenceError::InvalidContentTable => {
                write!(f, "invalid content name table in save")
            }
            PersistenceError::InvalidContentName(name) => {
                write!(f, "invalid content name `{name}` in save")
            }
            PersistenceError::UnknownItemIndex(index) => {
                write!(f, "unknown item index {index} in save")
            }
            PersistenceError::InvalidContentMeta => {
                write!(f, "invalid content metadata in save")
            }
        }
    }
}

impl std::error::Error for PersistenceError {}

/// Rejects every block id the v1-v7 wire format cannot represent.
///
/// Each block byte on disk is a `u8` and the decoder accepts only the frozen
/// built-in ids `0..=22`, so a [`BlockId::Modded`] id would silently truncate
/// into a different built-in id. This scan runs before the first output byte
/// so such a save is rejected with an empty buffer instead.
#[cfg(test)]
fn validate_legacy_blocks(data: &WorldSaveData) -> Result<(), PersistenceError> {
    fn check_block(block: BlockId) -> Result<(), PersistenceError> {
        if !block.is_builtin() {
            return Err(PersistenceError::InvalidBlockId(block.to_raw()));
        }
        Ok(())
    }

    fn check_item(item: Option<Item>) -> Result<(), PersistenceError> {
        if let Some(item) = item {
            if let ItemId::Block(block) = item.id() {
                check_block(block)?;
            }
        }
        Ok(())
    }

    for edits in data.chunk_edits.values() {
        for &block in edits.values() {
            check_block(block)?;
            // Water is worldgen-only. A liquid in a chunk edit would
            // round-trip a value this schema deliberately refuses to load, so
            // refuse to write it (still before the first byte).
            if block.is_liquid() {
                return Err(PersistenceError::InvalidBlockId(block.to_raw()));
            }
        }
    }

    for slot in data.player.inventory.slots() {
        check_item(*slot)?;
    }
    check_item(data.player.inventory.cursor())?;

    // The v2/v3 hotbar header is a separate block-only projection of the
    // inventory; validate it explicitly so the header can never emit a
    // truncated id even if the projection changes.
    let legacy_hotbar = data.player.legacy_hotbar();
    for stack in legacy_hotbar.stacks().iter().flatten() {
        check_block(stack.block)?;
    }

    // Both crafting grids: `slot` is bounds-checked and returns `None` past
    // the grid's own slot count, so scanning the full MAX_GRID_SLOTS range is
    // safe for the 2x2 and 4x4 grids alike.
    for index in 0..MAX_GRID_SLOTS {
        check_item(data.player.player_grid.slot(index))?;
        check_item(data.player.table_grid.slot(index))?;
    }

    for container in data.chests.values() {
        for slot in container.slots() {
            check_item(*slot)?;
        }
    }

    Ok(())
}

/// Writes the frozen v1-v7 wire format.
///
/// Test-only since v8 became the public format: the migration suite uses this
/// encoder to build byte-valid v4-v7 fixtures instead of truncating the live
/// v8 writer's output (which has a wider item vocabulary and a content table).
#[cfg(test)]
fn write_legacy_world(out: &mut Vec<u8>, data: &WorldSaveData) -> Result<(), PersistenceError> {
    // The v1-v7 wire vocabulary is built-in only (a block id is one `u8` and
    // the decoder accepts 0..=22). Reject every unrepresentable block before
    // writing a single byte so a modded id can never be truncated into a
    // different built-in id on disk, and a rejected save leaves `out`
    // untouched (atomic-rejection contract).
    validate_legacy_blocks(data)?;

    // The two persisted grid positions are fixed, so a grid whose kind
    // disagrees with its slot would be unloadable. Validate before writing a
    // single byte so a rejected save leaves `out` untouched.
    if data.player.player_grid.kind() != GridKind::Player
        || data.player.table_grid.kind() != GridKind::Table
    {
        return Err(PersistenceError::InvalidCraftingState);
    }

    // The chest section carries a u32 count. Validate the bound and the
    // narrowing before the first byte is written: a rejected save must leave
    // `out` untouched (atomic-rejection contract).
    let chest_count = checked_chest_count(data.chests.len())?;

    // The v7 mob tail carries a u32 count and fixed-size records. Validate the
    // bound, every record, and the duplicate rule before the first byte, then
    // keep the canonical sort order for the write below. A rejected save must
    // leave `out` empty, exactly like every earlier section.
    let mob_count = checked_mob_count(data.mobs.len())?;
    let mut sorted_mobs = data.mobs.clone();
    for mob in &sorted_mobs {
        if !mob.position.iter().all(|c| c.is_finite()) || !mob.yaw.is_finite() {
            return Err(PersistenceError::InvalidMobState);
        }
        if mob.health > mob.kind.max_health() {
            return Err(PersistenceError::InvalidMobState);
        }
    }
    sorted_mobs.sort_by(|a, b| {
        a.kind
            .tag()
            .cmp(&b.kind.tag())
            .then_with(|| a.position[0].total_cmp(&b.position[0]))
            .then_with(|| a.position[1].total_cmp(&b.position[1]))
            .then_with(|| a.position[2].total_cmp(&b.position[2]))
    });
    if sorted_mobs.windows(2).any(|pair| {
        pair[0].kind == pair[1].kind
            && pair[0].position[0] == pair[1].position[0]
            && pair[0].position[1] == pair[1].position[1]
            && pair[0].position[2] == pair[1].position[2]
    }) {
        return Err(PersistenceError::InvalidMobState);
    }

    out.extend_from_slice(SAVE_MAGIC);
    out.extend_from_slice(&LEGACY_WRITE_VERSION.to_le_bytes());
    out.extend_from_slice(&data.seed.to_le_bytes());
    for c in data.player.position {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&data.player.yaw.to_le_bytes());
    out.extend_from_slice(&data.player.pitch.to_le_bytes());
    // Canonicalise the legacy fly flag through the persisted mode: a Survival
    // save can never carry a flying player, so a reload cannot resurrect
    // flight the mode forbids.
    out.push(u8::from(data.player.fly_mode && data.mode.allows_fly()));
    out.push(data.player.selected_slot.min(8));
    out.extend_from_slice(&data.player.render_distance.clamp(4, 36).to_le_bytes());

    // v2 hotbar header: 9 slots of `block_id: u8` + `count: u16` LE.
    // `block_id == 0` (Air) is the empty-slot sentinel and carries count 0.
    // This header is a block-only projection; non-block hotbar items are
    // authoritative in the v4 extension below.
    let legacy_hotbar = legacy_hotbar_projection(&data.player.inventory);
    for stack in legacy_hotbar.stacks() {
        match stack {
            Some(stack) => {
                out.push(
                    stack
                        .block
                        .builtin_u8()
                        .ok_or(PersistenceError::InvalidBlockId(stack.block.to_raw()))?,
                );
                out.extend_from_slice(&stack.count.to_le_bytes());
            }
            None => {
                out.push(0);
                out.extend_from_slice(&0u16.to_le_bytes());
            }
        }
    }

    // v3 time-of-day header: one u32 immediately after the hotbar block and
    // before the variable-length chunk section (absolute offset 63).
    out.extend_from_slice(&data.time_of_day.clamp(0, DAY_TICKS).to_le_bytes());

    let mut non_empty_chunks: Vec<(&ChunkPos, &FxHashMap<u16, BlockId>)> = data
        .chunk_edits
        .iter()
        .filter(|(_, edits)| !edits.is_empty())
        .collect();
    non_empty_chunks.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

    out.extend_from_slice(&(non_empty_chunks.len() as u32).to_le_bytes());
    for (pos, edits) in non_empty_chunks {
        out.extend_from_slice(&pos.x.to_le_bytes());
        out.extend_from_slice(&pos.y.to_le_bytes());
        out.extend_from_slice(&pos.z.to_le_bytes());

        let mut sorted_edits: Vec<(u16, BlockId)> =
            edits.iter().map(|(&idx, &b)| (idx, b)).collect();
        sorted_edits.sort_by_key(|&(idx, _)| idx);

        out.extend_from_slice(&(sorted_edits.len() as u32).to_le_bytes());
        for (idx, block) in sorted_edits {
            out.extend_from_slice(&idx.to_le_bytes());
            out.push(
                block
                    .builtin_u8()
                    .ok_or(PersistenceError::InvalidBlockId(block.to_raw()))?,
            );
        }
    }

    // v4 inventory extension, after every v3-known field: 36 validated slot
    // records, the cursor record, then the two unfinished crafting grids.
    write_inventory_extension(
        out,
        &data.player.inventory,
        &data.player.player_grid,
        &data.player.table_grid,
    )?;

    // v5 world calendar tail: `day_index` then `season_length_days`, both
    // `u32` LE, immediately after the v4 extension. The season length is
    // normalised the same way the loader clamps it, so a save/load round-trip
    // is byte-stable for every in-memory value (0 and absurd lengths become
    // the default) instead of silently changing on the next load.
    out.extend_from_slice(&data.day_index.to_le_bytes());
    out.extend_from_slice(
        &data
            .season_length_days
            .clamp(1, MAX_SEASON_LENGTH_DAYS)
            .to_le_bytes(),
    );

    // v6 survival tail, appended after the v5 calendar: mode, vitals, then
    // the chest count and sorted records. The calendar offsets 130/134 stay
    // fixed because everything new comes after them.
    out.push(game_mode_tag(data.mode));
    out.extend_from_slice(&data.player.survival.health().to_le_bytes());
    out.extend_from_slice(&data.player.survival.hunger().to_le_bytes());
    out.extend_from_slice(&data.player.survival.air().to_le_bytes());
    out.extend_from_slice(&chest_count.to_le_bytes());

    let mut sorted_chests: Vec<(&IVec3, &Container)> = data.chests.iter().collect();
    sorted_chests.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
    for (pos, container) in sorted_chests {
        out.extend_from_slice(&pos.x.to_le_bytes());
        out.extend_from_slice(&pos.y.to_le_bytes());
        out.extend_from_slice(&pos.z.to_le_bytes());
        for slot in container.slots() {
            write_item(out, *slot)?;
        }
    }

    // v7 mob tail, appended after the last chest record: the count, then one
    // fixed-size record per mob in `(kind tag, x, y, z)` order. `health == 0`
    // is legal (a mob that died in the same tick) and is written as-is.
    out.extend_from_slice(&mob_count.to_le_bytes());
    for mob in sorted_mobs {
        out.push(mob.kind.tag());
        for c in mob.position {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&mob.yaw.to_le_bytes());
        out.extend_from_slice(&mob.health.to_le_bytes());
    }

    Ok(())
}

/// Writes a v8 save using the built-in content registry.
///
/// Callers that loaded a world with a pack stack should use
/// [`save_world_to_bytes_with`] so modded names round-trip instead of being
/// rejected as unnameable.
pub fn save_world_to_bytes(
    out: &mut Vec<u8>,
    data: &WorldSaveData,
) -> Result<(), PersistenceError> {
    save_world_to_bytes_with(out, data, ContentRegistry::builtin())
}

/// Bounds-checks the persisted mob count before any bytes are emitted.
fn checked_mob_count(len: usize) -> Result<u32, PersistenceError> {
    if len > MAX_SAVED_MOBS {
        return Err(PersistenceError::InvalidMobState);
    }
    u32::try_from(len).map_err(|_| PersistenceError::InvalidMobState)
}

/// Bounds-checks the persisted chest count before any bytes are emitted.
fn checked_chest_count(len: usize) -> Result<u32, PersistenceError> {
    if len > MAX_SAVED_CHESTS {
        return Err(PersistenceError::InvalidChestState);
    }
    u32::try_from(len).map_err(|_| PersistenceError::InvalidChestState)
}

fn grid_kind_tag(kind: GridKind) -> u8 {
    match kind {
        GridKind::Player => GRID_KIND_PLAYER,
        GridKind::Table => GRID_KIND_TABLE,
    }
}

fn grid_kind_from_tag(tag: u8) -> Option<GridKind> {
    match tag {
        GRID_KIND_PLAYER => Some(GridKind::Player),
        GRID_KIND_TABLE => Some(GridKind::Table),
        _ => None,
    }
}

#[cfg(test)]
fn material_tag(material: Material) -> u8 {
    match material {
        Material::Stick => MATERIAL_STICK,
        Material::Apple => MATERIAL_APPLE,
        Material::Wheat => MATERIAL_WHEAT,
        Material::WheatSeeds => MATERIAL_WHEAT_SEEDS,
        Material::Bread => MATERIAL_BREAD,
        Material::Leather => MATERIAL_LEATHER,
        Material::RawPorkchop => MATERIAL_RAW_PORKCHOP,
        Material::RawBeef => MATERIAL_RAW_BEEF,
        Material::RottenFlesh => MATERIAL_ROTTEN_FLESH,
        Material::Bone => MATERIAL_BONE,
        Material::Arrow => MATERIAL_ARROW,
    }
}

fn material_from_tag(tag: u8) -> Option<Material> {
    match tag {
        MATERIAL_STICK => Some(Material::Stick),
        MATERIAL_APPLE => Some(Material::Apple),
        MATERIAL_WHEAT => Some(Material::Wheat),
        MATERIAL_WHEAT_SEEDS => Some(Material::WheatSeeds),
        MATERIAL_BREAD => Some(Material::Bread),
        MATERIAL_LEATHER => Some(Material::Leather),
        MATERIAL_RAW_PORKCHOP => Some(Material::RawPorkchop),
        MATERIAL_RAW_BEEF => Some(Material::RawBeef),
        MATERIAL_ROTTEN_FLESH => Some(Material::RottenFlesh),
        MATERIAL_BONE => Some(Material::Bone),
        MATERIAL_ARROW => Some(Material::Arrow),
        _ => None,
    }
}

#[cfg(test)]
fn tool_kind_tag(kind: ToolKind) -> u8 {
    match kind {
        ToolKind::Pickaxe => TOOL_KIND_PICKAXE,
        ToolKind::Axe => TOOL_KIND_AXE,
        ToolKind::Shovel => TOOL_KIND_SHOVEL,
        ToolKind::Hoe => TOOL_KIND_HOE,
        ToolKind::Sword => TOOL_KIND_SWORD,
        ToolKind::Bow => TOOL_KIND_BOW,
    }
}

fn tool_kind_from_tag(tag: u8) -> Option<ToolKind> {
    match tag {
        TOOL_KIND_PICKAXE => Some(ToolKind::Pickaxe),
        TOOL_KIND_AXE => Some(ToolKind::Axe),
        TOOL_KIND_SHOVEL => Some(ToolKind::Shovel),
        TOOL_KIND_HOE => Some(ToolKind::Hoe),
        TOOL_KIND_SWORD => Some(ToolKind::Sword),
        TOOL_KIND_BOW => Some(ToolKind::Bow),
        _ => None,
    }
}

#[cfg(test)]
fn tool_tier_tag(tier: ToolTier) -> u8 {
    match tier {
        ToolTier::Wood => TOOL_TIER_WOOD,
        ToolTier::Stone => TOOL_TIER_STONE,
        ToolTier::Iron => TOOL_TIER_IRON,
    }
}

fn tool_tier_from_tag(tag: u8) -> Option<ToolTier> {
    match tag {
        TOOL_TIER_WOOD => Some(ToolTier::Wood),
        TOOL_TIER_STONE => Some(ToolTier::Stone),
        TOOL_TIER_IRON => Some(ToolTier::Iron),
        _ => None,
    }
}

/// Appends one fixed-size item record: `tag` then a tag-specific payload.
///
/// Block payloads narrow through [`BlockId::builtin_u8`] and surface
/// [`PersistenceError::InvalidBlockId`] instead of truncating a modded id. The
/// pre-write [`validate_legacy_blocks`] scan makes that branch unreachable for
/// a save that reached the writer, but the writer keeps the check explicit so
/// no unchecked cast can be reintroduced.
#[cfg(test)]
fn write_item(out: &mut Vec<u8>, item: Option<Item>) -> Result<(), PersistenceError> {
    match item {
        None => out.push(ITEM_TAG_EMPTY),
        Some(item) => match item.id() {
            ItemId::Block(block) => {
                out.push(ITEM_TAG_BLOCK);
                out.push(
                    block
                        .builtin_u8()
                        .ok_or(PersistenceError::InvalidBlockId(block.to_raw()))?,
                );
                out.extend_from_slice(&item.count().to_le_bytes());
            }
            ItemId::Material(material) => {
                out.push(ITEM_TAG_MATERIAL);
                out.push(material_tag(material));
                out.extend_from_slice(&item.count().to_le_bytes());
            }
            ItemId::Tool { kind, tier } => {
                out.push(ITEM_TAG_TOOL);
                out.push(tool_kind_tag(kind));
                out.push(tool_tier_tag(tier));
                out.extend_from_slice(&item.durability().unwrap_or(0).to_le_bytes());
            }
            ItemId::Item(_) => return Err(PersistenceError::UnsupportedItem),
            // Pack tools are v8-only content (T05). Legacy versions fail
            // closed rather than truncating the tool index.
            ItemId::ModTool(_) => return Err(PersistenceError::UnsupportedItem),
        },
    }
    Ok(())
}

#[cfg(test)]
fn write_grid(out: &mut Vec<u8>, grid: &CraftingGrid) -> Result<(), PersistenceError> {
    out.push(grid_kind_tag(grid.kind()));
    for index in 0..grid.kind().slot_count() {
        write_item(out, grid.slot(index))?;
    }
    Ok(())
}

#[cfg(test)]
fn write_inventory_extension(
    out: &mut Vec<u8>,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
) -> Result<(), PersistenceError> {
    for item in inventory.slots() {
        write_item(out, *item)?;
    }
    write_item(out, inventory.cursor())?;
    write_grid(out, player_grid)?;
    write_grid(out, table_grid)?;
    Ok(())
}

// ---------------------------------------------------------------------
// v8 content-table codec
// ---------------------------------------------------------------------

/// Whether `name` is a well-formed wire content name.
///
/// A wire name is either a frozen built-in item/block/material/tool name or a
/// non-reserved namespaced id. Reserved namespaces and unqualified non-builtin
/// spellings are forged payloads, not unknowns.
fn is_wire_content_name(name: &str) -> bool {
    if names::builtin_item_id_by_name(name).is_some() {
        return true;
    }
    if !names::is_namespaced_id(name) {
        return false;
    }
    let namespace = name.split(':').next().unwrap_or("");
    !names::is_reserved_namespace(namespace)
}

/// Whether `name` may be materialised as a deterministic placeholder.
fn is_placeholder_content_name(name: &str) -> bool {
    names::is_namespaced_id(name) && {
        let namespace = name.split(':').next().unwrap_or("");
        !names::is_reserved_namespace(namespace)
    }
}

/// The save name of a block reference.
///
/// Liquids are worldgen-only and an unnameable id is a writer bug, so both are
/// rejected before the first output byte.
fn content_name_of_block(
    registry: &ContentRegistry,
    block: BlockId,
) -> Result<&str, PersistenceError> {
    if block.is_liquid() {
        return Err(PersistenceError::InvalidBlockId(block.to_raw()));
    }
    registry
        .block_name(block)
        .ok_or(PersistenceError::InvalidBlockId(block.to_raw()))
}

/// The save name of an item reference.
fn content_name_of_item(registry: &ContentRegistry, item: Item) -> Result<&str, PersistenceError> {
    match item.id() {
        ItemId::Block(block) => content_name_of_block(registry, block),
        ItemId::Material(material) => Ok(names::builtin_material_id_name(material)),
        ItemId::Tool { kind, tier } => names::builtin_tool_id_name(kind, tier)
            .ok_or_else(|| PersistenceError::InvalidContentName(format!("{kind:?}:{tier:?}"))),
        ItemId::Item(index) => registry
            .item_def_name(index)
            .ok_or(PersistenceError::UnknownItemIndex(index)),
        // Pack tools are v8-only content (T05); until then they cannot be
        // named by the legacy writer.
        ItemId::ModTool(_) => Err(PersistenceError::UnsupportedItem),
    }
}

/// Sorted, unique save-local content names plus their `u16` wire indices.
struct ContentNameTable {
    names: Vec<String>,
    index: FxHashMap<String, u16>,
}

impl ContentNameTable {
    /// Collects every referenced name in one pre-write pass.
    ///
    /// This is the atomic-rejection contract: every unnameable reference, every
    /// liquid edit, every name over the wire cap, and every out-of-range table
    /// is rejected before the first output byte.
    fn collect(data: &WorldSaveData, registry: &ContentRegistry) -> Result<Self, PersistenceError> {
        fn add_name(
            set: &mut std::collections::BTreeSet<String>,
            name: &str,
        ) -> Result<(), PersistenceError> {
            if name.is_empty()
                || name.len() > MAX_SAVE_CONTENT_NAME_LEN
                || !is_wire_content_name(name)
            {
                return Err(PersistenceError::InvalidContentName(name.to_string()));
            }
            set.insert(name.to_string());
            Ok(())
        }

        fn add_block(
            set: &mut std::collections::BTreeSet<String>,
            registry: &ContentRegistry,
            block: BlockId,
        ) -> Result<(), PersistenceError> {
            add_name(set, content_name_of_block(registry, block)?)
        }

        fn add_item(
            set: &mut std::collections::BTreeSet<String>,
            registry: &ContentRegistry,
            item: Item,
        ) -> Result<(), PersistenceError> {
            match item.id() {
                // Pack tools are named by the v8 table through their authored
                // stable id; the legacy naming helper stays fail-closed.
                ItemId::ModTool(index) => {
                    let name = registry
                        .tool_def_name(index)
                        .ok_or(PersistenceError::UnknownItemIndex(index))?;
                    add_name(set, name)
                }
                _ => add_name(set, content_name_of_item(registry, item)?),
            }
        }

        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for edits in data.chunk_edits.values() {
            for &block in edits.values() {
                add_block(&mut set, registry, block)?;
            }
        }
        for item in data.player.inventory.slots().iter().flatten() {
            add_item(&mut set, registry, *item)?;
        }
        if let Some(item) = data.player.inventory.cursor() {
            add_item(&mut set, registry, item)?;
        }
        for index in 0..MAX_GRID_SLOTS {
            if let Some(item) = data.player.player_grid.slot(index) {
                add_item(&mut set, registry, item)?;
            }
            if let Some(item) = data.player.table_grid.slot(index) {
                add_item(&mut set, registry, item)?;
            }
        }
        for container in data.chests.values() {
            for item in container.slots().iter().flatten() {
                add_item(&mut set, registry, *item)?;
            }
        }

        if set.len() > MAX_SAVE_CONTENT_NAMES {
            return Err(PersistenceError::InvalidContentTable);
        }
        let names: Vec<String> = set.into_iter().collect();
        let mut index = FxHashMap::default();
        for (position, name) in names.iter().enumerate() {
            let wire =
                u16::try_from(position).map_err(|_| PersistenceError::InvalidContentTable)?;
            index.insert(name.clone(), wire);
        }
        Ok(Self { names, index })
    }

    fn index_of(&self, name: &str) -> Result<u16, PersistenceError> {
        self.index
            .get(name)
            .copied()
            .ok_or_else(|| PersistenceError::InvalidContentName(name.to_string()))
    }

    fn name_index_of_block(
        &self,
        registry: &ContentRegistry,
        block: BlockId,
    ) -> Result<u16, PersistenceError> {
        self.index_of(content_name_of_block(registry, block)?)
    }

    fn name_index_of_item(
        &self,
        registry: &ContentRegistry,
        item: Item,
    ) -> Result<u16, PersistenceError> {
        match item.id() {
            ItemId::ModTool(index) => {
                let name = registry
                    .tool_def_name(index)
                    .ok_or(PersistenceError::UnknownItemIndex(index))?;
                self.index_of(name)
            }
            _ => self.index_of(content_name_of_item(registry, item)?),
        }
    }
}

/// Writes one v8 item record: `tag` then a tag-specific payload built from a
/// `u16` content-name index.
fn write_v8_item(
    out: &mut Vec<u8>,
    item: Option<Item>,
    registry: &ContentRegistry,
    table: &ContentNameTable,
) -> Result<(), PersistenceError> {
    match item {
        None => out.push(ITEM_TAG_EMPTY),
        Some(item) => match item.id() {
            ItemId::Block(_) => {
                out.push(ITEM_TAG_BLOCK);
                out.extend_from_slice(&table.name_index_of_item(registry, item)?.to_le_bytes());
                out.extend_from_slice(&item.count().to_le_bytes());
            }
            ItemId::Material(_) => {
                out.push(ITEM_TAG_MATERIAL);
                out.extend_from_slice(&table.name_index_of_item(registry, item)?.to_le_bytes());
                out.extend_from_slice(&item.count().to_le_bytes());
            }
            ItemId::Tool { .. } => {
                out.push(ITEM_TAG_TOOL);
                out.extend_from_slice(&table.name_index_of_item(registry, item)?.to_le_bytes());
                out.extend_from_slice(&item.durability().unwrap_or(0).to_le_bytes());
            }
            ItemId::Item(_) => {
                out.push(ITEM_TAG_GENERIC);
                out.extend_from_slice(&table.name_index_of_item(registry, item)?.to_le_bytes());
                out.extend_from_slice(&item.count().to_le_bytes());
            }
            ItemId::ModTool(_) => {
                out.push(ITEM_TAG_MOD_TOOL);
                out.extend_from_slice(&table.name_index_of_item(registry, item)?.to_le_bytes());
                out.extend_from_slice(&item.durability().unwrap_or(0).to_le_bytes());
            }
        },
    }
    Ok(())
}

fn write_v8_grid(
    out: &mut Vec<u8>,
    grid: &CraftingGrid,
    registry: &ContentRegistry,
    table: &ContentNameTable,
) -> Result<(), PersistenceError> {
    out.push(grid_kind_tag(grid.kind()));
    for index in 0..grid.kind().slot_count() {
        write_v8_item(out, grid.slot(index), registry, table)?;
    }
    Ok(())
}

fn write_v8_inventory_extension(
    out: &mut Vec<u8>,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
    registry: &ContentRegistry,
    table: &ContentNameTable,
) -> Result<(), PersistenceError> {
    for item in inventory.slots() {
        write_v8_item(out, *item, registry, table)?;
    }
    write_v8_item(out, inventory.cursor(), registry, table)?;
    write_v8_grid(out, player_grid, registry, table)?;
    write_v8_grid(out, table_grid, registry, table)?;
    Ok(())
}

/// Validates and canonically sorts the persisted pack identities.
fn checked_packs(registry: &ContentRegistry) -> Result<Vec<PackIdentity>, PersistenceError> {
    let mut packs = registry.packs().to_vec();
    if packs.len() > MAX_SAVED_PACKS {
        return Err(PersistenceError::InvalidContentMeta);
    }
    for pack in &packs {
        if pack.folder.is_empty() || pack.folder.len() > MAX_SAVE_CONTENT_NAME_LEN {
            return Err(PersistenceError::InvalidContentMeta);
        }
        if let Some(version) = &pack.version {
            if version.is_empty() || version.len() > MAX_SAVE_CONTENT_NAME_LEN {
                return Err(PersistenceError::InvalidContentMeta);
            }
        }
    }
    packs.sort_by(|a, b| a.folder.cmp(&b.folder));
    if packs
        .windows(2)
        .any(|pair| pair[0].folder == pair[1].folder)
    {
        return Err(PersistenceError::InvalidContentMeta);
    }
    Ok(packs)
}

fn write_pack_identity(out: &mut Vec<u8>, pack: &PackIdentity) -> Result<(), PersistenceError> {
    let folder_len =
        u16::try_from(pack.folder.len()).map_err(|_| PersistenceError::InvalidContentMeta)?;
    out.extend_from_slice(&folder_len.to_le_bytes());
    out.extend_from_slice(pack.folder.as_bytes());
    match &pack.version {
        None => out.push(0),
        Some(version) => {
            out.push(1);
            let version_len =
                u16::try_from(version.len()).map_err(|_| PersistenceError::InvalidContentMeta)?;
            out.extend_from_slice(&version_len.to_le_bytes());
            out.extend_from_slice(version.as_bytes());
        }
    }
    Ok(())
}

/// Writes the v8 format: a sorted save-local content table in the header, a
/// `u16` name index for every content reference, and a pack-identity + digest
/// tail.
fn write_v8_world(
    out: &mut Vec<u8>,
    data: &WorldSaveData,
    registry: &ContentRegistry,
) -> Result<(), PersistenceError> {
    // Every content reference, name bound, liquid edit and id lookup is
    // validated here, before the first output byte.
    let table = ContentNameTable::collect(data, registry)?;

    if data.player.player_grid.kind() != GridKind::Player
        || data.player.table_grid.kind() != GridKind::Table
    {
        return Err(PersistenceError::InvalidCraftingState);
    }

    let chest_count = checked_chest_count(data.chests.len())?;

    let mob_count = checked_mob_count(data.mobs.len())?;
    let mut sorted_mobs = data.mobs.clone();
    for mob in &sorted_mobs {
        if !mob.position.iter().all(|c| c.is_finite()) || !mob.yaw.is_finite() {
            return Err(PersistenceError::InvalidMobState);
        }
        if mob.health > mob.kind.max_health() {
            return Err(PersistenceError::InvalidMobState);
        }
    }
    sorted_mobs.sort_by(|a, b| {
        a.kind
            .tag()
            .cmp(&b.kind.tag())
            .then_with(|| a.position[0].total_cmp(&b.position[0]))
            .then_with(|| a.position[1].total_cmp(&b.position[1]))
            .then_with(|| a.position[2].total_cmp(&b.position[2]))
    });
    if sorted_mobs.windows(2).any(|pair| {
        pair[0].kind == pair[1].kind
            && pair[0].position[0] == pair[1].position[0]
            && pair[0].position[1] == pair[1].position[1]
            && pair[0].position[2] == pair[1].position[2]
    }) {
        return Err(PersistenceError::InvalidMobState);
    }

    let packs = checked_packs(registry)?;

    out.extend_from_slice(SAVE_MAGIC);
    out.extend_from_slice(&SAVE_VERSION.to_le_bytes());
    out.extend_from_slice(&data.seed.to_le_bytes());
    for c in data.player.position {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&data.player.yaw.to_le_bytes());
    out.extend_from_slice(&data.player.pitch.to_le_bytes());
    out.push(u8::from(data.player.fly_mode && data.mode.allows_fly()));
    out.push(data.player.selected_slot.min(8));
    out.extend_from_slice(&data.player.render_distance.clamp(4, 36).to_le_bytes());

    // v8 content table: count, then each `u16 len + UTF-8 name` in Rust
    // `String` order (UTF-8 byte order), duplicate-free by construction.
    let name_count =
        u32::try_from(table.names.len()).map_err(|_| PersistenceError::InvalidContentTable)?;
    out.extend_from_slice(&name_count.to_le_bytes());
    for name in &table.names {
        let len = u16::try_from(name.len())
            .map_err(|_| PersistenceError::InvalidContentName(name.to_string()))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }

    // Unchanged v2 hotbar projection. A modded or non-block slot has no u8
    // identity, so it projects to the empty sentinel; the v4 extension below
    // is authoritative.
    let legacy_hotbar = legacy_hotbar_projection(&data.player.inventory);
    for stack in legacy_hotbar.stacks() {
        match stack.and_then(|stack| stack.block.builtin_u8().map(|raw| (raw, stack.count))) {
            Some((raw, count)) => {
                out.push(raw);
                out.extend_from_slice(&count.to_le_bytes());
            }
            None => {
                out.push(0);
                out.extend_from_slice(&0u16.to_le_bytes());
            }
        }
    }

    out.extend_from_slice(&data.time_of_day.clamp(0, DAY_TICKS).to_le_bytes());

    let mut non_empty_chunks: Vec<(&ChunkPos, &FxHashMap<u16, BlockId>)> = data
        .chunk_edits
        .iter()
        .filter(|(_, edits)| !edits.is_empty())
        .collect();
    non_empty_chunks.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

    out.extend_from_slice(&(non_empty_chunks.len() as u32).to_le_bytes());
    for (pos, edits) in non_empty_chunks {
        out.extend_from_slice(&pos.x.to_le_bytes());
        out.extend_from_slice(&pos.y.to_le_bytes());
        out.extend_from_slice(&pos.z.to_le_bytes());

        let mut sorted_edits: Vec<(u16, BlockId)> =
            edits.iter().map(|(&idx, &b)| (idx, b)).collect();
        sorted_edits.sort_by_key(|&(idx, _)| idx);

        out.extend_from_slice(&(sorted_edits.len() as u32).to_le_bytes());
        for (idx, block) in sorted_edits {
            out.extend_from_slice(&idx.to_le_bytes());
            out.extend_from_slice(&table.name_index_of_block(registry, block)?.to_le_bytes());
        }
    }

    write_v8_inventory_extension(
        out,
        &data.player.inventory,
        &data.player.player_grid,
        &data.player.table_grid,
        registry,
        &table,
    )?;

    out.extend_from_slice(&data.day_index.to_le_bytes());
    out.extend_from_slice(
        &data
            .season_length_days
            .clamp(1, MAX_SEASON_LENGTH_DAYS)
            .to_le_bytes(),
    );

    out.push(game_mode_tag(data.mode));
    out.extend_from_slice(&data.player.survival.health().to_le_bytes());
    out.extend_from_slice(&data.player.survival.hunger().to_le_bytes());
    out.extend_from_slice(&data.player.survival.air().to_le_bytes());
    out.extend_from_slice(&chest_count.to_le_bytes());

    let mut sorted_chests: Vec<(&IVec3, &Container)> = data.chests.iter().collect();
    sorted_chests.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
    for (pos, container) in sorted_chests {
        out.extend_from_slice(&pos.x.to_le_bytes());
        out.extend_from_slice(&pos.y.to_le_bytes());
        out.extend_from_slice(&pos.z.to_le_bytes());
        for slot in container.slots() {
            write_v8_item(out, *slot, registry, &table)?;
        }
    }

    out.extend_from_slice(&mob_count.to_le_bytes());
    for mob in sorted_mobs {
        out.push(mob.kind.tag());
        for c in mob.position {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&mob.yaw.to_le_bytes());
        out.extend_from_slice(&mob.health.to_le_bytes());
    }

    // v8 tail: pack stack (sorted by folder) then the FNV-1a registry digest.
    let pack_count =
        u32::try_from(packs.len()).map_err(|_| PersistenceError::InvalidContentMeta)?;
    out.extend_from_slice(&pack_count.to_le_bytes());
    for pack in &packs {
        write_pack_identity(out, pack)?;
    }
    out.extend_from_slice(&registry.digest().to_le_bytes());

    Ok(())
}

/// Writes a v8 save using `registry` to name every content reference.
///
/// Rejects the whole payload before the first output byte if any reference is
/// unnameable, any name exceeds the wire cap, or the pack metadata is invalid.
pub fn save_world_to_bytes_with(
    out: &mut Vec<u8>,
    data: &WorldSaveData,
    registry: &ContentRegistry,
) -> Result<(), PersistenceError> {
    write_v8_world(out, data, registry)
}

fn read_u8<R: Read>(r: &mut R) -> Result<u8, PersistenceError> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16, PersistenceError> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64, PersistenceError> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_i32<R: Read>(r: &mut R) -> Result<i32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_le_bytes(b))
}

fn read_f32<R: Read>(r: &mut R) -> Result<f32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    let v = f32::from_le_bytes(b);
    if !v.is_finite() {
        return Err(PersistenceError::InvalidPlayerState);
    }
    Ok(v)
}

/// Reads one finite `f32` inside the v7 mob tail. Kept separate from
/// [`read_f32`] so a malformed mob record reports `InvalidMobState` rather
/// than blaming the player record.
fn read_mob_f32<R: Read>(r: &mut R) -> Result<f32, PersistenceError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    let v = f32::from_le_bytes(b);
    if !v.is_finite() {
        return Err(PersistenceError::InvalidMobState);
    }
    Ok(v)
}

/// Decodes the frozen v1-v7 wire format.
///
/// Behaviour is unchanged from the pre-v8 reader; only the accepted version
/// range narrows so a v8 payload can never fall through into the legacy
/// section layout.
fn decode_legacy(bytes: &[u8]) -> Result<WorldSaveData, PersistenceError> {
    let mut cur = Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic)?;
    if &magic != SAVE_MAGIC {
        return Err(PersistenceError::InvalidMagic);
    }

    let version = read_u32(&mut cur)?;
    // v1..=v7 is every version before the content table. v8 never reaches
    // this reader; it is dispatched to [`decode_v8`] by version.
    if !(1..=MOB_VERSION).contains(&version) {
        return Err(PersistenceError::UnsupportedVersion(version));
    }

    let seed = read_u32(&mut cur)?;
    let px = read_f32(&mut cur)?;
    let py = read_f32(&mut cur)?;
    let pz = read_f32(&mut cur)?;
    let yaw = read_f32(&mut cur)?;
    let pitch = read_f32(&mut cur)?;
    let fly_raw = read_u8(&mut cur)?;
    let selected_slot = read_u8(&mut cur)?;
    let render_distance = read_u16(&mut cur)?;

    if selected_slot >= 9 || !(4..=36).contains(&render_distance) {
        return Err(PersistenceError::InvalidPlayerState);
    }

    // v2 added the 27-byte hotbar block between render_distance and
    // chunk_count. v1 files predate it and migrate to an empty hotbar.
    let hotbar = if version >= HOTBAR_HEADER_VERSION {
        read_hotbar(&mut cur, version)?
    } else {
        Hotbar::empty()
    };

    // v3 added the time-of-day u32 between the hotbar and chunk_count.
    // v1/v2 saves migrate to noon. A scalar degrades gracefully, so an
    // out-of-range value clamps instead of failing the whole save (unlike
    // the strictly validated hotbar). `DAY_TICKS` wraps to dawn.
    let time_of_day = if version >= TIME_OF_DAY_VERSION {
        read_u32(&mut cur)?.min(DAY_TICKS - 1)
    } else {
        DEFAULT_TIME_OF_DAY
    };

    let chunk_count = read_u32(&mut cur)?;
    if chunk_count > 1_000_000 {
        return Err(PersistenceError::InvalidPlayerState);
    }

    let mut chunk_edits: ChunkEditMap = FxHashMap::default();
    for _ in 0..chunk_count {
        let cx = read_i32(&mut cur)?;
        let cy = read_i32(&mut cur)?;
        let cz = read_i32(&mut cur)?;
        let edit_count = read_u32(&mut cur)?;
        if edit_count == 0 || edit_count as usize > CHUNK_VOLUME {
            return Err(PersistenceError::InvalidPlayerState);
        }

        let mut edits = FxHashMap::default();
        for _ in 0..edit_count {
            let local_idx = read_u16(&mut cur)?;
            if local_idx as usize >= CHUNK_VOLUME {
                return Err(PersistenceError::InvalidLocalIndex(local_idx));
            }
            let block_raw = read_u8(&mut cur)?;
            let Some(block) = BlockId::from_u8(block_raw) else {
                return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
            };
            // v4 validation keeps rejecting id 15 even though `from_u8` now
            // accepts it: water is generated only by the terrain pass and must
            // never round-trip through a persisted chunk edit. Treating a
            // forged water edit as an unknown id fails the whole save closed.
            if block.is_liquid() {
                return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
            }
            // Ids 16..=22 are v6 wire identities. A pre-v6 payload that
            // claims one is corrupt (or forged), so fail closed instead of
            // inventing a block under a version that never could have
            // written it. Water keeps its own unconditional rejection above.
            if version < SURVIVAL_VERSION && is_survival_only_block_id(block) {
                return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
            }
            edits.insert(local_idx, block);
        }
        chunk_edits.insert(ChunkPos::new(cx, cy, cz), edits);
    }

    // v4 appends the validated inventory aggregate after every v3-known
    // field. v5 then appends the world calendar tail (day_index +
    // season_length_days). Missing bytes are a truncated record; extra bytes
    // mean the payload does not match the declared version.
    let (inventory, player_grid, table_grid) = if version >= INVENTORY_EXTENSION_VERSION {
        read_inventory_extension(&mut cur, version)?
    } else {
        // v1-v3 stored only the hotbar. Migrate it losslessly into the same
        // nine hotbar slots and leave all new state empty/default.
        (
            Inventory::from_legacy_hotbar(&hotbar),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        )
    };

    // v1-v4 predate the calendar and migrate to day 0 with the default
    // season length. A v5 season length of 0 or beyond the sane upper bound
    // is a corrupt scalar: clamp it to the default rather than bricking the
    // save, mirroring the time_of_day clamp precedent. Fail-closed strictness
    // stays with the exact-length check below.
    let (day_index, season_length_days) = if version >= SEASON_VERSION {
        let day_index = read_u32(&mut cur)?;
        let raw_length = read_u32(&mut cur)?;
        let season_length_days = if (1..=MAX_SEASON_LENGTH_DAYS).contains(&raw_length) {
            raw_length
        } else {
            DEFAULT_SEASON_LENGTH_DAYS
        };
        (day_index, season_length_days)
    } else {
        (0, DEFAULT_SEASON_LENGTH_DAYS)
    };

    // v6 appends the survival tail after the calendar. v1-v5 migrate to the
    // Creative/full/empty baseline; only a v6 payload is allowed to claim a
    // mode or vitals. Every field is strictly validated because values
    // derived from an untrusted file must never enter the domain unchecked.
    let (mode, survival, chests) = if version >= SURVIVAL_VERSION {
        let mode_raw = read_u8(&mut cur)?;
        let mode =
            game_mode_from_tag(mode_raw).ok_or(PersistenceError::InvalidGameMode(mode_raw))?;
        let health = read_u16(&mut cur)?;
        let hunger = read_u16(&mut cur)?;
        let air = read_u16(&mut cur)?;
        let survival =
            SurvivalState::new(health, hunger, air).ok_or(PersistenceError::InvalidVitals {
                health,
                hunger,
                air,
            })?;
        let chest_count = read_u32(&mut cur)?;
        if chest_count as usize > MAX_SAVED_CHESTS {
            return Err(PersistenceError::InvalidChestState);
        }
        let mut chests: ChestMap = FxHashMap::default();
        for _ in 0..chest_count {
            let x = read_i32(&mut cur)?;
            let y = read_i32(&mut cur)?;
            let z = read_i32(&mut cur)?;
            let pos = IVec3::new(x, y, z);
            if chests.contains_key(&pos) {
                return Err(PersistenceError::InvalidChestState);
            }
            let mut slots: ChestSlots = [None; CHEST_SLOTS];
            for slot in slots.iter_mut() {
                *slot = read_item(&mut cur, version)?;
            }
            chests.insert(pos, Container::from_slots(slots));
        }
        (mode, survival, chests)
    } else {
        (
            GameMode::Creative,
            SurvivalState::full(),
            ChestMap::default(),
        )
    };

    // v7 appends the mob tail after the last chest record. v1-v6 decode to
    // zero mobs. Every record is strictly validated: a forged kind tag, a
    // non-finite coordinate/facing, health above the kind maximum, a
    // duplicate `(kind, position)`, or an out-of-bound count fails the whole
    // load closed. `health == 0` is legal (a mob that died in the same tick).
    let mobs = if version >= MOB_VERSION {
        let mob_count = read_u32(&mut cur)?;
        if mob_count as usize > MAX_SAVED_MOBS {
            return Err(PersistenceError::InvalidMobState);
        }
        let mut mobs: Vec<SavedMob> = Vec::with_capacity(mob_count as usize);
        for _ in 0..mob_count {
            let tag = read_u8(&mut cur)?;
            let Some(kind) = MobKind::from_tag(tag) else {
                return Err(PersistenceError::InvalidMobKind(tag));
            };
            let x = read_mob_f32(&mut cur)?;
            let y = read_mob_f32(&mut cur)?;
            let z = read_mob_f32(&mut cur)?;
            let yaw = read_mob_f32(&mut cur)?;
            let health = read_u16(&mut cur)?;
            if health > kind.max_health() {
                return Err(PersistenceError::InvalidMobState);
            }
            mobs.push(SavedMob {
                kind,
                position: [x, y, z],
                yaw,
                health,
            });
        }
        // Duplicate `(kind, position)` is ambiguous state, not a merge. Sort
        // a key copy so detection stays O(n log n) at the wire bound.
        let mut keys: Vec<(u8, f32, f32, f32)> = mobs
            .iter()
            .map(|mob| {
                (
                    mob.kind.tag(),
                    mob.position[0],
                    mob.position[1],
                    mob.position[2],
                )
            })
            .collect();
        keys.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.total_cmp(&b.1))
                .then_with(|| a.2.total_cmp(&b.2))
                .then_with(|| a.3.total_cmp(&b.3))
        });
        if keys.windows(2).any(|pair| {
            pair[0].0 == pair[1].0
                && pair[0].1 == pair[1].1
                && pair[0].2 == pair[1].2
                && pair[0].3 == pair[1].3
        }) {
            return Err(PersistenceError::InvalidMobState);
        }
        mobs
    } else {
        Vec::new()
    };

    // Exact-length check AFTER the newest known tail (now the v7 mob section)
    // so v5/v6 saves still load while trailing bytes still fail closed. v1-v3
    // never had the check (their payload is the whole file by construction).
    if version >= INVENTORY_EXTENSION_VERSION && cur.position() != bytes.len() as u64 {
        return Err(PersistenceError::InvalidPlayerState);
    }

    Ok(WorldSaveData {
        seed,
        time_of_day,
        day_index,
        season_length_days,
        mode,
        player: SavedPlayerState {
            position: [px, py, pz],
            yaw,
            pitch,
            fly_mode: fly_raw != 0,
            selected_slot,
            render_distance,
            inventory,
            player_grid,
            table_grid,
            survival,
        },
        chests,
        mobs,
        chunk_edits,
    })
}

/// Reads the magic and version word without decoding the body.
fn peek_save_version(bytes: &[u8]) -> Result<u32, PersistenceError> {
    let mut cur = Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic)?;
    if &magic != SAVE_MAGIC {
        return Err(PersistenceError::InvalidMagic);
    }
    read_u32(&mut cur)
}

/// Resolves v8 name indices against a cloned base registry, appending a
/// deterministic placeholder for each unknown-but-well-formed namespaced name.
struct V8Resolver<'a> {
    names: &'a [String],
    registry: ContentRegistry,
    unresolved: usize,
}

impl<'a> V8Resolver<'a> {
    fn new(names: &'a [String], base: &ContentRegistry) -> Self {
        Self {
            names,
            registry: base.clone(),
            unresolved: 0,
        }
    }

    fn name(&self, index: u16) -> Result<&'a str, PersistenceError> {
        self.names
            .get(usize::from(index))
            .map(String::as_str)
            .ok_or(PersistenceError::InvalidContentTable)
    }

    /// Whether `name` exists in the registry under a kind other than the one
    /// the caller is resolving.
    fn exists_under_other_kind(&self, name: &str) -> bool {
        self.registry.item_by_name(name).is_some()
            || self.registry.tool_by_name(name).is_some()
            || self.registry.tier_by_name(name).is_some()
            || self.registry.recipe_by_name(name).is_some()
    }

    fn block(&mut self, index: u16) -> Result<BlockId, PersistenceError> {
        let name = self.name(index)?;
        if let Some(block) = names::builtin_block_id_by_name(name) {
            if block.is_liquid() {
                return Err(PersistenceError::InvalidBlockId(block.to_raw()));
            }
            return Ok(block);
        }
        if names::builtin_item_id_by_name(name).is_some() || self.exists_under_other_kind(name) {
            return Err(PersistenceError::InvalidContentName(name.to_string()));
        }
        if let Some(def) = self.registry.block_by_name(name) {
            return Ok(def.id);
        }
        if is_placeholder_content_name(name) {
            let id = self.registry.append_placeholder_block(name);
            self.unresolved += 1;
            return Ok(id);
        }
        Err(PersistenceError::InvalidContentName(name.to_string()))
    }

    fn material(&mut self, index: u16) -> Result<Material, PersistenceError> {
        let name = self.name(index)?;
        names::builtin_material_by_name(name)
            .ok_or_else(|| PersistenceError::InvalidContentName(name.to_string()))
    }

    fn tool(&mut self, index: u16) -> Result<(ToolKind, ToolTier), PersistenceError> {
        let name = self.name(index)?;
        match names::builtin_tool_id_by_name(name) {
            Some(ItemId::Tool { kind, tier }) => Ok((kind, tier)),
            _ => Err(PersistenceError::InvalidContentName(name.to_string())),
        }
    }

    /// Resolves a v8 pack-tool record.
    ///
    /// A name present as a live pack tool resolves to its dense identity. A
    /// name that is absent (or only known through a placeholder item created
    /// by an earlier load) degrades to a count-one placeholder item, exactly
    /// like a generic item placeholder. Any other known kind is a forged
    /// cross-kind reference and fails closed.
    fn mod_tool(&mut self, index: u16) -> Result<ItemId, PersistenceError> {
        let name = self.name(index)?;
        if let Some(def) = self.registry.tool_by_name(name) {
            return Ok(ItemId::ModTool(def.index));
        }
        if let Some(def) = self.registry.item_by_name(name) {
            if def.placeholder {
                return Ok(ItemId::Item(def.index));
            }
            return Err(PersistenceError::InvalidContentName(name.to_string()));
        }
        if names::builtin_item_id_by_name(name).is_some()
            || self.registry.block_by_name(name).is_some()
            || self.registry.tier_by_name(name).is_some()
            || self.registry.recipe_by_name(name).is_some()
        {
            return Err(PersistenceError::InvalidContentName(name.to_string()));
        }
        if is_placeholder_content_name(name) {
            let item_index = self.registry.append_placeholder_item(name);
            self.unresolved += 1;
            return Ok(ItemId::Item(item_index));
        }
        Err(PersistenceError::InvalidContentName(name.to_string()))
    }

    /// Authored maximum durability of the pack tool at dense `index`.
    fn mod_tool_max_durability(&self, index: u16) -> Option<u16> {
        let def = self.registry.tool(index)?;
        let tier = self.registry.tier(def.tier)?;
        Some(tier.durability.max(1))
    }

    fn item(&mut self, index: u16) -> Result<ItemId, PersistenceError> {
        let name = self.name(index)?;
        if let Some(def) = self.registry.item_by_name(name) {
            return Ok(ItemId::Item(def.index));
        }
        if names::builtin_item_id_by_name(name).is_some()
            || self.registry.block_by_name(name).is_some()
            || self.exists_under_other_kind(name)
        {
            return Err(PersistenceError::InvalidContentName(name.to_string()));
        }
        if is_placeholder_content_name(name) {
            let item_index = self.registry.append_placeholder_item(name);
            self.unresolved += 1;
            return Ok(ItemId::Item(item_index));
        }
        Err(PersistenceError::InvalidContentName(name.to_string()))
    }
}

fn read_v8_item<R: Read>(
    r: &mut R,
    resolver: &mut V8Resolver<'_>,
) -> Result<Option<Item>, PersistenceError> {
    let tag = read_u8(r)?;
    match tag {
        ITEM_TAG_EMPTY => Ok(None),
        ITEM_TAG_BLOCK => {
            let index = read_u16(r)?;
            let count = read_u16(r)?;
            let block = resolver.block(index)?;
            Item::new(ItemId::Block(block), count)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_MATERIAL => {
            let index = read_u16(r)?;
            let count = read_u16(r)?;
            let material = resolver.material(index)?;
            Item::new(ItemId::Material(material), count)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_TOOL => {
            let index = read_u16(r)?;
            let durability = read_u16(r)?;
            let (kind, tier) = resolver.tool(index)?;
            Item::tool_with_durability(kind, tier, durability)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_GENERIC => {
            let index = read_u16(r)?;
            let count = read_u16(r)?;
            let id = resolver.item(index)?;
            Item::new(id, count)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_MOD_TOOL => {
            let index = read_u16(r)?;
            let durability = read_u16(r)?;
            if durability == 0 {
                return Err(PersistenceError::InvalidItem(ItemError::InvalidDurability));
            }
            let id = resolver.mod_tool(index)?;
            match id {
                ItemId::ModTool(tool_index) => {
                    // Persistence policy: a saved durability above the
                    // authored maximum clamps instead of bricking the world.
                    // `tool_with_durability_with` itself stays strict.
                    let max = resolver.mod_tool_max_durability(tool_index).ok_or(
                        PersistenceError::InvalidContentName(resolver.name(index)?.to_string()),
                    )?;
                    Item::tool_with_durability_with(&resolver.registry, id, durability.min(max))
                        .map(Some)
                        .map_err(PersistenceError::InvalidItem)
                }
                // A degraded pack tool becomes a count-one placeholder item;
                // its stale durability is intentionally not carried over.
                placeholder => Item::new_with(&resolver.registry, placeholder, 1)
                    .map(Some)
                    .map_err(PersistenceError::InvalidItem),
            }
        }
        other => Err(PersistenceError::InvalidItemTag(other)),
    }
}

fn read_v8_grid<R: Read>(
    r: &mut R,
    resolver: &mut V8Resolver<'_>,
    expected: GridKind,
) -> Result<CraftingGrid, PersistenceError> {
    let kind_raw = read_u8(r)?;
    let kind = grid_kind_from_tag(kind_raw).ok_or(PersistenceError::InvalidGridKind(kind_raw))?;
    if kind != expected {
        return Err(PersistenceError::InvalidCraftingState);
    }

    let mut slots = [None; MAX_GRID_SLOTS];
    for slot in slots.iter_mut().take(kind.slot_count()) {
        *slot = read_v8_item(r, resolver)?;
    }
    CraftingGrid::from_slots(kind, slots).map_err(|_| PersistenceError::InvalidCraftingState)
}

fn read_v8_inventory_extension<R: Read>(
    r: &mut R,
    resolver: &mut V8Resolver<'_>,
) -> Result<(Inventory, CraftingGrid, CraftingGrid), PersistenceError> {
    let mut slots = [None; INVENTORY_SLOTS];
    for slot in slots.iter_mut() {
        *slot = read_v8_item(r, resolver)?;
    }
    let cursor = read_v8_item(r, resolver)?;
    let player_grid = read_v8_grid(r, resolver, GridKind::Player)?;
    let table_grid = read_v8_grid(r, resolver, GridKind::Table)?;
    Ok((
        Inventory::from_parts(slots, cursor),
        player_grid,
        table_grid,
    ))
}

/// Decodes the v8 format into a placeholder-augmented registry.
///
/// The whole content table is validated before any reference is resolved, and
/// every count/length/index is bounds-checked. A forged payload fails closed
/// with a typed error; this decoder never panics.
fn decode_v8(bytes: &[u8], base: &ContentRegistry) -> Result<LoadedWorld, PersistenceError> {
    let mut cur = Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic)?;
    if &magic != SAVE_MAGIC {
        return Err(PersistenceError::InvalidMagic);
    }

    let version = read_u32(&mut cur)?;
    if version != CONTENT_TABLE_VERSION {
        return Err(PersistenceError::UnsupportedVersion(version));
    }

    let seed = read_u32(&mut cur)?;
    let px = read_f32(&mut cur)?;
    let py = read_f32(&mut cur)?;
    let pz = read_f32(&mut cur)?;
    let yaw = read_f32(&mut cur)?;
    let pitch = read_f32(&mut cur)?;
    let fly_raw = read_u8(&mut cur)?;
    let selected_slot = read_u8(&mut cur)?;
    let render_distance = read_u16(&mut cur)?;

    if selected_slot >= 9 || !(4..=36).contains(&render_distance) {
        return Err(PersistenceError::InvalidPlayerState);
    }

    // Content string table: validate fully before resolving any reference.
    let name_count_raw = read_u32(&mut cur)?;
    let name_count =
        usize::try_from(name_count_raw).map_err(|_| PersistenceError::InvalidContentTable)?;
    if name_count > MAX_SAVE_CONTENT_NAMES {
        return Err(PersistenceError::InvalidContentTable);
    }
    let mut names: Vec<String> = Vec::with_capacity(name_count);
    for _ in 0..name_count {
        let len_raw = read_u16(&mut cur)?;
        let len = usize::from(len_raw);
        if len == 0 || len > MAX_SAVE_CONTENT_NAME_LEN {
            return Err(PersistenceError::InvalidContentTable);
        }
        let mut buf = vec![0u8; len];
        cur.read_exact(&mut buf)?;
        let name = String::from_utf8(buf).map_err(|_| PersistenceError::InvalidContentTable)?;
        if let Some(previous) = names.last() {
            if previous.as_str() >= name.as_str() {
                return Err(PersistenceError::InvalidContentTable);
            }
        }
        if !is_wire_content_name(&name) {
            return Err(PersistenceError::InvalidContentName(name));
        }
        names.push(name);
    }

    let mut resolver = V8Resolver::new(&names, base);

    // The v2 hotbar header is an unchanged block-only projection. It is
    // validated for structural integrity; the v4 extension is authoritative
    // for every item, including modded blocks projected as empty here.
    let _hotbar = read_hotbar(&mut cur, version)?;

    let time_of_day = read_u32(&mut cur)?.min(DAY_TICKS - 1);

    let chunk_count_raw = read_u32(&mut cur)?;
    let chunk_count =
        usize::try_from(chunk_count_raw).map_err(|_| PersistenceError::InvalidPlayerState)?;
    if chunk_count > 1_000_000 {
        return Err(PersistenceError::InvalidPlayerState);
    }

    let mut chunk_edits: ChunkEditMap = FxHashMap::default();
    for _ in 0..chunk_count {
        let cx = read_i32(&mut cur)?;
        let cy = read_i32(&mut cur)?;
        let cz = read_i32(&mut cur)?;
        let edit_count_raw = read_u32(&mut cur)?;
        let edit_count =
            usize::try_from(edit_count_raw).map_err(|_| PersistenceError::InvalidPlayerState)?;
        if edit_count == 0 || edit_count > CHUNK_VOLUME {
            return Err(PersistenceError::InvalidPlayerState);
        }

        let mut edits = FxHashMap::default();
        for _ in 0..edit_count {
            let local_idx = read_u16(&mut cur)?;
            if usize::from(local_idx) >= CHUNK_VOLUME {
                return Err(PersistenceError::InvalidLocalIndex(local_idx));
            }
            let name_index = read_u16(&mut cur)?;
            let block = resolver.block(name_index)?;
            edits.insert(local_idx, block);
        }
        chunk_edits.insert(ChunkPos::new(cx, cy, cz), edits);
    }

    let (inventory, player_grid, table_grid) =
        read_v8_inventory_extension(&mut cur, &mut resolver)?;

    let day_index = read_u32(&mut cur)?;
    let raw_length = read_u32(&mut cur)?;
    let season_length_days = if (1..=MAX_SEASON_LENGTH_DAYS).contains(&raw_length) {
        raw_length
    } else {
        DEFAULT_SEASON_LENGTH_DAYS
    };

    let mode_raw = read_u8(&mut cur)?;
    let mode = game_mode_from_tag(mode_raw).ok_or(PersistenceError::InvalidGameMode(mode_raw))?;
    let health = read_u16(&mut cur)?;
    let hunger = read_u16(&mut cur)?;
    let air = read_u16(&mut cur)?;
    let survival =
        SurvivalState::new(health, hunger, air).ok_or(PersistenceError::InvalidVitals {
            health,
            hunger,
            air,
        })?;
    let chest_count_raw = read_u32(&mut cur)?;
    let chest_count =
        usize::try_from(chest_count_raw).map_err(|_| PersistenceError::InvalidChestState)?;
    if chest_count > MAX_SAVED_CHESTS {
        return Err(PersistenceError::InvalidChestState);
    }
    let mut chests: ChestMap = FxHashMap::default();
    for _ in 0..chest_count {
        let x = read_i32(&mut cur)?;
        let y = read_i32(&mut cur)?;
        let z = read_i32(&mut cur)?;
        let pos = IVec3::new(x, y, z);
        if chests.contains_key(&pos) {
            return Err(PersistenceError::InvalidChestState);
        }
        let mut slots: ChestSlots = [None; CHEST_SLOTS];
        for slot in slots.iter_mut() {
            *slot = read_v8_item(&mut cur, &mut resolver)?;
        }
        chests.insert(pos, Container::from_slots(slots));
    }

    let mob_count_raw = read_u32(&mut cur)?;
    let mob_count =
        usize::try_from(mob_count_raw).map_err(|_| PersistenceError::InvalidMobState)?;
    if mob_count > MAX_SAVED_MOBS {
        return Err(PersistenceError::InvalidMobState);
    }
    let mut mobs: Vec<SavedMob> = Vec::with_capacity(mob_count);
    for _ in 0..mob_count {
        let tag = read_u8(&mut cur)?;
        let Some(kind) = MobKind::from_tag(tag) else {
            return Err(PersistenceError::InvalidMobKind(tag));
        };
        let x = read_mob_f32(&mut cur)?;
        let y = read_mob_f32(&mut cur)?;
        let z = read_mob_f32(&mut cur)?;
        let mob_yaw = read_mob_f32(&mut cur)?;
        let mob_health = read_u16(&mut cur)?;
        if mob_health > kind.max_health() {
            return Err(PersistenceError::InvalidMobState);
        }
        mobs.push(SavedMob {
            kind,
            position: [x, y, z],
            yaw: mob_yaw,
            health: mob_health,
        });
    }
    let mut keys: Vec<(u8, f32, f32, f32)> = mobs
        .iter()
        .map(|mob| {
            (
                mob.kind.tag(),
                mob.position[0],
                mob.position[1],
                mob.position[2],
            )
        })
        .collect();
    keys.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.total_cmp(&b.2))
            .then_with(|| a.3.total_cmp(&b.3))
    });
    if keys.windows(2).any(|pair| {
        pair[0].0 == pair[1].0
            && pair[0].1 == pair[1].1
            && pair[0].2 == pair[1].2
            && pair[0].3 == pair[1].3
    }) {
        return Err(PersistenceError::InvalidMobState);
    }

    // v8 tail: pack identities sorted by folder, then the FNV-1a digest.
    let pack_count_raw = read_u32(&mut cur)?;
    let pack_count =
        usize::try_from(pack_count_raw).map_err(|_| PersistenceError::InvalidContentMeta)?;
    if pack_count > MAX_SAVED_PACKS {
        return Err(PersistenceError::InvalidContentMeta);
    }
    let mut packs: Vec<PackIdentity> = Vec::with_capacity(pack_count);
    for _ in 0..pack_count {
        let folder_len_raw = read_u16(&mut cur)?;
        let folder_len = usize::from(folder_len_raw);
        if folder_len == 0 || folder_len > MAX_SAVE_CONTENT_NAME_LEN {
            return Err(PersistenceError::InvalidContentMeta);
        }
        let mut folder_bytes = vec![0u8; folder_len];
        cur.read_exact(&mut folder_bytes)?;
        let folder =
            String::from_utf8(folder_bytes).map_err(|_| PersistenceError::InvalidContentMeta)?;
        let has_version = read_u8(&mut cur)?;
        let version = match has_version {
            0 => None,
            1 => {
                let version_len_raw = read_u16(&mut cur)?;
                let version_len = usize::from(version_len_raw);
                if version_len == 0 || version_len > MAX_SAVE_CONTENT_NAME_LEN {
                    return Err(PersistenceError::InvalidContentMeta);
                }
                let mut version_bytes = vec![0u8; version_len];
                cur.read_exact(&mut version_bytes)?;
                Some(
                    String::from_utf8(version_bytes)
                        .map_err(|_| PersistenceError::InvalidContentMeta)?,
                )
            }
            _ => return Err(PersistenceError::InvalidContentMeta),
        };
        if let Some(previous) = packs.last() {
            if previous.folder.as_str() >= folder.as_str() {
                return Err(PersistenceError::InvalidContentMeta);
            }
        }
        packs.push(PackIdentity { folder, version });
    }
    let digest = read_u64(&mut cur)?;

    let end = u64::try_from(bytes.len()).map_err(|_| PersistenceError::InvalidContentMeta)?;
    if cur.position() != end {
        return Err(PersistenceError::InvalidContentMeta);
    }

    let unresolved = resolver.unresolved;
    Ok(LoadedWorld {
        data: WorldSaveData {
            seed,
            time_of_day,
            day_index,
            season_length_days,
            mode,
            player: SavedPlayerState {
                position: [px, py, pz],
                yaw,
                pitch,
                fly_mode: fly_raw != 0,
                selected_slot,
                render_distance,
                inventory,
                player_grid,
                table_grid,
                survival,
            },
            chests,
            mobs,
            chunk_edits,
        },
        registry: resolver.registry,
        saved_content: Some(SaveContentMeta { packs, digest }),
        unresolved,
    })
}

/// Loads any supported save version, returning the placeholder-augmented
/// registry and v8 content metadata alongside the world data.
pub fn load_world_from_bytes_with(
    bytes: &[u8],
    registry: &ContentRegistry,
) -> Result<LoadedWorld, PersistenceError> {
    let version = peek_save_version(bytes)?;
    if !(1..=SAVE_VERSION).contains(&version) {
        return Err(PersistenceError::UnsupportedVersion(version));
    }
    if version >= CONTENT_TABLE_VERSION {
        decode_v8(bytes, registry)
    } else {
        Ok(LoadedWorld {
            data: decode_legacy(bytes)?,
            registry: registry.clone(),
            saved_content: None,
            unresolved: 0,
        })
    }
}

/// Loads any supported save version with the built-in registry.
///
/// v1-v7 payloads decode through the frozen legacy reader (no placeholders to
/// create); v8 payloads resolve against [`ContentRegistry::builtin`] and drop
/// the placeholder registry, since callers of this compatibility wrapper only
/// need the world data.
pub fn load_world_from_bytes(bytes: &[u8]) -> Result<WorldSaveData, PersistenceError> {
    let version = peek_save_version(bytes)?;
    if !(1..=SAVE_VERSION).contains(&version) {
        return Err(PersistenceError::UnsupportedVersion(version));
    }
    if version >= CONTENT_TABLE_VERSION {
        load_world_from_bytes_with(bytes, ContentRegistry::builtin()).map(|loaded| loaded.data)
    } else {
        decode_legacy(bytes)
    }
}

/// Reads and strictly validates the v2 hotbar header block.
///
/// A zero block id is the empty-slot sentinel and must pair with count 0.
/// Any other id must decode, be carryable (not Air/Bedrock), and carry a
/// count within `1..=MAX_STACK_COUNT`. Ids appended in v6 are rejected on a
/// pre-v6 payload, mirroring [`read_item`]. Short reads surface as `Io`.
fn read_hotbar<R: Read>(r: &mut R, version: u32) -> Result<Hotbar, PersistenceError> {
    let mut slots: [Option<ItemStack>; crate::player::inventory::HOTBAR_SLOTS] =
        [None; crate::player::inventory::HOTBAR_SLOTS];

    for slot in slots.iter_mut() {
        let block_raw = read_u8(r)?;
        let count = read_u16(r)?;

        if u16::from(block_raw) == BlockId::Air.to_raw() {
            if count != 0 {
                return Err(PersistenceError::InvalidPlayerState);
            }
            continue;
        }

        let Some(block) = BlockId::from_u8(block_raw) else {
            return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
        };
        if version < SURVIVAL_VERSION && is_survival_only_block_id(block) {
            return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
        }
        if matches!(block, BlockId::Air | BlockId::Bedrock | BlockId::Water) {
            return Err(PersistenceError::InvalidPlayerState);
        }
        let Some(stack) = ItemStack::new(block, count) else {
            return Err(PersistenceError::InvalidPlayerState);
        };
        *slot = Some(stack);
    }

    Ok(Hotbar::from_slots(slots))
}

/// Reads one validated item record from the v4 extension.
///
/// Every payload is bounded and fixed-size: a block/material record is
/// `tag + id + u16 count`, a tool record is `tag + kind + tier + u16
/// durability` (count is implicit one). All construction funnels through
/// [`Item`]'s checked constructors, so forged counts/durability never enter
/// the domain.
///
/// The Apple/Wheat/WheatSeeds/Bread materials, the Hoe tool kind, and block
/// ids 16..=22 are v6 wire identities. A pre-v6 payload that contains one is
/// corrupt (or forged), so it fails closed instead of being silently
/// accepted under a version that never could have written it.
fn read_item<R: Read>(r: &mut R, version: u32) -> Result<Option<Item>, PersistenceError> {
    let tag = read_u8(r)?;
    match tag {
        ITEM_TAG_EMPTY => Ok(None),
        ITEM_TAG_BLOCK => {
            let block_raw = read_u8(r)?;
            let count = read_u16(r)?;
            let Some(block) = BlockId::from_u8(block_raw) else {
                return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
            };
            if version < SURVIVAL_VERSION && is_survival_only_block_id(block) {
                return Err(PersistenceError::InvalidBlockId(u16::from(block_raw)));
            }
            Item::new(ItemId::Block(block), count)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_MATERIAL => {
            let material_raw = read_u8(r)?;
            let count = read_u16(r)?;
            if version < SURVIVAL_VERSION && is_survival_only_material_tag(material_raw) {
                return Err(PersistenceError::InvalidMaterial(material_raw));
            }
            if version < ITEM_VOCAB_VERSION && is_melee_only_material_tag(material_raw) {
                return Err(PersistenceError::InvalidMaterial(material_raw));
            }
            let material = material_from_tag(material_raw)
                .ok_or(PersistenceError::InvalidMaterial(material_raw))?;
            Item::new(ItemId::Material(material), count)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_TOOL => {
            let kind_raw = read_u8(r)?;
            let tier_raw = read_u8(r)?;
            let durability = read_u16(r)?;
            if version < SURVIVAL_VERSION && kind_raw == TOOL_KIND_HOE {
                return Err(PersistenceError::InvalidToolKind(kind_raw));
            }
            if version < ITEM_VOCAB_VERSION
                && (kind_raw == TOOL_KIND_SWORD || kind_raw == TOOL_KIND_BOW)
            {
                return Err(PersistenceError::InvalidToolKind(kind_raw));
            }
            let kind =
                tool_kind_from_tag(kind_raw).ok_or(PersistenceError::InvalidToolKind(kind_raw))?;
            let tier =
                tool_tier_from_tag(tier_raw).ok_or(PersistenceError::InvalidToolTier(tier_raw))?;
            Item::tool_with_durability(kind, tier, durability)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        other => Err(PersistenceError::InvalidItemTag(other)),
    }
}

/// Block ids appended in v6; a pre-v6 payload may not contain them.
/// Water (id 15) is a distinct, older constraint handled by `is_liquid`.
fn is_survival_only_block_id(block: BlockId) -> bool {
    matches!(
        block,
        BlockId::Farmland
            | BlockId::WheatStage0
            | BlockId::WheatStage1
            | BlockId::WheatStage2
            | BlockId::WheatStage3
            | BlockId::Chest
            | BlockId::LootChest
    )
}

/// Materials appended in v6; a pre-v6 payload may not contain them.
fn is_survival_only_material_tag(tag: u8) -> bool {
    matches!(
        tag,
        MATERIAL_APPLE | MATERIAL_WHEAT | MATERIAL_WHEAT_SEEDS | MATERIAL_BREAD
    )
}

/// Materials appended in v7 (M009); a pre-v7 payload may not contain them.
///
/// Deliberately separate from [`is_survival_only_material_tag`]: that one is
/// named for, and gated at, the v6 introduction and must not widen.
fn is_melee_only_material_tag(tag: u8) -> bool {
    matches!(
        tag,
        MATERIAL_LEATHER
            | MATERIAL_RAW_PORKCHOP
            | MATERIAL_RAW_BEEF
            | MATERIAL_ROTTEN_FLESH
            | MATERIAL_BONE
            | MATERIAL_ARROW
    )
}

fn read_grid<R: Read>(
    r: &mut R,
    version: u32,
    expected: GridKind,
) -> Result<CraftingGrid, PersistenceError> {
    let kind_raw = read_u8(r)?;
    let kind = grid_kind_from_tag(kind_raw).ok_or(PersistenceError::InvalidGridKind(kind_raw))?;
    if kind != expected {
        return Err(PersistenceError::InvalidCraftingState);
    }

    let mut slots = [None; MAX_GRID_SLOTS];
    for slot in slots.iter_mut().take(kind.slot_count()) {
        *slot = read_item(r, version)?;
    }
    CraftingGrid::from_slots(kind, slots).map_err(|_| PersistenceError::InvalidCraftingState)
}

fn read_inventory_extension<R: Read>(
    r: &mut R,
    version: u32,
) -> Result<(Inventory, CraftingGrid, CraftingGrid), PersistenceError> {
    let mut slots = [None; INVENTORY_SLOTS];
    for slot in slots.iter_mut() {
        *slot = read_item(r, version)?;
    }
    let cursor = read_item(r, version)?;
    let player_grid = read_grid(r, version, GridKind::Player)?;
    let table_grid = read_grid(r, version, GridKind::Table)?;
    Ok((
        Inventory::from_parts(slots, cursor),
        player_grid,
        table_grid,
    ))
}

/// Writes `bytes` atomically with the established tmp + rename sequence.
fn write_save_file(path: &Path, bytes: &[u8]) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let tmp_path = path.with_extension("mcvx.tmp");
    {
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Writes a v8 save using the built-in content registry, atomically.
pub fn save_world_to_path(path: &Path, data: &WorldSaveData) -> Result<(), PersistenceError> {
    save_world_to_path_with(path, data, ContentRegistry::builtin())
}

/// Writes a v8 save using `registry`, atomically.
pub fn save_world_to_path_with(
    path: &Path,
    data: &WorldSaveData,
    registry: &ContentRegistry,
) -> Result<(), PersistenceError> {
    let mut bytes = Vec::with_capacity(1024);
    save_world_to_bytes_with(&mut bytes, data, registry)?;
    write_save_file(path, &bytes)
}

pub fn load_world_from_path(path: &Path) -> Result<WorldSaveData, PersistenceError> {
    let bytes = fs::read(path)?;
    load_world_from_bytes(&bytes)
}

/// Loads any supported save version with `registry`, atomically from disk.
pub fn load_world_from_path_with(
    path: &Path,
    registry: &ContentRegistry,
) -> Result<LoadedWorld, PersistenceError> {
    let bytes = fs::read(path)?;
    load_world_from_bytes_with(&bytes, registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{MAIN_SLOTS, MAX_STACK_COUNT};
    use crate::player::survival::{MAX_AIR, MAX_HEALTH, MAX_HUNGER};
    use crate::world::block::{BlockId, BUILTIN_BLOCK_COUNT};
    use crate::world::chunk::{linear_index, ChunkPos};

    #[test]
    fn test_save_and_load_roundtrip() {
        let mut edits = ChunkEditMap::default();
        let mut c0 = rustc_hash::FxHashMap::default();
        c0.insert(linear_index(0, 5, 31) as u16, BlockId::Air);
        c0.insert(linear_index(12, 10, 4) as u16, BlockId::Brick);
        edits.insert(ChunkPos::new(-2, 0, 3), c0);
        edits.insert(ChunkPos::new(9, 1, 9), rustc_hash::FxHashMap::default());

        let data = WorldSaveData {
            seed: 424242,
            time_of_day: 4321,
            day_index: 3,
            season_length_days: 2,
            mode: GameMode::Creative,
            player: SavedPlayerState {
                position: [-12.5, 64.25, 108.0],
                yaw: 1.25,
                pitch: -0.4,
                fly_mode: false,
                selected_slot: 4,
                render_distance: 20,
                inventory: Inventory::empty(),
                player_grid: CraftingGrid::new(GridKind::Player),
                table_grid: CraftingGrid::new(GridKind::Table),
                survival: SurvivalState::full(),
            },
            chests: ChestMap::default(),
            mobs: Vec::new(),
            chunk_edits: edits,
        };

        let tmp_path = std::env::temp_dir().join("mcvx_test_roundtrip.mcvx");
        save_world_to_path(&tmp_path, &data).expect("save should succeed");
        let loaded = load_world_from_path(&tmp_path).expect("load should succeed");
        let _ = std::fs::remove_file(&tmp_path);

        assert_eq!(loaded.seed, 424242);
        assert_eq!(
            loaded.day_index, 3,
            "day index survives the file round-trip"
        );
        assert_eq!(
            loaded.season_length_days, 2,
            "season length survives the file round-trip"
        );
        assert_eq!(loaded.player, data.player);
        assert_eq!(loaded.chunk_edits.len(), 1);
        assert_eq!(
            loaded.chunk_edits[&ChunkPos::new(-2, 0, 3)][&(linear_index(12, 10, 4) as u16)],
            BlockId::Brick
        );
    }

    #[test]
    fn test_save_roundtrips_additive_ore_block_ids() {
        let coal_index = linear_index(1, 2, 3) as u16;
        let iron_index = linear_index(4, 5, 6) as u16;
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(coal_index, BlockId::CoalOre);
        chunk.insert(iron_index, BlockId::IronOre);
        let mut edits = ChunkEditMap::default();
        edits.insert(ChunkPos::new(-1, 2, 3), chunk);

        let data = WorldSaveData {
            seed: 1337,
            time_of_day: 12345,
            day_index: 0,
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            mode: GameMode::Creative,
            player: SavedPlayerState {
                position: [1.0, 2.0, 3.0],
                yaw: 0.0,
                pitch: 0.0,
                fly_mode: false,
                selected_slot: 0,
                render_distance: 16,
                inventory: Inventory::empty(),
                player_grid: CraftingGrid::new(GridKind::Player),
                table_grid: CraftingGrid::new(GridKind::Table),
                survival: SurvivalState::full(),
            },
            chests: ChestMap::default(),
            mobs: Vec::new(),
            chunk_edits: edits,
        };

        assert_eq!(SAVE_VERSION, CONTENT_TABLE_VERSION);
        assert_eq!(CONTENT_TABLE_VERSION, MOB_VERSION + 1);
        assert_eq!(MOB_VERSION, SURVIVAL_VERSION + 1);
        assert_eq!(SURVIVAL_VERSION, SEASON_VERSION + 1);
        assert_eq!(SEASON_VERSION, INVENTORY_EXTENSION_VERSION + 1);
        assert_eq!(LEGACY_WRITE_VERSION, 7);
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");
        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");

        let loaded_chunk = loaded
            .chunk_edits
            .get(&ChunkPos::new(-1, 2, 3))
            .expect("edited chunk must round-trip");
        assert_eq!(loaded_chunk[&coal_index], BlockId::CoalOre);
        assert_eq!(loaded_chunk[&iron_index], BlockId::IronOre);
        assert_eq!(loaded.seed, data.seed);
    }

    #[test]
    fn test_save_roundtrips_every_known_block_id_through_22() {
        // v6 accepts every decodable id except water (worldgen-only, refused
        // by the writer). Pin the full append-only span 0..=22 so a future
        // palette addition cannot silently shift an existing id.
        let mut chunk = rustc_hash::FxHashMap::default();
        let mut expected = Vec::new();
        for raw in 0u8..=22 {
            let block = BlockId::from_u8(raw).expect("id 0..=22 decodes");
            if block == BlockId::Water {
                continue;
            }
            let index = linear_index(raw as usize, 1, 1) as u16;
            chunk.insert(index, block);
            expected.push((index, block));
        }
        assert_eq!(chunk.len(), 22, "ids 0..=22 minus water");

        let mut edits = ChunkEditMap::default();
        edits.insert(ChunkPos::new(0, 0, 0), chunk);
        let mut data = sample_data(sample_player());
        data.chunk_edits = edits;
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v6 palette must save");
        let loaded = load_world_from_bytes(&bytes).expect("v6 palette must load");
        let loaded_chunk = loaded
            .chunk_edits
            .get(&ChunkPos::new(0, 0, 0))
            .expect("edited chunk must round-trip");
        assert_eq!(loaded_chunk.len(), expected.len());
        for (index, block) in expected {
            assert_eq!(
                loaded_chunk[&index],
                block,
                "{block:?} (id {}) must round-trip",
                block.to_raw() as u8
            );
        }
    }

    #[test]
    fn test_corrupted_save_rejected() {
        assert!(matches!(
            load_world_from_bytes(b"NOPE0000"),
            Err(PersistenceError::InvalidMagic | PersistenceError::Io(_))
        ));

        let mut edits = ChunkEditMap::default();
        let mut c0 = rustc_hash::FxHashMap::default();
        c0.insert(10, BlockId::Stone);
        edits.insert(ChunkPos::new(0, 0, 0), c0);
        let data = WorldSaveData {
            seed: 1,
            time_of_day: DEFAULT_TIME_OF_DAY,
            day_index: 0,
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            mode: GameMode::Creative,
            player: SavedPlayerState {
                position: [0.0, 50.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                fly_mode: true,
                selected_slot: 0,
                render_distance: 16,
                inventory: Inventory::empty(),
                player_grid: CraftingGrid::new(GridKind::Player),
                table_grid: CraftingGrid::new(GridKind::Table),
                survival: SurvivalState::full(),
            },
            chests: ChestMap::default(),
            mobs: Vec::new(),
            chunk_edits: edits,
        };
        // A v7 fixture keeps the legacy byte offsets: v8 inserts the content
        // table at 36 and stores the block as a name index instead of a byte.
        let mut valid = legacy_world_bytes(&data, 7);

        // One chunk, one edit: the block byte sits after chunk_count (71),
        // the 12-byte chunk position, edit_count, and the u16 local index.
        const FIRST_EDIT_INDEX_OFFSET: usize = 87;
        const FIRST_EDIT_BLOCK_OFFSET: usize = 89;

        valid[FIRST_EDIT_BLOCK_OFFSET] = 99;
        assert!(matches!(
            load_world_from_bytes(&valid),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        valid[FIRST_EDIT_BLOCK_OFFSET] = BlockId::Stone.to_raw() as u8;
        valid[FIRST_EDIT_INDEX_OFFSET..FIRST_EDIT_INDEX_OFFSET + 2]
            .copy_from_slice(&40000u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&valid),
            Err(PersistenceError::InvalidLocalIndex(40000))
        ));
    }

    #[test]
    fn test_water_chunk_edit_is_rejected_on_load_and_save() {
        // Load side: a well-formed save whose only edit byte is water must
        // fail closed even though `BlockId::from_u8(15)` now decodes. Saving
        // legit stone first keeps the payload byte-for-byte valid apart from
        // the patched block id.
        let mut edits = ChunkEditMap::default();
        let mut c0 = rustc_hash::FxHashMap::default();
        c0.insert(10, BlockId::Stone);
        edits.insert(ChunkPos::new(0, 0, 0), c0);
        let mut data = sample_data(sample_player());
        data.chunk_edits = edits;

        let mut bytes = legacy_world_bytes(&data, 7);
        // One chunk, one edit: the block byte follows chunk_count, the 12-byte
        // chunk position, edit_count, and the u16 local index.
        const FIRST_EDIT_BLOCK_OFFSET: usize = 89;
        assert_eq!(
            bytes[FIRST_EDIT_BLOCK_OFFSET],
            BlockId::Stone.to_raw() as u8
        );
        bytes[FIRST_EDIT_BLOCK_OFFSET] = BlockId::Water.to_raw() as u8;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidBlockId(15))
        ));

        // Save side: an in-memory water edit is refused before any byte is
        // written, so a worldgen leak cannot reach disk.
        let mut water_edits = ChunkEditMap::default();
        let mut w0 = rustc_hash::FxHashMap::default();
        w0.insert(10, BlockId::Water);
        water_edits.insert(ChunkPos::new(0, 0, 0), w0);
        let mut data = sample_data(sample_player());
        data.chunk_edits = water_edits;
        let mut out = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut out, &data),
            Err(PersistenceError::InvalidBlockId(15))
        ));
        assert!(out.is_empty(), "a rejected save must not emit bytes");
    }

    #[test]
    fn rejects_modded_chunk_edit_before_first_byte() {
        // A registry-local id has no v1-v7 wire identity: the writer must
        // reject it rather than truncate it into a built-in byte.
        let data = v6_data_with_block_edit(BlockId::Modded(BUILTIN_BLOCK_COUNT));
        let mut out = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut out, &data),
            Err(PersistenceError::InvalidBlockId(raw)) if raw == BUILTIN_BLOCK_COUNT
        ));
        assert!(out.is_empty(), "a rejected save must not emit bytes");
    }

    #[test]
    fn rejects_modded_inventory_block_before_first_byte() {
        // Block identity is also written by the v4 inventory extension; a
        // modded stack in slot 0 must fail before the header is emitted, not
        // halfway through the payload.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(
            Item::new(ItemId::Block(BlockId::Modded(BUILTIN_BLOCK_COUNT)), 1)
                .expect("a one-count modded block item is structurally valid"),
        );
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let mut out = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut out, &sample_data(player)),
            Err(PersistenceError::InvalidBlockId(raw)) if raw == BUILTIN_BLOCK_COUNT
        ));
        assert!(out.is_empty(), "a rejected save must not emit bytes");
    }

    #[test]
    fn rejects_modded_chest_block_before_first_byte() {
        // Chest slots are a third block-carrying section (v6 tail); the
        // pre-write scan must cover them too.
        let mut chest_slots: ChestSlots = [None; CHEST_SLOTS];
        chest_slots[0] = Some(
            Item::new(ItemId::Block(BlockId::Modded(BUILTIN_BLOCK_COUNT)), 1)
                .expect("a one-count modded block item is structurally valid"),
        );
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(1, 2, 3), Container::from_slots(chest_slots));
        let data = v6_data_with_chests(chests);
        let mut out = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut out, &data),
            Err(PersistenceError::InvalidBlockId(raw)) if raw == BUILTIN_BLOCK_COUNT
        ));
        assert!(out.is_empty(), "a rejected save must not emit bytes");
    }

    #[test]
    fn test_v2_hotbar_rejects_water() {
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Water.to_raw() as u8, 1);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    fn hotbar_sample() -> Hotbar {
        Hotbar::from_slots([
            Some(ItemStack::new(BlockId::CoalOre, 3).unwrap()),
            Some(ItemStack::new(BlockId::IronOre, 2).unwrap()),
            None,
            None,
            Some(ItemStack::new(BlockId::Stone, MAX_STACK_COUNT).unwrap()),
            None,
            None,
            None,
            Some(ItemStack::new(BlockId::Brick, 1).unwrap()),
        ])
    }

    fn sample_player() -> SavedPlayerState {
        SavedPlayerState {
            position: [1.0, 64.0, -2.0],
            yaw: 0.5,
            pitch: -0.25,
            fly_mode: true,
            selected_slot: 4,
            render_distance: 16,
            inventory: Inventory::empty(),
            player_grid: CraftingGrid::new(GridKind::Player),
            table_grid: CraftingGrid::new(GridKind::Table),
            survival: SurvivalState::full(),
        }
    }

    fn sample_data(player: SavedPlayerState) -> WorldSaveData {
        WorldSaveData {
            seed: 2024,
            time_of_day: DEFAULT_TIME_OF_DAY,
            day_index: 0,
            season_length_days: DEFAULT_SEASON_LENGTH_DAYS,
            mode: GameMode::Creative,
            player,
            chests: ChestMap::default(),
            mobs: Vec::new(),
            chunk_edits: ChunkEditMap::default(),
        }
    }

    /// v1-v6 predate the mob tail and all migrate to the same baseline.
    fn assert_v6_migration_defaults(loaded: &WorldSaveData) {
        assert_eq!(loaded.mode, GameMode::Creative);
        assert_eq!(loaded.player.survival, SurvivalState::full());
        assert!(loaded.chests.is_empty());
        assert!(loaded.mobs.is_empty());
    }

    #[test]
    fn test_legacy_hotbar_header_projects_block_hotbar_exactly() {
        let hotbar = hotbar_sample();
        let mut player = sample_player();
        player.inventory = Inventory::from_legacy_hotbar(&hotbar);
        let data = sample_data(player);
        let legacy = legacy_world_bytes(&data, 7);

        // Header layout: 36 bytes through render_distance, then 9 * 3
        // hotbar bytes, then the v3 time u32, then chunk_count (4 bytes),
        // then the v4 inventory extension. The sample has four block hotbar
        // slots, each expanding its 1-byte empty record to a 4-byte block
        // record (+3 bytes each).
        assert_eq!(
            legacy.len(),
            V4_EXTENSION_OFFSET
                + V4_EXTENSION_LEN
                + V5_SEASON_LEN
                + V6_HEADER_LEN
                + 4 * 3
                + V7_MOB_COUNT_LEN
        );
        assert_eq!(&legacy[4..8], &LEGACY_WRITE_VERSION.to_le_bytes());

        let loaded = load_world_from_bytes(&legacy).expect("legacy load should succeed");
        assert_eq!(loaded.player, data.player);
        assert_eq!(loaded.player.selected_slot, 4);
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(
                loaded.player.legacy_hotbar().stack(slot),
                hotbar.stack(slot),
                "slot {slot} must round-trip"
            );
        }

        // The v8 writer keeps the same block-only projection but streams it
        // after the (here empty) content table.
        let mut v8 = Vec::new();
        save_world_to_bytes(&mut v8, &data).expect("v8 save should succeed");
        assert_eq!(&v8[4..8], &SAVE_VERSION.to_le_bytes());
        let loaded_v8 = load_world_from_bytes(&v8).expect("v8 load should succeed");
        assert_eq!(loaded_v8.player, data.player);
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(
                loaded_v8.player.legacy_hotbar().stack(slot),
                hotbar.stack(slot),
                "v8 slot {slot} must round-trip"
            );
        }
    }

    #[test]
    fn test_write_rejects_mismatched_grid_kinds() {
        let mut player = sample_player();
        player.player_grid = CraftingGrid::new(GridKind::Table);
        let mut bytes = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut bytes, &sample_data(player)),
            Err(PersistenceError::InvalidCraftingState)
        ));
        assert!(bytes.is_empty(), "a rejected save must not emit bytes");

        let mut player = sample_player();
        player.table_grid = CraftingGrid::new(GridKind::Player);
        let mut bytes = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut bytes, &sample_data(player)),
            Err(PersistenceError::InvalidCraftingState)
        ));
        assert!(bytes.is_empty(), "a rejected save must not emit bytes");
    }

    /// Hand-build the old v1 layout (no 27-byte hotbar block).
    fn hand_built_v1_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SAVE_MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&777u32.to_le_bytes());
        for c in [-4.5f32, 70.25, 9.0] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&1.5f32.to_le_bytes()); // yaw
        bytes.extend_from_slice(&(-0.25f32).to_le_bytes()); // pitch
        bytes.push(1); // fly_mode
        bytes.push(6); // selected_slot
        bytes.extend_from_slice(&18u16.to_le_bytes()); // render_distance

        // One chunk with two edits.
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2i32.to_le_bytes());
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.extend_from_slice(&3i32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        let coal_index = linear_index(1, 2, 3) as u16;
        bytes.extend_from_slice(&coal_index.to_le_bytes());
        bytes.push(BlockId::CoalOre.to_raw() as u8);
        let iron_index = linear_index(4, 5, 6) as u16;
        bytes.extend_from_slice(&iron_index.to_le_bytes());
        bytes.push(BlockId::IronOre.to_raw() as u8);
        bytes
    }

    #[test]
    fn test_v1_save_migrates_to_empty_hotbar() {
        let bytes = hand_built_v1_bytes();
        let coal_index = linear_index(1, 2, 3) as u16;
        let iron_index = linear_index(4, 5, 6) as u16;

        let loaded = load_world_from_bytes(&bytes).expect("v1 must migrate, not fail");

        assert_eq!(loaded.seed, 777);
        assert_eq!(
            loaded.time_of_day, DEFAULT_TIME_OF_DAY,
            "v1 predates game time and migrates to noon"
        );
        assert_eq!(loaded.day_index, 0, "v1 predates the calendar");
        assert_eq!(
            loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS,
            "v1 predates season lengths"
        );
        assert_eq!(loaded.player.position, [-4.5, 70.25, 9.0]);
        assert_eq!(loaded.player.yaw, 1.5);
        assert_eq!(loaded.player.pitch, -0.25);
        assert!(loaded.player.fly_mode);
        assert_eq!(loaded.player.selected_slot, 6);
        assert_eq!(loaded.player.render_distance, 18);
        assert_eq!(loaded.player.inventory, Inventory::empty());
        assert!(loaded.player.player_grid.is_empty());
        assert!(loaded.player.table_grid.is_empty());
        assert_v6_migration_defaults(&loaded);

        let chunk = loaded
            .chunk_edits
            .get(&ChunkPos::new(2, -1, 3))
            .expect("v1 chunk edits must survive migration");
        assert_eq!(chunk[&coal_index], BlockId::CoalOre);
        assert_eq!(chunk[&iron_index], BlockId::IronOre);
    }

    /// Byte offset of the first hotbar slot in a v2/v3/v4 save.
    const V2_HOTBAR_OFFSET: usize = 36;
    /// Byte offset of the v3 time-of-day u32 (after magic, version, seed,
    /// 12 position, 4 yaw, 4 pitch, fly, slot, render_distance, 27 hotbar).
    const V3_TIME_OFFSET: usize = 63;
    /// Byte offset of the v4 inventory extension with no chunk edits
    /// (36 header + 27 hotbar + 4 time + 4 chunk_count).
    const V4_EXTENSION_OFFSET: usize = 71;
    /// Extension size for an empty inventory: 36 slot records + cursor +
    /// player grid (kind + 4 cells) + table grid (kind + 16 cells).
    const V4_EXTENSION_LEN: usize = INVENTORY_SLOTS + 1 + (1 + 4) + (1 + 16);
    /// Byte offset of the v5 calendar tail with no chunk edits: right after
    /// the v4 extension ends (71 + 59 = 130).
    const V5_SEASON_OFFSET: usize = V4_EXTENSION_OFFSET + V4_EXTENSION_LEN;
    /// Calendar tail size: `day_index` u32 + `season_length_days` u32.
    const V5_SEASON_LEN: usize = 8;
    /// v6 survival tail size: mode u8 + health u16 + hunger u16 + air u16 +
    /// chest_count u32.
    const V6_HEADER_LEN: usize = 11;
    /// Byte offset of the v6 mode byte with no chunk edits (130 + 8 = 138).
    const V6_MODE_OFFSET: usize = V5_SEASON_OFFSET + V5_SEASON_LEN;
    const V6_HEALTH_OFFSET: usize = V6_MODE_OFFSET + 1;
    const V6_HUNGER_OFFSET: usize = V6_HEALTH_OFFSET + 2;
    const V6_AIR_OFFSET: usize = V6_HUNGER_OFFSET + 2;
    const V6_CHEST_COUNT_OFFSET: usize = V6_AIR_OFFSET + 2;
    /// First chest record: x/y/z i32 LE, then 27 item records.
    const V6_FIRST_CHEST_OFFSET: usize = V6_CHEST_COUNT_OFFSET + 4;
    /// First item record inside the first chest (after x/y/z).
    const V6_FIRST_CHEST_SLOT_OFFSET: usize = V6_FIRST_CHEST_OFFSET + 12;
    /// Byte offset of the v7 mob count with no chunk edits and no chests:
    /// immediately after the empty v6 chest section (149).
    const V7_MOB_COUNT_OFFSET: usize = V6_FIRST_CHEST_OFFSET;
    /// v7 tail prefix: the `u32` LE mob count.
    const V7_MOB_COUNT_LEN: usize = 4;
    /// First v7 mob record (153 for an edit-free save); each record is
    /// [`SAVED_MOB_LEN`] bytes.
    const V7_FIRST_MOB_OFFSET: usize = V7_MOB_COUNT_OFFSET + V7_MOB_COUNT_LEN;

    // v8 empty-table layout. The content table starts where the legacy
    // hotbar used to: immediately after `render_distance` (36). An edit-free
    // save with no content references writes only the `u32` name count, so
    // every older offset shifts by exactly four bytes.
    /// Empty content table: just the `u32` name count (36..40).
    const V8_CONTENT_TABLE_LEN: usize = 4;
    /// v8 tail after the mob section: `u32` pack count + `u64` FNV-1a digest.
    const V8_TAIL_LEN: usize = 12;
    const V8_HOTBAR_OFFSET: usize = V2_HOTBAR_OFFSET + V8_CONTENT_TABLE_LEN;
    const V8_TIME_OFFSET: usize = V8_HOTBAR_OFFSET + HOTBAR_SLOTS * 3;
    const V8_CHUNK_COUNT_OFFSET: usize = V8_TIME_OFFSET + 4;
    const V8_EXTENSION_OFFSET: usize = V8_CHUNK_COUNT_OFFSET + 4;
    const V8_SEASON_OFFSET: usize = V8_EXTENSION_OFFSET + V4_EXTENSION_LEN;
    const V8_MODE_OFFSET: usize = V8_SEASON_OFFSET + V5_SEASON_LEN;
    const V8_HEALTH_OFFSET: usize = V8_MODE_OFFSET + 1;
    const V8_HUNGER_OFFSET: usize = V8_HEALTH_OFFSET + 2;
    const V8_AIR_OFFSET: usize = V8_HUNGER_OFFSET + 2;
    const V8_CHEST_COUNT_OFFSET: usize = V8_AIR_OFFSET + 2;
    /// First chest record with no edits; also the v8 mob count for an
    /// empty-table, chest-free save (153).
    const V8_MOB_COUNT_OFFSET: usize = V8_CHEST_COUNT_OFFSET + 4;
    const V8_TAIL_OFFSET: usize = V8_MOB_COUNT_OFFSET + V7_MOB_COUNT_LEN;
    /// Measured length of an edit-free, chest-free, mob-free v8 save with an
    /// empty content table (36 header + 4 table + everything older + tail).
    const V8_EMPTY_SAVE_LEN: usize = V8_TAIL_OFFSET + V8_TAIL_LEN;

    /// Builds a byte-valid v4-v7 fixture with the frozen legacy encoder.
    ///
    /// The live writer emits v8, whose wider item vocabulary and header
    /// content table make "truncate the current payload" invalid. This helper
    /// always encodes the full v7 layout first, then drops the newer tails and
    /// patches the version word.
    fn legacy_world_bytes(data: &WorldSaveData, version: u32) -> Vec<u8> {
        assert!((4..=7).contains(&version), "legacy fixtures cover v4..=v7");
        let mut bytes = Vec::new();
        write_legacy_world(&mut bytes, data).expect("legacy fixture encoding");

        let mob_tail = V7_MOB_COUNT_LEN + SAVED_MOB_LEN * data.mobs.len();
        if version <= 6 {
            bytes.truncate(bytes.len() - mob_tail);
        }
        if version <= 5 {
            assert!(data.chests.is_empty(), "v{version} has no chest section");
            bytes.truncate(bytes.len() - V6_HEADER_LEN);
        }
        if version <= 4 {
            bytes.truncate(bytes.len() - V5_SEASON_LEN);
        }
        bytes[4..8].copy_from_slice(&version.to_le_bytes());
        bytes
    }

    fn set_v2_slot(bytes: &mut [u8], slot: usize, block: u8, count: u16) {
        let off = V2_HOTBAR_OFFSET + slot * 3;
        bytes[off] = block;
        bytes[off + 1..off + 3].copy_from_slice(&count.to_le_bytes());
    }

    fn hand_built_v3_bytes(hotbar: &Hotbar) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SAVE_MAGIC);
        bytes.extend_from_slice(&TIME_OF_DAY_VERSION.to_le_bytes());
        bytes.extend_from_slice(&2024u32.to_le_bytes());
        for c in [1.0f32, 64.0, -2.0] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&0.5f32.to_le_bytes()); // yaw
        bytes.extend_from_slice(&(-0.25f32).to_le_bytes()); // pitch
        bytes.push(1); // fly_mode
        bytes.push(4); // selected_slot
        bytes.extend_from_slice(&16u16.to_le_bytes()); // render_distance
        for stack in hotbar.stacks() {
            match stack {
                Some(stack) => {
                    bytes.push(stack.block.to_raw() as u8);
                    bytes.extend_from_slice(&stack.count.to_le_bytes());
                }
                None => {
                    bytes.push(0);
                    bytes.extend_from_slice(&0u16.to_le_bytes());
                }
            }
        }
        bytes.extend_from_slice(&4321u32.to_le_bytes()); // time_of_day
        bytes.extend_from_slice(&0u32.to_le_bytes()); // chunk_count
        bytes
    }

    fn valid_v3_bytes() -> Vec<u8> {
        hand_built_v3_bytes(&Hotbar::empty())
    }

    /// Hand-build a real v2 save: hotbar header present, no time-of-day.
    fn valid_v2_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SAVE_MAGIC);
        bytes.extend_from_slice(&HOTBAR_HEADER_VERSION.to_le_bytes());
        bytes.extend_from_slice(&2024u32.to_le_bytes());
        for c in [1.0f32, 64.0, -2.0] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&0.5f32.to_le_bytes()); // yaw
        bytes.extend_from_slice(&(-0.25f32).to_le_bytes()); // pitch
        bytes.push(1); // fly_mode
        bytes.push(4); // selected_slot
        bytes.extend_from_slice(&16u16.to_le_bytes()); // render_distance
        for _ in 0..crate::player::inventory::HOTBAR_SLOTS {
            bytes.push(0); // empty slot sentinel
            bytes.extend_from_slice(&0u16.to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes()); // chunk_count
        bytes
    }

    #[test]
    fn test_v2_save_migrates_to_default_time_of_day() {
        let loaded = load_world_from_bytes(&valid_v2_bytes()).expect("v2 must migrate, not fail");
        assert_eq!(loaded.player.inventory, Inventory::empty());
        assert!(loaded.player.player_grid.is_empty());
        assert!(loaded.player.table_grid.is_empty());
        assert_v6_migration_defaults(&loaded);
        assert_eq!(
            loaded.time_of_day, DEFAULT_TIME_OF_DAY,
            "v2 predates game time and migrates to noon"
        );
        assert_eq!(loaded.day_index, 0, "v2 predates the calendar");
        assert_eq!(
            loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS,
            "v2 predates season lengths"
        );
    }

    #[test]
    fn test_v3_save_migrates_hotbar_into_inventory_and_defaults_new_state() {
        let hotbar = hotbar_sample();
        let bytes = hand_built_v3_bytes(&hotbar);
        let loaded = load_world_from_bytes(&bytes).expect("v3 must migrate, not fail");

        assert_eq!(loaded.time_of_day, 4321);
        assert_eq!(loaded.day_index, 0, "v3 predates the calendar");
        assert_eq!(
            loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS,
            "v3 predates season lengths"
        );
        assert_v6_migration_defaults(&loaded);
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(
                loaded
                    .player
                    .inventory
                    .hotbar(slot)
                    .and_then(|item| item.block_stack()),
                hotbar.stack(slot),
                "legacy hotbar slot {slot} must migrate losslessly"
            );
        }
        for slot in 0..MAIN_SLOTS {
            assert!(
                loaded.player.inventory.main(slot).is_none(),
                "v3 main slot {slot} must default to empty"
            );
        }
        assert!(loaded.player.inventory.cursor().is_none());
        assert!(loaded.player.player_grid.is_empty());
        assert!(loaded.player.table_grid.is_empty());
        assert_eq!(loaded.player.player_grid.kind(), GridKind::Player);
        assert_eq!(loaded.player.table_grid.kind(), GridKind::Table);
    }

    #[test]
    fn test_v3_time_of_day_roundtrips_exactly() {
        let mut data = sample_data(sample_player());
        data.time_of_day = 12_345;
        // The legacy layout assertion reads a v7 fixture; v8 inserts the
        // content table before the hotbar.
        let legacy = legacy_world_bytes(&data, 7);
        assert_eq!(
            &legacy[V3_TIME_OFFSET..V3_TIME_OFFSET + 4],
            &12_345u32.to_le_bytes(),
            "time-of-day must sit immediately after the hotbar block"
        );

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v8 save should succeed");
        assert_eq!(
            &bytes[V8_TIME_OFFSET..V8_TIME_OFFSET + 4],
            &12_345u32.to_le_bytes(),
            "v8 time-of-day follows the empty content table and hotbar"
        );
        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.time_of_day, 12_345);
        assert_eq!(loaded.time_of_day, data.time_of_day);
    }

    #[test]
    fn test_v3_out_of_range_time_is_clamped_on_load() {
        for garbage in [DAY_TICKS, DAY_TICKS + 1, u32::MAX] {
            let mut bytes = valid_v3_bytes();
            bytes[V3_TIME_OFFSET..V3_TIME_OFFSET + 4].copy_from_slice(&garbage.to_le_bytes());
            let loaded = load_world_from_bytes(&bytes).expect("clamp, not error");
            assert_eq!(
                loaded.time_of_day,
                DAY_TICKS - 1,
                "garbage time {garbage} must clamp into the cycle"
            );
        }
    }

    #[test]
    fn test_v3_truncated_time_field_is_typed_io_error() {
        let bytes = valid_v3_bytes();
        // Two time bytes present, two missing: the u32 read hits EOF.
        let truncated = &bytes[..V3_TIME_OFFSET + 2];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));
    }

    #[test]
    fn test_v2_hotbar_rejects_invalid_slots() {
        // Empty sentinel must carry count 0.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Air.to_raw() as u8, 5);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // A non-empty block id may not carry count 0.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Grass.to_raw() as u8, 0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // Bedrock cannot be carried.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Bedrock.to_raw() as u8, 1);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // Unknown ids surface as InvalidBlockId.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, 99, 1);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        // Counts above MAX_STACK_COUNT are invalid.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(
            &mut bytes,
            0,
            BlockId::CoalOre.to_raw() as u8,
            MAX_STACK_COUNT + 1,
        );
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    #[test]
    fn test_v3_hotbar_truncated_and_unsupported_version() {
        let bytes = valid_v3_bytes();
        // Cut mid-hotbar (after slot 1's block byte) so the count read hits EOF.
        let truncated = &bytes[..V2_HOTBAR_OFFSET + 4];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));

        let mut future = valid_v3_bytes();
        future[4..8].copy_from_slice(&(SAVE_VERSION + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&future),
            Err(PersistenceError::UnsupportedVersion(version)) if version == SAVE_VERSION + 1
        ));
    }

    fn rich_player() -> SavedPlayerState {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Block(BlockId::CoalOre), 37).unwrap());
        slots[1] =
            Some(Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 137).unwrap());
        slots[2] = Some(Item::new(ItemId::Material(Material::Stick), 12).unwrap());
        slots[HOTBAR_SLOTS] = Some(Item::new(ItemId::Block(BlockId::OakPlanks), 5).unwrap());
        let cursor = Some(Item::tool_with_durability(ToolKind::Axe, ToolTier::Stone, 1).unwrap());

        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, cursor);
        player
            .player_grid
            .set(
                0,
                Some(Item::new(ItemId::Block(BlockId::OakPlanks), 2).unwrap()),
            )
            .unwrap();
        player
            .player_grid
            .set(
                3,
                Some(Item::new(ItemId::Material(Material::Stick), 1).unwrap()),
            )
            .unwrap();
        player
            .table_grid
            .set(
                0,
                Some(Item::new(ItemId::Block(BlockId::OakLog), 1).unwrap()),
            )
            .unwrap();
        player
            .table_grid
            .set(
                4,
                Some(Item::new(ItemId::Material(Material::Stick), 2).unwrap()),
            )
            .unwrap();
        // A cell above the old 3x3 limit proves the 4x4 active region is what
        // gets persisted, not just the first nine slots.
        player
            .table_grid
            .set(
                15,
                Some(Item::new(ItemId::Block(BlockId::OakPlanks), 1).unwrap()),
            )
            .unwrap();
        player
    }

    #[test]
    fn test_v4_roundtrips_tool_durability_cursor_and_unfinished_grids() {
        let data = sample_data(rich_player());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");

        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.player, data.player);

        let tool = loaded.player.inventory.hotbar(1).expect("hotbar tool");
        assert_eq!(tool.durability(), Some(137));
        assert_eq!(
            tool.id(),
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron
            }
        );
        assert_eq!(
            loaded
                .player
                .inventory
                .cursor()
                .and_then(|item| item.durability()),
            Some(1),
            "cursor tool durability must survive exactly"
        );
        assert_eq!(
            loaded.player.inventory.main(0).map(|item| item.count()),
            Some(5)
        );
        assert!(!loaded.player.player_grid.is_empty());
        assert!(!loaded.player.table_grid.is_empty());
        assert_eq!(loaded.player.player_grid.kind(), GridKind::Player);
        assert_eq!(loaded.player.table_grid.kind(), GridKind::Table);
        assert_eq!(
            loaded.player.table_grid.slot(15).map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakPlanks)),
            "4x4 table cell above index 8 must survive load"
        );

        // The legacy block-only header cannot express the tool: it stays the
        // empty sentinel while the v4 extension is authoritative. Read the
        // legacy fixture because v8 shifts the hotbar behind a non-empty
        // content table (this player holds coal, planks and tools).
        let legacy = legacy_world_bytes(&data, 7);
        let header_slot = V2_HOTBAR_OFFSET + 3;
        assert_eq!(legacy[header_slot], BlockId::Air.to_raw() as u8);
        assert_eq!(
            &legacy[header_slot + 1..header_slot + 3],
            &0u16.to_le_bytes()
        );

        // Exact byte stability: load -> save reproduces the same file.
        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded).expect("re-save should succeed");
        assert_eq!(again, bytes);
    }

    #[test]
    fn test_v4_truncated_extension_is_typed_io_error() {
        let data = sample_data(rich_player());
        let bytes = legacy_world_bytes(&data, 4);

        // Cut inside the slot records.
        let truncated = &bytes[..V4_EXTENSION_OFFSET + 5];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));

        // Drop the final table-grid cell.
        let truncated = &bytes[..V5_SEASON_OFFSET - 1];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));
    }

    /// Build a byte-valid v4 save with the frozen legacy encoder.
    fn v4_bytes(data: &WorldSaveData) -> Vec<u8> {
        legacy_world_bytes(data, 4)
    }

    /// Build a byte-valid v5 save with the frozen legacy encoder.
    fn v5_bytes(data: &WorldSaveData) -> Vec<u8> {
        legacy_world_bytes(data, 5)
    }

    /// Build a byte-valid v6 save with the frozen legacy encoder.
    fn v6_bytes(data: &WorldSaveData) -> Vec<u8> {
        legacy_world_bytes(data, 6)
    }

    #[test]
    fn test_v5_roundtrips_day_index_and_season_length() {
        // Pin the absolute layout the smoke script reads: the tail starts
        // immediately after the fixed v4 extension.
        assert_eq!(V5_SEASON_OFFSET, 130);
        assert_eq!(V5_SEASON_LEN, 8);

        let mut data = sample_data(sample_player());
        data.day_index = 17;
        data.season_length_days = 4;

        // The absolute legacy layout is pinned on a v5 fixture; v8 inserts the
        // content table before the hotbar and appends its own tail.
        let legacy = legacy_world_bytes(&data, 5);
        assert_eq!(
            &legacy[V5_SEASON_OFFSET..V5_SEASON_OFFSET + 4],
            &17u32.to_le_bytes(),
            "day_index sits immediately after the v4 extension"
        );
        assert_eq!(
            &legacy[V5_SEASON_OFFSET + 4..V5_SEASON_OFFSET + V5_SEASON_LEN],
            &4u32.to_le_bytes(),
            "season_length_days follows day_index"
        );

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v8 save should succeed");
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());
        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.day_index, 17);
        assert_eq!(loaded.season_length_days, 4);
        assert_eq!(loaded, data);

        // Unbounded day index: u32::MAX is a valid scalar, not corruption.
        let mut extreme = sample_data(sample_player());
        extreme.day_index = u32::MAX;
        let mut extreme_bytes = Vec::new();
        save_world_to_bytes(&mut extreme_bytes, &extreme).unwrap();
        let loaded = load_world_from_bytes(&extreme_bytes).expect("day index must be unbounded");
        assert_eq!(loaded.day_index, u32::MAX);
    }

    #[test]
    fn test_v4_save_migrates_to_day_zero_and_default_season_length() {
        let mut data = sample_data(sample_player());
        data.day_index = 99;
        data.season_length_days = 8;
        let bytes = v4_bytes(&data);

        let loaded = load_world_from_bytes(&bytes).expect("v4 must migrate, not fail");
        assert_eq!(
            loaded.day_index, 0,
            "v4 predates the calendar and migrates to day 0"
        );
        assert_eq!(
            loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS,
            "v4 predates season lengths and migrates to the default"
        );
        assert_v6_migration_defaults(&loaded);
    }

    #[test]
    fn test_v4_rejects_trailing_bytes() {
        let mut bytes = v4_bytes(&sample_data(sample_player()));
        bytes.push(0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    #[test]
    fn test_v5_rejects_trailing_bytes() {
        let data = sample_data(sample_player());
        let mut bytes = legacy_world_bytes(&data, 5);
        bytes.push(0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    #[test]
    fn test_v5_truncated_calendar_tail_is_typed_io_error() {
        let data = sample_data(sample_player());
        let bytes = legacy_world_bytes(&data, 5);

        // Drop the final season-length byte entirely.
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));

        // Drop the whole season-length field but keep day_index.
        let truncated = &bytes[..V5_SEASON_OFFSET + 4];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));
    }

    #[test]
    fn test_v5_out_of_range_season_length_clamps_to_default() {
        for garbage in [0u32, MAX_SEASON_LENGTH_DAYS + 1, u32::MAX] {
            let data = sample_data(sample_player());
            let mut bytes = legacy_world_bytes(&data, 5);
            bytes[V5_SEASON_OFFSET + 4..V5_SEASON_OFFSET + V5_SEASON_LEN]
                .copy_from_slice(&garbage.to_le_bytes());
            let loaded = load_world_from_bytes(&bytes).expect("clamp, not error");
            assert_eq!(
                loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS,
                "garbage season length {garbage} must clamp to the default"
            );
        }

        // The v8 writer normalises the same way, keeping round-trips
        // byte-stable.
        let mut data = sample_data(sample_player());
        data.season_length_days = 0;
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS);
    }

    fn data_with_slot0(item: Item) -> WorldSaveData {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(item);
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        sample_data(player)
    }

    fn v4_bytes_with_slot0(item: Item) -> Vec<u8> {
        legacy_world_bytes(&data_with_slot0(item), 4)
    }

    // ------------------------------------------------------------------
    // v6 survival tail: mode, vitals, chests
    // ------------------------------------------------------------------

    fn survival_chest() -> Container {
        let mut chest: ChestSlots = [None; CHEST_SLOTS];
        chest[0] = Some(Item::new(ItemId::Material(Material::Bread), 3).unwrap());
        chest[26] = Some(Item::new(ItemId::Material(Material::Apple), 2).unwrap());
        Container::from_slots(chest)
    }

    fn v6_data_with_chests(chests: ChestMap) -> WorldSaveData {
        let mut data = sample_data(sample_player());
        data.mode = GameMode::Survival;
        data.player.fly_mode = false;
        data.player.survival = SurvivalState::new(7, 3, 12).unwrap();
        data.chests = chests;
        data
    }

    fn v6_bytes_with_chest(chest: Container) -> Vec<u8> {
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(1, 2, 3), chest);
        let data = v6_data_with_chests(chests);
        legacy_world_bytes(&data, 6)
    }

    fn v6_bytes_with_one_empty_chest() -> Vec<u8> {
        let bytes = v6_bytes_with_chest(Container::empty());
        assert_eq!(
            bytes.len(),
            V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS,
            "v6 writes one all-empty chest as a fixed 12 + 27 byte record"
        );
        bytes
    }

    #[test]
    fn test_v6_roundtrips_mode_vitals_and_chests_byte_stably() {
        // Pin the absolute layout: v6 is appended after the v5 calendar, so
        // the offsets the season smokes read never move.
        assert_eq!(V6_MODE_OFFSET, 138);
        assert_eq!(V6_HEALTH_OFFSET, 139);
        assert_eq!(V6_HUNGER_OFFSET, 141);
        assert_eq!(V6_AIR_OFFSET, 143);
        assert_eq!(V6_CHEST_COUNT_OFFSET, 145);
        assert_eq!(V6_FIRST_CHEST_OFFSET, 149);

        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(4, -2, 7), survival_chest());
        chests.insert(IVec3::new(-1, 0, 3), Container::empty());
        let data = v6_data_with_chests(chests);

        let bytes = legacy_world_bytes(&data, 6);

        assert_eq!(bytes[V6_MODE_OFFSET], GAME_MODE_SURVIVAL);
        assert_eq!(
            &bytes[V6_HEALTH_OFFSET..V6_HEALTH_OFFSET + 2],
            &7u16.to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_HUNGER_OFFSET..V6_HUNGER_OFFSET + 2],
            &3u16.to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_AIR_OFFSET..V6_AIR_OFFSET + 2],
            &12u16.to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_CHEST_COUNT_OFFSET..V6_CHEST_COUNT_OFFSET + 4],
            &2u32.to_le_bytes()
        );

        // Records are sorted by (x, y, z): (-1, 0, 3) comes first and is the
        // all-empty chest, so its 27 slot records are all empty tags.
        assert_eq!(
            &bytes[V6_FIRST_CHEST_OFFSET..V6_FIRST_CHEST_OFFSET + 4],
            &(-1i32).to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_FIRST_CHEST_OFFSET + 4..V6_FIRST_CHEST_OFFSET + 8],
            &0i32.to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_FIRST_CHEST_OFFSET + 8..V6_FIRST_CHEST_OFFSET + 12],
            &3i32.to_le_bytes()
        );
        assert!(
            bytes[V6_FIRST_CHEST_SLOT_OFFSET..V6_FIRST_CHEST_SLOT_OFFSET + CHEST_SLOTS]
                .iter()
                .all(|&tag| tag == ITEM_TAG_EMPTY),
            "an all-empty chest is written as-is, not dropped"
        );

        // Second record: (4, -2, 7) with Bread in slot 0 and Apple in slot 26.
        const SECOND_CHEST_OFFSET: usize = V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS;
        assert_eq!(
            &bytes[SECOND_CHEST_OFFSET..SECOND_CHEST_OFFSET + 4],
            &4i32.to_le_bytes()
        );
        assert_eq!(
            &bytes[SECOND_CHEST_OFFSET + 4..SECOND_CHEST_OFFSET + 8],
            &(-2i32).to_le_bytes()
        );
        assert_eq!(
            &bytes[SECOND_CHEST_OFFSET + 8..SECOND_CHEST_OFFSET + 12],
            &7i32.to_le_bytes()
        );
        let slot0 = SECOND_CHEST_OFFSET + 12;
        assert_eq!(bytes[slot0], ITEM_TAG_MATERIAL);
        assert_eq!(bytes[slot0 + 1], MATERIAL_BREAD);
        assert_eq!(&bytes[slot0 + 2..slot0 + 4], &3u16.to_le_bytes());
        let slot26 = slot0 + 4 + 25;
        assert_eq!(bytes[slot26], ITEM_TAG_MATERIAL);
        assert_eq!(bytes[slot26 + 1], MATERIAL_APPLE);
        assert_eq!(&bytes[slot26 + 2..slot26 + 4], &2u16.to_le_bytes());

        let loaded = load_world_from_bytes(&bytes).expect("v6 must load");
        assert_eq!(loaded, data);
        assert_eq!(loaded.mode, GameMode::Survival);
        assert_eq!(
            loaded.player.survival,
            SurvivalState::new(7, 3, 12).unwrap()
        );
        assert_eq!(loaded.chests.len(), 2);
        assert_eq!(
            loaded.chests[&IVec3::new(4, -2, 7)].slot(0),
            Some(Item::new(ItemId::Material(Material::Bread), 3).unwrap())
        );
        assert_eq!(
            loaded.chests[&IVec3::new(4, -2, 7)].slot(26),
            Some(Item::new(ItemId::Material(Material::Apple), 2).unwrap())
        );
        assert_eq!(loaded.chests[&IVec3::new(-1, 0, 3)], Container::empty());

        // A v8 re-save is deterministic and round-trips the same domain
        // values, while insertion order stays irrelevant.
        let mut v8 = Vec::new();
        save_world_to_bytes(&mut v8, &loaded).unwrap();
        assert_eq!(&v8[4..8], &SAVE_VERSION.to_le_bytes());
        let reloaded = load_world_from_bytes(&v8).unwrap();
        assert_eq!(reloaded, loaded);
        let mut v8_again = Vec::new();
        save_world_to_bytes(&mut v8_again, &reloaded).unwrap();
        assert_eq!(v8_again, v8, "v8 writes are byte-stable");

        let mut reversed = ChestMap::default();
        reversed.insert(IVec3::new(-1, 0, 3), Container::empty());
        reversed.insert(IVec3::new(4, -2, 7), survival_chest());
        let reversed_data = v6_data_with_chests(reversed);
        assert_eq!(
            legacy_world_bytes(&reversed_data, 6),
            bytes,
            "sorted chest records make the wire independent of insertion order"
        );
        let mut reversed_v8 = Vec::new();
        save_world_to_bytes(&mut reversed_v8, &reversed_data).unwrap();
        assert_eq!(
            reversed_v8, v8,
            "v8 writes are independent of chest insertion order"
        );
    }

    #[test]
    fn test_v6_present_empty_chest_is_distinct_from_missing_position() {
        // A missing position and a stored all-empty container are different
        // world states: only the latter survives a reload as a present record.
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(-1, 0, 3), Container::empty());
        let data = v6_data_with_chests(chests);

        let bytes = legacy_world_bytes(&data, 6);
        let loaded = load_world_from_bytes(&bytes).expect("v6 must load");

        assert_eq!(loaded.chests.len(), 1, "an empty chest is not dropped");
        assert_eq!(
            loaded.chests.get(&IVec3::new(-1, 0, 3)),
            Some(&Container::empty())
        );
        assert!(
            !loaded.chests.contains_key(&IVec3::new(0, 0, 0)),
            "a position without a record stays missing"
        );
        // The present-empty record still occupies its full 12 + 27 bytes.
        assert_eq!(bytes.len(), V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS);
        let mut v8 = Vec::new();
        save_world_to_bytes(&mut v8, &loaded).unwrap();
        let reloaded = load_world_from_bytes(&v8).expect("v8 must load");
        assert_eq!(reloaded.chests, loaded.chests);
    }

    // ------------------------------------------------------------------
    // v7 mob tail: kind, feet position, yaw, health
    // ------------------------------------------------------------------

    /// Four damaged, non-round yaw mobs covering every [`MobKind`] in tag
    /// order, so the wire layout pin exercises all four kind bytes.
    fn pinned_mobs() -> Vec<SavedMob> {
        vec![
            SavedMob {
                kind: MobKind::Pig,
                position: [1.5, 64.0, -3.25],
                yaw: 0.25,
                health: 4,
            },
            SavedMob {
                kind: MobKind::Cow,
                position: [-2.0, 65.5, 0.5],
                yaw: 0.75,
                health: 7,
            },
            SavedMob {
                kind: MobKind::Zombie,
                position: [3.0, 63.0, 8.0],
                yaw: -1.25,
                health: 11,
            },
            SavedMob {
                kind: MobKind::Skeleton,
                position: [7.5, 70.0, -4.5],
                yaw: 2.5,
                health: 14,
            },
        ]
    }

    fn v7_data_with_mobs(mobs: Vec<SavedMob>) -> WorldSaveData {
        let mut data = sample_data(sample_player());
        data.mobs = mobs;
        data
    }

    fn v7_bytes_with_pinned_mobs() -> Vec<u8> {
        legacy_world_bytes(&v7_data_with_mobs(pinned_mobs()), 7)
    }

    #[test]
    fn test_v7_roundtrips_mobs_and_pins_the_tail_layout() {
        // An edit-free, chest-free save keeps every older offset fixed and
        // puts the mob count at 149 with the first record at 153.
        assert_eq!(V7_MOB_COUNT_OFFSET, 149);
        assert_eq!(V7_FIRST_MOB_OFFSET, 153);
        assert_eq!(SAVED_MOB_LEN, 19);

        let data = v7_data_with_mobs(pinned_mobs());
        let bytes = legacy_world_bytes(&data, 7);

        assert_eq!(bytes.len(), V7_FIRST_MOB_OFFSET + 4 * SAVED_MOB_LEN);
        assert_eq!(bytes.len(), 229);
        assert_eq!(&bytes[4..8], &MOB_VERSION.to_le_bytes());
        assert_eq!(
            &bytes[V7_MOB_COUNT_OFFSET..V7_FIRST_MOB_OFFSET],
            &4u32.to_le_bytes(),
            "the mob count sits immediately after the empty chest section"
        );

        // First record: kind u8, x/y/z f32 LE, yaw f32 LE, health u16 LE.
        let first = V7_FIRST_MOB_OFFSET;
        assert_eq!(bytes[first], MobKind::Pig.tag());
        assert_eq!(&bytes[first + 1..first + 5], &1.5f32.to_le_bytes());
        assert_eq!(&bytes[first + 5..first + 9], &64.0f32.to_le_bytes());
        assert_eq!(&bytes[first + 9..first + 13], &(-3.25f32).to_le_bytes());
        assert_eq!(&bytes[first + 13..first + 17], &0.25f32.to_le_bytes());
        assert_eq!(&bytes[first + 17..first + 19], &4u16.to_le_bytes());

        let loaded = load_world_from_bytes(&bytes).expect("v7 must load");
        assert_eq!(loaded, data);
        assert_eq!(loaded.mobs, pinned_mobs());

        // v8 keeps the same record vocabulary behind an empty content table
        // (this world references no content names) and adds the 12-byte tail.
        let mut v8 = Vec::new();
        save_world_to_bytes(&mut v8, &data).expect("v8 mob save must succeed");
        assert_eq!(&v8[4..8], &SAVE_VERSION.to_le_bytes());
        assert_eq!(v8.len(), V8_TAIL_OFFSET + 4 * SAVED_MOB_LEN + V8_TAIL_LEN);
        assert_eq!(
            &v8[V8_MOB_COUNT_OFFSET..V8_TAIL_OFFSET],
            &4u32.to_le_bytes()
        );
        let loaded_v8 = load_world_from_bytes(&v8).expect("v8 must load");
        assert_eq!(loaded_v8, data);

        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded_v8).expect("v8 re-save must succeed");
        assert_eq!(again, v8, "a loaded v8 mob save re-saves byte-identically");
    }

    #[test]
    fn test_v7_writer_sorts_mobs_by_kind_and_position() {
        let mut scrambled = pinned_mobs();
        scrambled.reverse();
        // A second pig proves the position tie-break inside one kind.
        scrambled.push(SavedMob {
            kind: MobKind::Pig,
            position: [-1.0, 64.0, 0.0],
            yaw: 0.0,
            health: 10,
        });

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &v7_data_with_mobs(scrambled)).unwrap();
        let loaded = load_world_from_bytes(&bytes).expect("scrambled input still loads");

        let keys: Vec<(u8, [f32; 3])> = loaded
            .mobs
            .iter()
            .map(|mob| (mob.kind.tag(), mob.position))
            .collect();
        let mut expected = keys.clone();
        expected.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1[0].total_cmp(&b.1[0]))
                .then_with(|| a.1[1].total_cmp(&b.1[1]))
                .then_with(|| a.1[2].total_cmp(&b.1[2]))
        });
        assert_eq!(keys, expected, "wire order is sorted, not insertion order");
        assert_eq!(loaded.mobs[0].kind, MobKind::Pig);
        assert_eq!(loaded.mobs[0].position, [-1.0, 64.0, 0.0]);
        assert_eq!(loaded.mobs[1].position, [1.5, 64.0, -3.25]);
    }

    #[test]
    fn test_v7_rejects_malformed_mob_records() {
        let base = v7_bytes_with_pinned_mobs();

        // Truncating one tail byte shortens the final health u16.
        let truncated = &base[..base.len() - 1];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));

        // Unknown kind tag.
        let mut bytes = base.clone();
        bytes[V7_FIRST_MOB_OFFSET] = 4;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMobKind(4))
        ));

        // Health above the kind maximum (Pig max 10 -> 11).
        let mut bytes = base.clone();
        bytes[V7_FIRST_MOB_OFFSET + 17..V7_FIRST_MOB_OFFSET + 19]
            .copy_from_slice(&(MobKind::Pig.max_health() + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMobState)
        ));

        // A non-finite coordinate reports InvalidMobState, not
        // InvalidPlayerState (that is what the new reader helper is for).
        let mut bytes = base.clone();
        bytes[V7_FIRST_MOB_OFFSET + 1..V7_FIRST_MOB_OFFSET + 5]
            .copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMobState)
        ));

        // A non-finite facing is equally invalid.
        let mut bytes = base.clone();
        bytes[V7_FIRST_MOB_OFFSET + 13..V7_FIRST_MOB_OFFSET + 17]
            .copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMobState)
        ));

        // Duplicate (kind, position): append the first record and count 5.
        let mut bytes = base.clone();
        let record = bytes[V7_FIRST_MOB_OFFSET..V7_FIRST_MOB_OFFSET + SAVED_MOB_LEN].to_vec();
        bytes.extend_from_slice(&record);
        bytes[V7_MOB_COUNT_OFFSET..V7_FIRST_MOB_OFFSET].copy_from_slice(&5u32.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMobState)
        ));

        // An out-of-bound count fails before any record is read.
        let mut bytes = base;
        bytes[V7_MOB_COUNT_OFFSET..V7_FIRST_MOB_OFFSET]
            .copy_from_slice(&((MAX_SAVED_MOBS as u32) + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMobState)
        ));
    }

    #[test]
    fn test_v7_writer_rejects_invalid_mobs_before_any_byte() {
        // Non-finite coordinate.
        let mut mobs = pinned_mobs();
        mobs[0].position[1] = f32::NAN;
        let mut bytes = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut bytes, &v7_data_with_mobs(mobs)),
            Err(PersistenceError::InvalidMobState)
        ));
        assert!(bytes.is_empty(), "a rejected save must not emit bytes");

        // Health above the kind maximum.
        let mut mobs = pinned_mobs();
        mobs[0].health = MobKind::Pig.max_health() + 1;
        let mut bytes = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut bytes, &v7_data_with_mobs(mobs)),
            Err(PersistenceError::InvalidMobState)
        ));
        assert!(bytes.is_empty());

        // Duplicate (kind, position).
        let mut mobs = pinned_mobs();
        mobs.push(mobs[0]);
        let mut bytes = Vec::new();
        assert!(matches!(
            save_world_to_bytes(&mut bytes, &v7_data_with_mobs(mobs)),
            Err(PersistenceError::InvalidMobState)
        ));
        assert!(bytes.is_empty());

        // The wire bound is enforced before the header is written.
        let mut bytes = Vec::new();
        assert!(matches!(
            save_world_to_bytes(
                &mut bytes,
                &v7_data_with_mobs(vec![pinned_mobs()[0]; MAX_SAVED_MOBS + 1]),
            ),
            Err(PersistenceError::InvalidMobState)
        ));
        assert!(bytes.is_empty());
    }

    #[test]
    fn test_v7_zero_health_roundtrips() {
        // A mob that died in the same tick is legal wire state and must not
        // be filtered by the writer.
        let mut mobs = pinned_mobs();
        mobs[0].health = 0;
        let data = v7_data_with_mobs(mobs);
        let legacy = legacy_world_bytes(&data, 7);
        assert_eq!(
            &legacy[V7_FIRST_MOB_OFFSET + 17..V7_FIRST_MOB_OFFSET + 19],
            &0u16.to_le_bytes()
        );
        let loaded = load_world_from_bytes(&legacy).unwrap();
        assert_eq!(loaded.mobs[0].health, 0);
        assert_eq!(loaded, data);

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        let loaded_v8 = load_world_from_bytes(&bytes).unwrap();
        assert_eq!(loaded_v8.mobs[0].health, 0);
        assert_eq!(loaded_v8, data);
    }

    // ------------------------------------------------------------------
    // v7 item vocabulary: new materials and Sword/Bow save records
    // ------------------------------------------------------------------

    /// The six material identities appended in v7 (M009) with their tags.
    const V7_MELEE_MATERIALS: [(Material, u8); 6] = [
        (Material::Leather, MATERIAL_LEATHER),
        (Material::RawPorkchop, MATERIAL_RAW_PORKCHOP),
        (Material::RawBeef, MATERIAL_RAW_BEEF),
        (Material::RottenFlesh, MATERIAL_ROTTEN_FLESH),
        (Material::Bone, MATERIAL_BONE),
        (Material::Arrow, MATERIAL_ARROW),
    ];

    fn v7_melee_item(material: Material, count: u16) -> Item {
        Item::new(ItemId::Material(material), count).expect("material count is in range")
    }

    #[test]
    fn test_v7_roundtrips_new_materials_and_combat_tools_byte_stably() {
        // Every new material sits in one hotbar slot and one chest slot, and
        // both combat tool identities carry non-full durability.
        let mut slots = [None; INVENTORY_SLOTS];
        for (index, (material, _)) in V7_MELEE_MATERIALS.iter().enumerate() {
            slots[index] = Some(v7_melee_item(*material, 2 + index as u16));
        }
        slots[6] = Some(Item::tool_with_durability(ToolKind::Sword, ToolTier::Wood, 17).unwrap());
        slots[7] = Some(Item::tool_with_durability(ToolKind::Bow, ToolTier::Wood, 3).unwrap());

        let mut chest_slots: ChestSlots = [None; CHEST_SLOTS];
        for (index, (material, _)) in V7_MELEE_MATERIALS.iter().enumerate() {
            chest_slots[index] = Some(v7_melee_item(*material, 5));
        }
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(7, 65, -3), Container::from_slots(chest_slots));

        let mut data = sample_data(sample_player());
        data.player.inventory = Inventory::from_parts(slots, None);
        data.chests = chests;

        // The record-layout pins belong to the frozen v7 vocabulary; read a
        // legacy fixture for them, then prove the same domain values through
        // the public v8 writer.
        let legacy = legacy_world_bytes(&data, ITEM_VOCAB_VERSION);
        assert_eq!(&legacy[4..8], &ITEM_VOCAB_VERSION.to_le_bytes());

        let loaded_legacy = load_world_from_bytes(&legacy).expect("v7 item vocabulary must load");
        assert_eq!(loaded_legacy, data);

        for (index, (material, tag)) in V7_MELEE_MATERIALS.iter().enumerate() {
            // Slot-0 layout for a material record: tag, material tag, count.
            assert_eq!(legacy[V4_EXTENSION_OFFSET + index * 4], ITEM_TAG_MATERIAL);
            assert_eq!(legacy[V4_EXTENSION_OFFSET + index * 4 + 1], *tag);
            assert_eq!(
                loaded_legacy.player.inventory.slot(index),
                Some(v7_melee_item(*material, 2 + index as u16))
            );
            assert_eq!(
                loaded_legacy.chests[&IVec3::new(7, 65, -3)].slot(index),
                Some(v7_melee_item(*material, 5))
            );
        }

        // Slot-6/7 layout for a tool record: tag, kind, tier, durability u16.
        let sword = V4_EXTENSION_OFFSET + 6 * 4;
        assert_eq!(legacy[sword], ITEM_TAG_TOOL);
        assert_eq!(legacy[sword + 1], TOOL_KIND_SWORD);
        assert_eq!(legacy[sword + 2], TOOL_TIER_WOOD);
        assert_eq!(&legacy[sword + 3..sword + 5], &17u16.to_le_bytes());
        let bow = sword + 5;
        assert_eq!(legacy[bow], ITEM_TAG_TOOL);
        assert_eq!(legacy[bow + 1], TOOL_KIND_BOW);
        assert_eq!(legacy[bow + 2], TOOL_TIER_WOOD);
        assert_eq!(&legacy[bow + 3..bow + 5], &3u16.to_le_bytes());

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v8 item vocabulary must save");
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());
        let loaded = load_world_from_bytes(&bytes).expect("v8 item vocabulary must load");
        assert_eq!(loaded, data);

        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded).expect("re-save must succeed");
        assert_eq!(
            again, bytes,
            "a loaded v8 item save re-saves byte-identically"
        );
    }

    #[test]
    fn test_v6_rejects_v7_only_material_and_combat_tool_records() {
        // Material tag 9 (Bone) was appended in v7: a pre-v7 payload that
        // claims it fails closed, while the identical v7 payload loads.
        let bone_data = data_with_slot0(v7_melee_item(Material::Bone, 1));
        assert!(load_world_from_bytes(&legacy_world_bytes(&bone_data, 7)).is_ok());
        assert!(matches!(
            load_world_from_bytes(&legacy_world_bytes(&bone_data, 6)),
            Err(PersistenceError::InvalidMaterial(9))
        ));

        // Tool kind 4 (Sword) is likewise v7-only, and its v6 rejection is
        // independent of the v6-only Hoe gate.
        let sword_data = data_with_slot0(
            Item::tool_with_durability(ToolKind::Sword, ToolTier::Wood, 17).unwrap(),
        );
        assert!(load_world_from_bytes(&legacy_world_bytes(&sword_data, 7)).is_ok());
        assert!(matches!(
            load_world_from_bytes(&legacy_world_bytes(&sword_data, 6)),
            Err(PersistenceError::InvalidToolKind(4))
        ));
    }

    #[test]
    fn test_v7_rejects_forged_stone_bow_record() {
        // `(Bow, Stone)` is not a constructible tool identity; persistence
        // decode must funnel through the checked constructor and report the
        // tier gate instead of accepting the forged pair.
        let sword_data = data_with_slot0(
            Item::tool_with_durability(ToolKind::Sword, ToolTier::Wood, 17).unwrap(),
        );
        let mut bytes = legacy_world_bytes(&sword_data, 7);
        bytes[V4_EXTENSION_OFFSET + 1] = TOOL_KIND_BOW;
        bytes[V4_EXTENSION_OFFSET + 2] = TOOL_TIER_STONE;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::TierNotAllowed))
        ));
    }

    #[test]
    fn test_v7_legacy_encoder_keeps_the_153_byte_edit_free_baseline() {
        assert_eq!(ITEM_VOCAB_VERSION, MOB_VERSION);

        let data = sample_data(sample_player());
        let bytes = legacy_world_bytes(&data, ITEM_VOCAB_VERSION);

        assert_eq!(bytes.len(), V7_FIRST_MOB_OFFSET);
        assert_eq!(bytes.len(), 153);
        assert_eq!(&bytes[4..8], &ITEM_VOCAB_VERSION.to_le_bytes());
        assert_eq!(
            &bytes[V7_MOB_COUNT_OFFSET..V7_FIRST_MOB_OFFSET],
            &0u32.to_le_bytes()
        );

        let loaded = load_world_from_bytes(&bytes).expect("baseline must load");
        assert_eq!(loaded, data);
    }

    #[test]
    fn v8_empty_table_save_has_the_measured_169_byte_layout() {
        let data = sample_data(sample_player());
        assert!(data.mobs.is_empty());
        assert!(data.chests.is_empty());
        assert!(data.chunk_edits.is_empty());
        assert_eq!(data.player.inventory, Inventory::empty());

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("edit-free v8 save must succeed");

        assert_eq!(bytes.len(), V8_EMPTY_SAVE_LEN);
        assert_eq!(bytes.len(), 169);
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());
        assert_eq!(
            &bytes[V2_HOTBAR_OFFSET..V2_HOTBAR_OFFSET + 4],
            &0u32.to_le_bytes()
        );
        assert_eq!(
            &bytes[V8_MOB_COUNT_OFFSET..V8_TAIL_OFFSET],
            &0u32.to_le_bytes(),
            "the v8 mob count sits at offset 153 for an empty table"
        );
        assert_eq!(
            &bytes[V8_TAIL_OFFSET..V8_TAIL_OFFSET + 4],
            &0u32.to_le_bytes(),
            "no packs in the built-in stack"
        );

        let loaded = load_world_from_bytes(&bytes).expect("baseline must load");
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_v6_save_loads_with_zero_mobs() {
        let data = v7_data_with_mobs(pinned_mobs());
        let bytes = v6_bytes(&data);
        let loaded = load_world_from_bytes(&bytes).expect("v6 must migrate, not fail");
        assert!(loaded.mobs.is_empty());
        assert_eq!(loaded.chests, data.chests);

        // Re-saving migrates the payload to v8 with an explicit zero mob count.
        let mut resaved = Vec::new();
        save_world_to_bytes(&mut resaved, &loaded).unwrap();
        assert_eq!(&resaved[4..8], &SAVE_VERSION.to_le_bytes());
        assert_eq!(
            &resaved[V8_MOB_COUNT_OFFSET..V8_TAIL_OFFSET],
            &0u32.to_le_bytes()
        );
    }

    #[test]
    fn test_checked_mob_count_bounds_the_writer_before_any_byte() {
        assert_eq!(checked_mob_count(0).unwrap(), 0);
        assert_eq!(
            checked_mob_count(MAX_SAVED_MOBS).unwrap(),
            MAX_SAVED_MOBS as u32
        );
        assert!(matches!(
            checked_mob_count(MAX_SAVED_MOBS + 1),
            Err(PersistenceError::InvalidMobState)
        ));
    }

    #[test]
    fn test_version_constants_are_coupled() {
        assert_eq!(SAVE_VERSION, CONTENT_TABLE_VERSION);
        assert_eq!(CONTENT_TABLE_VERSION, MOB_VERSION + 1);
        assert_eq!(MOB_VERSION, SURVIVAL_VERSION + 1);
        assert_eq!(SURVIVAL_VERSION, SEASON_VERSION + 1);
        assert_eq!(SEASON_VERSION, INVENTORY_EXTENSION_VERSION + 1);
        assert_eq!(LEGACY_WRITE_VERSION, 7);
    }

    #[test]
    fn test_v6_accepts_zero_vitals_for_the_death_state() {
        let mut data = sample_data(sample_player());
        data.mode = GameMode::Survival;
        data.player.fly_mode = false;
        data.player.survival = SurvivalState::new(0, 0, 0).unwrap();

        // Vital offsets are shared by v6-v7; v8 shifts them behind the content
        // table, so the layout pin reads a legacy fixture.
        let legacy = legacy_world_bytes(&data, 7);
        assert_eq!(
            &legacy[V6_HEALTH_OFFSET..V6_HEALTH_OFFSET + 2],
            &0u16.to_le_bytes()
        );
        assert_eq!(
            &legacy[V6_HUNGER_OFFSET..V6_HUNGER_OFFSET + 2],
            &0u16.to_le_bytes()
        );
        assert_eq!(
            &legacy[V6_AIR_OFFSET..V6_AIR_OFFSET + 2],
            &0u16.to_le_bytes()
        );

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        assert_eq!(
            &bytes[V8_HEALTH_OFFSET..V8_HEALTH_OFFSET + 2],
            &0u16.to_le_bytes()
        );
        let loaded = load_world_from_bytes(&bytes).expect("a dead save must be loadable");
        assert_eq!(loaded.player.survival, SurvivalState::new(0, 0, 0).unwrap());
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_v6_canonicalizes_the_header_fly_byte_through_the_mode() {
        // Header layout: magic, version, seed, position, yaw, pitch, fly.
        const V3_FLY_OFFSET: usize = 32;

        let mut survival = sample_data(sample_player());
        assert!(survival.player.fly_mode);
        survival.mode = GameMode::Survival;
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &survival).unwrap();
        assert_eq!(
            bytes[V3_FLY_OFFSET], 0,
            "a Survival save may not carry a flying player"
        );
        let loaded = load_world_from_bytes(&bytes).unwrap();
        assert_eq!(loaded.mode, GameMode::Survival);
        assert!(!loaded.player.fly_mode);

        let mut creative = sample_data(sample_player());
        creative.mode = GameMode::Creative;
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &creative).unwrap();
        assert_eq!(bytes[V3_FLY_OFFSET], 1);
        let loaded = load_world_from_bytes(&bytes).unwrap();
        assert!(loaded.player.fly_mode);
    }

    #[test]
    fn test_v5_save_migrates_to_creative_full_vitals_and_no_chests() {
        let mut data = sample_data(sample_player());
        data.mode = GameMode::Survival;
        data.player.survival = SurvivalState::new(1, 2, 3).unwrap();
        let bytes = v5_bytes(&data);

        let loaded = load_world_from_bytes(&bytes).expect("v5 must migrate, not fail");
        assert_v6_migration_defaults(&loaded);
        // Everything the v5 payload actually carried still round-trips.
        assert_eq!(loaded.seed, data.seed);
        assert_eq!(loaded.time_of_day, data.time_of_day);
        assert_eq!(loaded.player.inventory, data.player.inventory);
    }

    #[test]
    fn test_v6_rejects_invalid_mode_tag() {
        let mut bytes = v6_bytes_with_one_empty_chest();
        bytes[V6_MODE_OFFSET] = 2;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidGameMode(2))
        ));
    }

    #[test]
    fn test_v6_rejects_out_of_range_vitals() {
        let base = v6_bytes_with_one_empty_chest();

        let mut bytes = base.clone();
        bytes[V6_HEALTH_OFFSET..V6_HEALTH_OFFSET + 2]
            .copy_from_slice(&(MAX_HEALTH + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidVitals { health, hunger: 3, air })
                if health == MAX_HEALTH + 1 && air == 12
        ));

        let mut bytes = base.clone();
        bytes[V6_HUNGER_OFFSET..V6_HUNGER_OFFSET + 2]
            .copy_from_slice(&(MAX_HUNGER + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidVitals { health: 7, hunger, air }) if hunger == MAX_HUNGER + 1 && air == 12
        ));

        let mut bytes = base;
        bytes[V6_AIR_OFFSET..V6_AIR_OFFSET + 2].copy_from_slice(&(MAX_AIR + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidVitals { health: 7, hunger: 3, air }) if air == MAX_AIR + 1
        ));
    }

    #[test]
    fn test_v6_rejects_invalid_chest_sections() {
        // Count above the decode bound fails before any record is read.
        let mut bytes = v6_bytes_with_one_empty_chest();
        bytes[V6_CHEST_COUNT_OFFSET..V6_CHEST_COUNT_OFFSET + 4]
            .copy_from_slice(&((MAX_SAVED_CHESTS as u32) + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidChestState)
        ));

        // Duplicate positions are ambiguous state, not a merge. Append the
        // copied record at the end of the v6 chest section (v6 has no mob
        // tail) so the two chest records stay contiguous.
        let mut bytes = v6_bytes_with_one_empty_chest();
        let record =
            bytes[V6_FIRST_CHEST_OFFSET..V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS].to_vec();
        bytes.extend_from_slice(&record);
        bytes[V6_CHEST_COUNT_OFFSET..V6_CHEST_COUNT_OFFSET + 4]
            .copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidChestState)
        ));

        // Unknown item tag inside a chest slot.
        let mut bytes = v6_bytes_with_one_empty_chest();
        bytes[V6_FIRST_CHEST_SLOT_OFFSET] = 200;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItemTag(200))
        ));

        // Unknown block id inside a chest slot.
        let mut slot0: ChestSlots = [None; CHEST_SLOTS];
        slot0[0] = Some(Item::new(ItemId::Block(BlockId::CoalOre), 1).unwrap());
        let mut bytes = v6_bytes_with_chest(Container::from_slots(slot0));
        bytes[V6_FIRST_CHEST_SLOT_OFFSET + 1] = 99;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        // Water stays worldgen-only even inside a chest: `Item::new` rejects
        // the carryable-block funnel as before.
        let mut water_slot: ChestSlots = [None; CHEST_SLOTS];
        water_slot[0] = Some(Item::new(ItemId::Block(BlockId::CoalOre), 1).unwrap());
        let mut bytes = v6_bytes_with_chest(Container::from_slots(water_slot));
        bytes[V6_FIRST_CHEST_SLOT_OFFSET + 1] = BlockId::Water.to_raw() as u8;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::UncarryableBlock))
        ));

        // Unknown material id inside a chest slot.
        let mut material_slot: ChestSlots = [None; CHEST_SLOTS];
        material_slot[0] = Some(Item::new(ItemId::Material(Material::Bread), 1).unwrap());
        let mut bytes = v6_bytes_with_chest(Container::from_slots(material_slot));
        // 11 is the first tag past the v7 melee/drop vocabulary.
        bytes[V6_FIRST_CHEST_SLOT_OFFSET + 1] = 11;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMaterial(11))
        ));

        // Unknown tool kind inside a chest slot.
        let mut tool_slot: ChestSlots = [None; CHEST_SLOTS];
        tool_slot[0] =
            Some(Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 10).unwrap());
        let mut bytes = v6_bytes_with_chest(Container::from_slots(tool_slot));
        bytes[V6_FIRST_CHEST_SLOT_OFFSET + 1] = 9;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidToolKind(9))
        ));

        // Trailing bytes and a truncated chest tail both fail closed.
        let mut bytes = v6_bytes_with_one_empty_chest();
        bytes.push(0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        let bytes = v6_bytes_with_one_empty_chest();
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));

        let truncated = &bytes[..V6_CHEST_COUNT_OFFSET + 2];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));
    }

    #[test]
    fn test_checked_chest_count_bounds_the_writer_before_any_byte() {
        assert_eq!(checked_chest_count(0).unwrap(), 0);
        assert_eq!(
            checked_chest_count(MAX_SAVED_CHESTS).unwrap(),
            MAX_SAVED_CHESTS as u32
        );
        assert!(matches!(
            checked_chest_count(MAX_SAVED_CHESTS + 1),
            Err(PersistenceError::InvalidChestState)
        ));
    }

    /// Block ids appended in M008: v6-only wire identities.
    const SURVIVAL_ONLY_BLOCKS: [BlockId; 7] = [
        BlockId::Farmland,
        BlockId::WheatStage0,
        BlockId::WheatStage1,
        BlockId::WheatStage2,
        BlockId::WheatStage3,
        BlockId::Chest,
        BlockId::LootChest,
    ];

    fn v6_data_with_block_edit(block: BlockId) -> WorldSaveData {
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(linear_index(3, 4, 5) as u16, block);
        let mut edits = ChunkEditMap::default();
        edits.insert(ChunkPos::new(1, 2, 3), chunk);
        let mut data = sample_data(sample_player());
        data.chunk_edits = edits;
        data
    }

    #[test]
    fn test_v6_roundtrips_survival_only_block_edits_and_chest_item() {
        // Every id 16..=22 is legal in a v6 save, both as a chunk edit and
        // (for the carryable Chest) as an inventory item.
        let mut chunk = rustc_hash::FxHashMap::default();
        for (offset, block) in SURVIVAL_ONLY_BLOCKS.iter().enumerate() {
            chunk.insert(linear_index(offset, 4, 5) as u16, *block);
        }
        let mut edits = ChunkEditMap::default();
        edits.insert(ChunkPos::new(1, 2, 3), chunk);

        // Slot 9 is the first main-inventory slot, so the legacy hotbar
        // projection stays empty and this item exercises `read_item` only.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[HOTBAR_SLOTS] = Some(Item::new(ItemId::Block(BlockId::Chest), 5).unwrap());
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let mut data = sample_data(player);
        data.chunk_edits = edits;

        let bytes = legacy_world_bytes(&data, 6);
        assert_eq!(&bytes[4..8], &SURVIVAL_VERSION.to_le_bytes());
        let loaded = load_world_from_bytes(&bytes).expect("v6 survival palette must load");

        let loaded_chunk = loaded
            .chunk_edits
            .get(&ChunkPos::new(1, 2, 3))
            .expect("edited chunk must round-trip");
        for (offset, block) in SURVIVAL_ONLY_BLOCKS.iter().enumerate() {
            assert_eq!(
                loaded_chunk[&(linear_index(offset, 4, 5) as u16)],
                *block,
                "{block:?} chunk edit must round-trip"
            );
        }
        assert_eq!(
            loaded.player.inventory.main(0),
            Some(Item::new(ItemId::Block(BlockId::Chest), 5).unwrap()),
            "a v6 Chest item must round-trip"
        );
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_v5_rejects_survival_only_block_edits_hotbar_and_chest_item() {
        // Chunk-edit funnel: every v6-only id is a typed error on a v5
        // payload, not a silently accepted block.
        for block in SURVIVAL_ONLY_BLOCKS {
            let bytes = v5_bytes(&v6_data_with_block_edit(block));
            assert!(
                matches!(
                    load_world_from_bytes(&bytes),
                    Err(PersistenceError::InvalidBlockId(raw)) if raw == block.to_raw()
                ),
                "a v5 chunk edit may not claim {block:?}"
            );
        }

        // Legacy-hotbar funnel: forge block id 16 into slot 0 of a byte-valid
        // v5 save (36-byte header, then 9 * 3 hotbar bytes).
        let mut bytes = v5_bytes(&sample_data(sample_player()));
        bytes[V2_HOTBAR_OFFSET] = BlockId::Farmland.to_raw() as u8;
        bytes[V2_HOTBAR_OFFSET + 1..V2_HOTBAR_OFFSET + 3].copy_from_slice(&1u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidBlockId(16))
        ));

        // Item funnel: Chest is the only carryable new id. A v5 payload that
        // claims one is corrupt; the identical v6 payload loads.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[HOTBAR_SLOTS] = Some(Item::new(ItemId::Block(BlockId::Chest), 5).unwrap());
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let data = sample_data(player);

        let v6_bytes = legacy_world_bytes(&data, 6);
        let loaded = load_world_from_bytes(&v6_bytes).expect("v6 Chest item must load");
        assert_eq!(loaded.player.inventory, data.player.inventory);

        let v5_bytes = v5_bytes(&data);
        assert!(matches!(
            load_world_from_bytes(&v5_bytes),
            Err(PersistenceError::InvalidBlockId(21))
        ));
    }

    #[test]
    fn test_v5_rejects_v6_only_material_and_hoe_records() {
        for material in [
            Material::Apple,
            Material::Wheat,
            Material::WheatSeeds,
            Material::Bread,
        ] {
            let item = Item::new(ItemId::Material(material), 1).unwrap();
            let mut slots = [None; INVENTORY_SLOTS];
            slots[0] = Some(item);
            let mut player = sample_player();
            player.inventory = Inventory::from_parts(slots, None);
            let bytes = v5_bytes(&sample_data(player));
            assert!(
                matches!(
                    load_world_from_bytes(&bytes),
                    Err(PersistenceError::InvalidMaterial(tag)) if tag == material_tag(material)
                ),
                "a v5 payload may not contain {material:?}"
            );
        }

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::tool_with_durability(ToolKind::Hoe, ToolTier::Wood, 10).unwrap());
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let bytes = v5_bytes(&sample_data(player));
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidToolKind(TOOL_KIND_HOE))
        ));
    }

    #[test]
    fn test_v4_rejects_unknown_item_tag() {
        let data = sample_data(sample_player());
        let mut bytes = legacy_world_bytes(&data, 4);
        bytes[V4_EXTENSION_OFFSET] = 200;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItemTag(200))
        ));
    }

    #[test]
    fn test_v4_rejects_invalid_block_and_material_records() {
        let block_record =
            v4_bytes_with_slot0(Item::new(ItemId::Block(BlockId::CoalOre), 8).unwrap());
        // Slot-0 layout: tag, block id, count u16.
        let mut bytes = block_record.clone();
        bytes[V4_EXTENSION_OFFSET + 1] = 99;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidBlockId(99))
        ));

        let mut bytes = block_record.clone();
        bytes[V4_EXTENSION_OFFSET + 1] = BlockId::Air.to_raw() as u8;
        bytes[V4_EXTENSION_OFFSET + 2..V4_EXTENSION_OFFSET + 4]
            .copy_from_slice(&1u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::UncarryableBlock))
        ));

        let mut bytes = block_record.clone();
        bytes[V4_EXTENSION_OFFSET + 1] = BlockId::Water.to_raw() as u8;
        bytes[V4_EXTENSION_OFFSET + 2..V4_EXTENSION_OFFSET + 4]
            .copy_from_slice(&1u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::UncarryableBlock))
        ));

        let mut bytes = block_record.clone();
        bytes[V4_EXTENSION_OFFSET + 2..V4_EXTENSION_OFFSET + 4]
            .copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::EmptyStack))
        ));

        let mut bytes = block_record;
        bytes[V4_EXTENSION_OFFSET + 2..V4_EXTENSION_OFFSET + 4]
            .copy_from_slice(&(MAX_STACK_COUNT + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::StackOverflow))
        ));

        let material_record =
            v4_bytes_with_slot0(Item::new(ItemId::Material(Material::Stick), 4).unwrap());
        let mut bytes = material_record.clone();
        // 11 is the first tag past the v7 melee/drop vocabulary.
        bytes[V4_EXTENSION_OFFSET + 1] = 11;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMaterial(11))
        ));

        // Materials cap at MATERIAL_STACK_LIMIT (64).
        let mut bytes = material_record;
        bytes[V4_EXTENSION_OFFSET + 2..V4_EXTENSION_OFFSET + 4]
            .copy_from_slice(&65u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::StackOverflow))
        ));
    }

    #[test]
    fn test_wire_tags_are_stable_and_append_only() {
        // Pre-M008 discriminants are on-disk format: existing v4/v5 saves
        // encode them, so they must never move.
        assert_eq!(MATERIAL_STICK, 0);
        assert_eq!(TOOL_KIND_PICKAXE, 0);
        assert_eq!(TOOL_KIND_AXE, 1);
        assert_eq!(TOOL_KIND_SHOVEL, 2);

        // M008 appends at the next free discriminants instead of renumbering.
        assert_eq!(MATERIAL_APPLE, 1);
        assert_eq!(MATERIAL_WHEAT, 2);
        assert_eq!(MATERIAL_WHEAT_SEEDS, 3);
        assert_eq!(MATERIAL_BREAD, 4);
        assert_eq!(TOOL_KIND_HOE, 3);

        // M009 appends the melee/drop vocabulary at the next free tags too.
        assert_eq!(MATERIAL_LEATHER, 5);
        assert_eq!(MATERIAL_RAW_PORKCHOP, 6);
        assert_eq!(MATERIAL_RAW_BEEF, 7);
        assert_eq!(MATERIAL_ROTTEN_FLESH, 8);
        assert_eq!(MATERIAL_BONE, 9);
        assert_eq!(MATERIAL_ARROW, 10);
        assert_eq!(TOOL_KIND_SWORD, 4);
        assert_eq!(TOOL_KIND_BOW, 5);

        for (material, tag) in [
            (Material::Stick, MATERIAL_STICK),
            (Material::Apple, MATERIAL_APPLE),
            (Material::Wheat, MATERIAL_WHEAT),
            (Material::WheatSeeds, MATERIAL_WHEAT_SEEDS),
            (Material::Bread, MATERIAL_BREAD),
            (Material::Leather, MATERIAL_LEATHER),
            (Material::RawPorkchop, MATERIAL_RAW_PORKCHOP),
            (Material::RawBeef, MATERIAL_RAW_BEEF),
            (Material::RottenFlesh, MATERIAL_ROTTEN_FLESH),
            (Material::Bone, MATERIAL_BONE),
            (Material::Arrow, MATERIAL_ARROW),
        ] {
            assert_eq!(material_tag(material), tag, "{material:?} tag moved");
            assert_eq!(material_from_tag(tag), Some(material));
        }

        for (kind, tag) in [
            (ToolKind::Pickaxe, TOOL_KIND_PICKAXE),
            (ToolKind::Axe, TOOL_KIND_AXE),
            (ToolKind::Shovel, TOOL_KIND_SHOVEL),
            (ToolKind::Hoe, TOOL_KIND_HOE),
            (ToolKind::Sword, TOOL_KIND_SWORD),
            (ToolKind::Bow, TOOL_KIND_BOW),
        ] {
            assert_eq!(tool_kind_tag(kind), tag, "{kind:?} tag moved");
            assert_eq!(tool_kind_from_tag(tag), Some(kind));
        }

        // The next unused discriminants still fail closed.
        assert_eq!(material_from_tag(11), None);
        assert_eq!(tool_kind_from_tag(6), None);
    }

    #[test]
    fn test_v4_rejects_invalid_tool_records() {
        let tool_record = v4_bytes_with_slot0(
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 200).unwrap(),
        );
        // Slot-0 layout: tag, kind, tier, durability u16.
        let mut bytes = tool_record.clone();
        bytes[V4_EXTENSION_OFFSET + 1] = 9;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidToolKind(9))
        ));

        let mut bytes = tool_record.clone();
        bytes[V4_EXTENSION_OFFSET + 2] = 9;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidToolTier(9))
        ));

        let mut bytes = tool_record.clone();
        bytes[V4_EXTENSION_OFFSET + 3..V4_EXTENSION_OFFSET + 5]
            .copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::InvalidDurability))
        ));

        // Iron caps at 250 durability.
        let mut bytes = tool_record;
        bytes[V4_EXTENSION_OFFSET + 3..V4_EXTENSION_OFFSET + 5]
            .copy_from_slice(&251u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::InvalidDurability))
        ));
    }

    #[test]
    fn test_v4_rejects_bad_grid_records() {
        let data = sample_data(sample_player());
        let bytes = legacy_world_bytes(&data, 4);

        const PLAYER_GRID_KIND_OFFSET: usize = V4_EXTENSION_OFFSET + INVENTORY_SLOTS + 1;
        const TABLE_GRID_KIND_OFFSET: usize = PLAYER_GRID_KIND_OFFSET + 1 + 4;

        let mut unknown = bytes.clone();
        unknown[PLAYER_GRID_KIND_OFFSET] = 9;
        assert!(matches!(
            load_world_from_bytes(&unknown),
            Err(PersistenceError::InvalidGridKind(9))
        ));

        let mut mismatched = bytes.clone();
        mismatched[PLAYER_GRID_KIND_OFFSET] = GRID_KIND_TABLE;
        assert!(matches!(
            load_world_from_bytes(&mismatched),
            Err(PersistenceError::InvalidCraftingState)
        ));

        let mut mismatched = bytes;
        mismatched[TABLE_GRID_KIND_OFFSET] = GRID_KIND_PLAYER;
        assert!(matches!(
            load_world_from_bytes(&mismatched),
            Err(PersistenceError::InvalidCraftingState)
        ));
    }

    #[test]
    fn test_cli_args_parsing() {
        let cfg = CliConfig::parse_args([
            "--seed",
            "999",
            "--save",
            "custom.mcvx",
            "--new",
            "--time-scale",
            "60",
        ]);
        assert_eq!(cfg.seed_override, Some(999));
        assert_eq!(cfg.save_path, std::path::PathBuf::from("custom.mcvx"));
        assert!(cfg.force_new_world);
        assert_eq!(cfg.time_scale, 60.0);
    }

    #[test]
    fn test_time_scale_defaults_and_rejects_bad_values() {
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert_eq!(bare.time_scale, DEFAULT_TIME_SCALE);

        // Missing value at end of argv, non-numeric, zero, negative, NaN, and
        // infinity all keep the default (lenient parse, no panic).
        for args in [
            vec!["--time-scale"],
            vec!["--time-scale", "fast"],
            vec!["--time-scale", "0"],
            vec!["--time-scale", "-4"],
            vec!["--time-scale", "NaN"],
            vec!["--time-scale", "inf"],
        ] {
            let cfg = CliConfig::parse_args(args.iter().copied());
            assert_eq!(
                cfg.time_scale, DEFAULT_TIME_SCALE,
                "bad value {:?} must keep the default",
                args
            );
        }

        // A fractional scale is accepted verbatim.
        let fractional = CliConfig::parse_args(["--time-scale", "2.5"]);
        assert_eq!(fractional.time_scale, 2.5);
    }

    #[test]
    fn test_season_length_parses_and_rejects_bad_values() {
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert_eq!(bare.season_length, DEFAULT_SEASON_LENGTH_DAYS);

        // Happy path: a positive length is accepted verbatim.
        let chosen = CliConfig::parse_args(["--season-length", "4"]);
        assert_eq!(chosen.season_length, 4);

        // Missing value at end of argv, non-numeric, zero, negative, and an
        // overflowing token all keep the default (lenient parse, no panic).
        for args in [
            vec!["--season-length"],
            vec!["--season-length", "fast"],
            vec!["--season-length", "0"],
            vec!["--season-length", "-4"],
            vec!["--season-length", "4294967296"],
        ] {
            let cfg = CliConfig::parse_args(args.iter().copied());
            assert_eq!(
                cfg.season_length, DEFAULT_SEASON_LENGTH_DAYS,
                "bad value {:?} must keep the default",
                args
            );
        }

        // An absurd length clamps to the persisted bound instead of being
        // rejected, so a typo never yields an unloadable calendar.
        let absurd = CliConfig::parse_args(["--season-length", "100000"]);
        assert_eq!(absurd.season_length, MAX_SEASON_LENGTH_DAYS);
    }

    #[test]
    fn test_demo_days_parses_and_defaults_to_one_cycle() {
        // No flag keeps the long-standing one-cycle bounded demo, which is
        // the contract `scripts/season_smoke.sh` relies on for day_index == 1.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert_eq!(bare.demo_days, DEFAULT_DEMO_DAYS);
        assert_eq!(bare.demo_days, 1);

        // Happy path: an explicit multi-day demo is accepted verbatim, so one
        // accelerated session can span a full four-day year.
        let chosen = CliConfig::parse_args(["--demo-days", "4"]);
        assert_eq!(chosen.demo_days, 4);

        // Missing value at end of argv, non-numeric, zero, negative, and an
        // overflowing token all keep the default (lenient parse, no panic).
        for args in [
            vec!["--demo-days"],
            vec!["--demo-days", "four"],
            vec!["--demo-days", "0"],
            vec!["--demo-days", "-4"],
            vec!["--demo-days", "4294967296"],
        ] {
            let cfg = CliConfig::parse_args(args.iter().copied());
            assert_eq!(
                cfg.demo_days, DEFAULT_DEMO_DAYS,
                "bad value {:?} must keep the one-cycle default",
                args
            );
        }

        // An absurd day count clamps to the bounded maximum instead of
        // yielding a demo that never terminates in practice.
        let absurd = CliConfig::parse_args(["--demo-days", "100000"]);
        assert_eq!(absurd.demo_days, MAX_DEMO_DAYS);
        assert_eq!(absurd.demo_days, 365);
    }

    #[test]
    fn test_demo_swim_parses_as_a_bare_switch() {
        // Absent keeps the default: a normal launch is byte-identical.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert!(!bare.demo_swim);

        // Present sets the seam and consumes no value, so the next token still
        // parses as its own flag.
        let swim = CliConfig::parse_args([
            "--new",
            "--seed",
            "12345",
            "--demo-swim",
            "--save",
            "swim.mcvx",
        ]);
        assert!(swim.demo_swim);
        assert!(swim.force_new_world);
        assert_eq!(swim.seed_override, Some(12345));
        assert_eq!(swim.save_path, std::path::PathBuf::from("swim.mcvx"));

        // The flag is a launch parameter, not a world intent: on its own it
        // must not force a direct-to-gameplay start. (The effect belongs to the
        // fresh-world startup branch only.)
        let loner = CliConfig::parse_args(["--demo-swim"]);
        assert!(loner.demo_swim);
        assert!(!loner.force_new_world);
        assert_eq!(loner.seed_override, None);
        assert_eq!(loner.save_path, std::path::PathBuf::from(DEFAULT_SAVE_PATH));
    }

    #[test]
    fn test_demo_survival_and_content_parse_as_bare_switches() {
        // Absent keeps both seams off: a normal launch is byte-identical.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert!(!bare.demo_survival);
        assert!(!bare.demo_content);

        // Each flag alone sets exactly its own seam and consumes no value.
        let survival = CliConfig::parse_args(["--demo-survival"]);
        assert!(survival.demo_survival);
        assert!(!survival.demo_content);
        assert!(!survival.force_new_world);
        assert_eq!(survival.seed_override, None);
        assert_eq!(
            survival.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );

        let content = CliConfig::parse_args(["--demo-content"]);
        assert!(content.demo_content);
        assert!(!content.demo_survival);
        assert!(!content.force_new_world);
        assert_eq!(content.seed_override, None);
        assert_eq!(
            content.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );

        // A token after each flag still parses as its own flag, proving the
        // bare switch consumed nothing.
        let after_survival = CliConfig::parse_args([
            "--demo-survival",
            "--new",
            "--seed",
            "7",
            "--save",
            "survival.mcvx",
        ]);
        assert!(after_survival.demo_survival);
        assert!(after_survival.force_new_world);
        assert_eq!(after_survival.seed_override, Some(7));
        assert_eq!(
            after_survival.save_path,
            std::path::PathBuf::from("survival.mcvx")
        );

        let after_content = CliConfig::parse_args([
            "--demo-content",
            "--new",
            "--seed",
            "1",
            "--save",
            "content.mcvx",
        ]);
        assert!(after_content.demo_content);
        assert!(after_content.force_new_world);
        assert_eq!(after_content.seed_override, Some(1));
        assert_eq!(
            after_content.save_path,
            std::path::PathBuf::from("content.mcvx")
        );

        // Both flags together arm both seams without cross-toggling.
        let both = CliConfig::parse_args(["--demo-survival", "--demo-content"]);
        assert!(both.demo_survival);
        assert!(both.demo_content);
    }

    #[test]
    fn test_demo_combat_parses_as_a_bare_switch() {
        // Absent keeps the seam off: a normal launch is byte-identical.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert!(!bare.demo_combat);
        assert!(!bare.demo_swim);
        assert!(!bare.demo_survival);
        assert!(!bare.demo_content);

        // The flag alone sets exactly its own seam and consumes no value.
        let combat = CliConfig::parse_args(["--demo-combat"]);
        assert!(combat.demo_combat);
        assert!(!combat.force_new_world);
        assert_eq!(combat.seed_override, None);
        assert_eq!(
            combat.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );

        // A following `--seed 5` is parsed as the seed, proving the bare
        // switch never swallowed the next token as its value.
        let with_seed = CliConfig::parse_args(["--demo-combat", "--seed", "5"]);
        assert!(with_seed.demo_combat);
        assert_eq!(with_seed.seed_override, Some(5));

        // A stray non-flag token after the switch is ignored like any other
        // unknown argument and never becomes the seed.
        let stray = CliConfig::parse_args(["--demo-combat", "5"]);
        assert!(stray.demo_combat);
        assert_eq!(stray.seed_override, None);
    }

    #[test]
    fn test_demo_ui_parses_as_a_bare_switch() {
        // Absent keeps the seam off: a normal launch is byte-identical, and
        // the other demo seams stay off too.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert!(!bare.demo_ui);
        assert!(!bare.demo_swim);
        assert!(!bare.demo_survival);
        assert!(!bare.demo_content);
        assert!(!bare.demo_combat);

        // The flag alone sets exactly its own seam and consumes no value.
        let ui = CliConfig::parse_args(["--demo-ui"]);
        assert!(ui.demo_ui);
        assert!(!ui.force_new_world);
        assert_eq!(ui.seed_override, None);
        assert_eq!(ui.save_path, std::path::PathBuf::from(DEFAULT_SAVE_PATH));

        // A following `--seed 5` is parsed as the seed, proving the bare
        // switch never swallowed the next token as its value.
        let with_seed = CliConfig::parse_args(["--demo-ui", "--seed", "5"]);
        assert!(with_seed.demo_ui);
        assert_eq!(with_seed.seed_override, Some(5));

        // A stray non-flag token after the switch is ignored like any other
        // unknown argument and never becomes the seed.
        let stray = CliConfig::parse_args(["--demo-ui", "5"]);
        assert!(stray.demo_ui);
        assert_eq!(stray.seed_override, None);
    }

    #[test]
    fn test_demo_presentation_parses_as_a_bare_switch() {
        // Absent keeps the seam off, and every other demo seam stays off too.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert!(!bare.demo_presentation);
        assert!(!bare.demo_ui);
        assert!(!bare.demo_swim);
        assert!(!bare.demo_survival);
        assert!(!bare.demo_content);
        assert!(!bare.demo_combat);

        // The flag alone sets exactly its own seam and consumes no value.
        let presentation = CliConfig::parse_args(["--demo-presentation"]);
        assert!(presentation.demo_presentation);
        assert!(!presentation.demo_ui);
        assert!(!presentation.force_new_world);
        assert_eq!(presentation.seed_override, None);
        assert_eq!(
            presentation.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );

        // A following `--save fixture.mcvx` is parsed as the save path, not
        // swallowed as the switch's value (the fixture-path contract).
        let with_save = CliConfig::parse_args(["--demo-presentation", "--save", "fixture.mcvx"]);
        assert!(with_save.demo_presentation);
        assert_eq!(
            with_save.save_path,
            std::path::PathBuf::from("fixture.mcvx")
        );

        // A stray non-flag token after the switch never becomes the seed.
        let stray = CliConfig::parse_args(["--demo-presentation", "5"]);
        assert!(stray.demo_presentation);
        assert_eq!(stray.seed_override, None);
    }

    #[test]
    fn test_validate_pack_is_a_value_switch_and_launch_only() {
        // Absent keeps the seam off.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert_eq!(bare.validate_pack, None);

        // The switch consumes exactly the following token as the pack name.
        let one = CliConfig::parse_args(["--validate-pack", "mymod-ruby"]);
        assert_eq!(one.validate_pack.as_deref(), Some("mymod-ruby"));
        assert!(!one.force_new_world);
        assert_eq!(one.seed_override, None);
        assert_eq!(one.save_path, std::path::PathBuf::from(DEFAULT_SAVE_PATH));

        // A following flag is not swallowed as a value: the missing name
        // fails closed instead of becoming a pack called `--seed`.
        let missing = CliConfig::parse_args(["--validate-pack"]);
        assert_eq!(missing.validate_pack.as_deref(), Some(""));

        // Other arguments keep parsing independently around it.
        let with_seed = CliConfig::parse_args(["--validate-pack", "demo", "--seed", "5"]);
        assert_eq!(with_seed.validate_pack.as_deref(), Some("demo"));
        assert_eq!(with_seed.seed_override, Some(5));

        // It is a launch switch, never a world intent (which lives in
        // main.rs), and never persisted: parse result alone carries it.
        let world_flag = CliConfig::parse_args(["--new", "--validate-pack", "demo"]);
        assert!(world_flag.force_new_world);
        assert_eq!(world_flag.validate_pack.as_deref(), Some("demo"));
    }

    #[test]
    fn test_cli_mode_defaults_creative_and_bare_flags_are_last_wins() {
        // Absent flags keep the pre-M008 Creative default, so every existing
        // CLI demo/smoke keeps its flying start.
        let bare = CliConfig::parse_args(Vec::<String>::new());
        assert_eq!(bare.mode, GameMode::Creative);

        // Each flag alone selects its mode.
        assert_eq!(
            CliConfig::parse_args(["--survival"]).mode,
            GameMode::Survival
        );
        assert_eq!(
            CliConfig::parse_args(["--creative"]).mode,
            GameMode::Creative
        );

        // Both flags: the last one on the command line wins, in either order.
        assert_eq!(
            CliConfig::parse_args(["--survival", "--creative"]).mode,
            GameMode::Creative
        );
        assert_eq!(
            CliConfig::parse_args(["--creative", "--survival"]).mode,
            GameMode::Survival
        );
        assert_eq!(
            CliConfig::parse_args(["--survival", "--creative", "--survival"]).mode,
            GameMode::Survival
        );

        // Mode flags are launch parameters, not world intent: alone they
        // leave every world-selection field at its default (the main-menu
        // entry decision is asserted in main.rs via cli_has_world_intent).
        let survival = CliConfig::parse_args(["--survival"]);
        assert!(!survival.force_new_world);
        assert_eq!(survival.seed_override, None);
        assert_eq!(
            survival.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );
        // A mode flag consumes no value: the following token still parses as
        // its own flag.
        let flags = CliConfig::parse_args(["--survival", "--new", "--seed", "7"]);
        assert_eq!(flags.mode, GameMode::Survival);
        assert!(flags.force_new_world);
        assert_eq!(flags.seed_override, Some(7));
    }

    #[test]
    fn test_cli_args_ignore_malformed_and_incomplete_flags() {
        // A non-numeric seed is ignored; "--save" still consumes the following
        // token as its value, so "--new" never becomes a flag.
        let cfg = CliConfig::parse_args(["--seed", "not-a-number", "--save", "--new"]);
        assert_eq!(cfg.seed_override, None);
        assert_eq!(cfg.save_path, std::path::PathBuf::from("--new"));
        assert!(!cfg.force_new_world);

        // A flag with no following value keeps the default path and seed.
        let incomplete = CliConfig::parse_args(["--seed"]);
        assert_eq!(incomplete.seed_override, None);
        assert_eq!(
            incomplete.save_path,
            std::path::PathBuf::from(DEFAULT_SAVE_PATH)
        );

        // A seed that overflows u32 is rejected rather than truncated.
        let overflow = CliConfig::parse_args(["--seed", "4294967296"]);
        assert_eq!(overflow.seed_override, None);

        // A missing --time-scale value is lenient: it does not swallow the
        // default and does not panic.
        let missing_scale = CliConfig::parse_args(["--time-scale"]);
        assert_eq!(missing_scale.time_scale, DEFAULT_TIME_SCALE);
    }
    // ------------------------------------------------------------------
    // v8 content table, placeholders and tail
    // ------------------------------------------------------------------

    use crate::content::build_registry;
    use crate::content::spec::{BlockEntry, BlockTextures, ItemEntry, PackAsset, PackContentSpec};

    fn ruby_pack() -> (PackIdentity, PackContentSpec) {
        let mut spec = PackContentSpec::default();
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_ore".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ruby_ore.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.0,
            drop: None,
            tool: None,
            stack_limit: 64,
        });
        spec.items.push(ItemEntry {
            id: "mymod:ruby".to_string(),
            display_name: None,
            icon: "ruby.png".to_string(),
            stack_limit: 64,
            food: None,
        });
        spec.assets.insert(
            "ruby_ore.png".to_string(),
            PackAsset {
                rgba: vec![0x11; 1024],
                source_side: 16,
            },
        );
        spec.assets.insert(
            "ruby.png".to_string(),
            PackAsset {
                rgba: vec![0x22; 1024],
                source_side: 16,
            },
        );
        (
            PackIdentity {
                folder: "mymod-ruby".to_string(),
                version: Some("1.0".to_string()),
            },
            spec,
        )
    }

    fn ruby_registry() -> ContentRegistry {
        let (registry, report) = build_registry(ContentRegistry::builtin(), &[ruby_pack()]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    /// A registry with the same ruby item plus an authored tier and pack tool,
    /// so widened `ModTool` identities can be constructed in writer tests.
    fn ruby_tool_registry() -> ContentRegistry {
        use crate::content::spec::{TierEntry, ToolEntry};

        let mut spec = PackContentSpec {
            tiers: vec![TierEntry {
                id: "mymod:ruby".to_string(),
                display_name: Some("Ruby".to_string()),
                speed: 9.0,
                durability: 900,
                damage: 9,
            }],
            tools: vec![ToolEntry {
                id: "mymod:ruby_pickaxe".to_string(),
                display_name: Some("Ruby Pickaxe".to_string()),
                kind: ToolKind::Pickaxe,
                tier: "mymod:ruby".to_string(),
                icon: "ruby_pickaxe.png".to_string(),
            }],
            ..PackContentSpec::default()
        };
        spec.assets.insert(
            "ruby_pickaxe.png".to_string(),
            PackAsset {
                rgba: vec![0x33; 1024],
                source_side: 16,
            },
        );
        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "mymod-ruby-tool".to_string(),
                    version: None,
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    #[test]
    fn pack_tools_fail_closed_in_the_legacy_writer_and_name_in_v8() {
        let registry = ruby_tool_registry();
        let tool = Item::new_with(&registry, ItemId::ModTool(0), 1).expect("pack tool");
        assert_eq!(tool.durability(), Some(900));

        // The legacy writer keeps rejecting the widened identity before any
        // byte; the v1-v7 wire format must never truncate a tool index.
        let mut out = Vec::new();
        assert!(matches!(
            write_item(&mut out, Some(tool)),
            Err(PersistenceError::UnsupportedItem)
        ));
        assert!(out.is_empty(), "legacy writer must reject before writing");
        assert!(matches!(
            content_name_of_item(&registry, tool),
            Err(PersistenceError::UnsupportedItem)
        ));

        // The v8 name-table pass names the pack tool through its authored
        // stable id instead of failing closed (T05).
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(tool);
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let data = sample_data(player);
        let table = ContentNameTable::collect(&data, &registry).expect("v8 names pack tools");
        assert_eq!(
            table
                .index_of("mymod:ruby_pickaxe")
                .expect("tool name is tabled"),
            0
        );
    }

    /// One inventory slot holding a pack tool at `durability`.
    fn pack_tool_player(registry: &ContentRegistry, durability: u16) -> SavedPlayerState {
        let tool = Item::tool_with_durability_with(registry, ItemId::ModTool(0), durability)
            .expect("pack tool");
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(tool);
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        player
    }

    /// Absolute offset of the first v8 inventory slot for a save with no
    /// chunk edits: header + content table + hotbar + time + chunk count.
    fn v8_first_inventory_slot(bytes: &[u8]) -> usize {
        let mut cur = Cursor::new(&bytes[36..]);
        let count = usize::try_from(read_u32(&mut cur).unwrap()).unwrap();
        let mut offset = 40usize;
        for _ in 0..count {
            let len = usize::from(read_u16(&mut cur).unwrap());
            cur.set_position(cur.position() + len as u64);
            offset += 2 + len;
        }
        offset + HOTBAR_SLOTS * 3 + 4 + 4
    }

    #[test]
    fn v8_roundtrips_pack_tool_identity_and_durability() {
        let registry = ruby_tool_registry();
        let data = sample_data(pack_tool_player(&registry, 875));

        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &data, &registry).expect("v8 names a pack tool");
        assert_eq!(v8_content_names(&bytes), vec!["mymod:ruby_pickaxe"]);

        let loaded = load_world_from_bytes_with(&bytes, &registry).expect("pack tool loads");
        assert_eq!(loaded.unresolved, 0);
        let tool = loaded.data.player.inventory.slots()[0].expect("tool survived");
        assert_eq!(tool.id(), ItemId::ModTool(0));
        assert_eq!(tool.count(), 1);
        assert_eq!(tool.durability(), Some(875));
        assert_eq!(tool.max_durability(), Some(900));

        let mut second = Vec::new();
        save_world_to_bytes_with(&mut second, &loaded.data, &loaded.registry).unwrap();
        assert_eq!(
            second, bytes,
            "a pack-tool reload must re-save byte-identically"
        );
    }

    #[test]
    fn v8_missing_pack_degrades_a_pack_tool_to_a_count_one_placeholder() {
        let registry = ruby_tool_registry();
        let data = sample_data(pack_tool_player(&registry, 875));
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &data, &registry).unwrap();

        let missing = load_world_from_bytes_with(&bytes, ContentRegistry::builtin())
            .expect("a missing pack never blocks the world");
        assert_eq!(missing.unresolved, 1);
        let placeholder = missing.data.player.inventory.slots()[0]
            .expect("the degraded tool still occupies its slot");
        assert_eq!(placeholder.id(), ItemId::Item(0));
        assert_eq!(placeholder.count(), 1);
        let def = missing
            .registry
            .item(0)
            .expect("the placeholder item is registered");
        assert_eq!(def.name, "mymod:ruby_pickaxe");
        assert!(def.placeholder);

        // Restoring the pack resolves the same wire name back to the live
        // tool instead of leaving the placeholder behind.
        let restored = load_world_from_bytes_with(&bytes, &registry).expect("pack resolves");
        assert_eq!(restored.unresolved, 0);
        let tool = restored.data.player.inventory.slots()[0].expect("tool");
        assert_eq!(tool.id(), ItemId::ModTool(0));
        assert_eq!(tool.durability(), Some(875));
    }

    #[test]
    fn v8_clamps_pack_tool_durability_but_builtin_tools_stay_strict() {
        let registry = ruby_tool_registry();
        let data = sample_data(pack_tool_player(&registry, 900));
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &data, &registry).unwrap();
        let slot = v8_first_inventory_slot(&bytes);
        assert_eq!(bytes[slot], ITEM_TAG_MOD_TOOL);
        let durability_at = slot + 3;
        bytes[durability_at..durability_at + 2].copy_from_slice(&901u16.to_le_bytes());

        let loaded = load_world_from_bytes_with(&bytes, &registry).expect("901 clamps to 900");
        let tool = loaded.data.player.inventory.slots()[0].expect("tool survived");
        assert_eq!(tool.durability(), Some(900));
        assert_eq!(tool.max_durability(), Some(900));

        // Zero durability is never a legal tool state, even for packs.
        let mut zero = bytes.clone();
        zero[durability_at..durability_at + 2].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&zero, &registry),
            Err(PersistenceError::InvalidItem(ItemError::InvalidDurability))
        ));

        // Built-in tools keep the strict reject-above-max policy.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 250)
                .expect("full iron pickaxe"),
        );
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let mut builtin = Vec::new();
        save_world_to_bytes_with(
            &mut builtin,
            &sample_data(player),
            ContentRegistry::builtin(),
        )
        .unwrap();
        let slot = v8_first_inventory_slot(&builtin);
        assert_eq!(builtin[slot], ITEM_TAG_TOOL);
        let durability_at = slot + 3;
        builtin[durability_at..durability_at + 2].copy_from_slice(&251u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&builtin, ContentRegistry::builtin()),
            Err(PersistenceError::InvalidItem(ItemError::InvalidDurability))
        ));
    }

    #[test]
    fn v8_roundtrips_pack_tool_in_cursor_and_crafting_grids() {
        let registry = ruby_tool_registry();
        let tool =
            Item::tool_with_durability_with(&registry, ItemId::ModTool(0), 42).expect("pack tool");
        let mut player = sample_player();
        player.inventory = Inventory::from_parts([None; INVENTORY_SLOTS], Some(tool));
        player.player_grid.set(0, Some(tool)).unwrap();
        player.table_grid.set(15, Some(tool)).unwrap();
        let data = sample_data(player);

        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &data, &registry).expect("v8 names grid tools");
        let loaded = load_world_from_bytes_with(&bytes, &registry).unwrap();
        assert_eq!(loaded.unresolved, 0);
        assert_eq!(loaded.data.player.inventory.cursor(), Some(tool));
        assert_eq!(loaded.data.player.player_grid.slot(0), Some(tool));
        assert_eq!(loaded.data.player.table_grid.slot(15), Some(tool));

        // The same single name degrades to one placeholder shared by all
        // three records; it is counted once, like any repeated item name.
        let missing = load_world_from_bytes_with(&bytes, ContentRegistry::builtin()).unwrap();
        assert_eq!(missing.unresolved, 1);
        let degraded = Item::new(ItemId::Item(0), 1).expect("placeholder item");
        assert_eq!(missing.data.player.inventory.cursor(), Some(degraded));
        assert_eq!(missing.data.player.player_grid.slot(0), Some(degraded));
        assert_eq!(missing.data.player.table_grid.slot(15), Some(degraded));
    }

    fn v8_data_with_ruby_content() -> WorldSaveData {
        let mut edits = ChunkEditMap::default();
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(10, BlockId::Modded(BUILTIN_BLOCK_COUNT));
        edits.insert(ChunkPos::new(1, 2, 3), chunk);

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Item(0), 5).expect("ruby stack is valid"));
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);

        let mut data = sample_data(player);
        data.chunk_edits = edits;
        data
    }

    /// Decodes the v8 header table straight out of the wire bytes.
    fn v8_content_names(bytes: &[u8]) -> Vec<String> {
        let mut cur = Cursor::new(&bytes[36..]);
        let count = usize::try_from(read_u32(&mut cur).unwrap()).unwrap();
        let mut names = Vec::new();
        for _ in 0..count {
            let len = usize::from(read_u16(&mut cur).unwrap());
            let mut buf = vec![0u8; len];
            cur.read_exact(&mut buf).unwrap();
            names.push(String::from_utf8(buf).unwrap());
        }
        names
    }

    fn v8_bytes_with_stone_and_stick() -> Vec<u8> {
        let mut edits = ChunkEditMap::default();
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(10, BlockId::Stone);
        edits.insert(ChunkPos::new(0, 0, 0), chunk);

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Material(Material::Stick), 3).unwrap());
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);

        let mut data = sample_data(player);
        data.chunk_edits = edits;
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &data, ContentRegistry::builtin()).unwrap();
        bytes
    }

    // Offsets into the stone+stick payload:
    // table count 36..40, "stick" 40..47, "stone" 47..54, hotbar 54..81,
    // time 81..85, chunk_count 85..89, chunk pos 89..101, edit_count
    // 101..105, local index 105..107, name index 107..109.
    const V8_PAIR_TABLE_COUNT: usize = 36;
    const V8_PAIR_FIRST_NAME_LEN: usize = 40;
    const V8_PAIR_FIRST_NAME_BYTES: usize = 42;
    const V8_PAIR_SECOND_NAME_BYTES: usize = 49;
    const V8_PAIR_FIRST_EDIT_NAME_INDEX: usize = 107;

    // Ruby payload: "mymod:ruby" (10) then "mymod:ruby_ore" (14).
    // Chunk edit name index sits at 121.
    const V8_RUBY_FIRST_EDIT_NAME_INDEX: usize = 121;

    #[test]
    fn v8_first_proof_roundtrips_names_and_placeholders() {
        let pack_registry = ruby_registry();
        let data = v8_data_with_ruby_content();

        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &data, &pack_registry).expect("v8 save must succeed");
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());

        let names = v8_content_names(&bytes);
        assert_eq!(names, vec!["mymod:ruby", "mymod:ruby_ore"]);
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "table must be duplicate-free");
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "table must be sorted by UTF-8 bytes: {names:?}"
        );

        let mut second = Vec::new();
        save_world_to_bytes_with(&mut second, &data, &pack_registry).unwrap();
        assert_eq!(second, bytes, "v8 writes are deterministic");

        // Load with the built-in registry: both names become placeholders.
        let loaded = load_world_from_bytes_with(&bytes, ContentRegistry::builtin())
            .expect("v8 with placeholders must load");
        assert_eq!(loaded.unresolved, 2);

        let block_def = loaded
            .registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .expect("placeholder block is registered");
        assert_eq!(block_def.name, "mymod:ruby_ore");
        assert!(block_def.placeholder);
        assert_eq!(block_def.drop, None);
        let placeholder_block = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        assert!(placeholder_block.is_solid());
        assert_eq!(placeholder_block.break_duration_secs(), None);

        let item_def = loaded
            .registry
            .item(0)
            .expect("placeholder item is registered");
        assert_eq!(item_def.name, "mymod:ruby");
        assert!(item_def.placeholder);
        assert_eq!(item_def.icon_layer, crate::content::PLACEHOLDER_ICON_LAYER);
        let stack = Item::new(ItemId::Item(0), 5).expect("placeholder items stack");
        assert_eq!(stack.count(), 5);

        let loaded_chunk = loaded
            .data
            .chunk_edits
            .get(&ChunkPos::new(1, 2, 3))
            .expect("ruby ore edit survived");
        assert_eq!(loaded_chunk[&10], placeholder_block);
        assert_eq!(
            loaded.data.player.inventory.slots()[0],
            Some(stack),
            "placeholder ruby stack survived"
        );

        // Re-saving with the placeholder registry preserves the names (the
        // pack tail is empty because placeholders never contribute packs), and
        // a fresh built-in load yields the same names and placeholder count.
        let mut again = Vec::new();
        save_world_to_bytes_with(&mut again, &loaded.data, &loaded.registry).unwrap();
        assert_eq!(&again[4..8], &SAVE_VERSION.to_le_bytes());
        assert_eq!(v8_content_names(&again), names);
        let reloaded = load_world_from_bytes_with(&again, ContentRegistry::builtin()).unwrap();
        assert_eq!(reloaded.unresolved, 2);
        assert_eq!(
            reloaded
                .registry
                .block_name(BlockId::Modded(BUILTIN_BLOCK_COUNT)),
            Some("mymod:ruby_ore")
        );

        // Reloading with the loaded registry appends nothing new.
        let stable = load_world_from_bytes_with(&again, &loaded.registry).unwrap();
        assert_eq!(stable.unresolved, 0);
        assert_eq!(
            stable.registry.blocks().len(),
            loaded.registry.blocks().len()
        );
        assert_eq!(stable.registry.items().len(), loaded.registry.items().len());

        // Loading with the pack registry resolves the real definitions.
        let resolved = load_world_from_bytes_with(&bytes, &pack_registry).unwrap();
        assert_eq!(resolved.unresolved, 0);
        let real = resolved
            .registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .unwrap();
        assert_eq!(real.name, "mymod:ruby_ore");
        assert!(!real.placeholder);
        assert_eq!(real.hardness, Some(1.0));
        let real_item = resolved.registry.item(0).unwrap();
        assert_eq!(real_item.name, "mymod:ruby");
        assert!(!real_item.placeholder);
        let meta = resolved.saved_content.expect("v8 metadata survives");
        assert_eq!(meta.digest, pack_registry.digest());
        assert_eq!(meta.packs, pack_registry.packs());

        // A pack-registry load re-saves byte-identically.
        let mut resolved_bytes = Vec::new();
        save_world_to_bytes_with(&mut resolved_bytes, &resolved.data, &resolved.registry).unwrap();
        assert_eq!(resolved_bytes, bytes);
    }

    #[test]
    fn loaded_world_content_notice_is_none_for_legacy_and_matching_stacks() {
        // v1-v7 carries no pack identity or digest, so it never warns.
        let legacy =
            load_world_from_bytes_with(&hand_built_v1_bytes(), ContentRegistry::builtin()).unwrap();
        assert!(legacy.saved_content.is_none());
        assert_eq!(legacy.content_notice(), None);

        let v7 = legacy_world_bytes(&migration_data(7), 7);
        let v7 = load_world_from_bytes_with(&v7, ContentRegistry::builtin()).unwrap();
        assert!(v7.saved_content.is_none());
        assert_eq!(v7.content_notice(), None);

        // A v8 save loaded against the exact same stack is silent.
        let pack_registry = ruby_registry();
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &v8_data_with_ruby_content(), &pack_registry).unwrap();
        let same = load_world_from_bytes_with(&bytes, &pack_registry).unwrap();
        assert_eq!(same.unresolved, 0);
        assert_eq!(same.content_notice(), None);

        // Removing the pack names it and counts the placeholders that stood in
        // for the two saved names, without blocking the load.
        let missing = load_world_from_bytes_with(&bytes, ContentRegistry::builtin()).unwrap();
        let notice = missing
            .content_notice()
            .expect("a changed stack yields a readable notice");
        assert!(notice.contains("missing pack `mymod-ruby`"), "{notice}");
        assert!(
            notice.contains("2 saved content names are unavailable and render as placeholders"),
            "{notice}"
        );
    }

    #[test]
    fn v8_roundtrips_builtin_content_without_placeholders() {
        // Builtin-only data (no modded ids) exercises the empty/plain table.
        let mut plain = sample_data(sample_player());
        plain.mobs = vec![SavedMob {
            kind: MobKind::Pig,
            position: [1.5, 64.0, -3.25],
            yaw: 0.25,
            health: 4,
        }];
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &plain, ContentRegistry::builtin()).unwrap();
        let loaded = load_world_from_bytes_with(&bytes, ContentRegistry::builtin()).unwrap();
        assert_eq!(loaded.unresolved, 0);
        assert_eq!(loaded.data, plain);
        assert_eq!(loaded.data.mobs, plain.mobs);
        let meta = loaded.saved_content.expect("v8 carries content metadata");
        assert!(meta.packs.is_empty());
        assert_eq!(meta.digest, crate::content::EMPTY_DIGEST);

        // Content tables are only as large as the referenced names.
        let mut empty = Vec::new();
        save_world_to_bytes_with(
            &mut empty,
            &sample_data(sample_player()),
            ContentRegistry::builtin(),
        )
        .unwrap();
        assert!(v8_content_names(&empty).is_empty());
        assert_eq!(&empty[36..40], &0u32.to_le_bytes());
    }

    #[test]
    fn v8_writer_rejects_unnameable_content_before_first_byte() {
        let mut edits = ChunkEditMap::default();
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(10, BlockId::Modded(9999));
        edits.insert(ChunkPos::new(0, 0, 0), chunk);
        let mut data = sample_data(sample_player());
        data.chunk_edits = edits;
        let mut out = Vec::new();
        assert!(matches!(
            save_world_to_bytes_with(&mut out, &data, ContentRegistry::builtin()),
            Err(PersistenceError::InvalidBlockId(9999))
        ));
        assert!(out.is_empty(), "a rejected save must not emit bytes");

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Item(9999), 1).unwrap());
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let mut out = Vec::new();
        assert!(matches!(
            save_world_to_bytes_with(&mut out, &sample_data(player), ContentRegistry::builtin()),
            Err(PersistenceError::UnknownItemIndex(9999))
        ));
        assert!(out.is_empty(), "a rejected save must not emit bytes");
    }

    #[test]
    fn v8_rejects_malformed_content_tables() {
        let base = ContentRegistry::builtin();

        let mut too_many = v8_bytes_with_stone_and_stick();
        too_many[V8_PAIR_TABLE_COUNT..V8_PAIR_TABLE_COUNT + 4]
            .copy_from_slice(&(MAX_SAVE_CONTENT_NAMES as u32 + 1).to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&too_many, base),
            Err(PersistenceError::InvalidContentTable)
        ));

        let mut zero = v8_bytes_with_stone_and_stick();
        zero[V8_PAIR_FIRST_NAME_LEN..V8_PAIR_FIRST_NAME_LEN + 2]
            .copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&zero, base),
            Err(PersistenceError::InvalidContentTable)
        ));

        let mut oversized = v8_bytes_with_stone_and_stick();
        oversized[V8_PAIR_FIRST_NAME_LEN..V8_PAIR_FIRST_NAME_LEN + 2]
            .copy_from_slice(&256u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&oversized, base),
            Err(PersistenceError::InvalidContentTable)
        ));

        let mut utf8 = v8_bytes_with_stone_and_stick();
        utf8[V8_PAIR_FIRST_NAME_BYTES] = 0xff;
        assert!(matches!(
            load_world_from_bytes_with(&utf8, base),
            Err(PersistenceError::InvalidContentTable)
        ));

        let mut duplicate = v8_bytes_with_stone_and_stick();
        duplicate[V8_PAIR_SECOND_NAME_BYTES..V8_PAIR_SECOND_NAME_BYTES + 5]
            .copy_from_slice(b"stick");
        assert!(matches!(
            load_world_from_bytes_with(&duplicate, base),
            Err(PersistenceError::InvalidContentTable)
        ));

        let mut unsorted = v8_bytes_with_stone_and_stick();
        unsorted[V8_PAIR_FIRST_NAME_BYTES..V8_PAIR_FIRST_NAME_BYTES + 5].copy_from_slice(b"stone");
        unsorted[V8_PAIR_SECOND_NAME_BYTES..V8_PAIR_SECOND_NAME_BYTES + 5]
            .copy_from_slice(b"stick");
        assert!(matches!(
            load_world_from_bytes_with(&unsorted, base),
            Err(PersistenceError::InvalidContentTable)
        ));

        let mut index = v8_bytes_with_stone_and_stick();
        index[V8_PAIR_FIRST_EDIT_NAME_INDEX..V8_PAIR_FIRST_EDIT_NAME_INDEX + 2]
            .copy_from_slice(&9999u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&index, base),
            Err(PersistenceError::InvalidContentTable)
        ));
    }

    #[test]
    fn v8_rejects_cross_kind_names() {
        // The ruby test names sort as ["mymod:ruby", "mymod:ruby_ore"], so the
        // writer stores index 1 for the block edit. Patching it to index 0 asks
        // the pack registry to resolve its item name as a block.
        let pack_registry = ruby_registry();
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &v8_data_with_ruby_content(), &pack_registry).unwrap();
        bytes[V8_RUBY_FIRST_EDIT_NAME_INDEX..V8_RUBY_FIRST_EDIT_NAME_INDEX + 2]
            .copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes_with(&bytes, &pack_registry),
            Err(PersistenceError::InvalidContentName(name)) if name == "mymod:ruby"
        ));
    }

    #[test]
    fn v8_rejects_truncated_and_trailing_tails() {
        let mut bytes = Vec::new();
        save_world_to_bytes_with(&mut bytes, &v8_data_with_ruby_content(), &ruby_registry())
            .unwrap();

        let truncated = &bytes[..bytes.len() - 4];
        assert!(matches!(
            load_world_from_bytes_with(truncated, ContentRegistry::builtin()),
            Err(PersistenceError::Io(_))
        ));

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            load_world_from_bytes_with(&trailing, ContentRegistry::builtin()),
            Err(PersistenceError::InvalidContentMeta)
        ));
    }

    // ------------------------------------------------------------------
    // v1-v7 migration into the public v8 format
    // ------------------------------------------------------------------

    /// Representative migrated world for the legacy encoder: chunk edit,
    /// inventory and crafting grids, plus the chest/mob tails when the target
    /// version can carry them.
    fn migration_data(version: u32) -> WorldSaveData {
        let mut data = sample_data(rich_player());
        data.time_of_day = 4_321;
        data.day_index = 9;
        data.season_length_days = 3;
        data.mode = GameMode::Survival;
        data.player.fly_mode = false;
        data.player.survival = SurvivalState::new(7, 3, 12).unwrap();

        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(10, BlockId::Stone);
        data.chunk_edits.insert(ChunkPos::new(2, -1, 3), chunk);

        if version >= SURVIVAL_VERSION {
            let mut chests = ChestMap::default();
            chests.insert(IVec3::new(1, 2, 3), survival_chest());
            data.chests = chests;
        }
        if version >= MOB_VERSION {
            data.mobs = pinned_mobs();
        }
        data
    }

    /// Loads the fixture for one legacy version, then proves the public v8
    /// path is deterministic: v -> load -> v8 -> load -> v8 is byte-stable and
    /// never changes the decoded domain values.
    fn assert_migrates_to_v8(fixture: &[u8], version: u32) {
        let loaded =
            load_world_from_bytes(fixture).expect("legacy fixture must load before migration");

        let mut v8 = Vec::new();
        save_world_to_bytes(&mut v8, &loaded)
            .unwrap_or_else(|error| panic!("v{version} -> v8 save failed: {error:?}"));
        assert_eq!(
            &v8[4..8],
            &SAVE_VERSION.to_le_bytes(),
            "v{version} must re-save as v8"
        );

        let reloaded = load_world_from_bytes(&v8)
            .unwrap_or_else(|error| panic!("v{version} v8 reload failed: {error:?}"));
        assert_eq!(
            reloaded, loaded,
            "v{version} -> v8 -> v8 must preserve every domain value"
        );

        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &reloaded).expect("v8 re-save must succeed");
        assert_eq!(again, v8, "v{version} migration must be byte-stable");
    }

    #[test]
    fn v1_to_v7_fixtures_migrate_to_v8_and_resave_byte_stably() {
        assert_migrates_to_v8(&hand_built_v1_bytes(), 1);
        assert_migrates_to_v8(&valid_v2_bytes(), 2);
        assert_migrates_to_v8(&hand_built_v3_bytes(&hotbar_sample()), 3);
        for version in 4..=7 {
            let fixture = legacy_world_bytes(&migration_data(version), version);
            assert_migrates_to_v8(&fixture, version);
        }
    }

    #[test]
    fn v7_migration_writes_a_sorted_unique_builtin_content_table() {
        let fixture = legacy_world_bytes(&migration_data(7), 7);
        let loaded = load_world_from_bytes(&fixture).expect("v7 fixture must load");
        let mut v8 = Vec::new();
        save_world_to_bytes(&mut v8, &loaded).expect("v7 -> v8 save must succeed");

        let names = v8_content_names(&v8);
        assert_eq!(
            names,
            vec![
                "apple",
                "bread",
                "coal_ore",
                "iron_pickaxe",
                "oak_log",
                "oak_planks",
                "stick",
                "stone",
                "stone_axe",
            ],
            "the table contains exactly the referenced built-in names, sorted"
        );
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "table must be duplicate-free");
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "table must be sorted by UTF-8 bytes: {names:?}"
        );

        // Every reference resolves without placeholders against the built-in
        // registry, and the saved tail carries no packs plus the builtin digest.
        let reloaded = load_world_from_bytes_with(&v8, ContentRegistry::builtin()).unwrap();
        assert_eq!(reloaded.unresolved, 0);
        let meta = reloaded.saved_content.expect("v8 metadata survives");
        assert!(meta.packs.is_empty());
        assert_eq!(meta.digest, crate::content::EMPTY_DIGEST);
        assert_eq!(reloaded.data, loaded);
    }

    #[test]
    fn v5_v6_v7_tails_still_decode_after_retargeting() {
        // v5: the calendar tail arrives; every later tail defaults.
        let v5_data = migration_data(5);
        let v5 = load_world_from_bytes(&legacy_world_bytes(&v5_data, 5)).unwrap();
        assert_eq!(v5.day_index, 9);
        assert_eq!(v5.season_length_days, 3);
        assert_v6_migration_defaults(&v5);

        // v6: vitals and the sorted chest records arrive; mobs stay empty.
        let v6_data = migration_data(6);
        let v6 = load_world_from_bytes(&legacy_world_bytes(&v6_data, 6)).unwrap();
        assert_eq!(v6.mode, GameMode::Survival);
        assert_eq!(v6.player.survival, SurvivalState::new(7, 3, 12).unwrap());
        assert_eq!(v6.chests, v6_data.chests);
        assert!(v6.mobs.is_empty());

        // v7: the mob tail arrives too.
        let v7_data = migration_data(7);
        let v7 = load_world_from_bytes(&legacy_world_bytes(&v7_data, 7)).unwrap();
        assert_eq!(v7.mobs, pinned_mobs());
        assert_eq!(v7.chests, v7_data.chests);
    }
}
