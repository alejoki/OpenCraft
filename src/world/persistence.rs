use std::fmt;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use crate::player::inventory::{
    CraftingGrid, GridKind, Hotbar, Inventory, Item, ItemError, ItemId, ItemStack, Material,
    ToolKind, ToolTier, HOTBAR_SLOTS, INVENTORY_SLOTS, MAX_GRID_SLOTS,
};
use crate::world::block::BlockId;
use crate::world::chunk::{ChunkPos, CHUNK_VOLUME};
use crate::world::daycycle::{DAY_TICKS, DEFAULT_TIME_OF_DAY};

pub const DEFAULT_SAVE_PATH: &str = "saves/default.mcvx";
pub const SAVE_MAGIC: &[u8; 4] = b"MCVX";
pub const SAVE_VERSION: u32 = 4;
/// First save version that carries the 27-byte hotbar header block.
pub const HOTBAR_HEADER_VERSION: u32 = 2;
/// First save version that carries the time-of-day `u32`.
pub const TIME_OF_DAY_VERSION: u32 = 3;
/// First save version that carries the inventory extension after the chunk
/// section (full 9 + 27 slots, cursor, and unfinished crafting grids).
pub const INVENTORY_EXTENSION_VERSION: u32 = 4;

// Stable wire discriminants for the version-4 inventory extension. These are
// part of the on-disk format; changing the in-memory enum order without
// updating these constants would silently corrupt saves.
const ITEM_TAG_EMPTY: u8 = 0;
const ITEM_TAG_BLOCK: u8 = 1;
const ITEM_TAG_MATERIAL: u8 = 2;
const ITEM_TAG_TOOL: u8 = 3;

const MATERIAL_STICK: u8 = 0;
const TOOL_KIND_PICKAXE: u8 = 0;
const TOOL_KIND_AXE: u8 = 1;
const TOOL_KIND_SHOVEL: u8 = 2;
const TOOL_TIER_WOOD: u8 = 0;
const TOOL_TIER_STONE: u8 = 1;
const TOOL_TIER_IRON: u8 = 2;

const GRID_KIND_PLAYER: u8 = 0;
const GRID_KIND_TABLE: u8 = 1;

/// Default acceleration scale for the game clock: 1.0 is the real 20-real-minute
/// cycle. `--time-scale` overrides it (a test seam for observing a full cycle in
/// a bounded run, MEM146).
pub const DEFAULT_TIME_SCALE: f32 = 1.0;

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

#[derive(Clone, Debug, PartialEq)]
pub struct WorldSaveData {
    pub seed: u32,
    /// Game time of day (0 = dawn, 6,000 = noon, 12,000 = dusk, 18,000 =
    /// midnight), persisted per world rather than derived from the wall
    /// clock. World state, so it sits next to `seed` rather than in
    /// `player`.
    pub time_of_day: u32,
    pub player: SavedPlayerState,
    pub chunk_edits: ChunkEditMap,
}

#[derive(Debug)]
pub enum PersistenceError {
    Io(std::io::Error),
    InvalidMagic,
    UnsupportedVersion(u32),
    InvalidBlockId(u8),
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
        }
    }
}

impl std::error::Error for PersistenceError {}

pub fn save_world_to_bytes(
    out: &mut Vec<u8>,
    data: &WorldSaveData,
) -> Result<(), PersistenceError> {
    // The two persisted grid positions are fixed, so a grid whose kind
    // disagrees with its slot would be unloadable. Validate before writing a
    // single byte so a rejected save leaves `out` untouched.
    if data.player.player_grid.kind() != GridKind::Player
        || data.player.table_grid.kind() != GridKind::Table
    {
        return Err(PersistenceError::InvalidCraftingState);
    }

    out.extend_from_slice(SAVE_MAGIC);
    out.extend_from_slice(&SAVE_VERSION.to_le_bytes());
    out.extend_from_slice(&data.seed.to_le_bytes());
    for c in data.player.position {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&data.player.yaw.to_le_bytes());
    out.extend_from_slice(&data.player.pitch.to_le_bytes());
    out.push(u8::from(data.player.fly_mode));
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
                out.push(stack.block as u8);
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
            out.push(block as u8);
        }
    }

    // v4 inventory extension, after every v3-known field: 36 validated slot
    // records, the cursor record, then the two unfinished crafting grids.
    write_inventory_extension(
        out,
        &data.player.inventory,
        &data.player.player_grid,
        &data.player.table_grid,
    );

    Ok(())
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
    }
}

fn material_from_tag(tag: u8) -> Option<Material> {
    match tag {
        MATERIAL_STICK => Some(Material::Stick),
        _ => None,
    }
}

