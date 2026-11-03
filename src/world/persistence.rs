use std::fmt;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use glam::IVec3;
use rustc_hash::FxHashMap;

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
pub const SAVE_VERSION: u32 = 7;
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

pub fn save_world_to_bytes(
    out: &mut Vec<u8>,
    data: &WorldSaveData,
) -> Result<(), PersistenceError> {
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
    out.extend_from_slice(&SAVE_VERSION.to_le_bytes());
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
        },
    }
    Ok(())
}

fn write_grid(out: &mut Vec<u8>, grid: &CraftingGrid) -> Result<(), PersistenceError> {
    out.push(grid_kind_tag(grid.kind()));
    for index in 0..grid.kind().slot_count() {
        write_item(out, grid.slot(index))?;
    }
    Ok(())
}

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

pub fn load_world_from_bytes(bytes: &[u8]) -> Result<WorldSaveData, PersistenceError> {
    let mut cur = Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic)?;
    if &magic != SAVE_MAGIC {
        return Err(PersistenceError::InvalidMagic);
    }

    let version = read_u32(&mut cur)?;
    if !(1..=SAVE_VERSION).contains(&version) {
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

pub fn save_world_to_path(path: &Path, data: &WorldSaveData) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut bytes = Vec::with_capacity(1024);
    save_world_to_bytes(&mut bytes, data)?;

    let tmp_path = path.with_extension("mcvx.tmp");
    {
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn load_world_from_path(path: &Path) -> Result<WorldSaveData, PersistenceError> {
    let bytes = fs::read(path)?;
    load_world_from_bytes(&bytes)
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

        assert_eq!(SAVE_VERSION, MOB_VERSION);
        assert_eq!(MOB_VERSION, SURVIVAL_VERSION + 1);
        assert_eq!(SURVIVAL_VERSION, SEASON_VERSION + 1);
        assert_eq!(SEASON_VERSION, INVENTORY_EXTENSION_VERSION + 1);
        assert_eq!(SAVE_VERSION, 7);
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

        let mut valid = Vec::new();
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
        save_world_to_bytes(&mut valid, &data).unwrap();

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

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("stone edit must be saveable");
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");

        // Header layout: 36 bytes through render_distance, then 9 * 3
        // hotbar bytes, then the v3 time u32, then chunk_count (4 bytes),
        // then the v4 inventory extension. The sample has four block hotbar
        // slots, each expanding its 1-byte empty record to a 4-byte block
        // record (+3 bytes each).
        assert_eq!(
            bytes.len(),
            V4_EXTENSION_OFFSET
                + V4_EXTENSION_LEN
                + V5_SEASON_LEN
                + V6_HEADER_LEN
                + 4 * 3
                + V7_MOB_COUNT_LEN
        );
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());

        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.player, data.player);
        assert_eq!(loaded.player.selected_slot, 4);
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(
                loaded.player.legacy_hotbar().stack(slot),
                hotbar.stack(slot),
                "slot {slot} must round-trip"
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

    #[test]
    fn test_v1_save_migrates_to_empty_hotbar() {
        // Hand-build the old v1 layout (no 27-byte hotbar block).
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");
        assert_eq!(
            &bytes[V3_TIME_OFFSET..V3_TIME_OFFSET + 4],
            &12_345u32.to_le_bytes(),
            "time-of-day must sit immediately after the hotbar block"
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
        // empty sentinel while the v4 extension is authoritative.
        let header_slot = V2_HOTBAR_OFFSET + 3;
        assert_eq!(bytes[header_slot], BlockId::Air.to_raw() as u8);
        assert_eq!(
            &bytes[header_slot + 1..header_slot + 3],
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();

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

    /// Build a byte-valid v4 save: the current writer's payload with the
    /// version field patched back to 4 and the v5+v6+v7 tails removed.
    fn v4_bytes_from_current(data: &WorldSaveData) -> Vec<u8> {
        assert!(data.chests.is_empty(), "v4 has no chest section");
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, data).unwrap();
        bytes.truncate(
            bytes.len()
                - V6_HEADER_LEN
                - V5_SEASON_LEN
                - V7_MOB_COUNT_LEN
                - SAVED_MOB_LEN * data.mobs.len(),
        );
        bytes[4..8].copy_from_slice(&INVENTORY_EXTENSION_VERSION.to_le_bytes());
        bytes
    }

    /// Build a byte-valid v5 save: the current writer's payload with the
    /// version field patched back to 5 and the v6+v7 tails removed.
    fn v5_bytes_from_current(data: &WorldSaveData) -> Vec<u8> {
        assert!(data.chests.is_empty(), "v5 has no chest section");
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, data).unwrap();
        bytes.truncate(
            bytes.len() - V6_HEADER_LEN - V7_MOB_COUNT_LEN - SAVED_MOB_LEN * data.mobs.len(),
        );
        bytes[4..8].copy_from_slice(&SEASON_VERSION.to_le_bytes());
        bytes
    }

    /// Build a byte-valid v6 save: the current writer's payload with the
    /// version field patched back to 6 and the v7 mob tail removed.
    fn v6_bytes_from_current(data: &WorldSaveData) -> Vec<u8> {
        assert!(data.chests.is_empty(), "v6 helper has no chest section");
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, data).unwrap();
        bytes.truncate(bytes.len() - V7_MOB_COUNT_LEN - SAVED_MOB_LEN * data.mobs.len());
        bytes[4..8].copy_from_slice(&SURVIVAL_VERSION.to_le_bytes());
        bytes
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("save should succeed");

        assert_eq!(
            &bytes[V5_SEASON_OFFSET..V5_SEASON_OFFSET + 4],
            &17u32.to_le_bytes(),
            "day_index sits immediately after the v4 extension"
        );
        assert_eq!(
            &bytes[V5_SEASON_OFFSET + 4..V5_SEASON_OFFSET + V5_SEASON_LEN],
            &4u32.to_le_bytes(),
            "season_length_days follows day_index"
        );

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
        let bytes = v4_bytes_from_current(&data);

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
        let mut bytes = v4_bytes_from_current(&sample_data(sample_player()));
        bytes.push(0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    #[test]
    fn test_v5_rejects_trailing_bytes() {
        let data = sample_data(sample_player());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        bytes.push(0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
    }

    #[test]
    fn test_v5_truncated_calendar_tail_is_typed_io_error() {
        let data = sample_data(sample_player());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();

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
            let mut bytes = Vec::new();
            save_world_to_bytes(&mut bytes, &data).unwrap();
            bytes[V5_SEASON_OFFSET + 4..V5_SEASON_OFFSET + V5_SEASON_LEN]
                .copy_from_slice(&garbage.to_le_bytes());
            let loaded = load_world_from_bytes(&bytes).expect("clamp, not error");
            assert_eq!(
                loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS,
                "garbage season length {garbage} must clamp to the default"
            );
        }

        // The writer normalises the same way, keeping round-trips byte-stable.
        let mut data = sample_data(sample_player());
        data.season_length_days = 0;
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        let loaded = load_world_from_bytes(&bytes).expect("load should succeed");
        assert_eq!(loaded.season_length_days, DEFAULT_SEASON_LENGTH_DAYS);
    }

    fn v4_bytes_with_slot0(item: Item) -> Vec<u8> {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(item);
        let mut player = sample_player();
        player.inventory = Inventory::from_parts(slots, None);
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &sample_data(player)).unwrap();
        bytes
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        bytes
    }

    fn v6_bytes_with_one_empty_chest() -> Vec<u8> {
        let bytes = v6_bytes_with_chest(Container::empty());
        assert_eq!(
            bytes.len(),
            V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS + V7_MOB_COUNT_LEN,
            "one all-empty chest writes a fixed 12 + 27 byte record, plus the v7 count"
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

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();

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

        // Re-saving a loaded world reproduces the exact bytes, and insertion
        // order is irrelevant because records are sorted before writing.
        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded).unwrap();
        assert_eq!(again, bytes);

        let mut reversed = ChestMap::default();
        reversed.insert(IVec3::new(-1, 0, 3), Container::empty());
        reversed.insert(IVec3::new(4, -2, 7), survival_chest());
        let mut reversed_bytes = Vec::new();
        save_world_to_bytes(&mut reversed_bytes, &v6_data_with_chests(reversed)).unwrap();
        assert_eq!(
            reversed_bytes, bytes,
            "sorted chest records make the wire independent of insertion order"
        );
    }

    #[test]
    fn test_v6_present_empty_chest_is_distinct_from_missing_position() {
        // A missing position and a stored all-empty container are different
        // world states: only the latter survives a reload as a present record.
        let mut chests = ChestMap::default();
        chests.insert(IVec3::new(-1, 0, 3), Container::empty());
        let data = v6_data_with_chests(chests);

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
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
        // The present-empty record still occupies its full 12 + 27 bytes and
        // re-saves byte-identically; the v7 mob count follows it.
        assert_eq!(
            bytes.len(),
            V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS + V7_MOB_COUNT_LEN
        );
        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded).unwrap();
        assert_eq!(again, bytes);
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &v7_data_with_mobs(pinned_mobs())).unwrap();
        bytes
    }

    #[test]
    fn test_v7_roundtrips_mobs_and_pins_the_tail_layout() {
        // An edit-free, chest-free save keeps every older offset fixed and
        // puts the mob count at 149 with the first record at 153.
        assert_eq!(V7_MOB_COUNT_OFFSET, 149);
        assert_eq!(V7_FIRST_MOB_OFFSET, 153);
        assert_eq!(SAVED_MOB_LEN, 19);

        let data = v7_data_with_mobs(pinned_mobs());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v7 mob save must succeed");

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

        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded).expect("re-save must succeed");
        assert_eq!(again, bytes, "a loaded v7 save re-saves byte-identically");
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        assert_eq!(
            &bytes[V7_FIRST_MOB_OFFSET + 17..V7_FIRST_MOB_OFFSET + 19],
            &0u16.to_le_bytes()
        );
        let loaded = load_world_from_bytes(&bytes).unwrap();
        assert_eq!(loaded.mobs[0].health, 0);
        assert_eq!(loaded, data);
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

    /// Rebuilds a byte-valid v6 payload from an edit-free, chest-free v7
    /// payload: patch the version back and drop the four-byte mob count.
    fn v6_bytes_from_v7(bytes: &[u8]) -> Vec<u8> {
        let mob_count_at = bytes.len() - V7_MOB_COUNT_LEN;
        assert_eq!(
            &bytes[mob_count_at..],
            &0u32.to_le_bytes(),
            "helper only rewrites a chest-free v7 save with no mobs"
        );
        let mut bytes = bytes.to_vec();
        bytes.truncate(mob_count_at);
        bytes[4..8].copy_from_slice(&SURVIVAL_VERSION.to_le_bytes());
        bytes
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

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v7 item vocabulary must save");
        assert_eq!(&bytes[4..8], &ITEM_VOCAB_VERSION.to_le_bytes());

        let loaded = load_world_from_bytes(&bytes).expect("v7 item vocabulary must load");
        assert_eq!(loaded, data);

        for (index, (material, tag)) in V7_MELEE_MATERIALS.iter().enumerate() {
            // Slot-0 layout for a material record: tag, material tag, count.
            assert_eq!(bytes[V4_EXTENSION_OFFSET + index * 4], ITEM_TAG_MATERIAL);
            assert_eq!(bytes[V4_EXTENSION_OFFSET + index * 4 + 1], *tag);
            assert_eq!(
                loaded.player.inventory.slot(index),
                Some(v7_melee_item(*material, 2 + index as u16))
            );
            assert_eq!(
                loaded.chests[&IVec3::new(7, 65, -3)].slot(index),
                Some(v7_melee_item(*material, 5))
            );
        }

        // Slot-6/7 layout for a tool record: tag, kind, tier, durability u16.
        let sword = V4_EXTENSION_OFFSET + 6 * 4;
        assert_eq!(bytes[sword], ITEM_TAG_TOOL);
        assert_eq!(bytes[sword + 1], TOOL_KIND_SWORD);
        assert_eq!(bytes[sword + 2], TOOL_TIER_WOOD);
        assert_eq!(&bytes[sword + 3..sword + 5], &17u16.to_le_bytes());
        let bow = sword + 5;
        assert_eq!(bytes[bow], ITEM_TAG_TOOL);
        assert_eq!(bytes[bow + 1], TOOL_KIND_BOW);
        assert_eq!(bytes[bow + 2], TOOL_TIER_WOOD);
        assert_eq!(&bytes[bow + 3..bow + 5], &3u16.to_le_bytes());

        let mut again = Vec::new();
        save_world_to_bytes(&mut again, &loaded).expect("re-save must succeed");
        assert_eq!(
            again, bytes,
            "a loaded v7 item save re-saves byte-identically"
        );
    }

    #[test]
    fn test_v6_rejects_v7_only_material_and_combat_tool_records() {
        // Material tag 9 (Bone) was appended in v7: a pre-v7 payload that
        // claims it fails closed, while the identical v7 payload loads.
        let bone = v4_bytes_with_slot0(v7_melee_item(Material::Bone, 1));
        assert!(load_world_from_bytes(&bone).is_ok());
        assert!(matches!(
            load_world_from_bytes(&v6_bytes_from_v7(&bone)),
            Err(PersistenceError::InvalidMaterial(9))
        ));

        // Tool kind 4 (Sword) is likewise v7-only, and its v6 rejection is
        // independent of the v6-only Hoe gate.
        let sword = v4_bytes_with_slot0(
            Item::tool_with_durability(ToolKind::Sword, ToolTier::Wood, 17).unwrap(),
        );
        assert!(load_world_from_bytes(&sword).is_ok());
        assert!(matches!(
            load_world_from_bytes(&v6_bytes_from_v7(&sword)),
            Err(PersistenceError::InvalidToolKind(4))
        ));
    }

    #[test]
    fn test_v7_rejects_forged_stone_bow_record() {
        // `(Bow, Stone)` is not a constructible tool identity; persistence
        // decode must funnel through the checked constructor and report the
        // tier gate instead of accepting the forged pair.
        let sword = v4_bytes_with_slot0(
            Item::tool_with_durability(ToolKind::Sword, ToolTier::Wood, 17).unwrap(),
        );
        let mut bytes = sword;
        bytes[V4_EXTENSION_OFFSET + 1] = TOOL_KIND_BOW;
        bytes[V4_EXTENSION_OFFSET + 2] = TOOL_TIER_STONE;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidItem(ItemError::TierNotAllowed))
        ));
    }

    #[test]
    fn test_v7_edit_free_item_vocabulary_save_keeps_the_153_byte_baseline() {
        assert_eq!(ITEM_VOCAB_VERSION, MOB_VERSION);

        let data = sample_data(sample_player());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("edit-free v7 save must succeed");

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
    fn test_v6_save_loads_with_zero_mobs() {
        let data = v7_data_with_mobs(pinned_mobs());
        let bytes = v6_bytes_from_current(&data);
        let loaded = load_world_from_bytes(&bytes).expect("v6 must migrate, not fail");
        assert!(loaded.mobs.is_empty());
        assert_eq!(loaded.chests, data.chests);

        // Re-saving migrates the payload to v7 with an explicit zero count.
        let mut resaved = Vec::new();
        save_world_to_bytes(&mut resaved, &loaded).unwrap();
        assert_eq!(&resaved[4..8], &MOB_VERSION.to_le_bytes());
        assert_eq!(
            &resaved[V7_MOB_COUNT_OFFSET..V7_FIRST_MOB_OFFSET],
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
        assert_eq!(SAVE_VERSION, MOB_VERSION);
        assert_eq!(MOB_VERSION, SURVIVAL_VERSION + 1);
        assert_eq!(SURVIVAL_VERSION, SEASON_VERSION + 1);
        assert_eq!(SEASON_VERSION, INVENTORY_EXTENSION_VERSION + 1);
        assert_eq!(SAVE_VERSION, 7);
    }

    #[test]
    fn test_v6_accepts_zero_vitals_for_the_death_state() {
        let mut data = sample_data(sample_player());
        data.mode = GameMode::Survival;
        data.player.fly_mode = false;
        data.player.survival = SurvivalState::new(0, 0, 0).unwrap();

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        assert_eq!(
            &bytes[V6_HEALTH_OFFSET..V6_HEALTH_OFFSET + 2],
            &0u16.to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_HUNGER_OFFSET..V6_HUNGER_OFFSET + 2],
            &0u16.to_le_bytes()
        );
        assert_eq!(
            &bytes[V6_AIR_OFFSET..V6_AIR_OFFSET + 2],
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
        let bytes = v5_bytes_from_current(&data);

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

        // Duplicate positions are ambiguous state, not a merge. Insert the
        // copied record before the v7 mob count so the two chest records stay
        // contiguous.
        let mut bytes = v6_bytes_with_one_empty_chest();
        let record =
            bytes[V6_FIRST_CHEST_OFFSET..V6_FIRST_CHEST_OFFSET + 12 + CHEST_SLOTS].to_vec();
        let mob_count_offset = bytes.len() - V7_MOB_COUNT_LEN;
        bytes.splice(mob_count_offset..mob_count_offset, record);
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

        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).expect("v6 survival palette must save");
        assert_eq!(&bytes[4..8], &SAVE_VERSION.to_le_bytes());
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
            let bytes = v5_bytes_from_current(&v6_data_with_block_edit(block));
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
        let mut bytes = v5_bytes_from_current(&sample_data(sample_player()));
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

        let mut v6_bytes = Vec::new();
        save_world_to_bytes(&mut v6_bytes, &data).expect("v6 Chest item must save");
        let loaded = load_world_from_bytes(&v6_bytes).expect("v6 Chest item must load");
        assert_eq!(loaded.player.inventory, data.player.inventory);

        let v5_bytes = v5_bytes_from_current(&data);
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
            let bytes = v5_bytes_from_current(&sample_data(player));
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
        let bytes = v5_bytes_from_current(&sample_data(player));
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidToolKind(TOOL_KIND_HOE))
        ));
    }

    #[test]
    fn test_v4_rejects_unknown_item_tag() {
        let data = sample_data(sample_player());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
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
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();

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
}