fn tool_kind_tag(kind: ToolKind) -> u8 {
    match kind {
        ToolKind::Pickaxe => TOOL_KIND_PICKAXE,
        ToolKind::Axe => TOOL_KIND_AXE,
        ToolKind::Shovel => TOOL_KIND_SHOVEL,
    }
}

fn tool_kind_from_tag(tag: u8) -> Option<ToolKind> {
    match tag {
        TOOL_KIND_PICKAXE => Some(ToolKind::Pickaxe),
        TOOL_KIND_AXE => Some(ToolKind::Axe),
        TOOL_KIND_SHOVEL => Some(ToolKind::Shovel),
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
/// Items are already validated, so encoding cannot fail.
fn write_item(out: &mut Vec<u8>, item: Option<Item>) {
    match item {
        None => out.push(ITEM_TAG_EMPTY),
        Some(item) => match item.id() {
            ItemId::Block(block) => {
                out.push(ITEM_TAG_BLOCK);
                out.push(block as u8);
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
}

fn write_grid(out: &mut Vec<u8>, grid: &CraftingGrid) {
    out.push(grid_kind_tag(grid.kind()));
    for index in 0..grid.kind().slot_count() {
        write_item(out, grid.slot(index));
    }
}

fn write_inventory_extension(
    out: &mut Vec<u8>,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
) {
    for item in inventory.slots() {
        write_item(out, *item);
    }
    write_item(out, inventory.cursor());
    write_grid(out, player_grid);
    write_grid(out, table_grid);
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
        read_hotbar(&mut cur)?
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
                return Err(PersistenceError::InvalidBlockId(block_raw));
            };
            edits.insert(local_idx, block);
        }
        chunk_edits.insert(ChunkPos::new(cx, cy, cz), edits);
    }

    // v4 appends the validated inventory aggregate after every v3-known
    // field. Missing bytes are a truncated record; extra bytes mean the
    // payload does not match the declared version.
    let (inventory, player_grid, table_grid) = if version >= INVENTORY_EXTENSION_VERSION {
        let extension = read_inventory_extension(&mut cur)?;
        if cur.position() != bytes.len() as u64 {
            return Err(PersistenceError::InvalidPlayerState);
        }
        extension
    } else {
        // v1-v3 stored only the hotbar. Migrate it losslessly into the same
        // nine hotbar slots and leave all new state empty/default.
        (
            Inventory::from_legacy_hotbar(&hotbar),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        )
    };

    Ok(WorldSaveData {
        seed,
        time_of_day,
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
        },
        chunk_edits,
    })
}

/// Reads and strictly validates the v2 hotbar header block.
///
/// A zero block id is the empty-slot sentinel and must pair with count 0.
/// Any other id must decode, be carryable (not Air/Bedrock), and carry a
/// count within `1..=MAX_STACK_COUNT`. Short reads surface as `Io`.
fn read_hotbar<R: Read>(r: &mut R) -> Result<Hotbar, PersistenceError> {
    let mut slots: [Option<ItemStack>; crate::player::inventory::HOTBAR_SLOTS] =
        [None; crate::player::inventory::HOTBAR_SLOTS];

    for slot in slots.iter_mut() {
        let block_raw = read_u8(r)?;
        let count = read_u16(r)?;

        if block_raw == BlockId::Air as u8 {
            if count != 0 {
                return Err(PersistenceError::InvalidPlayerState);
            }
            continue;
        }

        let Some(block) = BlockId::from_u8(block_raw) else {
            return Err(PersistenceError::InvalidBlockId(block_raw));
        };
        if matches!(block, BlockId::Air | BlockId::Bedrock) {
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
fn read_item<R: Read>(r: &mut R) -> Result<Option<Item>, PersistenceError> {
    let tag = read_u8(r)?;
    match tag {
        ITEM_TAG_EMPTY => Ok(None),
        ITEM_TAG_BLOCK => {
            let block_raw = read_u8(r)?;
            let count = read_u16(r)?;
            let Some(block) = BlockId::from_u8(block_raw) else {
                return Err(PersistenceError::InvalidBlockId(block_raw));
            };
            Item::new(ItemId::Block(block), count)
                .map(Some)
                .map_err(PersistenceError::InvalidItem)
        }
        ITEM_TAG_MATERIAL => {
            let material_raw = read_u8(r)?;
            let count = read_u16(r)?;
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

fn read_grid<R: Read>(r: &mut R, expected: GridKind) -> Result<CraftingGrid, PersistenceError> {
    let kind_raw = read_u8(r)?;
    let kind = grid_kind_from_tag(kind_raw).ok_or(PersistenceError::InvalidGridKind(kind_raw))?;
    if kind != expected {
        return Err(PersistenceError::InvalidCraftingState);
    }

    let mut slots = [None; MAX_GRID_SLOTS];
    for slot in slots.iter_mut().take(kind.slot_count()) {
        *slot = read_item(r)?;
    }
    CraftingGrid::from_slots(kind, slots).map_err(|_| PersistenceError::InvalidCraftingState)
}

fn read_inventory_extension<R: Read>(
    r: &mut R,
) -> Result<(Inventory, CraftingGrid, CraftingGrid), PersistenceError> {
    let mut slots = [None; INVENTORY_SLOTS];
    for slot in slots.iter_mut() {
        *slot = read_item(r)?;
    }
    let cursor = read_item(r)?;
    let player_grid = read_grid(r, GridKind::Player)?;
    let table_grid = read_grid(r, GridKind::Table)?;
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
    use crate::world::block::BlockId;
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
            },
            chunk_edits: edits,
        };

        let tmp_path = std::env::temp_dir().join("mcvx_test_roundtrip.mcvx");
        save_world_to_path(&tmp_path, &data).expect("save should succeed");
        let loaded = load_world_from_path(&tmp_path).expect("load should succeed");
        let _ = std::fs::remove_file(&tmp_path);

        assert_eq!(loaded.seed, 424242);
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
            },
            chunk_edits: edits,
        };

        assert_eq!(SAVE_VERSION, INVENTORY_EXTENSION_VERSION);
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
            },
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

        valid[FIRST_EDIT_BLOCK_OFFSET] = BlockId::Stone as u8;
        valid[FIRST_EDIT_INDEX_OFFSET..FIRST_EDIT_INDEX_OFFSET + 2]
            .copy_from_slice(&40000u16.to_le_bytes());
        assert!(matches!(
            load_world_from_bytes(&valid),
            Err(PersistenceError::InvalidLocalIndex(40000))
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
        }
    }

    fn sample_data(player: SavedPlayerState) -> WorldSaveData {
        WorldSaveData {
            seed: 2024,
            time_of_day: DEFAULT_TIME_OF_DAY,
            player,
            chunk_edits: ChunkEditMap::default(),
        }
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
        assert_eq!(bytes.len(), V4_EXTENSION_OFFSET + V4_EXTENSION_LEN + 4 * 3);
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
        bytes.push(BlockId::CoalOre as u8);
        let iron_index = linear_index(4, 5, 6) as u16;
        bytes.extend_from_slice(&iron_index.to_le_bytes());
        bytes.push(BlockId::IronOre as u8);

        let loaded = load_world_from_bytes(&bytes).expect("v1 must migrate, not fail");

        assert_eq!(loaded.seed, 777);
        assert_eq!(
            loaded.time_of_day, DEFAULT_TIME_OF_DAY,
            "v1 predates game time and migrates to noon"
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
                    bytes.push(stack.block as u8);
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
        assert_eq!(
            loaded.time_of_day, DEFAULT_TIME_OF_DAY,
            "v2 predates game time and migrates to noon"
        );
    }

    #[test]
    fn test_v3_save_migrates_hotbar_into_inventory_and_defaults_new_state() {
        let hotbar = hotbar_sample();
        let bytes = hand_built_v3_bytes(&hotbar);
        let loaded = load_world_from_bytes(&bytes).expect("v3 must migrate, not fail");

        assert_eq!(loaded.time_of_day, 4321);
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
        set_v2_slot(&mut bytes, 0, BlockId::Air as u8, 5);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // A non-empty block id may not carry count 0.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Grass as u8, 0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));

        // Bedrock cannot be carried.
        let mut bytes = valid_v3_bytes();
        set_v2_slot(&mut bytes, 0, BlockId::Bedrock as u8, 1);
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
        set_v2_slot(&mut bytes, 0, BlockId::CoalOre as u8, MAX_STACK_COUNT + 1);
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
            Err(PersistenceError::UnsupportedVersion(5))
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
        assert_eq!(bytes[header_slot], BlockId::Air as u8);
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
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(
            load_world_from_bytes(truncated),
            Err(PersistenceError::Io(_))
        ));
    }

    #[test]
    fn test_v4_rejects_trailing_bytes() {
        let data = sample_data(sample_player());
        let mut bytes = Vec::new();
        save_world_to_bytes(&mut bytes, &data).unwrap();
        bytes.push(0);
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidPlayerState)
        ));
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
        bytes[V4_EXTENSION_OFFSET + 1] = BlockId::Air as u8;
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
        bytes[V4_EXTENSION_OFFSET + 1] = 9;
        assert!(matches!(
            load_world_from_bytes(&bytes),
            Err(PersistenceError::InvalidMaterial(9))
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
